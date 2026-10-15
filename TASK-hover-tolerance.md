# TASK（期四第七刀·小刀三件套）：S新悬停迁移 + 点击容差 + 贴目标微抖修复

## ⚠️ 必须全程 TDD（Rust 侧行为先红后绿；前端改动跑 node 纯逻辑验证）

## 宪法依据

#029（飞行/悬停是代码对抗世界的能力，无税）；#030/#031（半径物理/点命中制——本刀是落地打磨）；#027（点击操控语义不变，只放宽选中容差）。

## 项目上下文

项目：D:/projects/CodeWorld（基线 628 测试全绿，git 86265a0）
昨夜实测发现的三个问题（土豆验收 battle.log 实锤）：

1. **S 还是老喷气 hack**：spy.wat 每拍 move_by(0,+9.8) 排到 21.8 → 重力拉回 12 → 每拍循环。坐标在 12.0↔21.8 剧烈弹跳：快照里 S 的 y 不稳定（前端显示/点击命中/其他生物 visible 都是弹跳坐标）；D 用旧坐标打它永远落空一部分是 S 自身弹跳导致。第六刀物理已有"免重力悬停集合"（physics step 的 aloft 参数：操控悬停在用）——S 应迁移到同机制：S 的沙箱代码每拍调一个新 import `hover()` 声明"本拍悬停"，世界把 S 加入 aloft 集合，免重力、坐标稳定在 12 层。
2. **点击命中容差太严**：前端 entityHitTest 严格圆内（r=0.5 的实体要点的很准）。所有点击类游戏都有选中容差。放宽：nearestEntityAtWorld 找"距点击点最近且距离 < 1.5"的实体（1.5 = 点击容差半径；点空地远处仍是移动）。注意：creation（塔）保持原 AABB 判定不变。
3. **贴目标微抖**：土豆昨夜日志（/tmp/b8.log 留档）P 被 S 挡住后原地蹭了 7 拍，y 在 ±0.08 颤动：next_move_step 每拍算新矢量步，剩余距离小于步长时"终进近一步"与"到达判定 0.5"边界反复。修：到达判定后目标清除要幂等（到达后不再生成新步）；被挡滑动后若与目标距离不再缩短（连续 2 拍位移 < 0.01）→ 视为到达清目标（防死循环蹭动）。

## 需求

### 1. hover() import（arena + S 迁移）
- arena sandbox env 新 import：`hover()`（无参，返回 void）——玩家代码声明"本拍我要悬停"；世界侧把该实体加入本拍 aloft 集合（免重力，与操控悬停同集合同机制）
- 每 tick 开始时 aloft 集合重建（上一拍的 hover 声明不延续——玩家代码必须每拍重申，"持续对抗世界规则"的代码体现，#029 无税但需持续出力声明）
- api.md 加 hover() 文档；net-protocol.md 若涉及则同步
- **S 的 .wat 重写**：删老 move_by(0,+9.8) hack，每拍 hover() + 原有侦察逻辑（visible/probe 每 4 拍照旧）不变；S 稳定站在 (25,12)
- **D 的行为验证**：D scan 到 S(25,12) 出刀——S 坐标稳定后 D 的刀"点在身体内"应命中（距离 0 点=center）。跑一局验证 D 能打中 S（或至少命中判定通过——S 脆皮可能直接被击杀，这是合法结果，记录即可）
- WAT 侧：hover 无参调用，pioneer.wat 不需要动（P 用操控悬停不走 hover）

### 2. 点击容差（前端 index.html）
- nearestEntityAtWorld：活动实体判定从"严格圆内"放宽为"距圆心距离 < 1.5（点击容差）"；命中多个仍取最近；操控目标自己仍排除；creation 保持 AABB
- decideClickMessage 不变（命中→attack 用实体快照坐标；未命中→move）
- 红框/刀光反馈链路不动（本来就对，是判定太严导致不触发）
- net-protocol.md v1.5 的前端判定节补一句"选中容差 1.5"
- node 纯逻辑验证：容差内选中最近/容差外是移动/creation AABB 照旧/自己排除

### 3. 贴目标抖动修复（input.rs next_move_step + main.rs 意图路由）
- 到达清除幂等：目标清除后同拍不再生成新步（查 ws_move_target 状态机，到达判定与步生成的顺序）
- 停滞检测：操控移动连续 2 拍实际位移 < 0.01（被挡滑不动）→ 视为到达，清目标 + INFO 日志"[ctrl] 操控目标停滞，清除目标（贴障）"
- Rust 测试：停滞清除（构造被挡场景 2 拍后目标清）；到达幂等（到达拍后不再有步）
- 前端预测镜像 applyMoveSteps 同步停滞语义吗？——不需要（预测不知道被挡，靠服务器快照和解纠偏，现状机制已覆盖；在 net-protocol.md 注明）

### 4. 文档
- api.md：hover() 完整文档（语义：本拍悬停声明，每拍需重申）
- net-protocol.md：选中容差 1.5 一句
- progress.md 更新
- decisions.md 不新增（都是 #029/#030/#031/#027 的落地打磨，报告里说明归属）

## 验收标准

1. TDD 红绿证据；628 基线零回归（新增测试净增）；clippy/check 三连过
2. 起服实测（CC 跑 headless 一局 ≥60 拍）：
   a. S 坐标全程稳定 (25,12)（battle.log 无 12↔21.8 弹跳 move_by 记录；有 hover 相关新日志）
   b. D 对 S 出刀命中（deliver_attacks 收件不再是"落空"——on_attack 拦截或命中结算，battle.log 可见）
   c. P 操控贴 S 被挡 2 拍后目标自动清除（日志"操控目标停滞"）
3. 土豆浏览器验收：点 S 附近（容差内）→ 红框+刀光出现（不再要求点得极准）；点 D → 稳定命中反馈；P 飞到 S 身边被挡 → 2 拍后自动停下不蹭动
4. 审查聚焦：hover 集合重建时序（每拍重建 vs 残留）；容差判定与 creation AABB 的边界（点塔附近该选塔还是不该——塔用 AABB，1.5 容差只作用于活动实体，点塔身仍是原判定）；停滞检测不会误杀正常慢速接近（阈值 0.01×2 拍 vs 每拍步长 1.0，安全边际大）

## 已知信息（不需要查）

- aloft 免重力集合：physics step 的第二参数（第六刀实现，操控悬停在用）；hover import 只是把玩家代码的声明接进同一集合
- S 现 .wat 位置：测试内嵌 WAT（arena/main 测试里）+ 运行时从 crates 或 bin 内嵌字符串加载——查 main.rs 生物登场处（spy 关键字）；重写时保留紫/脆皮/probe 假串 LIE9 人设
- D 每 4 拍 scan+出刀（站桩 15,0）；S 半径 0.5；D 打点 (25,12) 距 S 圆心 0 → 必中（#031 点命中）
- 前端 entityHitTest/nearestEntityAtWorld/decideClickMessage 在 index.html PURE 区（434-470 行附近），node 验证脚本模式照旧
- 抖动日志样本：/tmp/b8.log（16:13:29-16:13:36 七拍 y±0.08 蹭动）
- S 迁移后 pioneer.wat/example.wat 不受影响（它们不用 hover）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（单审一轮）

聚焦见验收 4。另查：api.md hover 文档与实现一致；S 人设保留（LIE9/侦察节奏）；容差改动没碰 creation 判定。

## 交付物

1. 中文 commit（feat: 期四第七刀——hover新悬停+点击容差+停滞修复）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版四节+土豆验收步骤）

不要弹选项；信息缺失超3次试错即停写报告。
