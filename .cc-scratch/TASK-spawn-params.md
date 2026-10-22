# TASK：spawn 新参数批——出生数据包 blob + 出生重叠拒绝锚 + 出生初速（CodeWorld kanban t_4b524940）

## ⚠️ 必须使用以下 Superpowers 技能

- **test-driven-development**：先写测试（红）→ 实现（绿）→ 重构。测试不通过不算完。

## ⚠️ 测试用例危险清点（写任何测试前必须执行）

先在 `.cc-context.md` 写一节「危险清点」：

1. 本项目无生产账号/支付/外部网络——危险项主要是我方并行施工冲突（见下）与 wasmtime fuel（测试 fuel 给足，禁故意耗尽用例——out-of-fuel 在 Windows 是进程 abort）
2. 负面路径必须有：超速拒绝/恰等极速放行/NaN 拒绝/blob 超限拒绝/空 blob 合法/容量不足返回负值——不许全 happy path
3. 有状态用例（往 world 塞实体）逐测试新建 WorldState，不共享
4. 计数断言用动态值，禁写死总测试数
5. 每条用例至少一个 assert

## 🔴🔴 并行施工警告（本仓最高风险，先读三遍）

**另有 7 个并行 worker 在同一工作树 D:/projects/CodeWorld 同时施工**（D-003 motion 第10参 / D-016 地形 / D-007 小刀批 / 协议小刀 / 文档同步等）。他们的改动可能随时落盘（含未提交工作区改动）。

铁律：
1. **每次 Edit 前重新 Read 目标文件最新态**，禁止凭任务书里的行号/旧锚点盲改——行号已漂移多次
2. **只改本任务清单列出的文件与改动面**，其他文件里看到的任何问题（包括测试失败）不修不动，报告里记录
3. **已知预存红测试**：`tests::process_spawns_dynamic_motion_birth_precheck_uses_circle`（bin codeworld）当前因 D-003 的 WAT 生成器 bug 失败（"0.75.0" 双点）。这是兄弟卡的地盘，**禁止修复它**。你的验收口径见硬 Gate 节
4. git commit 只提交你自己的改动文件（git add 具体文件名，禁 `git add -A`——会把别人的工作区改动卷进来）

## 项目上下文

- 项目路径：`D:/projects/CodeWorld`（Rust workspace + wasmtime WASM 沙箱）
- 项目宪法：CLAUDE.md + docs/decisions.md（能力 vs 规则：框架只修路不跑车；mass 等物理参数永不参与攻击结算）
- cargo 全路径：`C:/Users/Administrator/.cargo/bin/cargo.exe`（PATH 里没有；或用转发器 `node "$LOCALAPPDATA/Temp/cc-cargo.mjs" <args>`）
- 关键文件（改动面）：
  - `crates/arena/src/sandbox.rs`：PendingSpawn 结构 + env.spawn import（现在 10 参：code_ptr/code_len/x/y/glyph_ptr/color_ptr/radius/mass/binding/motion）+ 测试 WAT 生成器（spawner_wat/spawner_binding_wat/spawner_full_wat）
  - `bin/codeworld/src/main.rs`：process_spawns（阶段二造物）、EntitySandboxMap（create/collect_pending_spawns）、测试生成器 birth_spawner_wat/birth_motion_spawner_wat、CREATION_LOG_WAT
  - `players/pioneer.wat`（真玩家，spawn 调用点）+ `players/pioneer.wat.txt`（玩家模板副本，需同步）
  - `docs/api.md`（spawn 节 + import 清单「已开放 import（12 个）」）
  - `docs/dev-plan.md`（D-018 条目工程清单一行微调）、`docs/game-menu.md`（1.7 常数总账加一行）、`docs/progress.md`（收工追加一行）
- 可复用的既有零件（先读再仿，勿重造）：
  - `read_payload_from_caller(caller, ptr, len) -> Vec<u8>`（sandbox.rs，attack_at/shoot 同款读内存）
  - `write_bytes_to_caller(caller, ptr, bytes, cap) -> usize`（sandbox.rs，probe 同款写内存，原子防半截）
  - `codeworld_core::physics::WORLD_MAX_SPEED`（=25.0，极速常数唯一定义处）
  - 速度校验判据抄 env.shoot import（sandbox.rs 内）：有限性 + `speed2 > max*max` 严格大于拒绝（恰等放行）
  - `Entity::set_velocity((f64,f64))` / `entity.velocity()`（core entity.rs 已有）
  - D-003 刚落地的 motion 第 10 参全链路（PendingSpawn.motion / clamp_motion / spawner_full_wat）——你的参数追加在它后面，模式照抄

