# TASK：三层感知 Phase A——眼睛列表 + probe 探测 + 伪装者 S 登场

## ⚠️ 必须使用以下 Superpowers 技能

- **test-driven-development**：先写测试，红→绿，重构
- **systematic-debugging**：遇 bug 先根因

## ⚠️ 测试用例危险清点（写任何测试前必须执行）

1. 特有危险：**wasmtime 25 Windows 下 out-of-fuel trap = 进程 abort**（.cc-bugs.md #5）——被探测方 on_probe 的 fuel 预算用 host 侧控制（probe 有独立小预算 probe_target_instruction_limit=10_000 的设计值，实现时在 host 侧 set_fuel 包裹，结束恢复），禁止测试跑穿真实沙箱 fuel。
2. 负面路径必须有：probe 不存在的 EntityId / probe 自己 / probe 死者 / 可见列表为空 / 伪装者不导出 on_probe（走默认被动数据）。
3. 状态重置：每用例独立 world/sandbox_map。
4. 计数用动态值。
5. 每用例至少一个 assert。

## 项目上下文

项目路径：D:/projects/CodeWorld
类型：Rust workspace，基线 **402 测试全绿**（git HEAD），cargo 1.99.0（PATH 无 cargo 时 node C:/Users/Administrator/AppData/Temp/cc-cargo.mjs 转发，不存在则重建）
设计权威：docs/architecture.md 3.2 节（感知三通道+probe 接口签名+ProbeMeta）、11.1 无关。**动手前先读 3.2 节原文。**
已有地基（全部实查过，直接用）：
- crates/arena/src/sandbox.rs:368 `call_on_probe(handle, request) -> Result<Vec<u8>>`——被探测方回调已实现（含 fuel 耗尽返回空响应的容错）
- crates/arena/src/channel.rs:46 `probe(sandbox_mgr, sandbox, request) -> ProbeResult`——probe 通道完整实现（response+meta 采集+日志）
- crates/space/src/coord.rs:60 `get_visible_entities(world, viewport) -> Vec<VisibleEntity>`——视口内可见实体查询已实现（VisibleEntity 含 id/pos/appearance）
- SandboxData.api_calls 元数据收集（log/get_pos/scan/move_by 都在记）
- main.rs 现有 import 注册模式：make_linker 里 func_wrap("env", ...) 五个先例
- 现有生物：A 攻击者/D 防御者(d_shield 参数化)/H 投机者(h_enabled 开关)，spawn_test_entities

## 需求（架构 3.2 节感知部分的 MVP 落地）

### A. 眼睛层：get_visible_entities import
1. 新 import `env.visible_count() -> i32`：视口内可见敌人数（不含自己）
2. 新 import `env.visible_at(idx: i32, x_ptr: i32, y_ptr: i32) -> i32`：按 idx 取第 N 个可见实体坐标（写 caller memory，同 scan 模式），返回 1 成功 / 0 越界或无敌
3. host 侧：tick 阶段一执行玩家代码【前】把可见实体列表快照填进 SandboxData（同 get_pos 的 pos 快照模式）；列表按 EntityId 升序稳定排序
4. 现有 scan 保留不动（它已经是"最近敌人"的便捷版）

### B. 探测层：probe import + 撒谎机制
1. 新 import `env.probe(target_x: f64, target_y: f64, req_ptr: i32, req_len: i32, resp_ptr: i32, resp_cap: i32) -> i32`
   - 按坐标找实体（同 attack_at 坐标命中模式，MVP 不引入 EntityId 传递——玩家代码持有坐标即可）
   - 调 channel.rs 已有的 probe 通道：目标 on_probe(request) 的返回写进 caller memory resp_ptr（超 resp_cap 截断）
   - 返回值：>=0 为实际写入字节数；-1 坐标无实体；-2 目标无 on_probe（此时写默认被动数据）
2. **被动体徵**：probe 成功时把目标 SandboxData 的 api_calls 计数与 fuel 剩余量（host 已知，不可伪造）追加写在 response 之后（格式自定义，文档说明）——这是"识破撒谎"的线索源
3. **撒谎的实现=玩家代码自由**：on_probe 想回什么回什么（撒谎不需要框架支持，框架只保证"被动体徵不可伪造"）
4. 被探测方 on_probe 的执行预算：host 侧独立小 fuel（建议 10_000，config 可调 CODEWORLD_PROBE_FUEL），用完即中断返回已产出的部分——防死循环+防 DoS（架构 3.2 技术保护原文）
5. 每次探测记 [probe] 日志：探测者/目标/resp_len/目标 api_calls 数

