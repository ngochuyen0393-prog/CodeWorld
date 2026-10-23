//! 实体（Entity / EntityId / PlayerId / Appearance / ActionState）
//!
//! 实体是世界中的存在单位。world-core 只存元数据，
//! WASM Module/Instance 由 code-arena 层管理（架构文档 2.2 节）。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::guard::InstructionGuard;
use crate::survival::SurvivalPoints;

/// 实体唯一标识
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct EntityId(pub Uuid);

impl EntityId {
    /// 生成一个新的随机实体ID
    pub fn new() -> Self {
        let id = Self(Uuid::new_v4());
        tracing::debug!(entity_id = %id.0, "EntityId 生成");
        id
    }
}

impl Default for EntityId {
    fn default() -> Self {
        Self::new()
    }
}

/// 玩家唯一标识（实体创建者）
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlayerId(pub Uuid);

impl PlayerId {
    /// 生成一个新的随机玩家ID
    pub fn new() -> Self {
        let id = Self(Uuid::new_v4());
        tracing::debug!(player_id = %id.0, "PlayerId 生成");
        id
    }
}

impl Default for PlayerId {
    fn default() -> Self {
        Self::new()
    }
}

/// 外观数据（角色登录门槛：必须有，渲染模块画什么就靠这个）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Appearance {
    /// ASCII字符（MVP）或形状标识（后续Canvas/Bevy）
    pub glyph: String,
    /// 颜色
    pub color: String,
    /// 大小
    pub size: f64,
}

impl Appearance {
    /// 创建外观
    pub fn new(glyph: String, color: String, size: f64) -> Self {
        let app = Self { glyph, color, size };
        tracing::debug!(glyph = %app.glyph, color = %app.color, size, "Appearance 创建");
        app
    }
}

/// 动作状态（影响外观显示，如弯腰/蹲下/挥手）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionState {
    /// 待机
    Idle,
    /// 走路
    Walking,
    /// 跳跃
    Jumping,
    /// 蹲下
    Crouching,
    /// 挥手
    Waving,
    /// 玩家自定义动作
    Custom(String),
}

/// 实体（世界中的存在单位）
///
/// world-core 只存元数据与物理/生存状态；WASM Module/Instance 由 code-arena 管理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// 实体ID
    pub id: EntityId,
    /// 创建者
    pub owner: PlayerId,
    /// WASM 模块的 git commit hash（只定义不实现）
    pub code_hash: String,

    // 位置与速度（物理状态，经变换管线后的逻辑坐标）
    pos: (f64, f64),
    velocity: (f64, f64),

    // 身体半径（#030 半径物理：活动实体=圆，圆心即 pos；creation 不用它，用占地 AABB）
    /// serde default：旧快照无此字段时回退 0.5（存储向后兼容）
    #[serde(default = "default_radius")]
    radius: f64,

    // 质量（#034 物理人格属性：与 radius 并列；阻力定律分母，弹道/动量涌现的根基。
    // 铁律：mass 只参与物理反应，永不参与攻击结算——攻杀仍是代码对代码）
    /// serde default：旧快照无此字段时回退 1.0（存储向后兼容，抄 radius 先例）
    #[serde(default = "default_mass")]
    mass: f64,

    // 绑定契约（#034 修订四：出生时声明写进造物数据，世界结算引擎每拍托管执行
    // ——遗嘱模式：契约不需要链路，控制（实时指挥）才需要）
    /// serde default：旧快照无此字段时回退 unbound（现状语义）
    #[serde(default = "default_binding")]
    binding: i32,
    /// 绑定对象（fade_on_death 的"主人"：造物=创造者、弹丸=发射者；
    /// None=无绑定对象）。serde default 同上
    #[serde(default)]
    bound_to: Option<EntityId>,

    // 运动身份（D-003 motion 出生声明：spawn 第 10 参，声明族第 4 个与 radius/mass/binding
    // 同款，dev-plan 2026-10-17 拍板）。仅造物受声明影响：
    // - 0=static（默认=现状）：钉死——身体=占地格 AABB，无速度矢量物理；
    // - 1=dynamic：全活——受完整物理（重力落点解析/速度矢量阻力/极速钳制），
    //   身体=声明 radius 的圆，与生物同管线；"被撞飞"依赖碰撞响应（D-010）不在本档。
    // kinematic（剧本档）缓立：全活+意图移动已可表达电梯。
    // 进场生物不受此字段影响（本体天生 dynamic，写死全活不开放声明）。
    /// serde default：旧快照无此字段时回退 static（现状语义，存储向后兼容）
    #[serde(default = "default_motion")]
    motion: i32,

    // 力场出生声明家族（D-012：spawn 第 15/16/17 参，decisions.md #040——
    // "场造物"作为特殊品类在法律上不存在，世界只认声明不认身份；场源=物理
    // 身体，效果=以身体为锚向外延伸的参数改写区）。
    // 场域半径：以造物身体中心（pos）为锚向外延伸的参数改写区半径；
    /// serde default：旧快照无此字段时回退 0.0=无场（现状语义，存储向后兼容）
    #[serde(default = "default_field_radius")]
    field_radius: f64,
    // 重力缩放系数：生效重力 = 世界重力 × scale（乘法式缩放，叠加 v2 代数形式：
    // 多场叠乘，负向抵消靠 scale>1 实现）。
    /// serde default：旧快照无此字段时回退 1.0=中性缩放（重力不变）
    #[serde(default = "default_field_gravity_scale")]
    field_gravity_scale: f64,
    // 阻力改写量：加法式 delta（不是缩放——叠加 v2 代数抵消语义要求改写量是
    // delta，正负互吃：+2 与 -2 归零，对冲场是真实战术）。生效阻力 = 世界阻力 + Σdelta。
    /// serde default：旧快照无此字段时回退 0.0=中性（加 0 不改写）
    #[serde(default = "default_field_drag_delta")]
    field_drag_delta: f64,

    // 液体出生声明家族（D-009 水体：spawn 第 18/19 参——与 radius/mass/binding/
    // motion/field 并列，世界只认数字不认名字：油 0.75/水 1.0/蜂蜜 1.4 是玩家
    // 黑话，decisions.md #053）。liquid_density > 0 = 声明"我的身体区域（格对齐
    // AABB）是液体"——域内实体按液体物理被结算（浮沉/阻尼/附加质量全公式涌现，
    // 无浮沉清单）；0=非液体（默认=现状语义零回归）。
    /// serde default：旧快照无此字段时回退 0.0=非液体（存储向后兼容）
    #[serde(default)]
    liquid_density: f64,
    // 水中阻尼 k（每拍速度保留系数的 MC 语义：v ← v×k_eff + g_final；k>1 的
    // 声明合法但结算按 1 钳——不许凭空加速）。
    /// serde default：旧快照无此字段时回退 0.0=无阻尼（存储向后兼容）
    #[serde(default)]
    liquid_drag: f64,

    // 碰撞响应出生声明家族（D-010 碰撞响应刀：spawn 第 21/22 参——与 radius/
    // mass/binding/motion/field/liquid 同门，世界只认数字不认名字：钢珠/黏土/
    // 冰面/橡胶是玩家黑话）。e=弹性（撞=动量交换的程度）/f=摩擦（切向衰减）。
    // 组合规则（接触对）= Box2D 款：e=max(e1,e2) / f=√(f1×f2)，组合后才允许
    // 物理使用（crates/core/src/physics.rs combine_restitution/combine_friction）。
    // 弹性（出生声明第 21 参，域 [0,1]，0=撞停现状语义）：
    /// serde default：旧快照无此字段时回退 0.0=撞停（存储向后兼容）
    #[serde(default = "default_restitution")]
    restitution: f64,
    // 摩擦（出生声明第 22 参，域 [0,1]，0.6=Box2D v3 新默认"比冰滑比橡胶涩"）：
    /// serde default：旧快照无此字段时回退 0.6（存储向后兼容）
    #[serde(default = "default_friction")]
    friction: f64,

    // 链账本（D-011 世界内链路段：link=binding 家族新成员，decisions.md #038
    // 通道公理世界内段）。活链造物指向母体——与 bound_to 同款形态（Option<EntityId>
    // 实体字段）、同款快照待遇。链管通信（契约管生死，两轴正交自由组合）：
    // None=无链放生（默认=现状语义零回归）；Some=活链（母体 command 可达、
    // 自己 report 沿链上行）。断链=母体死时世界清算清 None（孤儿按 binding 活）。
    /// serde default：旧快照无此字段时回退 None=无链（存储向后兼容）
    #[serde(default)]
    link_to: Option<EntityId>,

    // 公开存储块（D-011 存储件：造物=移动硬盘的"盘面"——storage_write 全量
    // 覆盖自己的公开块，对方贴身 storage_read 拉读）。数据在世界侧非沙箱内存：
    // 不烧燃料、不占沙箱内存，随实体入快照（重启不丢=比沙箱内存耐用）。
    /// serde default：旧快照无此字段时回退空块（存储向后兼容）
    #[serde(default)]
    storage: Vec<u8>,

    // 外观与动作状态
    /// 渲染数据
    pub appearance: Appearance,
    action_state: ActionState,

    // 指令配额 + 生存点数（自身有带日志的方法）
    /// 指令配额（纯技术保护）
    pub instruction_guard: InstructionGuard,
    /// 生存点数（框架管理的唯一硬数值）
    pub survival: SurvivalPoints,

    // 生命周期
    alive: bool,
    /// 创建 tick
    pub created_tick: u64,

    // 沙箱资产装戴清单（#051 皮入沙箱）
    /// 沙箱资产装戴清单（#051 皮入沙箱）：asset_id 列表（av_+16hex，指向 bin 侧
    /// AssetMirror 镜像仓）。serde default 空（旧快照兼容）。与沙箱 SandboxData.assets
    /// 镜像一致（换血随行语义见 arena replace_sandbox_assets）；出生装戴由玩家声明
    /// （挂实体刀的 spawn 参数化后续刀），夺舍转移时随行不变（资产跟沙箱走）。
    #[serde(default)]
    pub assets: Vec<String>,
}

