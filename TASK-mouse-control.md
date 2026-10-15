# TASK（期四第三刀，中等任务）：鼠标操控——点击移动/点击攻击/操控独占

## ⚠️ 必须使用 test-driven-development（Rust 侧红→绿；JS 侧以逻辑测试+审查代）

## 宪法依据

decisions.md #022（双层模型）/ #024（me 声明操控身份）/ #025（**操控独占**：在线=操控独占，无意图拍角色不动，代码自主被完全抑制；下线=处置权）。#026（薄客户端：输入=意图流经服务器裁决）。

## 业界对标

client input → server authority（客户端只发意图，服务器全权裁决——SnapNet/标准 netcode）。预测（本地先行）留第四刀。

## 项目上下文

项目：D:/projects/CodeWorld（基线 554 测试全绿，git 4aef65d）
已有：
- WS 服务 axum：目前只广播快照，收客户端消息只 pong/忽略（第一刀立的接收循环框架）
- input.rs 意图路由器（键盘来源：crossterm 线程→Intent 队列→tick 路由）——第三刀的第二来源
- 操控目标：players.rs controlled_target（me.txt/CODEWORLD_ME）
- 操控行为：ControlledAction::Move(dx,dy)/Attack(tpos)（键盘按格移动）
- 终端键盘操控与 WS 并行存在
- 前端 index.html：Canvas+插值+鼠标世界坐标显示（点击的地基已铺）
- cargo 转发：node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs（不存在则重建）

## 需求

### A. WS 客户端→服务器意图通道（收方向）
1. 客户端 JSON 消息格式（net-protocol.md v1.2 新增"上行"节）：
   ```json
   {"type": "move", "x": 12.0, "y": 0.0}        // 点击世界坐标 → 持续目标
   {"type": "attack", "x": 15.0, "y": 0.0}      // 点击敌人坐标 → 单次攻击
   {"type": "stop"}                              // 停止（清除移动目标，站住）
   ```
2. 服务器 WS 接收循环真实现：解析→校验（未知 type/非法坐标 warn 丢弃）→写入与键盘共享的意图通道（input.rs 的队列新增来源——保持单入口路由原则）
3. **身份问题（关键设计）**：WS 连接≠操控权。MVP：单连接假设（第一个 WS 连接=me 声明的操控端；非声明来源的消息丢弃+warn）。多人/认证留后（#024 终态是账号，现在是 me.txt 的等价物：本机浏览器=我）。报告写清这个简化与升级路径

### B. 鼠标操控语义（前端+服务器配合）
1. **点击空地 = move 意图**（持续目标）：操控实体每拍向目标直线走一格（四方向分解：先横后竖或取主轴，实现自定并在 net-protocol.md 写死语义）——到达后停（目标清除）
2. **点击敌人实体 = attack 意图**（单次）：对点击坐标出刀（复用键盘空格的 Attack 逻辑）——注意不自动连击（点一下砍一刀，连点连砍）
3. **点击敌人后自动走近？MVP 不做**：攻击距离外点击敌人=只出刀（大概率落空）+ 前端提示"目标超出攻击范围"？不——更简单：不判距离，点了就砍（服务器照常结算 miss），玩家自己学会走近再点。宪法：玩法归玩家
4. **stop 语义**：右键或 S 键 = 清除移动目标站住（#025 操控独占的"我不动"）
5. move 意图与键盘意图冲突：后到优先（同拍互斥，自然解决）
6. 服务器侧 move 持续目标的执行处：tick_with_intents 里操控目标存在 WS move 目标时，逐拍生成 Move(±1格) 并抑制自主（与键盘 ControlledAction 同通路）

### C. #025 操控独占落地（本刀核心语义修正）
1. 现状修正：键盘"无意图拍回落自主"改为——**me 在线期间，操控目标实体的 on_tick 全程抑制**（角色不按键就站着，不会自己走）
2. pioneer.wat 的自主移动在 me 声明它时失效（站着等你）；me.txt 删除/下线后→自主恢复（这就是 #025 的"在线硬锁、下线交接给自主"）
3. 终端键盘与鼠标同享此语义（都是"在线操控"）
4. 生命周期日志："[ctrl] 操控独占开始 player=pioneer（自主挂起）" / "[ctrl] 操控释放（自主恢复）"

### D. 前端交互（index.html 升级）
1. 左键点击画布：世界坐标换算（已有鼠标坐标显示）→ 判定点击处是否有实体（快照最近帧查询）→ 有=attack 消息/无=move 消息
2. 右键/S 键：stop 消息（contextmenu preventDefault）
3. 移动目标可视化：操控实体与 move 目标间画一条虚线+目标点标记（点击后到到达前）
4. 攻击反馈：点击敌人瞬间目标格闪一个红框（前端本地效果，服务器结算结果下一帧快照自然呈现）
5. 底栏加操控提示文案："左键移动 · 右键停止 · 点击敌人攻击"
6. **观战端不变**：非操控 WS 连接（第二连接）只看不动（MVP 单操控连接语义）

### E. 文档
- net-protocol.md v1.2：上行消息节（三消息+语义+单连接假设）+ 下行不变
- decisions.md #027（鼠标操控定案：点击语义/操控独占落地/单连接MVP/多人认证留后）
- progress.md 更新

## 验收标准

1. Rust 新测试：WS 上行消息解析（三 type/非法丢弃）；意图汇入共享路由（WS move 与键盘 move 同通路）；持续 move 目标逐拍执行+到达停止；操控独占（me 在线=on_tick 抑制，删除 me=恢复，生命周期日志）；stop 清目标
2. 554 基线零回归（**注意**：操控独占改变"无意图拍回落自主"行为——相关旧测试按 #025 语义更新，报告列出）
3. 土豆浏览器验收（报告步骤）：连接→左键点地→P 平滑走去（插值+逐拍移动）→到达停住→右键随时停→点 D→出刀（battle.log 有操控刀）→D 反制可能杀你（物理照常）→me.txt 删除后 P 恢复自主
4. clippy/check 三连过
5. net-protocol.md v1.2/#027/progress 同步

## 已知信息（不需要查）

- WS 接收循环框架在 net.rs（第一刀立的，消息目前 pong 处理）——本刀真实现
- input.rs Intent 队列是 std mpsc——WS（tokio 侧）写入需桥（或直接 clone Sender，看现有结构，简单优先）
- 逐拍移动的方向分解：目标 dx/dy 各取 sign 生成一步（斜向=先横后竖的自定规则写死文档）
- 前端实体命中判定：快照 entities 里找最近实体距点击坐标 < 半格（0.5）
- pioneer 自主=每拍 move_by(1,0)；抑制后它完全静立（fuel 不消耗，无妨）
- attack 的 tpos 用点击的实体坐标（快照可能滞后 1.5 拍——插值显示的坐标，可接受；服务器以坐标找人）
- contextmenu 右键在画布 preventDefault（浏览器默认菜单屏蔽）
- 终端键盘操控保留（crossterm 与 WS 双来源并存）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（单审一轮）

聚焦：WS 意图与键盘意图的同通路汇入（无第二路由）；操控独占的状态机（me 在线/下线/无效三态×意图存在）；持续 move 的到达判定（浮点比较用格距<0.5 非精确相等）；前端命中判定与消息构造；协议文档与实现一致。

## 交付物

1. 中文 commit（feat: 期四第三刀——鼠标操控+操控独占#025落地）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节+土豆验收步骤）

不要弹选项；信息缺失超3次试错即停写报告。
