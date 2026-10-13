# TASK：最简可玩版——两只真性格生物在终端对打

## ⚠️ 必须使用以下 Superpowers 技能

- **test-driven-development**：先写测试，红→绿，重构
- **systematic-debugging**：遇 bug 先根因

## ⚠️ 测试用例危险清点（写任何测试前必须执行）

1. 特有危险：无生产账号/外部数据。涉及进程 abort 雷：**wasmtime 25 Windows 下沙箱 out-of-fuel trap = 进程 abort**（.cc-bugs.md #5 已实录）——所有测试的沙箱 fuel 必须给足，禁止故意耗尽 fuel 的用例；如需测 fuel 边界用 host 侧构造，不许在真沙箱里跑穿。
2. 负面路径必须有：移动越界/找不到敌人/攻击超限，都要有用例。
3. 状态重置：每用例独立 world/sandbox_map。
4. 计数用动态值。
5. 每用例至少一个 assert。

## 项目上下文

项目路径：D:/projects/CodeWorld
类型：Rust workspace，基线 **348 测试全绿**（git be52805），cargo 1.99.0（PATH 无 cargo 时用 node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs 转发，文件若不存在自行重建：node 前置 ~/.cargo/bin 后 spawn cargo.exe）
关键文件：
- crates/arena/src/sandbox.rs：make_linker 里现有 import 仅 2 个（env.log / env.attack_at(x,y,ptr,len)->i32）；call_on_tick 无参数
- bin/codeworld/src/main.rs：spawn_test_entities（现在塞 2 个占位 PLAYER_WAT 实体 @(0,10) @(5,20)）、create_test_entity、tick 循环、EntitySandboxMap::run_player_code（每 tick 复位 fuel）
- crates/core/src/pending.rs：PendingChange::Spawn/Destroy/SetPos/SetVelocity 已有，阶段二 a 步统一应用
- crates/core/src/entity.rs：Entity::new(id, player_id, name, pos, Appearance::new(glyph,color,size), tick, 指令配额)
- docs/architecture.md 3.1/3.2 节：玩家代码导出/import 接口设计（以此为准）；4.2 攻击语义铁律（上一任务刚写入：攻击只伤代码与算力，不碰生存点）
- .cc-bugs.md：历史坑全在，开工先读
- .cc-context.md：项目地图+数据流图

## 问题描述（现状缺口）