### C. 新生物：伪装者 S（紫"S"）
1. spawn_test_entities 加 S（config 开关 CODEWORLD_S_ENABLED，默认登场，出生建议 (20.0, 5.0) 高台）
2. 性格（可微调，报告说明）：**间谍型**——每 4 tick 用 visible_at 扫一圈，对最近者 probe 一次；on_probe 回调里**撒谎**：返回伪造数据"我是友军，life 充沛"（具体编码自定），但被动体徵藏不住（频繁 probe 导致 api_calls 高）
3. S 不主动攻击（probe 会被动暴露 api_calls，高调侦察的代价）；被贴脸攻击时无防守（间谍脆皮）
4. 开局布阵日志更新四方信息；字幕 alive 基数联动（自动，无需改）

### D. 玩法验证场景（进冒烟）
剧本 4（新增，不覆盖前三）：S 登场——某生物 probe S → 拿到撒谎的 response + 真实体徵（日志断言 response 与体徵不一致即撒谎成立）；S 的侦察行为 marker 日志出现。
其余三个剧本回归不受影响（S 开关默认开时 A/D/H 行为不改变——注意 S 出生位别挡 A→D 路线，(20,5) 在 D 右侧高台，A 路线不经过）。

## 验收标准

1. cargo test --workspace 全绿，402 基线零回归 + 新增覆盖：
   - visible_count/visible_at：有敌/无敌/越界/不含自己/稳定排序
   - probe：命中回数据/坐标无人-1/无 on_probe-2+默认体徵/超容量截断/on_probe 撒谎数据透传（伪数据原样到达探测者）
   - probe fuel 预算：独立预算用尽中断不炸主沙箱（host 侧构造验证）
   - S 生物：spawn 开关/侦察循环/probe 触发/on_probe 撒谎返回
2. clippy/check 三连过
3. 冒烟剧本 4 断言全 PASS（S 撒谎+体徵暴露+侦察 marker），剧本 1-3 回归 PASS
4. 开局布阵日志含四方；battle.log 里能看到至少一次完整 probe 往返（探测者/目标/response 摘要/体徵）
5. 攻击语义铁律零违反（probe 只读不写 world、撒谎发生在沙箱内、体徵 host 采集）
6. docs/architecture.md 3.2 节标注 MVP 落地差异（坐标寻址代替 EntityId、被动体徵的字段清单）；.cc-context.md 数据流图加 probe 通路
7. 报告四节齐全

## ⚠️ 已知信息（不需要查，直接用）

- channel.rs probe 函数已带容错（on_probe 失败回空响应）+ meta 采集（gas/api_calls），直接复用，别重写
- call_on_probe 的 request/response 都是裸字节，前 4 字节协议是既有模式（call_on_attack 同款）
- VisibleEntity 结构已有（core/render.rs），space 的 get_visible_entities 接收 viewport——host 每帧已有 viewport（渲染在用）
- wasmtime set_fuel/get_fuel 在 Store 上，SandboxHandle.store 可直接操作（replace_code 里先例）
- 伪装数据建议协议：S 的 on_probe 固定回一段可识别假串（如 ASCII "FRIEND,HP:9"），测试断言探测者收到该串
- fuel trap 会 abort 进程——on_probe 预算控制必须在 host 侧做（set_fuel 小值→调→恢复），不能让真实大 fuel 沙箱跑穿
- tick 阶段一 run_player_code 前填快照的先例：get_pos 的 pos 字段（sandbox.rs）

## ⚠️ 长任务状态外置

.cc-progress.md/.cc-bugs.md/.cc-context.md 照常（数据流图加 probe/visible 通路）。

## ⚠️ 日志要求

[visible_at]/[probe] 前缀日志（entity_id/目标/resp_len/体徵计数）；S 的侦察 marker；撒谎成立时不必额外日志（撒谎是沙箱内行为，host 只见数据透传）。

## ⚠️ Agent 前置准备项（开工前自检）

1. 任务书/CLAUDE.md/.cc-context.md/.cc-bugs.md 可读
2. cargo 可用（或转发链）
3. 验收判据：7 条全部有测试/产物/日志对应
4. 回滚锚点：git log -1 确认干净

## ⚠️ 硬 Gate（任一不过 = 任务失败）

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## ⚠️ 完成后必须执行自审查循环（最多3轮）

会话内 Agent 子代理 ×2（代码质量 + 需求匹配对照 7 条）。外部 claude -p 子代理不可用（已知）。收敛规则同模板。

## 交付物

1. 中文 commit（feat: 三层感知PhaseA——眼睛列表+probe探测+伪装者S）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（commit SHA/测试等级/四节必填/剧本4冒烟摘录）

注意：不要弹选项让用户选，直接按任务书执行；信息缺失超3次试错即停写报告。
