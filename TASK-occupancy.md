# TASK（轻装挡）：实体占位制——移动被挡+接触记双方+spawn 预检+推挤退役（#021）

## ⚠️ 必须使用 test-driven-development（红→绿）

## 宪法依据

decisions.md #021（本次写入）：实体占位制 = Screeps 阻挡制起步 + Factorio/EVE 半径制路线。调研结论：Minecraft/Wurm 证明格子制撑得起 3D；EVE 证明半径+服务器裁决；原神互穿模型不适用（玩家非对抗）；推挤=连体 bug 反面模式。体型未来开放时四件套（上下限/面积付费/总预算）同天上线。

## 项目上下文

项目：D:/projects/CodeWorld（Rust workspace，基线 474 测试全绿，git fd9ff89 之后最新）
现有碰撞体系（实查）：
- crates/physics/src/default.rs:111 detect_collisions——同坐标精确相等判碰撞（O(n²)全对扫，按 EntityId 排序保证确定性）
- crates/physics/src/default.rs:135 resolve_collision——"后创建的被挤到 x+1"（本任务退役的对象）
- main.rs tick 阶段二 c 步：detect_collisions（接触事件）→ physics.step（内部再 detect+resolve 推挤）→ handle_collisions（只记 count，不记双方）
- 移动应用：阶段二 a 步 apply_pending_changes 里 SetPos 直接写入坐标（无占位检查）
- spawn：阶段二 a1 process_spawns（有 ±10 格/上限/限流/编译检查，无占位检查）
- 已实证的 bug 场景：P 每拍 move +1 与 T 同格 → resolve 把 T 推 x+1 → 永久连体推挤环游世界（debug 日志 a=T b=P pos=(38→39→…) 实锤）

## 需求

### A. 移动占位检查（核心，Screeps 阻挡制）
阶段二 a 步应用 SetPos 前：目标格（x,y 取整 floor）是否已被其他存活实体占据？
- 被占 → 该移动被拒：实体留在原坐标，记 info 日志「[move] 移动被挡（目标格被占）」+ 记录接触对（双方 EntityId）
- 空闲 → 照常应用
- 注意顺序：本 tick 所有移动按 EntityId 升序统一裁决（先到先得，确定序），避免两个实体同时挤向同一格的裁决随机
- 占位判定用"格子"（floor(x),floor(y)）口径：一格一物。实体逻辑坐标仍是 f64 不变
- 重力下落（physics step 里的 y 变更）同样过占位检查：落到被占格 → 停在上一格（本拍不动，下拍重试）

### B. 接触事件记双方（修日志盲区）
handle_collisions 的日志从 count 改为逐对记录：双方 EntityId + 坐标（info 级）。tick 级汇总行保留。
接触语义扩充：来源=①物理同格检测（现有）②移动被挡（A 新增）。两类都算"接触"（为将来"接触开通道"铺路，本任务只记日志不开通道）。

### C. spawn 出生预检（Factorio 放置检查）
process_spawns 里造物落世界前：出生格被占 → 该次 spawn 失败（warn 日志「出生格被占」），不诞生、不算入限流（下拍可重试）。已有检查全部保留。

### D. 推挤逻辑退役
resolve_collision 从 tick 流程移除（函数与测试可保留标注 deprecated 或删除，自定；标注更稳——文档记录它的历史与退役原因）。physics.step 不再推挤。同格重叠在新规则下理论不可能发生（出生预检+移动被挡双闸），防御性保留 detect_collisions 的 debug 日志——若仍检出同格（异常路径），记 warn 不推挤（世界状态异常可见即可，不自动"修复"）。

### E. 文档与定案
- decisions.md 写入 #021 全文（占位制定案：调研四流派+原神+Minecraft/EVE 背书；半径制路线；体型四件套将来同天上线的承诺）
- api.md：move_by 条目补"目标格被占则移动被挡"；spawn 条目补"出生格被占则失败"
- players/README.md 若提及移动语义同步

### F. pioneer.wat 修复（玩家体验 bug）
塔的出生点从 +3 改 +5，且立塔那一拍不移动（tower_done 置 1 时 return，下一拍才开始走）——彻底错开 P 与 T 的出生格，避免再触发连体（新规则下是"被挡"而非连体，但拓荒者一出生就被自己塔挡住也不对）。改完实测：P 正常向东走、T 留在原地站岗。

## 验收标准

1. 新测试：移动被挡（目标格被占→留原位+接触对记录）；双方同时进同一格→先到者得（确定序）；重力落被占格→悬停上格；spawn 被占→失败可重试；退役后 tick 流程无推挤（P/T 同格场景验证不再连环 x+1）
2. 474 基线零回归（注意：resolve_collision 相关旧测试若语义冲突，按 deprecated 处理允许调整，报告列出）
3. 冒烟：pioneer.wat 修复版进场——立塔+5 格、立塔拍不移动、之后正常走，全程 T 不再跟着 P；日志有完整"移动被挡"链路（若有生物试图进占格）
4. 四生物剧本回归：A 冲锋 D 的路径上如有占格交互按新规则走（A 贴 D 时最后一格被挡=贴脸攻击照旧成立，距离≤1.5 攻击范围不变）——剧本时间线若因占位改变（如 A 被挡后贴脸提前/延后）如实报告并论证合理性
5. clippy/check 三连过
6. #021/api.md/README 同步完成

## 已知信息（不需要查）

- detect_collisions 的 O(n²) 在实体≤64 下无性能问题
- floor 格子口径：Entity pos 是 f64，floor(x),floor(y) 为格键；同格即占（MVP 无半径字段——radius 留给将来，接口形状可预留但本任务不实现半径判定）
- 移动应用处在 main.rs apply_pending_changes（SetPos 分支）；重力在 physics crate step——两处都要过同一占位判定（公共 helper）
- P 玩家代码每拍 move_by(1,0) 走整数格；A 的 WAT 用归一化步长（斜向时坐标非整数，floor 后仍是一格一格占）
- handle_collisions 在 main.rs（阶段二 c 步）；它的 pairs 参数来自 detect_collisions——移动被挡产生的接触对也要汇入（API 形状自定，报告写清）
- pioneer.wat.txt 是模板源头，players/pioneer.wat 是运行副本——两份都改
- cargo 转发：node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs（不存在则重建）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（轻装挡单审一轮）

聚焦：占位裁决确定序（无随机）；移动/重力/spawn 三处占位检查同源无绕过；退役彻底（tick 无推挤残留）；接触对日志完整；pioneer 修复实测。

## 交付物

1. 中文 commit（feat: 实体占位制#021——移动被挡/接触记双方/spawn预检/推挤退役）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节：改动/测试/冒烟摘录）

不要弹选项；信息缺失超3次试错即停写报告。
