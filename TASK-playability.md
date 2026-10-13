# TASK：可玩性三连——战况字幕 + 翻盘实验 + 第三只生物混战

## ⚠️ 必须使用以下 Superpowers 技能

- **test-driven-development**：先写测试，红→绿，重构
- **systematic-debugging**：遇 bug 先根因

## ⚠️ 测试用例危险清点（写任何测试前必须执行）

1. 特有危险：**wasmtime 25 Windows 下沙箱 out-of-fuel trap = 进程 abort**（.cc-bugs.md #5）——所有测试 fuel 给足；多生物混战测试里 3 个沙箱同 tick 跑，配额全部 100 万级。
2. 负面路径必须有：字幕在无敌人时（全场剩1个）、第三者死亡后、A 找不到目标时的显示；混战里攻击落空/死亡攻击者作废（已有机制复验）。
3. 状态重置：每用例独立 world/sandbox_map。
4. 计数用动态值，禁止硬编码总数。
5. 每用例至少一个 assert。

## 项目上下文

项目路径：D:/projects/CodeWorld
类型：Rust workspace，基线 **368 测试全绿**（git 5f24ae6），cargo 1.99.0（PATH 无 cargo 时 node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs 转发，文件不存在则重建：node 前置 ~/.cargo/bin 后 spawn cargo.exe）
关键文件与现状（全部实查过）：
- bin/codeworld/src/main.rs:820-861 —— spawn_test_entities：A 红attack@(5,0)、D 青defender@(15,0)、d_shield=10 硬编码；defender_wat(shield) 已参数化（盾=拦截配额，每拦-1，每4tick反制）；ATTACKER_WAT 常量@884
- bin/codeworld/src/config.rs —— Config：tick_interval_ms/term_rows(50)/term_cols(80)/viewport 等，全部 CODEWORLD_* 环境变量可覆盖
- crates/arena/src/sandbox.rs —— env.log（玩家 marker 日志）/get_pos/scan/move_by/attack_at 五个 import；SandboxData.api_calls 元数据
- crates/render/ —— TerminalBuffer/Viewport/ANSI 渲染（color_to_ansi、flush_diff 按差异重绘）
- docs/architecture.md 4.2 —— 攻击语义铁律（攻击只伤代码与算力不碰生存点；字幕只读展示不得违反）
- .cc-bugs.md/.cc-context.md/.cc-progress.md —— 开工先读，踩坑追加

## 问题描述（三件事，用户拍板"先把1,2,3做了"）

### ① 战况字幕（顶部实时战况条）
终端画面顶部固定一行字幕，实时显示战局，让战斗过程肉眼可读：
- 内容（MVP 够用即可）：`tick=12 | A→D 距离=2.4 | D盾≈7 | 存活:3`
- 实现约束：
  - 盾量在 D 的 WAT 全局变量里，host 不可见——**不能为字幕破坏沙箱隔离**。两条正路任选其一（自定，报告说明选型理由）：a) 玩家代码主动上报（新 env import `report(v)` 或复用 env.log 的 marker 语义，host 侧解析 player_log marker 更新字幕状态——推荐，零新机制）b) 字幕不显示盾量，只显示 host 可知信息（距离/存活数/上次拦截方）——更简
  - 字幕走渲染层：写进 TerminalBuffer 顶部行（render crate 加一个"字幕模块"或 main.rs 渲染前写 buffer 均可，架构上字幕=展示不是游戏状态，不得写回 world）
  - 刷新节奏随 tick 渲染，位置固定屏幕第一行，用视口外信息不挤占战场
- 验收：跑起来顶部字幕随 tick 更新（距离变化、存活数变化、D 拦截时盾量变化——若实现选项 a）；测试断言字幕内容生成函数（纯函数可测）

### ② 翻盘实验（参数化开局 + 双剧本演示）
- d_shield 从硬编码 10 改为 Config 可配（环境变量 CODEWORLD_D_SHIELD，默认 10），spawn_test_entities 接收参数
- start.bat 增加开场提示（echo 现有剧本说明：默认 D 赢；改环境变量 CODEWORLD_D_SHIELD=1 后 A 磨穿盾反杀）
- 新增第二个入口 start-reversal.bat：`set CODEWORLD_D_SHIELD=1` 后同 start.bat 流程（同样 ASCII only——**GBK 编码坑已踩两次，bat 禁止中文**）
- 验收：测试覆盖 d_shield 透传（Config 解析）；实跑两个 bat 各出对应结局（冒烟日志断言：默认版 D 击杀 A；reversal 版 A 击杀 D——剧本见 main.rs:820-824 注释）

