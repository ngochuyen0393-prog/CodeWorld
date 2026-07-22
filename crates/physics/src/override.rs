//! 规则覆盖接口：OverrideManager 负责覆盖的注册/移除/查询/执行
//!
//! 架构文档 2.8 节 + 3.2 节 + 4.1 节 b 步。
//! MVP 不执行真实 WASM（IdentityExecutor 恒等），arena 模块完成后替换。

use codeworld_core::entity::{Entity, EntityId};
use codeworld_core::physics::{
    PhysicsChange, PhysicsOverride, PhysicsOverrideId, PhysicsReason, PhysicsScope,
};
use codeworld_core::world::WorldState;

/// 物理规则执行器 trait（抽象 WASM 执行）
///
/// MVP 提供 [`IdentityExecutor`]（恒等，返回原坐标）；arena 完成后替换为 WasmExecutor。
pub trait PhysicsRuleExecutor {
    /// 执行规则字节码：输入实体 pos+velocity -> 输出新坐标
    fn execute(
        &self,
        rule_bytecode: &[u8],
        entity_pos: (f64, f64),
        entity_velocity: (f64, f64),
    ) -> (f64, f64);
}

/// 恒等执行器（MVP 默认）
///
/// 直接返回输入坐标，不执行 WASM 字节码。
pub struct IdentityExecutor;

impl PhysicsRuleExecutor for IdentityExecutor {
    fn execute(
        &self,
        _rule_bytecode: &[u8],
        entity_pos: (f64, f64),
        _entity_velocity: (f64, f64),
    ) -> (f64, f64) {
        tracing::debug!(?entity_pos, "IdentityExecutor 恒等执行");
        entity_pos
    }
}

/// 覆盖规则管理器（架构文档 2.8 + 3.2 + 4.1b）
///
/// 无状态：覆盖存于 `WorldState.physics_overrides`。
pub struct OverrideManager;

impl OverrideManager {
    /// 创建管理器
    pub fn new() -> Self {
        tracing::debug!("OverrideManager 创建");
        OverrideManager
    }

    /// 注册覆盖到 WorldState（架构 3.2：register_physics_override）
    pub fn register_override(
        world: &mut WorldState,
        owner: EntityId,
        scope: PhysicsScope,
        rule_code: Vec<u8>,
        priority: i32,
        created_tick: u64,
    ) -> PhysicsOverrideId {
        let ov = PhysicsOverride::new(owner, scope, rule_code, priority, created_tick);
        let id = ov.id;
        tracing::info!(?id, priority, "物理覆盖注册");
        world.physics_overrides.push(ov);
        id
    }

    /// 移除覆盖（架构 3.2：remove_physics_override）
    ///
    /// 移除成功返回 true，不存在返回 false。
    pub fn remove_override(world: &mut WorldState, id: PhysicsOverrideId) -> bool {
        let before = world.physics_overrides.len();
        world.physics_overrides.retain(|o| o.id != id);
        let removed = world.physics_overrides.len() < before;
        tracing::info!(?id, removed, "物理覆盖移除");
        removed
    }

    /// 查询影响某实体的所有覆盖 ID（架构 3.2：get_active_overrides）
    ///
    /// scope 过滤：Entity 匹配 id；Zone 匹配实体所在 zone；Global 影响所有。
    ///
    /// 注意：MVP 不检查 owner 存活状态（core 设计"owner 死亡则覆盖失效"留 Phase 2）。
    pub fn get_active_overrides(world: &WorldState, entity_id: EntityId) -> Vec<PhysicsOverrideId> {
        let pos = world.entities.get(&entity_id).map(|e| e.pos());
        let ids: Vec<PhysicsOverrideId> = world
            .physics_overrides
            .iter()
            .filter(|o| match &o.scope {
                PhysicsScope::Entity(eid) => *eid == entity_id,
                PhysicsScope::Zone(zid) => match (pos, world.zones.get(zid)) {
                    (Some((x, y)), Some(zone)) => zone.bounds.contains(x, y),
                    _ => false,
                },
                PhysicsScope::Global => true,
            })
            .map(|o| o.id)
            .collect();
        tracing::debug!(?entity_id, count = ids.len(), "查询活跃覆盖");
        ids
    }

    /// 获取所有覆盖按 priority 降序排序
    pub fn get_overrides_sorted(world: &WorldState) -> Vec<&PhysicsOverride> {
        let mut sorted: Vec<&PhysicsOverride> = world.physics_overrides.iter().collect();
        // priority 降序；同 priority 按 created_tick 升序（先创建先执行），增强确定性
        sorted.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.created_tick.cmp(&b.created_tick))
        });
        tracing::debug!(count = sorted.len(), "覆盖排序");
        sorted
    }

    /// 执行单条覆盖规则（架构 4.1b：覆盖规则跑在 WASM 沙箱）
    ///
    /// 用 executor 执行规则字节码，返回 PhysicsChange(Override(id))。
    ///
    /// 注意：MVP IdentityExecutor 恒等（位置不变），仍返回 Some(PhysicsChange)。
    /// 真实 WasmExecutor（arena 模块）会改变坐标；位置未变是否返回 None 留 Phase 2。
    pub fn execute_override(
        override_entry: &PhysicsOverride,
        entity: &Entity,
        executor: &dyn PhysicsRuleExecutor,
    ) -> Option<PhysicsChange> {
        let new_pos =
            executor.execute(&override_entry.rule_code, entity.pos(), entity.velocity());
        tracing::debug!(?override_entry.id, ?new_pos, "覆盖规则执行");
        Some(PhysicsChange::new(
            entity.id,
            new_pos,
            PhysicsReason::Override(override_entry.id),
            Some(override_entry.id),
        ))
    }
}

