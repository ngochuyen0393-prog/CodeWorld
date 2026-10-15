# TASK（期四第四刀，中等任务）：手感打磨——本地预测+攻击反馈+镜头跟随

## ⚠️ 前端为主（手写原生 JS 零依赖宪法不变）；Rust 侧仅日志级别微调；逻辑测试以审查+土豆人工验收为准

## 宪法依据

decisions.md #026（薄客户端/插值路线——本刀补齐"己方预测"半边：Valve 标准组合"己方预测+他人插值"）/ #024（操控身份）/ #025（操控独占语义不变）。

## 业界对标

client-side prediction + reconciliation（Valve Source/FPS 通用）：本地输入立即作用于画面（零等待），服务器权威快照到达后对账——一致则平滑续走，分歧则校正。我们 1秒/拍+1.5拍插值延迟下，预测是消除"点击后 2 秒才动"观感的唯一正解。camera follow（所有 ARPG/MMO 的镜头跟随）。攻击/受击表现层反馈（游戏感 juice 的最小集）。

## 项目上下文

项目：D:/projects/CodeWorld（基线 580 测试全绿，git 6ba0e3c）
已有：
- 前端 static/index.html：Canvas+快照插值+鼠标操控（左键 move/右键 stop/点敌 attack）+绿色虚线路径+红框闪
- 服务器 next_move_step（Rust，bin/codeworld/src/input.rs）：持续 move 目标逐拍一步，方向分解规则已在 net-protocol.md 写死（先横后竖——**先查证确认**，以代码为准）
- 操控移动应用日志是 debug 级（"应用 SetPos"）——battle.log（INFO）看不到，土豆实测误判"P 没动"
- viewport 快照固定（x0,y0,w,h 世界域），画布映射全 viewport

## 需求

### A. 本地预测+和解（本刀灵魂）
1. **己方实体预测**：浏览器发出 move/stop 后，P 的渲染位置立即按本地预测走（不等快照）——点击瞬间 P 就动
2. 预测规则 = 服务器 next_move_step 的 JS 复刻（逐拍一步、先横后竖、到达即停）；**代码顶部注释声明"与 input.rs 的 next_move_step 是镜像规则，改一处必须同步另一处"**，并标注 Rust 函数名/行号
3. **和解（reconciliation）**：每帧快照到达，取 P 的服务器坐标与"该 tick 的预测坐标"对账：
   - 一致（距离 < 0.25 格）→ 预测确认，继续
   - 分歧（被挡/被外力移动/异常）→ P 瞬移回服务器坐标，清除本地 move 目标与虚线（玩家看到校正）
   - P 死亡（快照消失）→ 清预测状态
4. **他人实体照旧插值**（己方预测/他人插值的标准分工）；断线重连清预测
5. 键盘操控（终端来源）无法预测——只有浏览器自己发的意图才预测（前端只对自己发的 move/stop 建预测）

### B. 攻击反馈（表现层 juice 最小集）
1. 点击敌人出刀：P 与目标间一道**快速淡出的攻击线**（0.3 秒生命周期，Canvas 动画）+ 目标格红框闪（已有，保留）
2. **击杀标记**：快照中实体消失时，在其最后坐标留一个渐隐的 ✕（1.5 秒）——玩家能看见"这里死过一个"
3. 全部纯前端本地效果，不碰协议；服务器结算结果照旧由下一帧快照自然呈现

### C. 镜头跟随
1. 相机模式：**跟随 P**（默认）——画布以 P 为中心（相机位置自身做 lerp 平滑），世界边界处 clamp（不越出 viewport 域）
2. 边界可视化：相机贴边时画布边缘画出 viewport 边界线（玩家知道世界到头了）
3. 切换：按 F 键或底栏按钮切「跟随/全景」（全景=现状整 viewport 铺满）；P 不存在（未连/死亡）自动回落全景
4. 鼠标世界坐标换算适配相机（提示栏坐标仍正确）

### D. Rust 侧微调（唯一后端改动）
1. apply_pending_changes 的 SetPos 应用日志：**仅当实体是 control_target 时升 INFO**（其余保持 debug——A/D/S 每拍都动，全升 INFO 会刷爆日志）；文案带 from→to 坐标
2. 不改任何语义/协议/测试断言（若现有测试断言日志级别需微调，列出）

### E. 文档
1. net-protocol.md v1.3：新增"客户端预测与和解"节（预测规则镜像声明/对账阈值 0.25 格/分歧即校正清目标/仅自发意图预测）
2. progress.md 更新；decisions.md 是否新增条目自行判断（若只是表现层实现可不入宪法，说明理由）

## 验收标准

1. 580 基线零回归（Rust 只有日志级别变化）；clippy/check 三连过
2. 土豆浏览器验收（报告写步骤）：
   a. 点击远处空地 → **P 立刻开始走**（不等 1-2 秒）——预测生效
   b. 走到被 D 或墙挡住 → P 弹回真实位置（校正可见）
   c. 镜头跟着 P 走，贴世界边缘时出边界线；F 切全景再切回
   d. 点 D 出刀 → 攻击线一闪；若发生击杀 → ✕ 渐隐标记
   e. 杀掉世界（Ctrl+C）→ 断线提示 → 重启 → 重连后预测状态干净
3. JS 审查聚焦：预测/对账边界（快照滞后/同拍多步/P死亡/断线重连）；next_move_step 镜像规则与 Rust 实现逐行一致性（**审查时必须对照 Rust 源码**）；相机 clamp 数学；攻击线动画不泄漏（定时器/rAF 清理）

## 已知信息（不需要查）

- next_move_step 的方向分解规则以 input.rs 现实现为准（任务开始先读它，别信文档记忆）
- 插值延迟当前 = 1.5×平均帧间隔（自适应）；他人插值管线不动
- P 死亡判定：快照 entities 里找不到 control_target 的 id
- viewport 快照字段（x0,y0,w,h）每帧都有；世界 y 向上，画布 y 翻转已有
- 攻击线用 Canvas 每帧重画时附带绘制（存 {from,to,born} 数组，超过 0.3s 剔除）；✕ 同理存 {pos,born}
- 相机 lerp 系数建议 0.08-0.15（每帧，60fps）
- 底栏提示文案更新："左键移动 · 右键停止 · 点击敌人攻击 · F 切镜头"

## 硬 Gate（Rust 侧）

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（单审一轮）

聚焦见验收 3。另查：零依赖声明属实（无 CDN/script src）；协议文档与实现一致。

## 交付物

1. 中文 commit（feat: 期四第四刀——本地预测+攻击反馈+镜头跟随）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节+土豆验收步骤清单）

不要弹选项；信息缺失超3次试错即停写报告。
