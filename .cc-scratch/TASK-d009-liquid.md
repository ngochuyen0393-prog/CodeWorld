# TASK D-009 水体+浮力（液体造物声明家族 + 浮沉涌现 + 附加质量立法）

[Superpowers 激活：本任务走 test-driven-development——每块先写 RED 测试再实现 GREEN；收尾跑 verification-before-completion 心检。]

你是 CodeWorld（D:/projects/CodeWorld）的实现工程师。宪法级大刀，土豆已拍板。定案全文在 docs/dev-plan.md「D-009 水体系统」条目（53-66 行）——**设计已全部定死，禁止再设计、禁止引入定案外的新概念**。本任务书自含全部定案要点+实现细节裁决，与 dev-plan 冲突时以 dev-plan 原文为准；任务书内公式是定案公式的实现展开，照写。

## 背景（一段话）

世界地形已支持起伏（D-016：Terrain 高度数组，坑谷=天然湖底）。D-009 让"水"进入世界：**液体=造物出生声明家族新成员**（与 radius/mass/binding/motion/field 并列）——玩家 spawn 造物时声明"我是液体：密度 ρ+阻尼 k"，造物身体（格对齐 AABB）围成的矩形区域成为水域，域内实体按液体物理被结算（浮沉/阻尼/附加质量），**全公式涌现零特例**（木浮铁沉自动出来，无浮沉清单——MC 硬编码清单是反面教材违 #011）。不做守恒水体（水不流动不搬运——MC 1.13 教训）；不做世界内置材料表（油/岩浆是玩家黑话，世界只认数字）。

## 交付物（7 项，验收逐条对账）

### 交付物 1：spawn 液体声明（第 18/19 参）

spawn 加 2 个参数（17 参 → 19 参，破坏性变更）：
- 第 18 参 `liquid_density: f64` —— 液体密度 ρ。域 [0.0, 5.0]，0=非液体（默认=现状语义零回归）；非有限/负值钳 0 并 warn；超上限钳 5.0。>0 即声明"我的身体区域是液体"。
- 第 19 参 `liquid_drag: f64` —— 水中阻尼 k（每拍速度保留系数的 MC 语义，见交付物 2）。域 [0.0, 5.0]，非有限/负值钳 0 并 warn；超上限钳 5.0。（k=0 配 ρ>0 合法：无阻尼理想液体。）

实现落点（照 field 三参的 D-012 先例逐处抄结构）：
- `crates/core/src/entity.rs`：Entity 加字段 `liquid_density: f64 / liquid_drag: f64`（各带 `#[serde(default)]`，默认 0=非液体——旧快照反序列化零行为变化）；常量 LIQUID_DENSITY_MAX=5.0 / LIQUID_DRAG_MAX=5.0 + clamp 函数（照 clamp_field_radius 模式）；getter/setter（照 set_field_radius 直存模式）。
- `crates/arena/src/sandbox.rs`：PendingSpawn 加 2 字段；env.spawn 闭包加 2 参+校验（照 field 校验段模式）+INFO 日志带 liquid 两值。
- `bin/codeworld/src/main.rs` process_spawn_requests：`entity.set_liquid_density(...)` / `entity.set_liquid_drag(...)` 两行。
- **液体造物=纯布景无沙箱**：`liquid_density > 0.0` 的造物出生**跳过 create_sandbox**（液体是物理域不是居民——"有沙箱=居民"判据），但**照常占世界实体名额**（防刷水弹幕）。INFO 日志注明"液体造物：无沙箱布景"。
- **液体造物无视 motion 声明**，一律按 static 格对齐 AABB 处理（水不会动——"不做守恒水体"的实现面）。INFO 日志注明。
- **破坏性变更全调用点同一 commit 修完**（漏一处=玩家进场即 translation error）：players/ 全部 .wat 与 .wat.txt（locals 显式补 2 参 0.0/0.0，含 _template.wat.txt）、bin main 全部 spawner 生成器 WAT（birth_motion_spawner_wat 等）、arena sandbox.rs 内嵌 WAT 测试、examples/ 含 spawn 调用者（field-demo.wat / sentinel.wat——grep 确认后改）。

### 交付物 2：浮力结算（纯函数 + 物理步接入）

**结算次序定死（测试锚定）**：①先力场 effective_gravity_drag（现状不动）得 (g_field, drag_field)；②再浮力改写；③水中阻尼；④附加质量进阻力项。浮力与力场是正交改写、代数叠加。

