//! 延迟变更队列（架构文档 4.1 节阶段一/阶段二）
//!
//! 玩家代码执行期间产生的状态变更放入延迟队列，阶段二统一应用。

use crate::entity::EntityId;

/// 延迟变更（架构文档 4.1 节 PendingChanges 队列元素）
///
/// #034 后 Spawn 变体（整个 Entity，含 mass 字段）与 SetPos 变体的大小差越过
/// clippy large_enum_variant 阈值——队列是瞬时数据非长期持有，Box 化只会扩散
/// 构造点改动，allow 留档。
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum PendingChange {
    /// 生成新实体（实体数据放入队列，阶段二插入 WorldState）
    Spawn {
        entity: crate::entity::Entity,
    },
    /// 销毁实体（标记死亡，阶段二的死亡清理会移除）
    Destroy {
        entity: EntityId,
    },
    /// 设置实体坐标（玩家代码调 set_pos 产生的变更）
    SetPos {
        entity: EntityId,
        new_pos: (f64, f64),
    },
    /// 设置实体速度
    SetVelocity {
        entity: EntityId,
        new_velocity: (f64, f64),
    },
}

#[cfg(test)]
mod tests {
    use super::PendingChange;
    use crate::entity::{Appearance, Entity, EntityId, PlayerId};
    use uuid::Uuid;

    /// 构造固定 ID 的实体（确定性，便于断言）
    fn sample_entity(id_u: u128) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "hash".to_string(),
            (1.0, 2.0),
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    #[test]
    fn spawn_constructs_and_exposes_entity() {
        // 创建：Spawn 变体持有实体，可取出字段
        let entity = sample_entity(7);
        let change = PendingChange::Spawn {
            entity: entity.clone(),
        };
        match change {
            PendingChange::Spawn { entity } => {
                assert_eq!(entity.id, EntityId(Uuid::from_u128(7)));
                assert_eq!(entity.pos(), (1.0, 2.0));
            }
            _ => panic!("应为 Spawn 变体"),
        }
    }

    #[test]
    fn destroy_constructs_with_entity_id() {
        // 创建：Destroy 变体持有 EntityId
        let id = EntityId(Uuid::from_u128(42));
        let change = PendingChange::Destroy { entity: id };
        match change {
            PendingChange::Destroy { entity } => assert_eq!(entity, id),
            _ => panic!("应为 Destroy 变体"),
        }
    }

    #[test]
    fn setpos_constructs_with_entity_and_pos() {
        // 创建：SetPos 变体持有 EntityId 与新坐标
        let id = EntityId(Uuid::from_u128(3));
        let change = PendingChange::SetPos {
            entity: id,
            new_pos: (9.0, -1.0),
        };
        match change {
            PendingChange::SetPos { entity, new_pos } => {
                assert_eq!(entity, id);
                assert_eq!(new_pos, (9.0, -1.0));
            }
            _ => panic!("应为 SetPos 变体"),
        }
    }

    #[test]
    fn setvelocity_constructs_with_entity_and_velocity() {
        // 创建：SetVelocity 变体持有 EntityId 与新速度
        let id = EntityId(Uuid::from_u128(5));
        let change = PendingChange::SetVelocity {
            entity: id,
            new_velocity: (2.5, 0.0),
        };
        match change {
            PendingChange::SetVelocity {
                entity,
                new_velocity,
            } => {
                assert_eq!(entity, id);
                assert_eq!(new_velocity, (2.5, 0.0));
            }
            _ => panic!("应为 SetVelocity 变体"),
        }
    }
}
