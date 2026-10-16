//! 物理规则（PhysicsOverride / PhysicsScope / DefaultPhysics / PhysicsChange / PhysicsReason）
//!
//! 默认规则不可删除但可覆盖；覆盖规则跑在 WASM 沙箱里（架构文档 2.8 节）。
//! 本模块只定义数据结构，规则执行逻辑由 physics crate 实现。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::entity::EntityId;
use crate::world::ZoneId;

/// 物理规则覆盖唯一标识
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct PhysicsOverrideId(pub Uuid);

impl PhysicsOverrideId {
    /// 生成一个新的随机覆盖规则ID
    pub fn new() -> Self {
        let id = Self(Uuid::new_v4());
        tracing::debug!(physics_override_id = %id.0, "PhysicsOverrideId 生成");
        id
    }
}

impl Default for PhysicsOverrideId {
    fn default() -> Self {
        Self::new()
    }
}

/// 物理规则覆盖的作用域
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PhysicsScope {
    /// 只影响自己的实体
    Entity(EntityId),
    /// 影响某区域内所有实体
    Zone(ZoneId),
    /// 影响整个世界（改了=暴露面大=可被攻击）
    Global,
}

/// 物理规则覆盖注册条目
///
/// `rule_code` 为 WASM 字节码（数据结构层面用 Vec<u8> 表示，
/// 实际编译/实例化由 code-arena 负责）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhysicsOverride {
    /// 覆盖规则ID
    pub id: PhysicsOverrideId,
    /// 注册者（死亡则覆盖失效）
    pub owner: EntityId,
    /// 作用域
    pub scope: PhysicsScope,
    /// 规则代码（WASM 字节码）
    pub rule_code: Vec<u8>,
    /// 优先级（高=先执行）
    pub priority: i32,
    /// 创建 tick
    pub created_tick: u64,
}

impl PhysicsOverride {
    /// 创建覆盖规则（id 自动生成）
    pub fn new(
        owner: EntityId,
        scope: PhysicsScope,
        rule_code: Vec<u8>,
        priority: i32,
        created_tick: u64,
    ) -> Self {
        let ov = Self {
            id: PhysicsOverrideId::new(),
            owner,
            scope,
            rule_code,
            priority,
            created_tick,
        };
        tracing::debug!(
            override_id = %ov.id.0,
            owner = %ov.owner.0,
            priority,
            created_tick,
            "物理覆盖规则注册"
        );
        ov
    }
}

/// 世界极速（#034 修订一：视觉可读性常数，telegraphing constant）。
/// 任何离体之物一拍位移的硬上限：一拍最大位移不超过世界宽度（100 格）的 1/4，
/// 任何飞行物在画面上至少存在多拍、肉眼可追随，攻击永远"看得见飞过来"；
/// 同时天然保证防御反应窗口（极速内任何弹至少飞 1 拍以上，零窗口狙击物理上不存在）。
/// 不是玩法平衡数字，是世界的物理常数（与重力 -9.8 同级）。
/// 标定锚：横穿全图（100 格）最短约 4 拍的观感；拍速变更时随世界常数批次
/// 重标定（不自动公式耦合，明改明公告）。
pub const WORLD_MAX_SPEED: f64 = 25.0;

/// 默认物理规则参数（框架内置，不可删除，可通过覆盖替换行为）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultPhysics {
    /// 重力加速度（如 -9.8）
    pub gravity: f64,
    /// 地面高度（如 0.0）
    pub ground_y: f64,
    /// 基础碰撞（两实体不能占同一坐标）
    pub collision_enabled: bool,
    /// 最大移动速度
    pub max_velocity: f64,
    /// 阻力常量（#034 世界基石参数，与重力同级：对一切离体之物生效不豁免不分类）。
    /// 阻力定律：v = v - drag*v/mass（线性阻力，方向不变模衰减）。
    /// 默认 0.05（标定判据：默认参数下标准上抛弹 vx=4/vy=+9.8/mass=1.0 射程
    /// 3~6 格，physics crate projectile_range_calibration_default_drag 测试锚定；
    /// #034 修订二 CCD 后落点=地面交点，锚随量测口径重标，drag 常数未变）。
    /// serde default：旧参数快照无此字段回退 0.05。
    #[serde(default = "default_drag")]
    pub drag_coefficient: f64,
    /// 世界极速（#034 修订一：视觉可读性常数 WORLD_MAX_SPEED）。
    /// 对弹丸生效的两处钳制：发射时初速超限拒绝 + 飞行中每拍积分后归一化压回
    /// （重力累积从此封顶，双审 P1-A 修复）。
    /// serde default：旧参数快照无此字段回退世界极速默认 25.0。
    #[serde(default = "default_max_speed")]
    pub max_speed: f64,
}