核心公式（全部定死，CC 照写）：
- **域影响判定**：实体圆（圆心 c、半径 r）与液体 AABB 相交（水平方向 [c.x−r, c.x+r] 与 [min_x, max_x] 有重叠，且竖直方向同理）。相交才计算浸入分数。
- **浸入分数 f**（竖径夹逼近似，纯函数）：`f = clamp((min(y_max, c.y+r) − max(y_min, c.y−r)) / (2r), 0.0, 1.0)`。f=0（只擦边）=不受液体影响。三个精确锚：全浸没 f=1、圆心在液面 f=0.5、不接触 f=0；中间值连续单调。【精确弓形面积留 D-013 形状物理包时代，此近似在 decisions.md 注记——别自作主张升级成弓形】
- **推导密度**：`ρ物 = m / (π·r²)`（实体真实质量 m——密度已从出生参数删除，浮力用推导密度；m 有下限 0.05 保证除法安全，仍加防御：ρ物 ≤ 0 时跳过浮力并 warn 一次）。
- **浮力改写重力**：`g_final = g_field × (1 − (ρ液/ρ物) × f)`，钳制 [−98.0, +98.0]（浮力档对称十倍域——**注意与力场档 [−98,0] 不同**，浮力恰恰要向上；钳制只作用于浮力改写后的最终值）。性质自检：ρ物=ρ液 且 f=1 → g_final=0 悬浮；ρ液>ρ物（木）→ 乘数为负 → g_final 与 g 反向=向上推；力场重力×0 的域内 → g_final=0（零重力场里浮力也消失——物理自洽，浮力源于重力）。
- **水中阻尼（MC 次序）**：f>0 的实体本拍速度先乘保留系数再加（与 MC 三步乘法加法同构）：`v ← v × k_eff + g_final`，其中 `k_eff = clamp(k, 0.0, 1.0)`（声明域 [0,5]，有效保留系数钳 [0,1]——k>1 的声明合法但按 1 结算，不许凭空加速）。锚点：k=0.35 → 入水一拍速度掉 65%（"入水一拍掉 60~70%"锚）。
- **附加质量（独门立法）**：`m_eff = m + ρ液 × π × r² × f`（2D 圆解析解=排开流体质量，f 加权：半沉排开一半）。**作用点=apply_drag 的质量参数**（现有定律质量为分母——质量大阻力影响小；水下加速钝从阻力响应涌现）。**ρ物 计算用真实 m 不用 m_eff**（防递归）。若现状 thrust 冲量直接进速度不做 /m（api.md 如此），**保持同构不发明新除法**，decisions 注记"附加质量当前作用点=阻力项+未来动量结算"。
- **多域取最高密度**：实体同时接触多个液体域 → 取 ρ液 最高者结算（Noita 分层范式的咱家形态：最稠的算），k 取同域配对值（ρ 与 k 是同一造物的配对声明）。
- **aloft（悬停）优先级高于浮力**：与力场同款——悬停声明本拍免重力免浮力免阻尼，浮力不覆盖它。

**物理步路由（关键裁决，防"半沉"锚做不出来）**：f>0 的实体（生物+dynamic 造物）**一律走 D-010a 冲量积分路径，即使速度为零**——水中动力学=速度制（阻尼定律 v×k 需要速度状态才成立；零速走位置制则每拍位移=全额 g_final，水面附近会 ±9.8 格疯狂弹跳，"典型物体半沉"永远看不到）。积分次序（复用现有冲量路径结构，测试锚定）：冲量入速度（若有）→ 重力（g_final）→ 水中阻尼（v ← v×k_eff + g_final 一步完成，替代原"重力入速度"步）→ apply_drag(v, m_eff, drag_field) → 世界极速钳 → 中点位移 → 落地解析（触湖底=贴面+速度清零，规则照旧）。水面平衡=逐拍结算自然收敛（阻尼 k<1 的线性系统），**不写平衡点求解器**。域外（f=0）实体行为与现在逐位一致（零回归锚）。
- 弹丸（step_projectiles）：同款 g_final/阻尼/m_eff 接进弹丸积分器（弹丸圆心+半径算 f；ρ物=m/πr²——标准弹 r=0.15/m=1 → ρ物≈14 很稠，任何液体都缓沉不浮，物理诚实）。弹丸入水改弹道=本刀可见效果之一。
- static 造物/液体造物自己：不受液体影响（static 不吃物理现状照旧）。

### 交付物 3：纯函数 liquid_effect 单独抽出+单测

