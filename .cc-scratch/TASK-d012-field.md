# TASK D-012 力场系统（field 出生声明家族 + 零计费 + 叠加 v2 + 协议 v3 + 分离式发射器）

[Superpowers 激活：本任务走 test-driven-development——每块先写 RED 测试再实现 GREEN；收尾跑 verification-before-completion 心检。]

你是 CodeWorld（D:/projects/CodeWorld）的实现工程师。宪法级大刀，土豆 2026-10-21 拍板开工。定案全文在 docs/dev-plan.md「D-012 力场」条目（约 144-168 行）+ skill rust-sandbox-game-dev 力场立法段——**设计已全部定死，禁止再设计、禁止引入定案外的新概念**。本任务书自含全部定案要点，与 dev-plan 冲突时以 dev-plan 原文为准。

## 背景（一段话）

世界物理现在是全局常数（DefaultPhysics：gravity=-9.8 / drag_coefficient=0.05 / max_speed=25，crates/core/src/physics.rs + crates/physics/src/default.rs）。D-012 让玩家造物出生时声明"我身边一片区域内的物理常数被改写"（阻力/重力缩放/极速三参数），域内实体按改后参数被结算——护的是弹道不是血量。field 是出生声明家族新成员（与 radius/mass/binding/motion 并列），**"场造物"作为特殊品类在法律上不存在**，世界只认声明不认身份。

## 交付物（8 项，验收逐条对账）

### 交付物 1：field 出生声明（spawn 第 15/16/17 参）

spawn 加 3 个参数（14 参 → 17 参，破坏性变更）：
- 第 15 参 `field_radius: f64` —— 场域半径（以造物身体中心为锚向外延伸）。域：0.0=无场（默认，现状语义零回归）；>0 生效。钳制域 [0.0, 25.0]，非有限/负值钳 0 并 warn。
- 第 16 参 `field_gravity_scale: f64` —— 重力缩放系数。域 [0.0, 10.0]，非有限钳 1.0。生效重力 = 世界重力 × scale。
- 第 17 参 `field_drag_delta: f64` —— 阻力改写量（**加法式**，不是缩放：叠加 v2 的代数抵消语义要求改写量是 delta）。域 [-1.0, +25.0]，非有限钳 0。生效阻力 = 世界阻力 + Σ各场 delta。
- 极速不改写（本刀不做极速场；dev-plan 说"可改参数域：阻力/重力缩放/极速"是家族全景，本刀只落前两个+发射器，极速场留后续——域护栏保守起步）。

实现落点（照 motion 先例逐处抄结构）：
- `crates/core/src/entity.rs`：Entity 加字段 `field_radius: f64 / field_gravity_scale: f64 / field_drag_delta: f64`（各带 `#[serde(default)]`，默认 0/1.0/0=无场现状）；常量+clamp 函数（clamp_field_radius / clamp_field_gravity_scale / clamp_field_drag_delta，照 clamp_motion 模式）；getter/setter（照 set_motion 直存模式）。**serde default 必须与无场语义一致**：field_radius 默认 0.0（无场）、gravity_scale 默认 1.0（缩放 1=不变）、drag_delta 默认 0.0（加 0=不变）——旧快照反序列化零行为变化。
- `crates/arena/src/sandbox.rs`：PendingSpawn 加 3 字段；env.spawn 闭包加 3 参+校验（照 motion 的校验段模式：非有限/超界 warn+钳制）+INFO 日志带 field 三值。
- `bin/codeworld/src/main.rs` process_spawn_requests：`entity.set_field_radius(...)` 等三行（照 set_motion 直存位置）。
- **破坏性变更全调用点同一 commit 修完**（漏一处=玩家进场即 translation error）：players/pioneer.wat 与 pioneer.wat.txt（locals 显式传参补 3 参：0.0 / 1.0 / 0.0）、bin main 的 birth_motion_spawner_wat 等全部 spawn 生成器、arena/main 全部内嵌 WAT 测试调用点、examples/sentinel.wat 如含 spawn 调用也要改。

### 交付物 2：零计费（成本=普通造物开销）

无任何场专用计费。场造物=普通造物：日常开销（每拍几步的声明应答）+实体名额（上限 64 内）。**禁止**实现负载费/空场底租/任何世界向场收税的逻辑。本条是"什么都不做"条款——实现里不得出现场专用费用代码；在 api.md 写明"场的成本=普通造物日常开销+实体名额"。

### 交付物 3：叠加 v2（代数抵消+域钳制）

