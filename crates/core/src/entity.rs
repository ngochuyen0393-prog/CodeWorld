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
}

/// radius 默认值（新实体与旧快照回退共用：四生物/玩家/测试构造器全兼容）
fn default_radius() -> f64 {
    0.5
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
            appearance,
            action_state: ActionState::Idle,
            instruction_guard: InstructionGuard::new(per_tick_instruction_limit),
            survival: SurvivalPoints::initial(created_tick),
            alive: true,
            created_tick,
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
}