主程序能跑 tick/渲染/攻击结算，但世界里的两个实体用占位代码（只会活着+log 42），从未有真实行为。examples/*.as 是伪代码（不接编译链，不使用）。玩家代码缺三个接口：不知道自己在哪、不知道敌人在哪、不能移动。**目标：让两只真性格生物在黑窗口里真的打起来，土豆双击就能看。**

## 需求（三块）

### A. 新增玩家 import 接口（对齐 architecture.md 3.2 节命名，若文档有出入以文档为准并在报告标注）
1. `get_pos() -> (f64, f64)` 自己的坐标（写入沙箱 memory 或双返回值，实现自定）
2. `scan() -> 敌人信息`：MVP 最简=返回最近敌人的 (x, y) 坐标（屏幕可见层感知，上帝视角，单敌人）。数据放 memory 的编解码方案自定（参考 attack_at 的 ptr/len 内存读写模式）
3. `move_by(dx: f64, dy: f64) -> i32`：相对移动，经 PendingChange::SetPos 队列走（阶段二应用，不是即时改坐标——与攻击排队同模式）
- 每个 import 都记 api_calls 元数据（与 log/attack_at 一致）+ tracing 日志
- 注意 borrow：import 回调里拿不到 world——参照现有模式，sandbox 侧收集请求、阶段二统一应用（move_by 排进 pending 变更队列的通路需要设计，main.rs 编排）

### B. 两只真性格生物（手写 WAT，与项目现有测试同模式）
替换 spawn_test_entities 的占位实体：
1. **攻击者 "A"**（红色）：开局 scan 找敌人 -> 逐 tick move_by 逼近（每 tick 一步，步长自定建议 1.0）-> 距离进攻击范围（自定建议 ≤1.5）后每 tick attack_at(敌坐标, 空 payload)（空 payload=默认击杀 WAT，走上一任务修好的两阶段结算）
2. **防御者 "D"**（青色）：原地站立（不移动），导出 on_attack 返回 true=拦截（有防守）；再加简单反制策略自定（如每 N tick 主动攻击攻击者一次，N≥3）
3. 两者都导出 life()=true、on_tick
4. WAT 手写内嵌 Rust 常量（现有模式），加中文注释说明每个函数的策略
5. 胜负演进：A 磨 D 的防守（fuel），D 拦截但配额有限/或反制打死 A——**两种结局都要能发生**（测试模拟两种参数路径），日志+画面可见

### C. 可玩性包装
1. 战斗日志：现有 [deliver_attacks]/[settle_attacks] 日志已中文，补关键节点 info 日志：开局布阵、A 锁定目标、进入攻击范围、D 拦截计数、击杀发生、掠夺转移
2. `start.bat`（项目根目录）：删除旧 db（codeworld.db 之类）开新世界 + cargo run --release -p codeworld + 结束后 pause。双击即看打架。release 模式编译慢，加 --release 或不加自定并说明
3. docs/progress.md 追加本次进展；.cc-context.md 数据流图更新（新增 move_by/scan/get_pos 通路）

## 验收标准

1. `cargo test --workspace` 全绿，348 基线零回归 + 新增测试覆盖：三个新 import 的单元测试（get_pos 返回真坐标/scan 返回最近敌/ move_by 排队且阶段二生效）+ 两只生物的行为测试（A 逼近轨迹正确/D 拦截计数/攻击发生且走两阶段结算/燃料继承正确）
2. `cargo clippy --workspace --tests -- -D warnings` 零警告
3. 实跑冒烟：cargo run -p codeworld 跑 ≥5 tick（CI 式：超时限制内自动退出），stderr 日志能看到：A 移动轨迹、锁定、进入范围、攻击排队、D 拦截或被击杀、（若击杀）掠夺转移。冒烟方式自定（如临时脚本 timeout+日志断言），报告里贴日志摘录
4. 画面：终端 ANSI 渲染两个不同颜色字符（A 红 D 青）位置随 tick 变化（渲染层已好，实体 glyph/color 设对即可；验证=日志坐标变化，不要求截图）
5. start.bat 存在且逻辑正确（删旧库+跑起来+pause）
6. 攻击语义铁律不违反：新 import 与生物代码零 survival 操作
7. .cc-bugs.md 若踩新坑追加；报告四节齐全（经验教训/失败路径/gstack注入/环境变化有则写）

## ⚠️ 已知信息（不需要查，直接用）

- wasmtime 25 fuel trap 会 abort 进程（坑#5）：测试 fuel 给足，别跑穿
- attack_at 空 payload = DEFAULT_KILL_WAT（life()=false 默认击杀），已有
- PendingChange::SetPos { entity, pos } 已有 apply 通路（阶段二 a 步）
- WASM 线性内存读写模式：read_payload_from_caller（sandbox.rs）是现成参考
- call_on_tick(handle) 无参数——scan 的结果不传入 on_tick，玩家代码每次 on_tick 自己调 scan import 获取
- create_test_entity 的 Appearance("@" 红色) 是占位，新生物各自设置 glyph/color
- examples/ 目录伪代码不动（README 已说明是设计文档性质）
- Windows 下 cargo test 输出 GBK 无关，测试断言在 Rust 内部，无编码问题

## ⚠️ 长任务状态外置

.cc-progress.md 每步追加；.cc-bugs.md 踩坑追加；.cc-context.md 数据流图变更时更新（新增 move_by/scan/get_pos 三条通路必须画进去）。

## ⚠️ 日志要求

新 import 每个都要日志（[get_pos]/[scan]/[move_by] 前缀，含 entity_id 与关键值）。战斗节点日志见验收#3。

## ⚠️ Agent 前置准备项（开工前自检）

1. 任务书/CLAUDE.md/.cc-context.md/.cc-bugs.md 可读
2. cargo 可用（或 cc-cargo.mjs 转发链可用）
3. 验收判据：7 条全部有测试/产物/日志对应
4. 回滚锚点：git be52805 干净

## ⚠️ 硬 Gate（任一不过 = 任务失败）

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## ⚠️ 完成后必须执行自审查循环（最多3轮）

第1轮：会话内 Agent 子代理 ×2（**不要用 claude -p --model 外部子代理——glm-5.1/sonnet 实测全挂，见上一任务环境变化**）：
1. 代码质量：审查最近 commit（逻辑/内存安全/borrow/边界）
2. 需求匹配：对照本任务书 7 条验收逐条核
收敛规则同模板：第1轮有 P0/P1 修复后第2轮确认；第2轮无则收工；最多3轮。

## 交付物

1. 中文 commit（feat: 最简可玩版——两只生物对打）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（commit SHA/测试结果标等级/四节必填）

注意：不要弹选项让用户选，直接按任务书执行；信息缺失超3次试错即停写报告。