结算规则（每拍、每个域内实体）：
- 生效重力 = 世界重力 × 域内各场 gravity_scale 的**乘积**（缩放参数叠乘是缩放的代数形式：两场 0.5×0.5=0.25，负向抵消靠 scale>1 实现）。
- 生效阻力 = 世界阻力 + Σ(域内各场 drag_delta)（代数和，正负互吃：+2 与 -2 归零）。
- **域钳制兜底**：生效阻力钳到 [0.0, 25.0]（堆场买不到定律外胜利）；生效重力钳到 [-98.0, 0.0]（不许向上飞的重力、不许超过 10 倍）。
- 优先级不实现（降级为边角工具，本刀不建 priority 字段）。
- 账本不认老板：所有场一律入账，无所有权标签。

### 交付物 4：协议 v3（询问应答制）

每拍阶段一，对每个"声明了场的活造物"（field_radius>0），世界调用其沙箱导出的 `on_field(n_ptr: i32, ids_cap: i32, ids_ptr: i32) -> i32`（**第 14 个玩家 import**，新增 host→沙箱调用）：
- host 先把域内实体清单（EntityId 低 64 位，每实体 8 字节 LE）写进造物 memory [ids_ptr, ids_ptr+8×n)。n=域内实体数（含非玩家实体，不含场源自己）。**只报"谁在"不报"谁是谁"**——零预设，身份判断归场代码。
- 造物返回 i32：**0=维持出生声明**（笨场几步完事）；**正数 k** = 本次应答实际报告的场强度修正…… **不，定案再简化**：返回 0=维持声明；非 0=按返回值临时改写本拍 drag_delta（域钳后）。这就是"应答分级"：笨场 return 0 几步完事；智能场可以按 ids 清单烧识别步数后返回差异化数值（分级是玩家代码自己的事，世界只提供通道）。
- 未导出 on_field：按出生声明结算（现状默认，no-op 防御，与 on_attack 未导出同款）。
- **场只报数世界合账**：场有表达权（每拍可改本拍生效 delta），世界有结算权（域钳制+代数和最终裁定）。
- on_field 跑独立小 fuel 预算（照 call_on_contact 的预算包裹模式：存剩余→set 预算→调→恢复；预算 config 可调，默认 10_000，env 名 CODEWORLD_FIELD_FUEL）。
- 失联即灭：造物死了/沙箱没了场即消失（天然，无需额外代码）；on_field 抛错/超预算→本拍按出生声明结算（世界诚实，不用坏数据）。

### 交付物 5：物理结算接入（每拍、域内实体按改后参数被结算）

落点 crates/physics/src/default.rs：
- `step()`（生物/dynamic 造物的重力落点解析）：实体在某场域内→重力用生效值。判定=圆心距（实体 pos 与场源 pos 距离 < 场源 field_radius + 实体 radius？**不**——定案是"域内实体"，判定取圆心在域内即 pos 距场源 < field_radius，简单确定）。
- `step_projectiles()`（弹丸积分：重力/阻力/极速）：弹丸 pos 在域内→重力/阻力用生效值（重力×scale、阻力+Σdelta 后钳制）。弹丸是场的第一消费者（护弹道）。
- **实现形态建议**（重要，防 O(n²) 爆炸）：物理步先收"本拍场清单"（alive + field_radius>0 的造物，按 EntityId 升序确定序），每实体结算时找覆盖它的场（多场取并集合账：先乘所有覆盖场的 gravity_scale、再加所有覆盖场的 drag_delta）。给 DefaultPhysicsEngine.step/step_projectiles 加一个 `fields: &[FieldEffect]` 参数（FieldEffect{source_id, pos, radius, gravity_scale, drag_delta} 新结构体，physics crate 定义），main.rs 每拍在调物理步前收集。**纯函数 `effective_gravity_drag(pos, world_defaults, fields) -> (f64, f64)` 单独抽出+单测**（叠加/抵消/钳制/无场/多场覆盖五组锚）。
- 零回归锚：无场世界（全部 field_radius=0）行为与现在逐字节一致——physics 现有测试全部不动必须照绿。
- aloft（悬停免重力）优先级高于场：悬停声明本拍免重力，场不覆盖它（每拍重申语义不变）。

### 交付物 6：可视铁律（telegraphing）+ 成型期