fn default_drag() -> f64 {
    0.05
}

fn default_max_speed() -> f64 {
    WORLD_MAX_SPEED
}

impl DefaultPhysics {
    /// 默认物理参数：gravity=-9.8, ground_y=0.0, collision_enabled=true,
    /// max_velocity=100.0, drag=0.05, max_speed=25.0（世界极速）
    pub fn default_values() -> Self {
        Self {
            gravity: -9.8,
            ground_y: 0.0,
            collision_enabled: true,
            max_velocity: 100.0,
            drag_coefficient: default_drag(),
            max_speed: default_max_speed(),
        }
    }
}

impl Default for DefaultPhysics {
    fn default() -> Self {
        let p = Self::default_values();
        tracing::debug!(
            gravity = p.gravity,
            ground_y = p.ground_y,
            collision_enabled = p.collision_enabled,
            max_velocity = p.max_velocity,
            drag = p.drag_coefficient,
            "DefaultPhysics 创建"
        );
        p
    }
}

/// 物理规则执行产生的状态变更（放入 PendingChanges 队列）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhysicsChange {
    /// 受影响实体
    pub entity: EntityId,
    /// 物理计算后的新坐标
    pub new_pos: (f64, f64),
    /// 重力/碰撞/覆盖规则
    pub reason: PhysicsReason,
    /// 哪条覆盖规则产生的（默认规则产生的为 None）
    pub source_rule: Option<PhysicsOverrideId>,
}

impl PhysicsChange {
    /// 创建物理状态变更
    pub fn new(
        entity: EntityId,
        new_pos: (f64, f64),
        reason: PhysicsReason,
        source_rule: Option<PhysicsOverrideId>,
    ) -> Self {
        let ch = Self {
            entity,
            new_pos,
            reason,
            source_rule,
        };
        tracing::debug!(
            entity = %ch.entity.0,
            ?new_pos,
            ?source_rule,
            "物理状态变更产生"
        );
        ch
    }
}