## 需求（三项，语义已定死，照此实现）

### ① spawn 加出生初速 vx/vy（第 11/12 参，f64 各一）

- import 层校验：vx/vy 必须有限（NaN/inf 拒绝，返回 0 不入队）；模长 > 世界极速 25（WORLD_MAX_SPEED，经 SandboxManager 装配的 max_speed，与 shoot 同源）→ 拒绝返回 0 + warn 日志；**恰等于 25 放行**（严格 > 判据，与 shoot 完全同口径）
- 合法 → PendingSpawn 记 (vx,vy)，阶段二 process_spawns 里 `entity.set_velocity((vx,vy))` 写进造物实体
- **位置积分（造物被初速带着飞）明确不做**——那是 D-018「通用有速度实体物理」的活（独立 kanban 卡），D-003 的 dynamic 造物速度矢量阻力/极速钳制已在 physics 落地，你只负责"出生时把速度写进数据 + 发射口钳制"。api.md 里如实写明当前生效范围
- static 造物（motion=0）声明初速：合法照存（velocity 字段所有实体都有，static 物理不理它），api.md 注明"static 钉死不吃速度物理，初速仅对 dynamic 造物有物理意义"

### ② spawn 加出生数据包 blob_ptr/blob_len（第 13/14 参，i32 各一）

立法语义（dev-plan D-017 挂靠段，2026-10-20 拍板）：**世界不解释内容**（母体传讯/代际遗传/子弹带任务参数），通道归世界内容归玩家——修路不跑车的数据版。上限 64KB。