- 前端 bin/codeworld/static/index.html：快照协议加 fields 数组（net.rs WorldSnapshot 加 `fields: Vec<FieldSnapshot>`，FieldSnapshot{x,y,radius}——场源造物本体已照常渲染，fields 层供画"可见气流环"）；index.html 按 fields 画**半透明圆环**（圆心=场源、半径=field_radius、低透明度描边，照温迪气流环范本；canvas arc 一圈 stroke 即可，别过度设计粒子）。
- 成型期 N 拍可穿过：场从诞生到生效有 N 拍成型期（**常数 FIELD_FORMING_TICKS=3**，config 可调 env CODEWORLD_FIELD_FORMING，入 game-menu 1.7 常数总账）。成型期内场不结算物理（可穿过），但已可见（前端画虚线环=成型中，实线环=已成型——给进攻方反应窗，Kinetic Field 判例）。物理步收集场清单时按 created_tick 过滤。
- 登记制 tick 刷场：每拍重算覆盖关系（不做 enter/exit 缓存挂摘——0.1s 判例的实现底座就是每拍刷）。

### 交付物 7：tick 刷场登记制 + 边界防抖

- 每拍全量重算（上面已含）。边界单位级触发冷却防抖（Veigar 判例）：**本刀不做**——咱们的场无事件回调（on_field 是询问不是触发器），逐拍询问天然无"进出边界抖动"问题。在 decisions 里注记"边界冷却挂信号：on_field 拿到清单已含进出信息，场代码可自记边沿；世界级冷却待有事件语义再议"。

### 交付物 8：分离式发射器（力场脉冲推存量弹药——D-018 退役工程依赖）

新 import（**第 15 个**）`env.field_pulse(x: f64, y: f64, vx: f64, vy: f64, radius: f64) -> i32`：
- 语义：以 (x,y) 为中心、radius 半径圆域内，给所有带速度实体（弹丸+dynamic 造物）速度一次性加上 (vx,vy) 冲量（物理推，不是规则层）。**限流每实体每拍 1 次**（与 spawn/shoot 同款）。
- 校验：x/y/vx/vy 有限；冲量模长 ≤ 世界极速 25（严格 > 拒绝恰等放行，与 spawn 初速同口径）；radius 钳 [0.1, 25.0]；(x,y) 必须在调用者本拍快照 ±10 格内（SPAWN_RANGE 同款防远程）。
- 作用对象：域内**带速度实体**（弹丸 + dynamic 造物 + 任何 velocity≠0 的实体）；推完照常受极速钳制。对 velocity≡0 的实体无效（推不动静止的——物理推是速度叠加，D-010 冲量三件套的碰撞响应另刀）。
- 阶段二结算（import 只排队：PendingFieldPulse{source, x, y, vx, vy, radius}，照 PendingShoot 模式）；结算时按 EntityId 升序遍历域内实体加冲量+INFO 日志（被推者 id/推前推后速度）。
- **这就是分离式发射器**：存量炮弹（spawn motion=1 + 初速 0 的 dynamic 造物）+ 炮身（场造物）每拍 field_pulse 推膛内炮弹——D-018 shoot 退役的物理路初速来源。

## 验收标准（硬 Gate）

1. `cargo test --workspace` 全绿（基线 902 + 新测试 ≥ 35），awk 累加 test result 行计数入报告。
2. `cargo clippy --workspace` 零 warning 零 error。
3. grep 对账：`field_radius|field_pulse|on_field|effective_gravity_drag|FIELD_FORMING` 在 entity.rs/sandbox.rs/default.rs/main.rs/net.rs/index.html 各有真实命中。
4. 新测试覆盖清单（TDD 先红后绿）：
   - core：clamp 三函数（边界值/非法值/非有限）×3、serde 旧快照 default（无 field 字段的 JSON 反序列化=无场语义）×1
   - arena：spawn 第 15-17 参校验（钳制/warn）×3、PendingSpawn 透传 ×1、on_field 未导出 no-op ×1、on_field 预算包裹+燃料恢复 ×1、field_pulse 校验五条（非有限/超模长/恰等放行/radius 钳/±10 格）×5
   - physics：effective_gravity_drag 五锚（无场/单场重力/单场阻力/双场代数抵消/域钳制兜底）×5、step_projectiles 域内弹丸改弹道 ×1、成型期外不结算 ×1、域外实体不受影响 ×1
   - main：17 参 spawn 端到端（造物带 field 落库）×1、场影响生物重力落点 ×1、field_pulse 推 dynamic 造物 ×1、field_pulse 推弹丸 ×1、快照含 fields ×1、成型期拍数 ×1
