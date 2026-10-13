# TASK：攻击流程修复与两阶段结算（P0+P1+机制升级）

## ⚠️ 必须使用以下 Superpowers 技能

- **test-driven-development**：先写测试，红→绿，重构，测试不通过不算完
- **systematic-debugging**：bug 修复前先理解根因

## ⚠️ 测试用例危险清点（写任何测试前必须执行）

1. **本项目特有危险**：纯 Rust workspace 无生产账号/支付/外部数据，无危险操作类用例。仍需执行清点：涉及删除实体/销毁沙箱的用例，用独立 WorldState 实例，不共享全局状态。
2. **负面路径覆盖**：必须有失败/边界用例（非法 payload、死亡攻击者、超限 payload、自攻）。全 happy path = 红旗。
3. **有状态用例标注重置方法**：每个测试函数构造自己的 world/sandbox_map，不依赖执行顺序。
4. **计数用动态值**（results.len() 类），禁止硬编码总数。
5. **断言密度**：每条用例至少一个 assert，零断言不算测试。

## 项目上下文

项目路径：D:/projects/CodeWorld
项目类型：Rust workspace（终端沙盒游戏框架，"代码即现实"）
基线：333 测试全绿（git c97fa6b），Windows 原生工具链 cargo 1.99.0 已就绪
关键文件：
- crates/arena/src/sandbox.rs：SandboxManager、replace_sandbox、env.attack_at import、read_payload_from_caller
- crates/arena/src/attack.rs：PendingAttack、DEFAULT_KILL_WAT
- bin/codeworld/src/main.rs：EntitySandboxMap（attack_target/collect_pending_attacks）、process_attacks、plunder_survival、tick 阶段二
- docs/architecture.md 4.2 节：攻击流程权威设计
- 双模型独立审查报告：C:/Users/Administrator/AppData/Local/Temp/cc-review-req.md（先读，P0/P1 定位都在里面）

## 问题描述（三块，按优先级）

### A. P0 僵尸实体 bug（双模型审查共识）
attack_target（main.rs:156-166）先 remove 旧沙箱，replace_sandbox（sandbox.rs:142-153）先 drop(handle) 再 Module::new。payload 是非法 WASM 时编译失败返回 Err，旧沙箱已销毁、新沙箱未建、map 无条目，但实体仍存活。此后该实体永不可被攻击、代码永不运行，破坏"存活实体必有沙箱"不变量。

### B. P1 逻辑缺陷（审查发现）
1. process_attacks（main.rs:497-498 附近）只校验 target 存活，不校验 attacker。同一批攻击中攻击者已被击杀时，其排队攻击仍执行且掠夺的点数转入死者，随死亡清理蒸发（点数凭空消失）。
2. env.attack_at（sandbox.rs:92-99）不校验 payload 长度，最大可拷 64MB，且 Module::new 编译耗时不受 fuel 约束，可阻塞 tick。
3. attack_target（main.rs:160）换血时 fuel 取 fuel_limit 而非当前剩余量，每次被攻击回满配额，可被刷。

### C. 机制升级：攻击两阶段模型（用户拍板的设计）
当前：process_attacks 逐刀即时结算，B 先处理则 A 的沙箱被换血，C 后到的刀触发的 on_attack 已是 B 的代码——"谁被攻击"取决于程序内遍历顺序（随机），不是攻击者的选择。
要求：改为"先收件、再结算"两阶段：
1. 收件阶段：本 tick 全部 PendingAttack 按目标分组，对每个目标，其【当前沙箱】的 on_attack 逐刀被调用（同一防守者面对全部压力源）。收件期间不换血、不击杀。
2. 结算阶段：目标对所有刀表完态后，未被拦截的攻击才依次执行（换血/击杀）。多刀都未拦截时按攻击者 EntityId 升序固定顺序执行（确定序，消灭随机性）。
3. 防守消耗自然成立：每次 on_attack 消耗防守者 fuel，围殴=磨算力，此机制不需额外代码但测试要覆盖。

## 攻击语义铁律（用户 2026-10-13 拍板，写进 docs/architecture.md 4.2 节，所有实现不得违反）

1. 攻击只伤对方的**代码**（换血/覆写）与**算力**（防守烧 fuel），不碰生存点数。
2. 生存点数唯一变动路径：时间流逝衰减 + 击杀瞬间掠夺转移。攻击路径无任何 survival 写操作。
3. 血条/血量概念只允许存在于渲染层展示，不存在于数据模型。
4. 掠夺只属于击杀：无击杀=无掠夺（无吸血式）。

## 验收标准

