# TASK（轻装挡）：scan 视野化清算——感知回归设计稿 4.2

## ⚠️ 必须使用 test-driven-development（红→绿）

## 宪法依据

docs/architecture.md 4.2（攻击前置条件三条路：视距内/接触式/超视距）+ decisions.md #018（超视距零供给：框架不提供任何超视距坐标能力）。

## 项目上下文

项目：D:/projects/CodeWorld（Rust workspace，基线 452 测试全绿，git 9a3264a）
现状（已人赃俱获，勿重新排查）：
- `scan`（arena/sandbox.rs env import）从 `SandboxData.nearest_enemy` 读坐标
- `nearest_enemy` 由 main.rs:1102 `find_nearest_enemy(world, &entity_id)` 填充——**遍历全世界所有存活实体无范围限制**（代码注释自认"上帝视角单敌人 MVP 感知"）
- 另一套感知 `visible_count/visible_at`（眼睛层）已有 viewport 过滤（main.rs:72 `fill_visible_snapshots`，BoundingBox.contains），S 在用
- view端口径：main.rs fill_visible_snapshots 用 renderer viewport 的 BoundingBox
- D 的行为：scan 锁定→每4拍反制一刀，无射程概念→已实测 15 格外点杀 example 玩家（E）
- ProbeBridge（main.rs:43-60 区域）visible_ids/visible_positions 也是 viewport 过滤产物

## 需求

### A. scan 降级为视野内（核心）
`find_nearest_enemy` 加 viewport（BoundingBox）参数过滤：只遍历 viewport.contains 的实体。与 fill_visible_snapshots 用同一个 BoundingBox 实例（同源，别造两套口径）。视野内无目标时 nearest_enemy=None → scan 返回 0（现有语义已支持）。
- arena 侧 scan import 不用改（读的还是 nearest_enemy 字段，变的是 host 填充时过滤）
- 日志更新：视野内无目标时 debug 日志改为"[scan] 视野内无目标"（现在是"无敌人（世界无其他存活实体）"——语义不再准确）

### B. 老生物迁移适配
A 和 D 的 WAT 用的还是 scan。它们的行为会变（视野外锁不到人）——这是预期效果，但需逐一核对行为不崩：
- D（站桩炮台）：视野外无人时 scan 返 0 → D 的 WAT 现在怎么处理返 0？读 main.rs defender_wat 确认（若 scan 返 0 仍出刀打旧坐标/垃圾坐标→改为返 0 时跳过本拍出刀）
- A（冲锋）：同理——视野内无目标时站住别动（不是朝垃圾坐标冲）
- A 的追击预期：A 靠 scan 找 D——两者距离 10 格，viewport 半宽 50，A/D 互相在视野内，剧本 1-3 行为应零变化（H 出生 (10,8) 也在视野内）。**回归验收点：四生物剧本时间线（tick4 H 死/tick12 A 死）不变**
- E 类玩家（players/example.wat）：不 scan，无影响

### C. viewport 边界确认
确认 Config 里 viewport 参数（center/half_width/half_height）当前值下的实际视野矩形，报告里写明（比如中心(0,0)半宽50=横[-50,50]）。玩家出生在 [30,45]，D 在 (15,0)——**D 距玩家最远 30 格 < 50，玩家仍在 D 视野内！** 若如此，这次清算后 D 依然点得到 E（15 格也在视野内）。这不是 bug——viewport 是"屏幕"语义，屏幕内=看得见=合法目标。**任务目标不是"让 D 打不到 E"，是"感知诚实"**：屏幕外才打不到。报告里把这个边界效应讲清楚，别自作主张改 viewport 尺寸。

### D. 文档同步
- api.md：scan 条目加"仅视野内（viewport），视野外返回 0；超视距手段见 #018（自建哨兵/推理盲打）"
- decisions.md：追加 #019（scan 视野化定案：眼睛诚实三路前置条件完整兑现）

## 验收标准

1. 新测试：find_nearest_enemy 视口内/外实体的过滤（视野外有更近实体时不选中）；scan 在视野内无目标时返回 0
2. 452 基线零回归 + 四生物剧本时间线不变（tick4/tick12 击杀照旧）
3. 冒烟：跑一局，确认 D 的 scan 只报视野内目标；把 example 玩家放更远处（临时改出生点或用 CODEWORLD_VIEWPORT_* 缩小视野验证打不到的边界），日志证明视野外不可锁定
4. clippy/check 三连过
5. api.md/decisions.md 同步完成

## 已知信息（不需要查）

- BoundingBox 在 render crate，contains(x,y) 已有
- fill_visible_snapshots 的调用处在 tick 阶段一（快照填充段），nearest_enemy 的填充同段（main.rs:135-142）
- WAT 里 scan 返 0 的现有处理可参考 S 的写法（visible_count 为 0 直接 return）
- cargo 转发：node C:/Users/Administrator/AppData/Local/Temp/cc-cargo.mjs（不存在则重建）

## 硬 Gate

```bash
cd D:/projects/CodeWorld && cargo test --workspace
cd D:/projects/CodeWorld && cargo clippy --workspace --tests -- -D warnings
cd D:/projects/CodeWorld && cargo check --workspace
```

## 审查（轻装挡单审一轮）

聚焦：viewport 过滤与 fill_visible_snapshots 同源（无第二口径）、WAT 返0处理无垃圾坐标出刀、剧本零回归。

## 交付物

1. 中文 commit（feat: scan 视野化——感知回归设计稿4.2三路前置）
2. C:/Users/Administrator/AppData/Local/Temp/cc-report.md（简版三节）

不要弹选项；信息缺失超3次试错即停写报告。