/// radius 默认值（新实体与旧快照回退共用：四生物/玩家/测试构造器全兼容）
fn default_radius() -> f64 {
    0.5
}

/// mass 默认值（#034：新实体与旧快照回退共用）
fn default_mass() -> f64 {
    1.0
}

/// 绑定契约默认值（#034 修订四：新实体与旧快照回退共用 unbound）
fn default_binding() -> i32 {
    BINDING_UNBOUND
}

/// 运动身份默认值（D-003：新实体与旧快照回退共用 static——现状语义）
fn default_motion() -> i32 {
    MOTION_STATIC
}

/// 场域半径默认值（D-012：新实体与旧快照回退共用 0.0=无场——现状语义）
fn default_field_radius() -> f64 {
    0.0
}

/// 场重力缩放默认值（D-012：新实体与旧快照回退共用 1.0=中性——重力不变）
fn default_field_gravity_scale() -> f64 {
    1.0
}

/// 场阻力改写量默认值（D-012：新实体与旧快照回退共用 0.0=中性——加 0 不改写）
fn default_field_drag_delta() -> f64 {
    0.0
}

/// 弹性默认值（D-010：新实体与旧快照回退共用 0.0=撞停现状语义）
fn default_restitution() -> f64 {
    0.0
}

/// 摩擦默认值（D-010：新实体与旧快照回退共用 0.6=Box2D v3 新默认）
fn default_friction() -> f64 {
    0.6
}

/// 液体密度合法上限（D-009：域 [0.0, 5.0]——0=非液体默认，5=岩浆级上限。
/// 参照系=水 1.0 无量纲，油 0.75/蜂蜜 1.4/岩浆 2.5 都是玩家黑话非内置清单）
pub const LIQUID_DENSITY_MAX: f64 = 5.0;

/// 水中阻尼声明合法上限（D-009：域 [0.0, 5.0]——声明域宽松、结算域收紧：
/// k>1 的声明合法但有效保留系数按 1 结算，不许凭空加速）
pub const LIQUID_DRAG_MAX: f64 = 5.0;

/// 弹性校验钳制（spawn 第 21 参口径，D-010 碰撞响应刀）：域 [0.0, 1.0] 硬钳，
/// 非有限值回退默认 0.0（撞停现状语义）并 warn（带原值与钳后值，与 mass 的
/// "超界警告并钳制"同款措辞风格——出生声明家族哲学：超界钳回+警告非拒绝）
pub fn clamp_restitution(v: f64) -> f64 {
    const DEFAULT: f64 = 0.0;
    if !v.is_finite() {
        tracing::warn!(value = v, clamped = DEFAULT, "[restitution] 非有限值，回退默认 0.0（撞停）");
        return DEFAULT;
    }
    if !(0.0..=1.0).contains(&v) {
        let clamped = v.clamp(0.0, 1.0);
        tracing::warn!(value = v, clamped, "[restitution] 超出域 [0,1]，钳制");
        return clamped;
    }
    v
}

/// 摩擦校验钳制（spawn 第 22 参口径，D-010 碰撞响应刀）：域 [0.0, 1.0] 硬钳，
/// 非有限值回退默认 0.6（Box2D v3 新默认）并 warn（带原值与钳后值，同款措辞）
pub fn clamp_friction(v: f64) -> f64 {
    const DEFAULT: f64 = 0.6;
    if !v.is_finite() {
        tracing::warn!(value = v, clamped = DEFAULT, "[friction] 非有限值，回退默认 0.6");
        return DEFAULT;
    }
    if !(0.0..=1.0).contains(&v) {
        let clamped = v.clamp(0.0, 1.0);
        tracing::warn!(value = v, clamped, "[friction] 超出域 [0,1]，钳制");
        return clamped;
    }
    v
}

/// 液体密度校验钳制（spawn 第 18 参口径）：域 [0.0, 5.0]，负值/非有限钳 0
/// （非液体最保守回退）并 warn，超上限钳 5.0——出生声明家族哲学：超界钳回+
/// 警告非拒绝
pub fn clamp_liquid_density(v: f64) -> f64 {
    if !v.is_finite() {
        tracing::warn!(value = v, clamped = 0.0, "[liquid] 液体密度非有限值，钳 0（非液体）");
        return 0.0;
    }
    if v < 0.0 {
        tracing::warn!(value = v, clamped = 0.0, "[liquid] 液体密度为负，钳 0（非液体）");
        return 0.0;
    }
    if v > LIQUID_DENSITY_MAX {
        tracing::warn!(value = v, clamped = LIQUID_DENSITY_MAX, "[liquid] 液体密度超上限，钳制");
        return LIQUID_DENSITY_MAX;
    }
    v
}

/// 水中阻尼校验钳制（spawn 第 19 参口径）：域 [0.0, 5.0]，负值/非有限钳 0
/// （无阻尼）并 warn，超上限钳 5.0（结算时有效保留系数另钳 [0,1]）
pub fn clamp_liquid_drag(v: f64) -> f64 {
    if !v.is_finite() {
        tracing::warn!(value = v, clamped = 0.0, "[liquid] 水中阻尼非有限值，钳 0（无阻尼）");
        return 0.0;
    }
    if v < 0.0 {
        tracing::warn!(value = v, clamped = 0.0, "[liquid] 水中阻尼为负，钳 0（无阻尼）");
        return 0.0;
    }
    if v > LIQUID_DRAG_MAX {
        tracing::warn!(value = v, clamped = LIQUID_DRAG_MAX, "[liquid] 水中阻尼超上限，钳制");
        return LIQUID_DRAG_MAX;
    }
    v
}