1. 非法 WASM payload 攻击后：目标沙箱原样保留（测试：垃圾字节 payload → attack_target 返回 Err 或攻击失败，但 sandbox_map 中目标条目仍在且可继续 on_tick）。
2. 攻击者已死亡的排队攻击不执行（测试：同批 A 杀 B 后，B 之前排的对 A 的攻击不执行、无点数转移）。
3. env.attack_at 拒绝超限 payload（上限 1MB：超限截断或拒绝+warn 日志，二选一实现，日志含 attacker/actual_len）。
4. 换血时新沙箱 fuel 取旧沙箱剩余量而非 fuel_limit（测试：fuel 消耗过半的沙箱被换血后剩余量正确）。
5. 两攻一防场景（核心新测试）：B、C 同 tick 攻击 A，A 的 on_attack 被调用两次（同一原版沙箱），两刀全拦→A 完好；只拦 B→仅 C 的攻击执行；全不拦→按 EntityId 升序执行，先执行者的换血生效。
6. 上述全场景日志完整（攻击分组/逐刀拦截/结算顺序，格式 [process_attacks] 类前缀）。
7. docs/architecture.md 4.2 节补录：攻击语义铁律四条 + 两阶段模型说明 + 占有权说明（后到攻击触发的是当前占有者代码的 on_attack，防守权跟随占有权）。
8. workspace 333 基线无回归 + 新增测试全绿；clippy -D warnings 零警告。

## ⚠️ 已知信息（不需要查，直接用）

- 审查报告 cc-review-req.md 里 P1-1/P1-2/P2 各条的行号基于当前 HEAD（c97fa6b），改动后行号会漂移，以函数名定位。
- replace_sandbox 的"先建后拆"修法方向审查已给（sandbox.rs:152 前移 Module::new）；具体实现自行设计，测试先行。
- 攻击两阶段模型可能需要拆 process_attacks 为两个函数（收件/结算），模块边界不变（arena 不碰 world，main 编排），命名自行决定。
- fuel 剩余量读取：wasmtime Store::get_fuel，SandboxData 里有 fuel_limit 字段可参照。
- Rust 1.99 clippy 严格 lint（manual_is_multiple_of 等）易触发，注意。

## ⚠️ 长任务状态外置（多步任务必须执行）

- `.cc-progress.md`：每完成一步追加（含本次任务）
- `.cc-bugs.md`：失败方案追加
- `.cc-context.md`：数据流图变更时更新（攻击流程从"逐刀即时结算"改为"两阶段"，必须更新）

## ⚠️ 日志要求

按项目 CLAUDE.md 日志规范。本次重点：攻击分组数、每刀拦截结果、结算顺序、fuel 剩余量、超限 payload 拒绝。格式 [函数名] 描述: 值。

## ⚠️ Agent 前置准备项（开工前自检）

1. 任务书/CLAUDE.md/.cc-context.md 可读
2. cargo 环境可用（cargo --version 通过）
3. 验收判据：上述 8 条全部有对应测试或文档 diff
4. 回滚锚点：git c97fa6b，开工前确认 git status 干净

## ⚠️ 硬 Gate（任一不过 = 任务失败）

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```
（全 workspace 三连，基线 333 无回归 + 新增全绿）

## ⚠️ 完成后必须执行自审查循环（最多3轮，每轮2个子代理）

第1轮（全面审查）：
1. 代码质量审查：cd D:/projects/CodeWorld && claude -p --model glm-5.1 "审查最近 commit 的代码质量：逻辑正确性/安全/错误处理/性能/规范。输出 P0/P1/P2 清单。" --allowedTools "Read,Bash(cat *)" --disallowedTools "Bash" --max-turns 30
2. 需求匹配审查：cd D:/projects/CodeWorld && claude -p --model sonnet "审查最近 commit 是否满足需求。需求：攻击流程修复（僵尸bug/死亡攻击者/payload上限/fuel剩余量）+ 两阶段结算模型 + 攻击语义铁律四条写进架构文档。检查做了没/多做没/边界全没。输出 P0/P1/P2。" --allowedTools "Read,Bash(cat *)" --disallowedTools "Bash" --max-turns 30

注意：deepseek-v4-pro 和 minimax-m3 模型名在当前 CC 版本已不可用（实测 400 模型不存在），用 glm-5.1 和 sonnet 替代。

收敛规则：第1轮无 P0/P1 → 再跑1轮确认；有 → 修复 → 下轮确认；第3轮仍有 → 记录收工。改动跨 arena+codeworld 两 crate 属逻辑变更，不许跳过审查循环。

## ⚠️ 经验教训 / 失败路径 / 环境变化 / gstack 注入信息

完成后在 C:/Users/Administrator/AppData/Local/Temp/cc-report.md 写全四节（经验教训/失败路径/gstack 注入信息必填，环境变化有则写）。

## 交付物

1. 全部修复+测试+文档改动，中文 commit（fix: 攻击流程修复与两阶段结算）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（含 commit SHA、测试结果标证据等级、四节必填章节）

注意：不要弹出选项让用户选，直接按任务要求执行。发现信息缺失超3次试错即停，写报告说明。