### ③ 第三只生物 H（黄"H" 搅局者）多方混战
- 新增 third_creature WAT，出生位 (10.0, 8.0)（A、D 中场上空，落地后会受重力，行为自定但要有性格）
- 建议性格（可自定，报告说明）： opportunistic 投机者——scan 找最近敌人，距离>3 时观望（原地小幅游走 move_by 随机方向±0.5），距离≤3 时参战攻击；被攻击时 on_attack 50%概率拦截（用 tick 奇偶做伪随机，WAT 内可实现）
- spawn_test_entities 加第三个实体；开局布阵日志更新三方信息
- 验收：三方混战测试——H 的行为分支（观望/参战）有测试；A 或 D 被第三方击杀的链路跑通（复用两阶段结算）；字幕存活数从 3 变化
- start.bat 里三生物说明同步更新

## 验收标准（总）

1. `cargo test --workspace` 全绿，368 基线零回归 + 三块各有新测试
2. clippy/check 三连过
3. 实跑冒烟：默认剧本（D 赢）+ 翻盘剧本（A 赢）+ 混战剧本（三方交战，H 至少出刀一次），日志断言各自结局，报告贴摘录
4. 字幕实跑可见且随 tick 变化（冒烟日志或渲染 buffer 断言）
5. start.bat / start-reversal.bat 双击可用（ASCII only，GBK 免疫）
6. 攻击语义铁律零违反（字幕只读、盾上报不写 world、H 行为全走既有 import）
7. 报告四节齐全 + 环境变化有则写

## ⚠️ 已知信息（不需要查，直接用）

- 盾在 WAT global $shield（main.rs:935 defender_wat(shield) 生成），host 读不到——字幕盾量的两条正路见①
- env.log 的 marker 已有 player_log 日志通路（marker=7 拦截 / 8 盾尽 / 9 反制 / 2 进范围 / 3 移动中，各生物注释里都有定义）
- term_rows=50/term_cols=80，字幕放第 1 行不影响战场区
- Config 模式：CODEWORLD_ 前缀环境变量 + 默认值，config.rs:46 Config::load() 照抄模式加一个字段即可
- 落地实体（y>0）受重力会下落，H 出生 (10,8) 落地后 y=0 站稳——scan/attack 坐标匹配依赖地面稳定（.cc-bugs 相关教训）
- bat 中文=GBK 炸弹（已踩两次），两个 bat 全 ASCII
- 战斗节奏 tick 1秒/拍，混战三方后战斗时长会拉长，属预期

## ⚠️ 长任务状态外置

.cc-progress.md/.cc-bugs.md/.cc-context.md 照常（数据流图加 report/import 通路与字幕数据流）。

## ⚠️ 日志要求

字幕状态更新（距离/盾变化/存活数变化）info 日志一条（不要每 tick 刷屏——只在值变化时打）。H 的行为分支切换（观望→参战）info 日志。其余按项目规范。

## ⚠️ Agent 前置准备项（开工前自检）

1. 任务书/CLAUDE.md/.cc-context.md/.cc-bugs.md 可读
2. cargo 可用（或转发链可用）
3. 验收判据：7 条全部有测试/产物/日志对应
4. 回滚锚点：git 5f24ae6 干净（注意：start.bat 当前版本已含日志分离）

## ⚠️ 硬 Gate（任一不过 = 任务失败）

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## ⚠️ 完成后必须执行自审查循环（最多3轮）

会话内 Agent 子代理 ×2（代码质量 + 需求匹配对照本任务书 7 条）。**不要用 claude -p --model 外部子代理（glm-5.1/sonnet 实测全挂）**。收敛规则同模板。

## 交付物

1. 中文 commit（feat: 战况字幕+翻盘实验+第三只生物混战）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（commit SHA/测试结果标等级/四节必填/三剧本冒烟摘录）

注意：不要弹选项让用户选，直接按任务书执行；信息缺失超3次试错即停写报告。
