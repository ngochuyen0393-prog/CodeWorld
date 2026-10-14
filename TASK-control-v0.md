# TASK（轻装挡）：期三·操控层 v0——终端键盘操控（#022 首个落地）

## ⚠️ 必须使用 test-driven-development（红→绿）

## 宪法依据

decisions.md #022（双层玩家模型：操控=意图流经角色代码执行）+ #014（外部通道，操控通道是其首个应用）+ #021（占位制：意图照常过物理裁决）。

## 借鉴定案（roguelike 事实标准，crossterm 生态）

- raw mode 裸键输入（crossterm::terminal::enable_raw_mode）：按键即达、不回显、不等回车
- 独立输入线程 + 意图队列，tick 每拍 drain
- alternate screen + 退出全恢复
- 键位：方向键 + WASD 双支持（Vi 传统），q/Enter 退出，空格=攻击最近可见敌人
- 按键=排队本拍意图（回合制 roguelike 传统），不即时改坐标——物理照常裁决

## 项目上下文

项目：D:/projects/CodeWorld（Rust workspace，基线 495 测试全绿，git cc7b9f4）
现状（实查）：
- main.rs:533-538 已有 stdin 后台线程（read_line 等 Enter 置退出标志）——本任务升级它
- 渲染：ANSI 清屏/隐藏光标/差异重绘已有（crates/render ansi.rs），无 alternate screen
- 玩家进场：players/ 目录（PlayerDirectory），P=pioneer 进场后代码自主移动
- 意图执行路径：阶段二 apply_pending_changes 的 SetPos（占位制裁决）——操控意图的落点
- workspace 无 crossterm 依赖（新增，选它因为纯 std 之外的事实标准、Windows 支持好）
- cargo 转发：node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs（不存在则重建）

## 需求

### A. 输入系统升级（crossterm + raw mode）
1. Cargo.toml workspace 加 crossterm（最新稳定版）
2. 替换现有 stdin 线程：raw mode + 事件循环（crossterm::event::read 或 poll），按键事件转 Intent 入队（mpsc channel / Mutex<VecDeque>，实现自定）
3. Intent 枚举：Move{dx,dy}（方向键/WASD→四方向）、Attack（空格：对最近可见敌人坐标出刀——复用现有 scan 语义但走操控意图）、Quit（q/Enter/Ctrl+C）
4. 退出路径全恢复：raw mode disable + alternate screen 退出 + 现有 exit_terminal 逻辑保留；panic hook 也要恢复 raw mode（否则崩溃后用户终端报废）
5. alternate screen（crossterm EnterAlternateScreen/LeaveAlternateScreen）——画面画在备用屏，退出还原用户 shell 现场

### B. 操控意图的路由（双层模型核心）
1. 谁操控谁：**players/ 目录进场的第一个玩家实体**（MVP 单玩家假设；无玩家进场=按键仅 Quit 生效+提示"no player online"日志一次）
2. 意图流经角色代码执行（#022）MVP 最简形态：host 把操控意图转成一次"代码层注入"——实现自定，推荐最小路径：main 拿到 Move 意图时**抑制该实体本拍的代码自主 move_by**（若有）并代为排队操控方向移动。即：操控优先于自主，玩家按了键就以按键为准，没按则代码自主（挂机AI）。Attack 意图同理：操控刀优先
3. 语义边界写清日志："[input] 操控意图 Move(1,0) player=pioneer（覆盖自主移动）"——让双层叠加在日志可观测
4. 占位/攻击结算照常：操控移动一样会被挡（塔前停下），操控攻击一样走两阶段结算——操控不豁免物理（宪法）
5. 实现形状预留：Intent 路由器写成独立模块/函数（input.rs），将来 WebSocket 意图流（期四）复用同一入口——只换来源不换路由

### C. 操控时的视觉反馈
1. 字幕（caption）追加操控状态：操控目标在线时显示 "ctrl:P"（如 "tick=5 | A->D dist=... | alive=3 | ctrl:P"）
2. 被操控实体高亮：其 glyph 用白色或反色渲染（Appearance.color 支持，host 侧动态覆盖或渲染时特判，实现自定但别改 core 数据结构——渲染层特判优先）

### D. 玩法闭环验收场景
pioneer.wat 保持不动（它有自主移动逻辑=挂机AI）。实测流程：P 进场自主东行 → 按住 A 键 → P 转向西走（操控覆盖自主）→ 松手 → P 恢复自主东行 → 遇塔被挡（占位制生效）→ 空格 → P 对最近敌人出刀。

### E. 文档
- api.md 新增"操控层 v0"节：当前键位表、意图语义、"操控优先于自主"规则、期四 WebSocket 预告
- decisions.md #023（操控层v0定案：crossterm raw mode+意图队列+操控覆盖自主+占位照常；借鉴 roguelike 标准）
- progress.md 更新

## 验收标准

1. 新测试：Intent 解析（键→意图映射全表）；意图路由（Move 意图抑制自主 move_by 并排队操控方向——可用注入假意图测 tick 级）；无玩家时按键安全（不崩+日志）；Attack 意图排队攻击；操控移动被占位阻挡（塔前停）
2. 495 基线零回归（四生物剧本不变；pioneer 自主行为不变——无人按键时与现在完全一致）
3. 冒烟（人工步骤写入报告，CC 无法真按键）：启动→画面正常（alternate screen）→无操作30拍自主行为与旧版一致。按键路径以单元测试+注入意图测试覆盖
4. panic 安全：故意 panic 时终端恢复（raw mode + alternate screen 都还原）——测试或代码审证明
5. clippy/check 三连过
6. api.md/#023/progress 同步

## 已知信息（不需要查）

- crossterm 是 Rust 终端操控事实标准（ratatui 底层），Windows 原生支持好；event::poll(timeout) 可非阻塞探测，输入线程不占主循环
- 现有 stdin 线程退出逻辑（read_line+EOF）退役，被新输入系统替代；battle.log/build.log 分离的 start.bat 不受影响（stderr 照旧）
- "抑制自主 move_by"的挂点：run_player_code 执行玩家代码前检查本拍操控意图，有 Move 意图时跳过该实体的 on_tick（或仅跳过其移动副作用——前者简单，MVP 取前者并在注释说明取舍：操控期间完全接管）
- Attack 的"最近可见敌人"：复用 fill_visible_snapshots 的可见列表（viewport 过滤）取最近——与 scan 同源
- fuel trap 坑（.cc-bugs #5）：操控路径不经玩家沙箱，无 fuel 问题
- crossterm 依赖走 rsproxy 镜像（cargo config 已配）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（轻装挡单审一轮）

聚焦：raw mode 全路径恢复（正常退出/panic/EOF 三条路）；意图队列线程安全；操控覆盖自主的边界（按键瞬间 vs tick 边界）；占位/攻击裁决无绕过；无玩家场景安全。

## 交付物

1. 中文 commit（feat: 期三操控层v0——键盘意图流+操控覆盖自主）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节）

不要弹选项；信息缺失超3次试错即停写报告。