/// 物理状态变更原因
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PhysicsReason {
    /// 默认重力
    Gravity,
    /// 默认碰撞
    Collision,
    /// 玩家自定义规则
    Override(PhysicsOverrideId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityId;
    use crate::world::ZoneId;
    use uuid::Uuid;

    // === PhysicsOverrideId ===

    #[test]
    fn physics_override_id_new_unique_and_hashable() {
        let a = PhysicsOverrideId::new();
        let b = PhysicsOverrideId::new();
        assert_ne!(a, b);
        let mut m = std::collections::HashMap::new();
        m.insert(a, 1);
        assert_eq!(m.get(&a), Some(&1));
    }

    // === PhysicsScope ===

    #[test]
    fn physics_scope_variants_construct() {
        // 创建：三种作用域可构造
        let eid = EntityId(Uuid::from_u128(1));
        let zid = ZoneId(Uuid::from_u128(2));
        assert_eq!(PhysicsScope::Entity(eid), PhysicsScope::Entity(eid));
        assert_eq!(PhysicsScope::Zone(zid), PhysicsScope::Zone(zid));
        assert_eq!(PhysicsScope::Global, PhysicsScope::Global);
        assert_ne!(PhysicsScope::Global, PhysicsScope::Entity(eid));
    }

    #[test]
    fn physics_scope_serialize_roundtrip() {
        let s = PhysicsScope::Zone(ZoneId(Uuid::from_u128(5)));
        let json = serde_json::to_string(&s).unwrap();
        let back: PhysicsScope = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    // === PhysicsOverride ===

    #[test]
    fn physics_override_new_stores_fields_and_generates_id() {
        // 创建：覆盖规则字段正确，id 自动生成
        let owner = EntityId(Uuid::from_u128(1));
        let ov = PhysicsOverride::new(
            owner,
            PhysicsScope::Entity(owner),
            vec![1, 2, 3], // rule_code（WASM字节）
            10,
            1000,
        );
        assert_eq!(ov.owner, owner);
        assert_eq!(ov.scope, PhysicsScope::Entity(owner));
        assert_eq!(ov.rule_code, vec![1, 2, 3]);
        assert_eq!(ov.priority, 10);
        assert_eq!(ov.created_tick, 1000);
        assert_ne!(ov.id, PhysicsOverrideId::new()); // id 唯一
    }

    #[test]
    fn physics_override_serialize_roundtrip() {
        let owner = EntityId(Uuid::from_u128(1));
        let ov = PhysicsOverride::new(
            owner,
            PhysicsScope::Global,
            vec![0xDE, 0xAD],
            -5,
            7,
        );
        let json = serde_json::to_string(&ov).unwrap();
        let back: PhysicsOverride = serde_json::from_str(&json).unwrap();
        assert_eq!(back.priority, -5);
        assert_eq!(back.rule_code, vec![0xDE, 0xAD]);
        assert_eq!(back.scope, PhysicsScope::Global);
    }

    // === DefaultPhysics ===

    #[test]
    fn default_physics_values_match_doc() {
        // 创建：默认物理参数严格等于任务规定值
        let p = DefaultPhysics::default();
        assert_eq!(p.gravity, -9.8);
        assert_eq!(p.ground_y, 0.0);
        assert!(p.collision_enabled);
        assert_eq!(p.max_velocity, 100.0);
    }

    #[test]
    fn default_physics_serialize_roundtrip() {
        let p = DefaultPhysics::default();
        let json = serde_json::to_string(&p).unwrap();
        let back: DefaultPhysics = serde_json::from_str(&json).unwrap();
        assert_eq!(back.gravity, -9.8);
        assert!(back.collision_enabled);
    }

    // === 世界极速（#034 修订一：视觉可读性常数）===

    #[test]
    fn world_max_speed_default_is_twenty_five() {
        // 创建：世界极速默认 25 格/拍（视觉可读性常数：一拍最大位移不超过世界宽度
        // 1/4，任何飞行物画面上至少存在多拍肉眼可追随；标定锚：横穿全图 100 格
        // 最短约 4 拍。拍速变更时随世界常数批次重标定，不自动公式耦合）
        assert_eq!(WORLD_MAX_SPEED, 25.0);
        let p = DefaultPhysics::default_values();
        assert_eq!(p.max_speed, WORLD_MAX_SPEED, "默认参数应采用世界极速常数");
    }

    #[test]
    fn default_physics_max_speed_serde_backcompat() {
        // 边界：旧参数快照（无 max_speed 字段）反序列化回退世界极速默认
        //（serde default 与 drag/radius/mass 同款向后兼容）
        let json = r#"{"gravity":-9.8,"ground_y":0.0,"collision_enabled":true,"max_velocity":100.0,"drag_coefficient":0.05}"#;
        let back: DefaultPhysics = serde_json::from_str(json).unwrap();
        assert_eq!(back.max_speed, WORLD_MAX_SPEED, "旧快照应回退世界极速默认");
    }

    // === PhysicsReason ===

    #[test]
    fn physics_reason_variants_construct() {
        let oid = PhysicsOverrideId(Uuid::from_u128(3));
        assert_eq!(PhysicsReason::Gravity, PhysicsReason::Gravity);
        assert_eq!(PhysicsReason::Collision, PhysicsReason::Collision);
        assert_eq!(PhysicsReason::Override(oid), PhysicsReason::Override(oid));
        assert_ne!(PhysicsReason::Gravity, PhysicsReason::Collision);
    }

    #[test]
    fn physics_reason_serialize_roundtrip() {
        let r = PhysicsReason::Override(PhysicsOverrideId(Uuid::from_u128(9)));
        let json = serde_json::to_string(&r).unwrap();
        let back: PhysicsReason = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    // === PhysicsChange ===

    #[test]
    fn physics_change_new_stores_fields() {
        // 创建：物理状态变更字段正确
        let eid = EntityId(Uuid::from_u128(1));
        let oid = PhysicsOverrideId(Uuid::from_u128(2));
        let ch = PhysicsChange::new(
            eid,
            (5.0, 6.0),
            PhysicsReason::Override(oid),
            Some(oid),
        );
        assert_eq!(ch.entity, eid);
        assert_eq!(ch.new_pos, (5.0, 6.0));
        assert_eq!(ch.reason, PhysicsReason::Override(oid));
        assert_eq!(ch.source_rule, Some(oid));
    }

    #[test]
    fn physics_change_default_physics_has_no_source_rule() {
        // 边界：默认重力/碰撞产生的变更 source_rule=None
        let eid = EntityId(Uuid::from_u128(1));
        let ch = PhysicsChange::new(eid, (0.0, 0.0), PhysicsReason::Gravity, None);
        assert_eq!(ch.source_rule, None);
    }

    #[test]
    fn physics_change_serialize_roundtrip() {
        let eid = EntityId(Uuid::from_u128(1));
        let ch = PhysicsChange::new(
            eid,
            (1.0, 2.0),
            PhysicsReason::Collision,
            None,
        );
        let json = serde_json::to_string(&ch).unwrap();
        let back: PhysicsChange = serde_json::from_str(&json).unwrap();
        assert_eq!(back.entity, eid);
        assert_eq!(back.reason, PhysicsReason::Collision);
    }
}