5. 文档同步（一个 commit 内）：api.md（spawn 17 参签名+参数表 3 行+field 节+on_field 导出节+field_pulse 节+import 计数 13→15）；decisions.md #051（力场落地：field 家族/零计费/叠加 v2/协议 v3/成型期 3 拍/分离式发射器，≤5 行为什么）；dev-plan.md D-012 改 ✅；game-menu.md 1.7 常数总账补 FIELD_FORMING_TICKS=3 + field 参数域；progress.md「现在」更新。
6. examples/ 加 `field-demo.wat`（声明 field 的哨塔示例：出生声明 40 倍阻力护盾场 radius 3.0，注释讲清参数）——**玩家文档的一部分，非测试**。

## 已知信息（不需要查，直接用）

- cargo 转发器：CC 沙箱里 cargo 不在 PATH，用 `node $LOCALAPPDATA/Temp/cc-cargo.mjs test --workspace`（参数原样转发；文件已存在，丢了按头两行注释重建）。
- spawn 现签名 14 参：`(param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32)`，本刀 +3 参成 17 参 `(param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64)`。
- clamp_motion 在 crates/core/src/entity.rs:180 附近；set_motion 直存在 :325 附近；process_spawn_requests 的 set_motion 位置在 main.rs:1927 附近——field 三参照抄这三处结构。
- env.spawn 闭包在 crates/arena/src/sandbox.rs:861 起（校验段+INFO 日志+PendingSpawn push 在 :1034 附近）。
- 物理步：DefaultPhysicsEngine::step 在 crates/physics/src/default.rs:412；step_projectiles 在 :192；apply_drag 在 :72；参数结构 DefaultPhysics 在 crates/core/src/physics.rs:100（gravity=-9.8/drag=0.05/max_speed=25 默认）。
- on_contact 预算包裹范式在 sandbox.rs:1394 call_on_contact（存剩余→set→调→恢复→重算 consumed）；contact_fuel config 模式在 bin config.rs（CODEWORLD_CONTACT_FUEL）。
- 快照：bin/codeworld/src/net.rs EntitySnapshot:127、build_snapshot:164；STATIC_DIR="bin/codeworld/static"；前端 index.html:990 附近画 creation 圆，1524 行总量，加 fields 环照现有 canvas 段。
- 破坏性变更调用点清单：players/pioneer.wat:56、players/pioneer.wat.txt 同、main.rs birth_motion_spawner_wat:9112（+全部 spawner 生成器）、arena sandbox.rs 内嵌 WAT 测试、examples/sentinel.wat（grep 确认是否含 spawn 调用再改）。
- 世界极速常量 WORLD_MAX_SPEED=25.0（core/src/physics.rs:97）；SPAWN_RANGE=10 在 arena sandbox.rs。
- 现有 import 计数 13 个（api.md「已开放 import（13 个）」标题），本刀 +2 成 15：on_field（host→沙箱方向，不是 env import，**api.md 归"实体导出函数契约"节**，import 计数只加 field_pulse 一个成 14——**注意：api.md 标题写 13→14**，on_field 是导出不是 import，别数错）。
- decisions.md 最新号 #050，本刀 #051。
- 测试基线 902 全绿（2026-10-21 实测）；.cc-bugs.md 有历史坑列表开工前扫一眼。
- **物理常量以代码实测为准**：重力 -9.8/阻力 0.05/极速 25 已在上面 grep 核对过，可信。

## 硬约束

- 代码一律你写（CC 全权实现），但**禁止引入定案外新概念**（不做极速场/不做优先级/不做 enter-exit 事件/不做世界侧收费/不做场所有权标签）。
- 数值护栏是域钳制（合法域内自由，超界钳回+警告非拒绝——出生声明家族哲学）。
- 每个新增/修改函数加日志（tracing，错误必打 errMsg 原文）；数据流两端日志（收集场清单→物理结算→快照）。
- TDD：每块 RED→GREEN；超 100 工具调用时写 5 行 handoff 到 .cc-progress.md 再继续。
- 收尾三步：①`node $LOCALAPPDATA/Temp/cc-cargo.mjs test --workspace` 全绿计数 ②clippy 干净 ③git commit（逐文件 add，message 前缀 `feat(physics): D-012 力场——`），然后把报告写到 `$LOCALAPPDATA/Temp/cc-report.md`（结构：改动清单/测试计数/冒烟说明/文档同步/遗留）。

## ⚠️ 完成后必须报告

报告写入 `$LOCALAPPDATA/Temp/cc-report.md` 后才算任务完成。报告必须含：全 workspace 测试通过总数（awk 累加）、clippy 状态、commit hash、8 项交付物逐条对账表（做没做/在哪/测试名）。