纯函数（建议 crates/physics/src/liquid.rs，或并入 fields.rs——CC 定，命名照 effective_gravity_drag 先例）：
- 输入：实体 (pos, radius, mass, velocity)、力场结果 (g_field, drag_field)、液体域清单 `&[LiquidDomain{min, max, density, drag_k}]`
- 输出：`(g_final, k_eff, m_eff, f, domain_id: Option)`——f>0 即在水中（供日志与前端高亮判定）
- 单测六锚：域外零回归（与无液体逐位一致）/ 全浸悬浮（ρ物=ρ液, f=1 → g_final=0）/ 木浮（g_final>0 向上）/ 铁沉（g_final 仍负但 |g| 变小）/ 多域取最高密度 / 浮力档钳制触发（极端 ρ液/ρ物 比值钳 +98）/ 半沉锚（f=0.5、ρ液=2ρ物 → g_final=0，"典型物体半沉"的平衡点）。

### 交付物 4：快照协议 + 前端水域渲染

- `bin/codeworld/src/net.rs`：WorldSnapshot 加 `liquids: Vec<LiquidSnapshot>`，LiquidSnapshot{x, y, w, h, density}（AABB 转矩形）。serde 照 fields 先例。
- 前端 index.html：按 liquids 画半透明矩形，**颜色=密度映射**（渲染层装饰非世界事实，玩家造 ρ=0.8 的"蓝油"世界不拦不判）：查表+线性插值——ρ 0.75 淡黄 / 1.0 蓝 / 1.4 琥珀 / 2.5 橙红；[0.1,0.75) 白蓝、(2.5,5.0] 深红。alpha 固定半透明（如 0.45），别过度设计。照 fields 环先例写绘制段。

### 交付物 5：感知走现有原语（零新事件零新 import）

- 液体实体=世界实体：照常被 scan/probe 探测（布景与规则分层，WC3 先例）。
- **"我在水里"=玩家代码自己算**：scan 探测到液体造物 + 自己坐标在域内 = 在水里。**世界不做"我在水里"专属事件/回调/状态广播**（感知环境→切换行为=玩家代码通用范式，定案原文）。状态机（岸上/入水/半沉/全浸没/出水五态）是玩家的功课。
- **世界生成器出生水域=本刀不做**（归"出生地形多样化"后续刀；官方示例活水另行安排）——本刀只落"液体造物声明+结算+渲染"。dev-plan 挂起项注记。
- 换血/攻击对液体造物：无沙箱无代码可换——on_attack 路径遇无沙箱实体**照现有无沙箱实体（弹丸/尸体若有同款分支）的现状处理照抄**；grep 现状先看，别发明新分支。

### 交付物 6：文档同步（一个 commit 内）

- api.md：spawn 19 参签名+参数表 2 行+「液体（D-009）」节（声明/浮沉公式/感知范式/官方示例密度表=社区预设非内置清单）；**import 计数 16 不变（本刀零新 import）**。
- decisions.md #053（≤5 行为什么）：液体造物声明家族/浮力公式 g×(1−ρ比×f)/附加质量 m_eff=m+ρπr²f/多域最高密度/浸入分数竖径夹逼近似（精确弓形挂 D-013）。
- dev-plan.md：D-009 改 ✅（挂起注记：世界生成器水域+官方示例活水归后续）。
- game-menu.md：浮力词条 ⬜→✅；1.7 常数总账补 LIQUID_DENSITY_MAX=5.0 / LIQUID_DRAG_MAX=5.0 / 有效保留钳 [0,1] / 浮力档钳 [−98,+98]。
- progress.md「现在」更新。

### 交付物 7：examples/liquid-demo.wat（玩家文档）

出生声明 ρ=1.0 / k=0.35 水域的示例造物（一块 5×3 水坑），注释讲清：两参数语义、"油 0.75/水 1.0/蜂蜜 1.4/岩浆 2.5"是社区预设非世界内置、"我在水里"感知范式（scan+坐标比对）。玩家文档的一部分，非测试。

## 验收标准（硬 Gate）