/// 运动身份档位（D-003 motion 出生声明，dev-plan 拍板 2026-10-17；kinematic 剧本档缓立）
/// - static（默认=现状）：造物钉死——格对齐 AABB 身体，无速度矢量物理
/// - dynamic：造物全活——受完整物理（重力/阻力/极速），身体=声明 radius 的圆
///
/// 仅造物受声明影响；进场生物/弹丸不受此字段影响（生物天生 dynamic，弹丸速度积分制）
pub const MOTION_STATIC: i32 = 0;
pub const MOTION_DYNAMIC: i32 = 1;

/// 运动身份校验钳制（spawn 第 10 参口径，与 clamp_binding 同款）：非法值钳 static
/// 并 warn——坏声明不进世界，回退最保守的现状语义
pub fn clamp_motion(v: i32) -> i32 {
    match v {
        MOTION_STATIC | MOTION_DYNAMIC => v,
        other => {
            tracing::warn!(value = other, clamped = MOTION_STATIC, "[motion] 非法运动身份值，钳 static");
            MOTION_STATIC
        }
    }
}

/// 场域半径合法上限（D-012：域 [0.0, 25.0]——0=无场，25=世界尺度护栏，
/// 与 radius 身体上限同值：场再大盖不过世界一格内的身体上限尺度）
pub const FIELD_RADIUS_MAX: f64 = 25.0;

/// 场重力缩放合法上限（D-012：域 [0.0, 10.0]——0=零重力域，10=十倍上限）
pub const FIELD_GRAVITY_SCALE_MAX: f64 = 10.0;

/// 场重力生效值钳制下限（D-012 叠加 v2 域钳制兜底：-98.0 = 世界重力 9.8 ×
/// 缩放上限 10——堆场买不到定律外胜利：不许向上飞的重力、不许超过十倍）
pub const FIELD_GRAVITY_EFFECTIVE_MIN: f64 = -98.0;

/// 场阻力生效值钳制区间（D-012 叠加 v2 域钳制兜底：[0.0, 25.0]——阻力永不
/// 负值（负阻力=凭空加速，不合法），上限恰与世界极速同值不同义）
pub const FIELD_DRAG_EFFECTIVE_MIN: f64 = 0.0;
pub const FIELD_DRAG_EFFECTIVE_MAX: f64 = 25.0;

/// 场域半径校验钳制（spawn 第 15 参口径）：域 [0.0, 25.0]，负值/非有限钳 0
/// （无场最保守回退）并 warn，超上限钳 25.0——出生声明家族哲学：超界钳回+
/// 警告非拒绝
pub fn clamp_field_radius(v: f64) -> f64 {
    if !v.is_finite() {
        tracing::warn!(value = v, clamped = 0.0, "[field] 场域半径非有限值，钳 0（无场）");
        return 0.0;
    }
    if v < 0.0 {
        tracing::warn!(value = v, clamped = 0.0, "[field] 场域半径为负，钳 0（无场）");
        return 0.0;
    }
    if v > FIELD_RADIUS_MAX {
        tracing::warn!(value = v, clamped = FIELD_RADIUS_MAX, "[field] 场域半径超上限，钳制");
        return FIELD_RADIUS_MAX;
    }
    v
}

/// 场重力缩放校验钳制（spawn 第 16 参口径）：域 [0.0, 10.0]，负值钳 0、
/// 非有限钳 1.0（中性缩放）并 warn，超上限钳 10.0
pub fn clamp_field_gravity_scale(v: f64) -> f64 {
    if !v.is_finite() {
        tracing::warn!(value = v, clamped = 1.0, "[field] 重力缩放非有限值，钳 1.0（中性）");
        return 1.0;
    }
    if v < 0.0 {
        tracing::warn!(value = v, clamped = 0.0, "[field] 重力缩放为负，钳 0（零重力域）");
        return 0.0;
    }
    if v > FIELD_GRAVITY_SCALE_MAX {
        tracing::warn!(value = v, clamped = FIELD_GRAVITY_SCALE_MAX, "[field] 重力缩放超上限，钳制");
        return FIELD_GRAVITY_SCALE_MAX;
    }
    v
}

/// 场阻力改写量校验钳制（spawn 第 17 参口径）：域 [-1.0, +25.0]，非有限钳 0
/// （中性）并 warn，超域钳边界——加法式 delta，正负互吃（叠加 v2 代数抵消）
pub fn clamp_field_drag_delta(v: f64) -> f64 {
    if !v.is_finite() {
        tracing::warn!(value = v, clamped = 0.0, "[field] 阻力改写量非有限值，钳 0（中性）");
        return 0.0;
    }
    if v < -1.0 {
        tracing::warn!(value = v, clamped = -1.0, "[field] 阻力改写量低于下限，钳 -1.0");
        return -1.0;
    }
    if v > FIELD_DRAG_EFFECTIVE_MAX {
        tracing::warn!(value = v, clamped = FIELD_DRAG_EFFECTIVE_MAX, "[field] 阻力改写量超上限，钳 25.0");
        return FIELD_DRAG_EFFECTIVE_MAX;
    }
    v
}

/// 绑定契约档位（#034 修订四，design-binding.md 机制层 MVP 三档）
/// - unbound（默认）：离体即世界之物——主人死照飞照命中（弹丸）/独立存在（造物）
/// - fade_on_death（同生）：主人死→在飞弹丸/造物当拍即刻消亡（世界每拍托管执行）
/// - grace_owner（攻击豁免）：造物出刀不结算 bound_to 主人（命中主人=哑弹，
///   deliver 现场读声明，D-018b 接触通道重接）；不声明则误伤主人合法——
///   世界不豁免，豁免是声明
pub const BINDING_UNBOUND: i32 = 0;
pub const BINDING_FADE_ON_DEATH: i32 = 1;
pub const BINDING_GRACE_OWNER: i32 = 2;

/// 契约值校验钳制（shoot 第 6 参/spawn 第 9 参口径）：非法值钳 unbound 并 warn——
/// 出生声明写进造物数据，坏声明不进世界（回退到最保守的现状语义）
pub fn clamp_binding(v: i32) -> i32 {
    match v {
        BINDING_UNBOUND | BINDING_FADE_ON_DEATH | BINDING_GRACE_OWNER => v,
        other => {
            tracing::warn!(value = other, clamped = BINDING_UNBOUND, "[binding] 非法契约值，钳 unbound");
            BINDING_UNBOUND
        }
    }
}

/// 链声明档位（D-011：spawn 第 20 参 link，binding 家族新成员——契约管生死、
/// 链管通信，两轴正交自由组合）
/// - 无链（默认）：放生——现状语义零回归（离体之物独立存在不可遥控）
/// - 活链：可控+可传——母体 command 下行广播可达、造物 report 沿链上行
pub const LINK_UNLINKED: i32 = 0;
pub const LINK_ACTIVE: i32 = 1;

/// 链声明校验钳制（spawn 第 20 参口径，与 clamp_binding 同款）：非法值钳无链
/// 并 warn——坏声明不进世界，回退最保守的放生现状语义
pub fn clamp_link(v: i32) -> i32 {
    match v {
        LINK_UNLINKED | LINK_ACTIVE => v,
        other => {
            tracing::warn!(value = other, clamped = LINK_UNLINKED, "[link] 非法链声明值，钳无链");
            LINK_UNLINKED
        }
    }
}

