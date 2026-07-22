//! 冲突执行：ConflictResolver 处理多规则对同一实体的冲突变更
//!
//! 架构文档 4.1 节 b 步 + 8.1 节。
//! MVP 简化：last-write-wins（真正规则互相 attack 需 arena 沙箱通道，Phase 2）。

use codeworld_core::entity::EntityId;
use codeworld_core::physics::PhysicsChange;
use std::collections::HashMap;

/// 冲突解决器（架构文档 4.1b + 8.1）
///
/// MVP 简化：last-write-wins（真正规则互相 attack 需 arena 沙箱通道，Phase 2）。
pub struct ConflictResolver;

impl ConflictResolver {
    /// 创建解决器
    pub fn new() -> Self {
        tracing::debug!("ConflictResolver 创建");
        ConflictResolver
    }

    /// 解决冲突：按实体分组，同实体的多条变更按 last-write-wins 保留最后一条
    ///
    /// 不同实体的变更全部保留。
    pub fn resolve(changes: Vec<PhysicsChange>) -> Vec<PhysicsChange> {
        let mut map: HashMap<EntityId, PhysicsChange> = HashMap::new();
        for c in changes {
            map.insert(c.entity, c); // 后插入覆盖前者 = last-write-wins
        }
        let result: Vec<PhysicsChange> = map.into_values().collect();
        tracing::info!(count = result.len(), "冲突解决完成");
        result
    }

    /// 合并默认规则变更与覆盖规则变更（覆盖优先于默认）
    ///
    /// 同实体时覆盖规则变更胜出（default 先入 map，override 后入覆盖）；
    /// 不同实体的变更全部保留。
    pub fn merge_changes(
        default_changes: Vec<PhysicsChange>,
        override_changes: Vec<PhysicsChange>,
    ) -> Vec<PhysicsChange> {
        let mut map: HashMap<EntityId, PhysicsChange> = HashMap::new();
        for c in default_changes {
            map.insert(c.entity, c);
        }
        for c in override_changes {
            map.insert(c.entity, c); // override 覆盖 default
        }
        let result: Vec<PhysicsChange> = map.into_values().collect();
        tracing::info!(count = result.len(), "变更合并完成");
        result
    }
}

impl Default for ConflictResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::EntityId;
    use codeworld_core::physics::{PhysicsChange, PhysicsOverrideId, PhysicsReason};
    use uuid::Uuid;

    fn change(entity_u: u128, x: f64, y: f64, reason: PhysicsReason) -> PhysicsChange {
        PhysicsChange::new(EntityId(Uuid::from_u128(entity_u)), (x, y), reason, None)
    }

    #[test]
    fn resolve_same_entity_keeps_last_write() {
        // last-write-wins：同实体多条变更保留最后一条
        let e = EntityId(Uuid::from_u128(1));
        let c1 = PhysicsChange::new(e, (1.0, 1.0), PhysicsReason::Gravity, None);
        let c2 = PhysicsChange::new(e, (2.0, 2.0), PhysicsReason::Collision, None);
        let c3 = PhysicsChange::new(e, (3.0, 3.0), PhysicsReason::Gravity, None);
        let result = ConflictResolver::resolve(vec![c1, c2, c3]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].new_pos, (3.0, 3.0)); // 保留最后一条
    }

    #[test]
    fn resolve_different_entities_keeps_all() {
        // 不同实体的变更全部保留
        let c1 = change(1, 1.0, 1.0, PhysicsReason::Gravity);
        let c2 = change(2, 2.0, 2.0, PhysicsReason::Gravity);
        let result = ConflictResolver::resolve(vec![c1, c2]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn merge_changes_override_wins_over_default() {
        // 同实体：覆盖规则变更优先于默认规则变更（覆盖胜出）
        let e = EntityId(Uuid::from_u128(1));
        let oid = PhysicsOverrideId(Uuid::from_u128(7));
        let default_c = PhysicsChange::new(e, (1.0, 1.0), PhysicsReason::Gravity, None);
        let override_c =
            PhysicsChange::new(e, (9.0, 9.0), PhysicsReason::Override(oid), Some(oid));
        let result = ConflictResolver::merge_changes(vec![default_c], vec![override_c]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].new_pos, (9.0, 9.0));
        assert_eq!(result[0].reason, PhysicsReason::Override(oid));
    }

    #[test]
    fn merge_changes_different_entities_keeps_all() {
        // 不同实体：默认和覆盖变更都保留
        let default_c = change(1, 1.0, 1.0, PhysicsReason::Gravity);
        let override_c = change(
            2,
            2.0,
            2.0,
            PhysicsReason::Override(PhysicsOverrideId(Uuid::from_u128(3))),
        );
        let result = ConflictResolver::merge_changes(vec![default_c], vec![override_c]);
        assert_eq!(result.len(), 2);
    }
}