1. `cargo test --workspace` 全绿（基线 1069 + 新测试 ≥ 30），awk 累加 test result 行计数入报告。
2. `cargo clippy --workspace` 零 warning 零 error。
3. grep 对账：`liquid_density|liquid_effect|LiquidDomain|LiquidSnapshot|LIQUID_DENSITY_MAX` 在 entity.rs / sandbox.rs / default.rs（或 liquid.rs）/ main.rs / net.rs / index.html 各有真实命中。
4. **开工先实测核对**：apply_drag 口径（衰减速率与质量关系）、thrust 冲量是否做 /m、无沙箱实体现状处理——与任务书描述不符时以代码为准并在报告注明差异。
5. 新测试覆盖清单（TDD 先红后绿）：
   - core：clamp 两函数（边界/非法/非有限）×3、serde 旧快照 default ×1
   - arena：spawn 第 18/19 参校验（钳制/warn）×3、PendingSpawn 透传 ×1、液体造物跳过沙箱 ×1
   - physics：纯函数六锚（见交付物 3）×7、水中阻尼衰减（k=0.35 一拍掉 65%）×1、附加质量（m_eff=1+π×0.25≈1.7854 数值锚+水下阻力响应更钝）×2、弹丸入水改弹道 ×1、多域最高密度 ×1、域外零回归（无液体世界全测试照绿）×1、aloft 优先于浮力 ×1
   - main/bin：19 参端到端（液体造物落库+AABB+无沙箱）×1、木浮铁沉长跑断言（≥10 拍：木球上浮到面/铁球沉底）×1、快照含 liquids ×1
6. 前端人工验收清单写进报告（两分钟级编号步骤+预期现象）：起世界→造水（liquid-demo 或测试 WAT）→看蓝色水域→丢轻球看上浮半沉→丢重球看沉底→射弹入水看弹道变缓。
7. 报告尾附**7 项交付物对账表**（做没做/在哪/测试名 三栏）。

## 已知信息（不需要查，直接用）

- cargo 转发器：CC 沙箱里 cargo 不在 PATH，用 `node $LOCALAPPDATA/Temp/cc-cargo.mjs test --workspace`（参数原样转发；文件已存在，丢了按其头两行注释重建）。
- spawn 现签名 17 参 `(param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 f64)`，本刀 +2 成 19 参（尾加 `f64 f64`）。
- 结构先例锚点：clamp_field_radius 在 crates/core/src/entity.rs:252、set_field_radius :455；env.spawn 闭包 crates/arena/src/sandbox.rs:861 起（校验+INFO+PendingSpawn push ~:1034）；物理步 DefaultPhysicsEngine::step 在 crates/physics/src/default.rs:462、step_projectiles :215、apply_drag :72 附近；DefaultPhysics 在 crates/core/src/physics.rs:100（gravity=−9.8/drag=0.05/max_speed=25）；力场 FieldEffect fields.rs:30 / effective_gravity_drag :53 / collect_fields :106（液体域收集照 collect_fields 模式：alive+liquid_density>0 的 AABB，EntityId 升序）；快照 net.rs EntitySnapshot:127 / build_snapshot:164；前端 index.html ~:990 画 creation 圆、fields 环先例已在。
- 调用点清扫清单：players/pioneer.wat、pioneer.wat.txt、_template.wat.txt、bridge-demo.wat、jumper.wat（grep spawn 确认）；examples/field-demo.wat、sentinel.wat（grep 确认）；bin main 全部 spawner WAT 生成器；arena sandbox.rs 内嵌 WAT 测试。
- 测试基线 1069 全绿（2026-10-22 实测）；decisions.md 最新 #052，本刀 #053；api.md「已开放 import（16 个）」标题数字不动。
- .cc-bugs.md 有历史坑列表，开工前扫一眼。
- bat/cmd 禁中文；中文进 md/rust 源码没问题。

## 硬约束

- 代码一律你写（CC 全权实现），但**禁止引入定案外新概念**。六项禁令：不做守恒水体（水不流动）/不做世界材料清单（无内置浮沉名单）/不做平衡点求解器（逐拍结算自然收敛）/不做"我在水里"专属事件/不做液体专属计费/不做液体所有权标签。
- 数值护栏=域钳制（合法域内自由，超界钳回+警告非拒绝——出生声明家族哲学）。
- 每个新增/修改函数加日志（tracing，错误必打 errMsg 原文）；数据流两端日志（收集液体域→物理结算→快照；实体入水/出水 INFO 级带 f 值）。
- TDD：每块 RED→GREEN；超 100 工具调用时写 5 行 handoff 到 .cc-progress.md 再继续。
- 收尾三步：①cargo 转发器全绿计数 ②clippy 干净 ③git commit（逐文件 add，message 前缀 `feat(physics): D-009 水体——`），然后报告写 `$LOCALAPPDATA/Temp/cc-report.md`。

## ⚠️ 完成后必须报告

报告写入 `$LOCALAPPDATA/Temp/cc-report.md` 后才算任务完成。报告必须含：全 workspace 测试通过总数（awk 累加）、clippy 状态、commit hash、7 项交付物逐条对账表（做没做/在哪/测试名）、前端人工验收清单、遗留项。