impl Entity {
    /// 创建新实体
    ///
    /// - `pos` 初始逻辑坐标
    /// - `per_tick_instruction_limit` 每tick指令上限（由 Constraints 提供）
    /// - 速度默认 (0,0)，动作默认 Idle，存活默认 true，生存点数初始3天
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: EntityId,
        owner: PlayerId,
        code_hash: String,
        pos: (f64, f64),
        appearance: Appearance,
        created_tick: u64,
        per_tick_instruction_limit: u64,
    ) -> Self {
        let e = Self {
            id,
            owner,
            code_hash,
            pos,
            velocity: (0.0, 0.0),
            radius: default_radius(),
            mass: default_mass(),
            binding: default_binding(),
            bound_to: None,
            motion: default_motion(),
            field_radius: default_field_radius(),
            field_gravity_scale: default_field_gravity_scale(),
            field_drag_delta: default_field_drag_delta(),
            liquid_density: 0.0,
            liquid_drag: 0.0,
            restitution: default_restitution(),
            friction: default_friction(),
            link_to: None,
            storage: Vec::new(),
            appearance,
            action_state: ActionState::Idle,
            instruction_guard: InstructionGuard::new(per_tick_instruction_limit),
            survival: SurvivalPoints::initial(created_tick),
            alive: true,
            created_tick,
            assets: Vec::new(),
        };
        tracing::debug!(
            entity_id = %e.id.0,
            owner = %e.owner.0,
            pos = ?e.pos,
            radius = e.radius,
            created_tick,
            "实体创建"
        );
        e
    }

    /// 当前逻辑坐标
    pub fn pos(&self) -> (f64, f64) {
        self.pos
    }

    /// 身体半径（#030：活动实体圆半径；creation 不用它，身体=占地格 AABB）
    pub fn radius(&self) -> f64 {
        self.radius
    }

    /// 设置身体半径（超界 warn 并钳制到 0.05..=25.0，非有限值回退 0.5——
    /// 与 spawn 第 7 参校验同口径，clamp 逻辑唯一实现在 collision::clamp_radius）
    pub fn set_radius(&mut self, radius: f64) {
        let clamped = crate::collision::clamp_radius(radius);
        if clamped != radius {
            tracing::warn!(
                entity_id = %self.id.0,
                requested = radius,
                clamped,
                "实体半径校验钳制"
            );
        }
        self.radius = clamped;
    }

    /// 质量（#034 物理人格属性：阻力定律分母，只参与物理反应永不参与攻击结算）
    pub fn mass(&self) -> f64 {
        self.mass
    }

    /// 设置质量（超界 warn 并钳制到 0.05..=100.0，非有限值回退 1.0——
    /// 与 spawn 第 8 参/shoot 第 3 参校验同口径，clamp 逻辑唯一实现在 collision::clamp_mass）
    pub fn set_mass(&mut self, mass: f64) {
        let clamped = crate::collision::clamp_mass(mass);
        if clamped != mass {
            tracing::warn!(
                entity_id = %self.id.0,
                requested = mass,
                clamped,
                "实体质量校验钳制"
            );
        }
        self.mass = clamped;
    }

    /// 绑定契约档位（#034 修订四）
    pub fn binding(&self) -> i32 {
        self.binding
    }

    /// 设置绑定契约（import 层已 clamp_binding，此处直存）
    pub fn set_binding(&mut self, binding: i32) {
        self.binding = binding;
    }

    /// 绑定对象（fade_on_death 的"主人"）
    pub fn bound_to(&self) -> Option<EntityId> {
        self.bound_to
    }

    /// 设置绑定对象
    pub fn set_bound_to(&mut self, owner: Option<EntityId>) {
        self.bound_to = owner;
    }

    /// 运动身份档位（D-003）
    pub fn motion(&self) -> i32 {
        self.motion
    }

    /// 设置运动身份（import 层已 clamp_motion，此处直存）
    pub fn set_motion(&mut self, motion: i32) {
        self.motion = motion;
    }

    /// 场域半径（D-012：0.0=无场）
    pub fn field_radius(&self) -> f64 {
        self.field_radius
    }

    /// 设置场域半径（import 层已 clamp_field_radius，此处直存）
    pub fn set_field_radius(&mut self, field_radius: f64) {
        self.field_radius = field_radius;
    }

    /// 场重力缩放系数（D-012：1.0=中性）
    pub fn field_gravity_scale(&self) -> f64 {
        self.field_gravity_scale
    }

    /// 设置场重力缩放（import 层已 clamp_field_gravity_scale，此处直存）
    pub fn set_field_gravity_scale(&mut self, field_gravity_scale: f64) {
        self.field_gravity_scale = field_gravity_scale;
    }

    /// 场阻力改写量（D-012：加法式 delta，0.0=中性）
    pub fn field_drag_delta(&self) -> f64 {
        self.field_drag_delta
    }

    /// 设置场阻力改写量（import 层已 clamp_field_drag_delta，此处直存）
    pub fn set_field_drag_delta(&mut self, field_drag_delta: f64) {
        self.field_drag_delta = field_drag_delta;
    }

    /// 液体密度（D-009：0.0=非液体）
    pub fn liquid_density(&self) -> f64 {
        self.liquid_density
    }

    /// 设置液体密度（import 层已 clamp_liquid_density，此处直存）
    pub fn set_liquid_density(&mut self, liquid_density: f64) {
        self.liquid_density = liquid_density;
    }

    /// 水中阻尼（D-009：每拍速度保留系数的 MC 语义声明值）
    pub fn liquid_drag(&self) -> f64 {
        self.liquid_drag
    }

    /// 设置水中阻尼（import 层已 clamp_liquid_drag，此处直存）
    pub fn set_liquid_drag(&mut self, liquid_drag: f64) {
        self.liquid_drag = liquid_drag;
    }

    /// 弹性（D-010 出生声明第 21 参：0=撞停现状语义，域 [0,1]）
    pub fn restitution(&self) -> f64 {
        self.restitution
    }

    /// 设置弹性（import 层已 clamp_restitution，此处直存）
    pub fn set_restitution(&mut self, restitution: f64) {
        self.restitution = restitution;
    }

    /// 摩擦（D-010 出生声明第 22 参：0.6=Box2D v3 新默认，域 [0,1]）
    pub fn friction(&self) -> f64 {
        self.friction
    }

    /// 设置摩擦（import 层已 clamp_friction，此处直存）
    pub fn set_friction(&mut self, friction: f64) {
        self.friction = friction;
    }

    /// 活链母体（D-011：None=无链放生，Some=活链可控+可传）
    pub fn link_to(&self) -> Option<EntityId> {
        self.link_to
    }

    /// 设置活链母体（import 层已 clamp_link，此处直存；断链清算也走此口清 None）
    pub fn set_link_to(&mut self, parent: Option<EntityId>) {
        tracing::debug!(
            entity_id = %self.id.0,
            parent = ?parent.map(|p| p.0.to_string()),
            "[link] 链账本变更"
        );
        self.link_to = parent;
    }

    /// 公开存储块（D-011 存储件：世界侧公开信箱，随实体入快照）
    pub fn storage(&self) -> &[u8] {
        &self.storage
    }

    /// 全量覆盖公开存储块（storage_write 语义：整块替换非追加）
    pub fn set_storage(&mut self, storage: Vec<u8>) {
        tracing::debug!(entity_id = %self.id.0, len = storage.len(), "[storage] 公开块覆盖");
        self.storage = storage;
    }

    /// 设置坐标（物理规则校验与变换管线由 space/physics 模块负责，此处只存）
    pub fn set_pos(&mut self, pos: (f64, f64)) {
        tracing::debug!(entity_id = %self.id.0, from = ?self.pos, to = ?pos, "实体坐标变更");
        self.pos = pos;
    }

    /// 当前速度
    pub fn velocity(&self) -> (f64, f64) {
        self.velocity
    }

    /// 设置速度（物理规则使用）
    pub fn set_velocity(&mut self, velocity: (f64, f64)) {
        tracing::debug!(
            entity_id = %self.id.0,
            from = ?self.velocity,
            to = ?velocity,
            "实体速度变更"
        );
        self.velocity = velocity;
    }

    /// 当前动作状态
    pub fn action_state(&self) -> &ActionState {
        &self.action_state
    }

    /// 设置动作状态
    pub fn set_action_state(&mut self, state: ActionState) {
        tracing::debug!(entity_id = %self.id.0, ?state, "实体动作状态变更");
        self.action_state = state;
    }

    /// 是否存活
    pub fn is_alive(&self) -> bool {
        self.alive
    }

    /// 标记死亡（静默删除前由框架调用）
    pub fn kill(&mut self) {
        tracing::info!(entity_id = %self.id.0, before = self.alive, "实体标记死亡");
        self.alive = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // === EntityId ===

    #[test]
    fn entity_id_new_generates_unique_ids() {
        // 创建：两次 new 生成不同的 id
        let a = EntityId::new();
        let b = EntityId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn entity_id_equality_same_uuid() {
        // 读写：相同 Uuid 的 EntityId 相等
        let u = Uuid::nil();
        assert_eq!(EntityId(u), EntityId(u));
    }

    #[test]
    fn entity_id_is_hashable_for_hashmap_key() {
        // 边界：EntityId 必须可作 HashMap key（WorldState.entities 需要）
        let mut m = std::collections::HashMap::new();
        let id = EntityId(Uuid::from_u128(42));
        m.insert(id, "marker");
        assert_eq!(m.get(&id), Some(&"marker"));
    }

    #[test]
    fn entity_id_serializes_as_uuid_string() {
        // 读写：序列化往返
        let id = EntityId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: EntityId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // === PlayerId ===

    #[test]
    fn player_id_new_unique_and_hashable() {
        let a = PlayerId::new();
        let b = PlayerId::new();
        assert_ne!(a, b);
        let mut m = std::collections::HashMap::new();
        m.insert(a, 1);
        assert_eq!(m.get(&a), Some(&1));
    }

    // === Appearance ===

    #[test]
    fn appearance_new_stores_fields() {
        // 创建：外观数据字段正确存取
        let app = Appearance::new("@".to_string(), "red".to_string(), 1.0);
        assert_eq!(app.glyph, "@");
        assert_eq!(app.color, "red");
        assert_eq!(app.size, 1.0);
    }

    #[test]
    fn appearance_clone_serialize_roundtrip() {
        let app = Appearance::new("A".to_string(), "blue".to_string(), 2.5);
        let cloned = app.clone();
        assert_eq!(cloned.glyph, "A");
        let json = serde_json::to_string(&app).unwrap();
        let back: Appearance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.size, 2.5);
    }

    // === ActionState ===

    #[test]
    fn action_state_variants_construct() {
        // 创建：各动作状态变体可构造
        assert_eq!(ActionState::Idle, ActionState::Idle);
        assert_eq!(ActionState::Walking, ActionState::Walking);
        assert_eq!(ActionState::Jumping, ActionState::Jumping);
        assert_eq!(ActionState::Crouching, ActionState::Crouching);
        assert_eq!(ActionState::Waving, ActionState::Waving);
        assert_eq!(ActionState::Custom("dance".to_string()), ActionState::Custom("dance".to_string()));
        assert_ne!(ActionState::Idle, ActionState::Walking);
    }

    #[test]
    fn action_state_serialize_roundtrip() {
        let s = ActionState::Custom("cast".to_string());
        let json = serde_json::to_string(&s).unwrap();
        let back: ActionState = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    // === Entity ===

    fn sample_entity() -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(1)),
            PlayerId(Uuid::from_u128(9)),
            "abc123".to_string(),
            (10.0, 20.0),
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            100,
            1_000_000,
        )
    }

    #[test]
    fn entity_new_sets_identity_and_defaults() {
        // 创建：new 设置身份字段，物理状态取默认值
        let e = sample_entity();
        assert_eq!(e.id, EntityId(Uuid::from_u128(1)));
        assert_eq!(e.owner, PlayerId(Uuid::from_u128(9)));
        assert_eq!(e.code_hash, "abc123");
        assert_eq!(e.pos(), (10.0, 20.0));
        assert_eq!(e.velocity(), (0.0, 0.0)); // 默认静止
        assert_eq!(e.action_state(), &ActionState::Idle); // 默认待机
        assert!(e.is_alive()); // 默认存活
        assert_eq!(e.created_tick, 100);
        assert_eq!(e.survival.points(), 3); // 初始3天
        assert_eq!(e.instruction_guard.per_tick_limit(), 1_000_000);
    }

    #[test]
    fn entity_set_pos_updates_position() {
        // 修改：set_pos 更新坐标
        let mut e = sample_entity();
        e.set_pos((5.0, 6.0));
        assert_eq!(e.pos(), (5.0, 6.0));
    }

    #[test]
    fn entity_set_velocity_updates_velocity() {
        let mut e = sample_entity();
        e.set_velocity((1.5, -2.0));
        assert_eq!(e.velocity(), (1.5, -2.0));
    }

    #[test]
    fn entity_set_action_state_updates_state() {
        let mut e = sample_entity();
        e.set_action_state(ActionState::Jumping);
        assert_eq!(e.action_state(), &ActionState::Jumping);
    }

    #[test]
    fn entity_kill_marks_dead() {
        // 修改：kill 后存活标志变 false
        let mut e = sample_entity();
        assert!(e.is_alive());
        e.kill();
        assert!(!e.is_alive());
    }

    // === radius（#030 半径物理）===

    #[test]
    fn entity_default_radius_is_half() {
        // 创建：radius 默认 0.5（四生物/A/D/S/H/P 与旧测试构造器全兼容）
        let e = sample_entity();
        assert_eq!(e.radius(), 0.5);
    }

    #[test]
    fn entity_set_radius_clamps_to_bounds() {
        // 校验：0.05 ≤ radius ≤ 25.0，超界钳制（spawn 第 7 参同口径）
        let mut e = sample_entity();
        e.set_radius(2.0);
        assert_eq!(e.radius(), 2.0);
        e.set_radius(100.0);
        assert_eq!(e.radius(), 25.0, "超上限钳到 25.0");
        e.set_radius(0.01);
        assert_eq!(e.radius(), 0.05, "低于下限钳到 0.05");
        e.set_radius(f64::NAN);
        assert_eq!(e.radius(), 0.5, "非有限值回退默认 0.5");
    }

    #[test]
    fn entity_radius_serde_backward_compat() {
        // 读写：旧快照 JSON 无 radius 字段 → 反序列化回退默认 0.5（存储兼容）
        let e = sample_entity();
        let mut json = serde_json::to_string(&e).unwrap();
        // 手工剥掉 radius 字段模拟旧版数据
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("radius");
        json = v.to_string();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.radius(), 0.5);
    }

    // === mass（#034 物理人格属性）===

    #[test]
    fn entity_default_mass_is_one() {
        // 创建：mass 默认 1.0（四生物/玩家/测试构造器全兼容）
        let e = sample_entity();
        assert_eq!(e.mass(), 1.0);
    }

    #[test]
    fn entity_set_mass_clamps_to_bounds() {
        // 校验：0.05 ≤ mass ≤ 100，超界钳制（spawn 第 8 参/shoot 第 3 参同口径）
        let mut e = sample_entity();
        e.set_mass(2.5);
        assert_eq!(e.mass(), 2.5);
        e.set_mass(1000.0);
        assert_eq!(e.mass(), 100.0, "超上限钳到 100.0");
        e.set_mass(0.001);
        assert_eq!(e.mass(), 0.05, "低于下限钳到 0.05");
        e.set_mass(f64::NAN);
        assert_eq!(e.mass(), 1.0, "非有限值回退默认 1.0");
    }

    #[test]
    fn entity_mass_serde_backward_compat() {
        // 读写：旧快照 JSON 无 mass 字段 → 反序列化回退默认 1.0（存储兼容，抄 radius 先例）
        let e = sample_entity();
        let mut json = serde_json::to_string(&e).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("mass");
        json = v.to_string();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mass(), 1.0);
    }

    #[test]
    fn entity_serialize_roundtrip() {
        // 读写：序列化往返保留关键字段
        let e = sample_entity();
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(e.id, back.id);
        assert_eq!(e.pos(), back.pos());
        assert_eq!(e.code_hash, back.code_hash);
        assert_eq!(e.action_state(), back.action_state());
        assert!(back.is_alive());
    }

    // === 绑定契约（#034 修订四：出生声明写进造物数据）===

    #[test]
    fn entity_binding_defaults_to_unbound() {
        // 创建：新实体默认 unbound（现状语义：离体之物独立存在，同生不共死）
        let e = sample_entity();
        assert_eq!(e.binding(), BINDING_UNBOUND);
        assert_eq!(e.bound_to(), None);
    }

    #[test]
    fn entity_binding_survives_serialize_roundtrip() {
        // 读写：契约与绑定对象序列化往返保留
        let mut e = sample_entity();
        e.set_binding(BINDING_FADE_ON_DEATH);
        e.set_bound_to(Some(crate::entity::EntityId(uuid::Uuid::from_u128(42))));
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.binding(), BINDING_FADE_ON_DEATH);
        assert_eq!(back.bound_to(), Some(crate::entity::EntityId(uuid::Uuid::from_u128(42))));
    }

    #[test]
    fn entity_binding_serde_backcompat_old_snapshot() {
        // 边界：旧快照（无 binding/bound_to 字段）反序列化回退 unbound/None（存储向后兼容）
        // 构造法：序列化真实体后删掉这两个键，等价旧版本存储的字段面
        let mut obj: serde_json::Value =
            serde_json::to_value(sample_entity()).unwrap();
        obj.as_object_mut().unwrap().remove("binding");
        obj.as_object_mut().unwrap().remove("bound_to");
        let json = serde_json::to_string(&obj).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.binding(), BINDING_UNBOUND, "旧快照应回退 unbound");
        assert_eq!(back.bound_to(), None);
    }

    #[test]
    fn clamp_binding_passes_valid_values() {
        // 校验：三档合法值原样通过
        assert_eq!(clamp_binding(0), BINDING_UNBOUND);
        assert_eq!(clamp_binding(1), BINDING_FADE_ON_DEATH);
        assert_eq!(clamp_binding(2), BINDING_GRACE_OWNER);
    }

    #[test]
    fn clamp_binding_rejects_invalid_to_unbound() {
        // 校验：非法值钳 unbound（出生声明坏值不进世界，回退最保守现状语义）
        assert_eq!(clamp_binding(9), BINDING_UNBOUND);
        assert_eq!(clamp_binding(-1), BINDING_UNBOUND);
        assert_eq!(clamp_binding(i32::MAX), BINDING_UNBOUND);
    }

    // === motion（D-003 运动身份出生声明）===

    #[test]
    fn entity_motion_defaults_to_static() {
        // 创建：新实体默认 static（现状语义：造物钉死；生物不受此字段影响）
        let e = sample_entity();
        assert_eq!(e.motion(), MOTION_STATIC);
    }

    #[test]
    fn entity_motion_survives_serialize_roundtrip() {
        // 读写：运动身份序列化往返保留（快照存档不断档）
        let mut e = sample_entity();
        e.set_motion(MOTION_DYNAMIC);
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.motion(), MOTION_DYNAMIC);
    }

    #[test]
    fn entity_motion_serde_backcompat_old_snapshot() {
        // 边界：旧快照（无 motion 字段）反序列化回退 static（存储向后兼容，
        // radius/mass/binding 同款先例——旧存量造物全部保持钉死现状）
        let mut obj: serde_json::Value = serde_json::to_value(sample_entity()).unwrap();
        obj.as_object_mut().unwrap().remove("motion");
        let json = serde_json::to_string(&obj).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.motion(), MOTION_STATIC, "旧快照应回退 static");
    }

    #[test]
    fn clamp_motion_passes_valid_values() {
        // 校验：两档合法值原样通过（kinematic 剧本档缓立，不在合法域）
        assert_eq!(clamp_motion(0), MOTION_STATIC);
        assert_eq!(clamp_motion(1), MOTION_DYNAMIC);
    }

    #[test]
    fn clamp_motion_rejects_invalid_to_static() {
        // 校验：非法值钳 static（坏声明不进世界，回退现状语义；2=将来 kinematic
        // 预留位，当前声明即钳回）
        assert_eq!(clamp_motion(2), MOTION_STATIC);
        assert_eq!(clamp_motion(-1), MOTION_STATIC);
        assert_eq!(clamp_motion(i32::MAX), MOTION_STATIC);
    }

    // === field 出生声明家族（D-012 力场：field_radius/field_gravity_scale/
    // === field_drag_delta 与 radius/mass/binding/motion 并列，世界只认声明不认身份）===

    #[test]
    fn clamp_field_radius_boundary_and_valid() {
        // 校验：边界值原样通过（0.0=无场默认 / 25.0=上限），域内值原样保留
        assert_eq!(clamp_field_radius(0.0), 0.0);
        assert_eq!(clamp_field_radius(25.0), 25.0);
        assert_eq!(clamp_field_radius(3.0), 3.0);
        assert_eq!(clamp_field_radius(0.1), 0.1);
    }

    #[test]
    fn clamp_field_radius_rejects_negative_and_overflow() {
        // 校验：负值=无场钳 0（域下限），超上限钳 25.0（出生声明家族哲学：
        // 超界钳回+警告非拒绝）
        assert_eq!(clamp_field_radius(-0.5), 0.0);
        assert_eq!(clamp_field_radius(100.0), 25.0);
    }

    #[test]
    fn clamp_field_radius_non_finite_to_no_field() {
        // 校验：非有限值（NaN/±inf）钳 0=无场（最保守回退）
        assert_eq!(clamp_field_radius(f64::NAN), 0.0);
        assert_eq!(clamp_field_radius(f64::INFINITY), 0.0);
        assert_eq!(clamp_field_radius(f64::NEG_INFINITY), 0.0);
    }

    #[test]
    fn clamp_field_gravity_scale_boundary_and_valid() {
        // 校验：边界值原样通过（0.0=零重力域 / 10.0=十倍上限），域内值保留
        assert_eq!(clamp_field_gravity_scale(0.0), 0.0);
        assert_eq!(clamp_field_gravity_scale(10.0), 10.0);
        assert_eq!(clamp_field_gravity_scale(0.5), 0.5);
        assert_eq!(clamp_field_gravity_scale(2.0), 2.0);
    }

    #[test]
    fn clamp_field_gravity_scale_rejects_negative_and_overflow() {
        // 校验：负值钳 0（缩放无负语义），超上限钳 10.0
        assert_eq!(clamp_field_gravity_scale(-1.0), 0.0);
        assert_eq!(clamp_field_gravity_scale(50.0), 10.0);
    }

    #[test]
    fn clamp_field_gravity_scale_non_finite_to_neutral() {
        // 校验：非有限值钳 1.0=中性缩放（重力不变，与 serde default 同语义）
        assert_eq!(clamp_field_gravity_scale(f64::NAN), 1.0);
        assert_eq!(clamp_field_gravity_scale(f64::INFINITY), 1.0);
        assert_eq!(clamp_field_gravity_scale(f64::NEG_INFINITY), 1.0);
    }

    #[test]
    fn clamp_field_drag_delta_boundary_and_valid() {
        // 校验：边界值原样通过（-1.0=下限 / +25.0=上限），域内值保留（加法式
        // 改写量：正负互吃是叠加 v2 的代数抵消根基）
        assert_eq!(clamp_field_drag_delta(-1.0), -1.0);
        assert_eq!(clamp_field_drag_delta(25.0), 25.0);
        assert_eq!(clamp_field_drag_delta(2.0), 2.0);
        assert_eq!(clamp_field_drag_delta(-0.5), -0.5);
    }

    #[test]
    fn clamp_field_drag_delta_rejects_out_of_domain() {
        // 校验：低于 -1.0 钳 -1.0，高于 +25.0 钳 +25.0
        assert_eq!(clamp_field_drag_delta(-5.0), -1.0);
        assert_eq!(clamp_field_drag_delta(100.0), 25.0);
    }

    #[test]
    fn clamp_field_drag_delta_non_finite_to_neutral() {
        // 校验：非有限值钳 0=中性（加 0=不改写，与 serde default 同语义）
        assert_eq!(clamp_field_drag_delta(f64::NAN), 0.0);
        assert_eq!(clamp_field_drag_delta(f64::INFINITY), 0.0);
        assert_eq!(clamp_field_drag_delta(f64::NEG_INFINITY), 0.0);
    }

    #[test]
    fn entity_field_defaults_are_no_field() {
        // 创建：新实体默认无场（radius=0=无场 / gravity_scale=1=缩放中性 /
        // drag_delta=0=加法中性）——现状语义零回归
        let e = sample_entity();
        assert_eq!(e.field_radius(), 0.0);
        assert_eq!(e.field_gravity_scale(), 1.0);
        assert_eq!(e.field_drag_delta(), 0.0);
    }

    #[test]
    fn entity_field_setters_direct_store() {
        // 修改：三个 setter 直存（import 层已钳制，照 set_motion 直存模式）
        let mut e = sample_entity();
        e.set_field_radius(3.0);
        e.set_field_gravity_scale(0.5);
        e.set_field_drag_delta(-0.04);
        assert_eq!(e.field_radius(), 3.0);
        assert_eq!(e.field_gravity_scale(), 0.5);
        assert_eq!(e.field_drag_delta(), -0.04);
    }

    #[test]
    fn entity_field_survives_serialize_roundtrip() {
        // 读写：场声明序列化往返保留（快照存档不断档）
        let mut e = sample_entity();
        e.set_field_radius(5.0);
        e.set_field_gravity_scale(0.25);
        e.set_field_drag_delta(1.5);
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.field_radius(), 5.0);
        assert_eq!(back.field_gravity_scale(), 0.25);
        assert_eq!(back.field_drag_delta(), 1.5);
    }

    #[test]
    fn entity_field_serde_backcompat_old_snapshot() {
        // 边界：旧快照（无 field 三字段）反序列化回退无场语义（radius=0/
        // scale=1/delta=0——serde default 与无场语义严格一致，旧存量造物零
        // 行为变化，存储向后兼容 radius/mass/binding/motion 同款先例）
        let mut obj: serde_json::Value = serde_json::to_value(sample_entity()).unwrap();
        obj.as_object_mut().unwrap().remove("field_radius");
        obj.as_object_mut().unwrap().remove("field_gravity_scale");
        obj.as_object_mut().unwrap().remove("field_drag_delta");
        let json = serde_json::to_string(&obj).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.field_radius(), 0.0, "旧快照应回退无场");
        assert_eq!(back.field_gravity_scale(), 1.0);
        assert_eq!(back.field_drag_delta(), 0.0);
    }

    // === #051 皮入沙箱：实体装戴清单（asset_id 列表，与 SandboxData.assets 镜像一致）===

    #[test]
    fn entity_assets_serde_roundtrip() {
        // 规则：装戴清单随实体序列化（存档/快照管线携带；夺沙箱=夺全套的
        // 持久化侧基础）
        let mut e = sample_entity();
        e.assets = vec!["av_0123456789abcdef".to_string()];
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.assets,
            vec!["av_0123456789abcdef".to_string()],
            "装戴清单应随实体序列化往返"
        );
    }

    #[test]
    fn entity_assets_serde_backcompat_old_snapshot() {
        // 负面（卡面负面路径④）：旧快照（无 assets 字段）反序列化回空清单
        //（serde default，存储向后兼容，radius/mass/binding/motion/field 同款先例）
        let mut obj: serde_json::Value = serde_json::to_value(sample_entity()).unwrap();
        obj.as_object_mut().unwrap().remove("assets");
        let json = serde_json::to_string(&obj).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert!(
            back.assets.is_empty(),
            "旧快照无 assets 字段应反序列化回空清单"
        );
    }

    #[test]
    fn new_entity_starts_with_empty_assets() {
        // 边界：新实体装戴清单默认空（出生装戴归 spawn 参数化后续刀，本刀
        // 只挂字段；玩家进场/弹丸恒无皮）
        let e = sample_entity();
        assert!(e.assets.is_empty(), "新实体装戴清单应为空");
    }

    // === 液体出生声明家族（D-009 水体：liquid_density/liquid_drag 与
    // === radius/mass/binding/motion/field 并列，世界只认数字不认名字）===

    #[test]
    fn clamp_liquid_density_boundary_and_valid() {
        // 校验：边界值原样通过（0.0=非液体默认 / 5.0=上限），域内值原样保留
        assert_eq!(clamp_liquid_density(0.0), 0.0);
        assert_eq!(clamp_liquid_density(5.0), 5.0);
        assert_eq!(clamp_liquid_density(1.0), 1.0);
        assert_eq!(clamp_liquid_density(0.1), 0.1);
    }

    #[test]
    fn clamp_liquid_density_rejects_negative_and_overflow() {
        // 校验：负值钳 0（非液体最保守回退），超上限钳 5.0（出生声明家族哲学：
        // 超界钳回+警告非拒绝）
        assert_eq!(clamp_liquid_density(-0.8), 0.0);
        assert_eq!(clamp_liquid_density(50.0), 5.0);
    }

    #[test]
    fn clamp_liquid_density_non_finite_to_zero() {
        // 校验：非有限值（NaN/±inf）钳 0=非液体（最保守回退）
        assert_eq!(clamp_liquid_density(f64::NAN), 0.0);
        assert_eq!(clamp_liquid_density(f64::INFINITY), 0.0);
        assert_eq!(clamp_liquid_density(f64::NEG_INFINITY), 0.0);
    }

    #[test]
    fn clamp_liquid_drag_boundary_and_valid() {
        // 校验：边界值原样通过（0.0=无阻尼理想液体 / 5.0=上限），域内值保留
        //（k=0 配 ρ>0 合法：无阻尼理想液体）
        assert_eq!(clamp_liquid_drag(0.0), 0.0);
        assert_eq!(clamp_liquid_drag(5.0), 5.0);
        assert_eq!(clamp_liquid_drag(0.35), 0.35);
    }

    #[test]
    fn clamp_liquid_drag_rejects_negative_and_overflow() {
        // 校验：负值钳 0，超上限钳 5.0
        assert_eq!(clamp_liquid_drag(-1.0), 0.0);
        assert_eq!(clamp_liquid_drag(99.0), 5.0);
    }

    #[test]
    fn clamp_liquid_drag_non_finite_to_zero() {
        // 校验：非有限值钳 0=无阻尼（与 serde default 同语义）
        assert_eq!(clamp_liquid_drag(f64::NAN), 0.0);
        assert_eq!(clamp_liquid_drag(f64::INFINITY), 0.0);
        assert_eq!(clamp_liquid_drag(f64::NEG_INFINITY), 0.0);
    }

    #[test]
    fn entity_liquid_defaults_are_not_liquid() {
        // 创建：新实体默认非液体（density=0/drag=0）——现状语义零回归
        let e = sample_entity();
        assert_eq!(e.liquid_density(), 0.0);
        assert_eq!(e.liquid_drag(), 0.0);
    }

    #[test]
    fn entity_liquid_setters_direct_store() {
        // 修改：两个 setter 直存（import 层已钳制，照 set_field_radius 直存模式）
        let mut e = sample_entity();
        e.set_liquid_density(1.0);
        e.set_liquid_drag(0.35);
        assert_eq!(e.liquid_density(), 1.0);
        assert_eq!(e.liquid_drag(), 0.35);
    }

    #[test]
    fn entity_liquid_survives_serialize_roundtrip() {
        // 读写：液体声明序列化往返保留（快照存档不断档）
        let mut e = sample_entity();
        e.set_liquid_density(2.5);
        e.set_liquid_drag(0.35);
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.liquid_density(), 2.5);
        assert_eq!(back.liquid_drag(), 0.35);
    }

    #[test]
    fn entity_liquid_serde_backcompat_old_snapshot() {
        // 边界：旧快照（无 liquid 两字段）反序列化回退非液体语义（density=0/
        // drag=0——serde default 与非液体语义严格一致，旧存量造物零行为变化，
        // 存储向后兼容 radius/mass/binding/motion/field 同款先例）
        let mut obj: serde_json::Value = serde_json::to_value(sample_entity()).unwrap();
        obj.as_object_mut().unwrap().remove("liquid_density");
        obj.as_object_mut().unwrap().remove("liquid_drag");
        let json = serde_json::to_string(&obj).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.liquid_density(), 0.0, "旧快照应回退非液体");
        assert_eq!(back.liquid_drag(), 0.0);
    }

    // === 链账本 + 公开存储块（D-011 世界内链路段：link=binding 家族新成员，
    // === storage=造物公开信箱；decisions.md #038 通道公理世界内段）===

    #[test]
    fn entity_link_defaults_to_unlinked() {
        // 创建：新实体默认无链（现状语义零回归——放生是链的默认态）
        let e = sample_entity();
        assert_eq!(e.link_to(), None);
    }

    #[test]
    fn entity_link_survives_serialize_roundtrip() {
        // 读写：活链序列化往返保留（快照存档不断档——链账本落在实体字段，
        // 与 bound_to 同款形态同款快照待遇）
        let mut e = sample_entity();
        e.set_link_to(Some(crate::entity::EntityId(uuid::Uuid::from_u128(42))));
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.link_to(),
            Some(crate::entity::EntityId(uuid::Uuid::from_u128(42))),
            "活链应随实体序列化往返"
        );
    }

    #[test]
    fn entity_link_serde_backcompat_old_snapshot() {
        // 边界：旧快照（无 link_to 字段）反序列化回退 None（存储向后兼容，
        // bound_to/binding 同款先例——旧存量造物全部保持放生现状）
        let mut obj: serde_json::Value = serde_json::to_value(sample_entity()).unwrap();
        obj.as_object_mut().unwrap().remove("link_to");
        let json = serde_json::to_string(&obj).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.link_to(), None, "旧快照应回退无链");
    }

    #[test]
    fn clamp_link_passes_valid_values() {
        // 校验：两档合法值原样通过（0=无链放生 / 1=活链可控+可传）
        assert_eq!(clamp_link(LINK_UNLINKED), LINK_UNLINKED);
        assert_eq!(clamp_link(LINK_ACTIVE), LINK_ACTIVE);
    }

    #[test]
    fn clamp_link_rejects_invalid_to_unlinked() {
        // 校验：非法值钳 0（出生声明家族哲学：坏声明不进世界，回退最保守现状语义）
        assert_eq!(clamp_link(2), LINK_UNLINKED);
        assert_eq!(clamp_link(-1), LINK_UNLINKED);
        assert_eq!(clamp_link(i32::MAX), LINK_UNLINKED);
    }

    #[test]
    fn entity_storage_defaults_empty() {
        // 创建：新实体公开存储块默认空（对方读它返回"无存储"，与空字节区分）
        let e = sample_entity();
        assert!(e.storage().is_empty());
    }

    #[test]
    fn entity_storage_survives_serialize_roundtrip() {
        // 读写：存储块随实体入快照（重启不丢=存储块比沙箱内存耐用，
        // design-link 五节）；全量覆盖语义下往返必须保真
        let mut e = sample_entity();
        e.set_storage(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.storage(), &[0xDE, 0xAD, 0xBE, 0xEF], "存储块应往返保真");
    }

    #[test]
    fn entity_storage_serde_backcompat_old_snapshot() {
        // 边界：旧快照（无 storage 字段）反序列化回退空块（serde default，
        // 存储向后兼容 link_to/bound_to 同款先例）
        let mut obj: serde_json::Value = serde_json::to_value(sample_entity()).unwrap();
        obj.as_object_mut().unwrap().remove("storage");
        let json = serde_json::to_string(&obj).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert!(back.storage().is_empty(), "旧快照应回退空存储块");
    }

    // === D-010 碰撞响应：出生声明第 21/22 参（restitution/friction）===

    #[test]
    fn entity_restitution_friction_setters_and_roundtrip() {
        // 创建/读写：第 21/22 参 setter 直存（import 层已钳制，照 liquid 直存模式）
        // + 序列化往返保留（快照存档不断档）
        let mut e = sample_entity();
        e.set_restitution(0.8);
        e.set_friction(0.2);
        assert_eq!(e.restitution(), 0.8);
        assert_eq!(e.friction(), 0.2);
        let json = serde_json::to_string(&e).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.restitution(), 0.8);
        assert_eq!(back.friction(), 0.2);
    }

    #[test]
    fn restitution_defaults_zero_six_on_old_snapshot() {
        // 边界：旧快照（无 e/f 字段）反序列化回退 e=0/f=0.6（serde default，
        // 存储向后兼容 radius/mass/field/liquid 同款先例；e=0=撞停现状语义
        // 零回归，f=0.6=Box2D v3 新默认）
        let mut obj: serde_json::Value = serde_json::to_value(sample_entity()).unwrap();
        obj.as_object_mut().unwrap().remove("restitution");
        obj.as_object_mut().unwrap().remove("friction");
        let json = serde_json::to_string(&obj).unwrap();
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.restitution(), 0.0, "旧快照 e 应回退 0（撞停现状）");
        assert_eq!(back.friction(), 0.6, "旧快照 f 应回退 0.6");
    }

    #[test]
    fn clamp_restitution_friction_domain_unit_interval() {
        // 出生声明家族哲学：超界钳回+警告非拒绝（域 [0,1] 硬钳）；非有限值
        // 回退默认（e=0.0 / f=0.6）——与 mass/radius 钳制同款措辞风格
        assert_eq!(clamp_restitution(1.5), 1.0);
        assert_eq!(clamp_restitution(-0.3), 0.0);
        assert_eq!(clamp_restitution(f64::NAN), 0.0);
        assert_eq!(clamp_restitution(f64::INFINITY), 0.0);
        assert_eq!(clamp_restitution(0.7), 0.7);
        assert_eq!(clamp_friction(1.5), 1.0);
        assert_eq!(clamp_friction(-0.3), 0.0);
        assert_eq!(clamp_friction(f64::NAN), 0.6);
        assert_eq!(clamp_friction(f64::NEG_INFINITY), 0.6);
        assert!((clamp_friction(0.4) - 0.4).abs() < 1e-12);
    }
}