impl Default for OverrideManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
    use codeworld_core::physics::{PhysicsOverride, PhysicsOverrideId, PhysicsReason, PhysicsScope};
    use codeworld_core::world::{BoundingBox, WorldState, Zone};
    use uuid::Uuid;

    fn make_entity(id_u: u128, x: f64, y: f64) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "h".to_string(),
            (x, y),
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    #[test]
    fn identity_executor_returns_input_pos() {
        // IdentityExecutor 直接返回输入坐标（不执行 WASM）
        let exec = IdentityExecutor;
        assert_eq!(exec.execute(&[0xAB], (3.0, 4.0), (1.0, 2.0)), (3.0, 4.0));
    }

    #[test]
    fn register_override_adds_to_world() {
        // 读写：注册后 WorldState.physics_overrides 有该覆盖
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let id = OverrideManager::register_override(
            &mut w,
            owner,
            PhysicsScope::Entity(owner),
            vec![0xAB],
            5,
            100,
        );
        assert_eq!(w.physics_overrides.len(), 1);
        assert_eq!(w.physics_overrides[0].id, id);
        assert_eq!(w.physics_overrides[0].priority, 5);
    }

    #[test]
    fn remove_override_existing_returns_true() {
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let id = OverrideManager::register_override(
            &mut w,
            owner,
            PhysicsScope::Entity(owner),
            vec![],
            1,
            0,
        );
        assert!(OverrideManager::remove_override(&mut w, id));
        assert_eq!(w.physics_overrides.len(), 0);
    }

    #[test]
    fn remove_override_missing_returns_false() {
        let mut w = WorldState::new();
        let id = PhysicsOverrideId::new();
        assert!(!OverrideManager::remove_override(&mut w, id));
    }

    #[test]
    fn get_active_overrides_scope_entity_match() {
        // scope 过滤：Entity scope 只匹配对应实体
        let mut w = WorldState::new();
        let e1 = EntityId(Uuid::from_u128(1));
        let e2 = EntityId(Uuid::from_u128(2));
        OverrideManager::register_override(&mut w, e1, PhysicsScope::Entity(e1), vec![], 10, 0);
        OverrideManager::register_override(&mut w, e2, PhysicsScope::Entity(e2), vec![], 5, 0);
        let ids = OverrideManager::get_active_overrides(&w, e1);
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn get_active_overrides_scope_zone_match() {
        // scope 过滤：Zone scope 匹配实体所在 zone
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let e = make_entity(1, 5.0, 5.0);
        w.entities.insert(e.id, e);
        let zone = Zone::new(
            PlayerId(Uuid::from_u128(9)),
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            "r".to_string(),
            0,
        );
        let zone_id = zone.id;
        w.zones.insert(zone.id, zone);
        OverrideManager::register_override(&mut w, owner, PhysicsScope::Zone(zone_id), vec![], 7, 0);
        let ids = OverrideManager::get_active_overrides(&w, EntityId(Uuid::from_u128(1)));
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn get_active_overrides_scope_zone_not_match() {
        // scope 过滤：实体不在 zone 内则不匹配
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let e = make_entity(1, 100.0, 100.0);
        w.entities.insert(e.id, e);
        let zone = Zone::new(
            PlayerId(Uuid::from_u128(9)),
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            "r".to_string(),
            0,
        );
        let zone_id = zone.id;
        w.zones.insert(zone.id, zone);
        OverrideManager::register_override(&mut w, owner, PhysicsScope::Zone(zone_id), vec![], 7, 0);
        let ids = OverrideManager::get_active_overrides(&w, EntityId(Uuid::from_u128(1)));
        assert_eq!(ids.len(), 0);
    }

    #[test]
    fn get_active_overrides_scope_global_affects_all() {
        // scope 过滤：Global 影响所有实体
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        OverrideManager::register_override(&mut w, owner, PhysicsScope::Global, vec![], 1, 0);
        let ids = OverrideManager::get_active_overrides(&w, EntityId(Uuid::from_u128(999)));
        assert_eq!(ids.len(), 1);
    }

    #[test]
    fn get_overrides_sorted_desc_by_priority() {
        // 排序：按 priority 降序
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        OverrideManager::register_override(&mut w, owner, PhysicsScope::Global, vec![], 1, 0);
        OverrideManager::register_override(&mut w, owner, PhysicsScope::Global, vec![], 10, 0);
        OverrideManager::register_override(&mut w, owner, PhysicsScope::Global, vec![], 5, 0);
        let sorted = OverrideManager::get_overrides_sorted(&w);
        assert_eq!(sorted.len(), 3);
        assert_eq!(sorted[0].priority, 10);
        assert_eq!(sorted[1].priority, 5);
        assert_eq!(sorted[2].priority, 1);
    }

    #[test]
    fn execute_override_returns_change_with_override_reason() {
        // 执行覆盖规则返回 PhysicsChange(Override(id))
        let owner = EntityId(Uuid::from_u128(1));
        let ov = PhysicsOverride::new(owner, PhysicsScope::Entity(owner), vec![0xAB], 5, 0);
        let entity = make_entity(1, 3.0, 4.0);
        let exec = IdentityExecutor;
        let change = OverrideManager::execute_override(&ov, &entity, &exec);
        assert!(change.is_some());
        let c = change.unwrap();
        assert_eq!(c.entity, entity.id);
        assert_eq!(c.new_pos, (3.0, 4.0)); // IdentityExecutor 返回原坐标
        assert_eq!(c.reason, PhysicsReason::Override(ov.id));
        assert_eq!(c.source_rule, Some(ov.id));
    }
}
