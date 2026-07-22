//! 坐标系：SpaceEngine 负责坐标读写与视口查询
//!
//! 架构文档 3.2 节（空间接口）+ 4.1 节 c 步（space-engine 在 tick 阶段二处理）。
//! SpaceEngine 无状态，所有坐标存于 WorldState.entities。

//! 坐标系：SpaceEngine 负责坐标读写与视口查询
//!
//! 架构文档 3.2 节（空间接口）+ 4.1 节 c 步（space-engine 在 tick 阶段二处理）。
//! SpaceEngine 无状态，所有坐标存于 WorldState.entities。

use codeworld_core::entity::EntityId;
use codeworld_core::render::VisibleEntity;
use codeworld_core::world::{BoundingBox, WorldState};
use thiserror::Error;

/// 空间操作错误
#[derive(Debug, Error)]
pub enum SpaceError {
    /// 实体不存在
    #[error("实体不存在: {0:?}")]
    EntityNotFound(EntityId),
}

/// 坐标系引擎（架构文档 3.2 节）
///
/// 无状态：所有坐标存于 `WorldState.entities`，方法接收 WorldState 引用。
pub struct SpaceEngine;

impl SpaceEngine {
    /// 创建引擎
    pub fn new() -> Self {
        tracing::debug!("SpaceEngine 创建");
        SpaceEngine
    }

    /// 读取实体坐标（架构 3.2：get_pos）
    ///
    /// 实体不存在时返回 None。
    pub fn get_entity_pos(world: &WorldState, entity_id: EntityId) -> Option<(f64, f64)> {
        let pos = world.entities.get(&entity_id).map(|e| e.pos());
        tracing::debug!(?entity_id, ?pos, "读取实体坐标");
        pos
    }

    /// 写入实体坐标（架构 3.2：set_pos）
    ///
    /// 直接写入 Entity.pos；变换管线由 transform.rs 处理。实体不存在返回错误。
    pub fn set_entity_pos(
        world: &mut WorldState,
        entity_id: EntityId,
        pos: (f64, f64),
    ) -> Result<(), SpaceError> {
        let entity = world
            .entities
            .get_mut(&entity_id)
            .ok_or(SpaceError::EntityNotFound(entity_id))?;
        tracing::debug!(?entity_id, ?pos, "写入实体坐标");
        entity.set_pos(pos);
        Ok(())
    }

    /// 获取视口内可见实体（架构 3.2：get_visible_entities）
    ///
    /// 返回坐标在 viewport BoundingBox 内的实体，只含 id + pos + appearance。
    pub fn get_visible_entities(world: &WorldState, viewport: &BoundingBox) -> Vec<VisibleEntity> {
        let visible: Vec<VisibleEntity> = world
            .entities
            .values()
            .filter(|e| {
                let (x, y) = e.pos();
                viewport.contains(x, y)
            })
            .map(|e| VisibleEntity::new(e.id, e.pos(), e.appearance.clone()))
            .collect();
        tracing::debug!(?viewport, count = visible.len(), "视口可见实体采集");
        visible
    }
}

impl Default for SpaceEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
    use codeworld_core::world::{BoundingBox, WorldState};
    use uuid::Uuid;

    /// 构造一个含单个实体（指定坐标）的世界
    fn world_with_entity(id_u: u128, x: f64, y: f64) -> WorldState {
        let mut w = WorldState::new();
        let e = Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "hash".to_string(),
            (x, y),
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        );
        w.entities.insert(e.id, e);
        w
    }

    #[test]
    fn get_entity_pos_returns_position() {
        // 读写：返回实体坐标
        let w = world_with_entity(1, 5.0, 6.0);
        assert_eq!(
            SpaceEngine::get_entity_pos(&w, EntityId(Uuid::from_u128(1))),
            Some((5.0, 6.0))
        );
    }

    #[test]
    fn get_entity_pos_missing_returns_none() {
        // 边界：实体不存在返回 None
        let w = WorldState::new();
        assert_eq!(
            SpaceEngine::get_entity_pos(&w, EntityId(Uuid::from_u128(999))),
            None
        );
    }

    #[test]
    fn set_entity_pos_updates_position() {
        // 修改：写入坐标后可读回
        let mut w = world_with_entity(1, 0.0, 0.0);
        SpaceEngine::set_entity_pos(&mut w, EntityId(Uuid::from_u128(1)), (3.0, 4.0)).unwrap();
        assert_eq!(
            SpaceEngine::get_entity_pos(&w, EntityId(Uuid::from_u128(1))),
            Some((3.0, 4.0))
        );
    }

    #[test]
    fn set_entity_pos_missing_returns_error() {
        // 边界：实体不存在返回错误
        let mut w = WorldState::new();
        let r = SpaceEngine::set_entity_pos(&mut w, EntityId(Uuid::from_u128(999)), (1.0, 1.0));
        assert!(r.is_err());
    }

    #[test]
    fn get_visible_entities_in_viewport() {
        // 读写：返回视口内实体（坐标在 BoundingBox 内）
        let mut w = WorldState::new();
        for (id_u, x, y) in [(1u128, 1.0, 1.0), (2, 5.0, 5.0), (3, 100.0, 100.0)] {
            let e = Entity::new(
                EntityId(Uuid::from_u128(id_u)),
                PlayerId(Uuid::from_u128(99)),
                "h".to_string(),
                (x, y),
                Appearance::new("@".to_string(), "red".to_string(), 1.0),
                0,
                1_000_000,
            );
            w.entities.insert(e.id, e);
        }
        let viewport = BoundingBox::new(0.0, 0.0, 10.0, 10.0);
        let visible = SpaceEngine::get_visible_entities(&w, &viewport);
        assert_eq!(visible.len(), 2, "视口内应有 2 个实体");
    }

    #[test]
    fn get_visible_entities_none_outside() {
        // 边界：实体都在视口外返回空
        let w = world_with_entity(1, 100.0, 100.0);
        let viewport = BoundingBox::new(0.0, 0.0, 10.0, 10.0);
        let visible = SpaceEngine::get_visible_entities(&w, &viewport);
        assert_eq!(visible.len(), 0);
    }

    #[test]
    fn get_visible_entities_contains_id_and_appearance() {
        // 读写：VisibleEntity 含 id + pos + appearance
        let w = world_with_entity(1, 5.0, 5.0);
        let viewport = BoundingBox::new(0.0, 0.0, 10.0, 10.0);
        let visible = SpaceEngine::get_visible_entities(&w, &viewport);
        let ve = &visible[0];
        assert_eq!(ve.id, EntityId(Uuid::from_u128(1)));
        assert_eq!(ve.pos, (5.0, 5.0));
        assert_eq!(ve.appearance.glyph, "@");
    }
}