- import 层：blob_len < 0 或 = 0 → 空数据包（合法，无包语义）；0 < blob_len ≤ 65536 → 从 caller memory [blob_ptr..blob_ptr+blob_len] 读字节（read_payload_from_caller 同款）；blob_len > 65536 → **整次 spawn 拒绝**（返回 0 + warn，在读内存之前拦下防大拷贝）——不裁剪，裁剪数据包=偷改玩家数据
- 常数：`pub const MAX_SPAWN_BLOB_LEN: usize = 64 * 1024;`（sandbox.rs，与 MAX_META_LEN/MAX_PAYLOAD_LEN 同风格）
- PendingSpawn 加 `blob: Vec<u8>`；阶段二造物诞生时把 blob 写进造物沙箱的 SandboxData（新字段 `birth_blob: Vec<u8>`，造物自己随时可读）
- **新 import `env.birth_blob(ptr: i32, cap: i32) -> i32`**（第 13 个玩家 import）——造物代码读自己的出生数据包：
  - 返回 0 = 无数据包（不写内存）
  - 返回正数 N = 已把 N 字节写入 memory[ptr..ptr+N]（原子：能整体写入才写，用 write_bytes_to_caller 同款防半截语义）
  - 返回负数 -N = 数据包长 N 但 cap 不足或指针非法，未写任何字节（warn 日志区分两种原因；玩家按 64KB 上限备缓冲一次必成）
  - 玩家（players/*.wat 进场）与弹丸无出生数据包 → 返回 0
  - 生命周期：随沙箱存活（每拍可读），沙箱不持久化故跨重启不保留——api.md 注明
- 日志：spawn 排队 info 行加 blob_len 与 vx/vy；birth_blob import 读路径加日志（新函数必须有日志）

### ③ 出生重叠拒绝+日志（已实现，验锚+补测）

process_spawns 已有出生预检（#030：造物身体与任何存活身体重叠 → 该次 spawn 失败 + warn 日志 + 不诞生不占名额，下拍可重试；身体判据按 motion 分流：static=出生格 1×1 AABB / dynamic=声明 radius 圆）。

你的活：
- 核实 static AABB 拒绝路径有测试锚（出生点压在创造者格上 → 不诞生）
- 核实"相邻不重叠（挪开半格/一格）→ 正常诞生"的边界锚存在；缺则补
- dynamic 圆判据的测试锚由 D-003 卡负责（那个已知红测试就是它），你只确认不重复建设

## 调用点同步（破坏性变更，同 commit 改完）

spawn 从 10 参变 14 参，**所有 env.spawn import 与调用点同一 commit 内补齐**，漏一处该 WAT 进场即 translation error：

1. `players/pioneer.wat`：import 签名 + 调用处补 `(f64.const 0.0) (f64.const 0.0) (i32.const 0) (i32.const 0)`（零初速/无数据包，行为不变）
2. `players/pioneer.wat.txt`：同步成与 pioneer.wat 一致的 14 参（玩家模板，旧签名=玩家陷阱；注意该文件 CRLF 行尾，Edit 保持）
3. `crates/arena/src/sandbox.rs` 测试生成器（spawner_wat / spawner_binding_wat / spawner_full_wat 及所有内嵌 spawn import 的测试 WAT）
4. `bin/codeworld/src/main.rs` 测试生成器（birth_spawner_wat / birth_motion_spawner_wat 及内嵌 WAT）
5. `docs/api.md` spawn 节：签名、参数表加 4 行（vx/vy/blob_ptr/blob_len）、示例 WAT 补 4 个实参、加 `birth_blob` import 节（第 13 个 import，含返回值语义表）、「已开放 import（12 个）」改 13 个

grep 自查命令（Gate 前跑）：在仓库根 grep 所有含 `"spawn"` import 声明的 .wat/.rs 文件，逐一确认签名是 14 参（i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32）。

## 文档同步（小刀，append 为主）

- `docs/dev-plan.md` D-018 工程清单第②项「spawn 加第 10 参出生初速」改注：初速已提前落地为第 11/12 参（D-003 motion 占第 10 参），D-018 剩余=③通用有速度实体物理等
- `docs/game-menu.md` 1.7 常数总账：出生参数域行补「出生数据包 ≤64KB（超限拒绝不裁剪）」
- `docs/progress.md`「已完成」顶部追加一行收工记录（格式仿现有行，含测试计数）

## 验收标准（全部满足才算完）

1. spawn 14 参全链路：import 校验（极速严格>拒绝/恰等放行/NaN 拒绝/blob 64KB 超限拒绝/空 blob 合法）→ PendingSpawn → process_spawns 落实体（velocity 写入）→ 造物 on_tick 经 env.birth_blob 原样读回字节（端到端字节一致）
2. birth_blob 三态返回（0 无包 / 正数写入 / 负数容量不足）各有测试锚
3. 重叠拒绝 static 路径 + 相邻放行边界有测试锚
4. pioneer.wat / pioneer.wat.txt / 全部测试生成器 / api.md 同步，grep 无漏旧签名
5. 新函数/新数据流两端有日志（tracing），风格与既有 import 一致
6. TDD：新测试先行（红→绿），新增测试数 ≥ 12，其中负面路径 ≥ 6

## ⚠️ 硬 Gate（任一不过 = 任务失败，不准进入审查）

```bash
cd D:/projects/CodeWorld
C:/Users/Administrator/.cargo/bin/cargo.exe test -p codeworld-arena
C:/Users/Administrator/.cargo/bin/cargo.exe test -p codeworld-core
C:/Users/Administrator/.cargo/bin/cargo.exe test -p codeworld
C:/Users/Administrator/.cargo/bin/cargo.exe clippy -p codeworld-arena -p codeworld -- -D warnings
C:/Users/Administrator/.cargo/bin/cargo.exe check --workspace
C:/Users/Administrator/.cargo/bin/cargo.exe test --workspace
```

全量 workspace test 口径：**全部通过，或仅 `tests::process_spawns_dynamic_motion_birth_precheck_uses_circle` 一个失败且你确认与本任务改动无关（git diff 不含该测试与其生成器中 D-003 新增段）**。若届时它已绿，则必须全绿。其余任何失败 = 你的责任，修。

## 完成标准

1. 硬 Gate 全过
2. `git add <你的文件清单> && git commit -m "feat: spawn出生参数批——出生初速vx/vy(极速25拒绝)+出生数据包blob≤64KB+birth_blob读回import+重叠拒绝测试锚(14参破坏性变更同步全部调用点)"`（commit 信息可微调但要素齐全）
3. 自审查循环（见下）
4. 报告写入 `D:/projects/CodeWorld/.cc-scratch/cc-report-spawn-params.md`（专用路径，别写 $LOCALAPPDATA/Temp/cc-report.md——兄弟任务的 CC 会互相覆盖）

## ⚠️ 完成后必须执行自审查循环（最多 2 轮，每轮 2 个只读子代理）

第 1 轮（commit 后）：两个只读审查并行——

1. 代码质量审：`claude -p "审查最近一次 commit 的代码质量：逻辑正确性/边界处理/与既有 import 风格一致性/测试断言质量。输出 P0/P1/P2 清单。" --allowedTools "Read,Bash(git *)" --disallowedTools "Bash(git commit *),Bash(git push *),Edit,Write" --max-turns 20`（模型可用 deepseek-v4-pro/minimax-m3 则指定，不可用则默认模型+一个 Agent 工具子审）
2. 需求匹配审：`claude -p "审查最近一次 commit 是否满足需求：<上述验收标准 1-6 逐条>。逐条判定做了没/做多了没。" --allowedTools "Read,Bash(git *)" --disallowedTools "Bash(git commit *),Edit,Write" --max-turns 20`

收敛：第 1 轮无 P0/P1 → 第 2 轮确认一轮 → 完成；有 P0/P1 → 修复 → 第 2 轮增量审；第 2 轮仍有 → 记录进报告收工，不进第 3 轮。

## ⚠️ 日志要求

新增/修改函数必须有日志（import 校验拒绝 warn、排队 info、process_spawns 诞生 info、birth_blob 读回 info/debug）；数据流两端（spawn 排队 ↔ 造物诞生 ↔ birth_blob 读回）都有日志可串。格式仿既有 `[spawn]`/`[process_spawns]` 前缀风格。

## ⚠️ 已知信息（不需要查，直接用）

- 世界极速 = `codeworld_core::physics::WORLD_MAX_SPEED` = 25.0（写死的唯一真值，别手写 25）
- SandboxManager::new_with_max_speed 装配的 max_speed 进 shoot 闭包的模式——spawn 的极速校验用同一装配值（研究 shoot import 闭包怎么拿 max_speed 的，照抄）
- read_payload_from_caller 对 ptr<0/len<=0/越界返回空 Vec（现成语义，blob 空包=合法所以直接用）
- Entity 已有 set_velocity/velocity；velocity 字段参与 serde 快照无需加 default
- 造物沙箱创建路径：process_spawns → sandbox_map.create(entity_id, &wasm, fuel) → sandboxes HashMap；之后可 `sandboxes.get_mut(&id).store.data_mut()` 写 SandboxData
- WASM 侧调用 14 参 import 用栈式直推即可（pioneer.wat 现有 10 参调用就是栈式，照着加 4 个）
- api.md spawn 节在 `### spawn(...)` 标题下，参数表/作废条款/示例 WAT 结构现成

## ⚠️ 发现信息缺失时的处理规则

缺信息（如既有代码与任务书描述不符）→ 试错 ≤3 次仍不明 → 停，写进报告「缺少前置项」等 Hermes 答。禁止自行大改无关模块凑答案。

## ⚠️ Agent 前置准备项（开工前自检）

1. 任务书/CLAUDE.md/decisions.md 可读 ✅
2. cargo 可用（全路径见上）✅
3. 验收判据可机器验证（Gate 命令+grep）✅
4. 回滚锚点：git log 记住当前 HEAD sha，你的 commit 之前的兄弟未提交工作区改动**不属于你**，别动

## ⚠️ gstack 注入信息 / 失败路径 / 经验教训 / 环境变化

报告末尾四节必填（无则写「无」）：gstack 注入信息（改动文件/技术决策/踩坑/项目级约束）、失败路径（试了不管用的方案）、经验教训（非显而易见的问题）、环境变化（构建/测试环境变化）。

## 交付物

1. git commit（要素齐全的中文 commit 信息）
2. `D:/projects/CodeWorld/.cc-scratch/cc-report-spawn-params.md`：改动清单/测试结果（全量计数+已知红说明）/审查轮次结论/commit SHA/四节附录
