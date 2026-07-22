//! 区域管理：ZoneManager 负责区域注册与查询
//!
//! 架构文档 2.5 节（区域）+ 3.2 节（区域接口）。
//! 无状态：区域存于 WorldState.zones。

use codeworld_core::entity::{EntityId, PlayerId};
use codeworld_core::world::{BoundingBox, WorldState, Zone, ZoneId};

/// 区域管理器（架构文档 2.5 节 + 3.2 节）
///
/// 无状态：区域存于 `WorldState.zones`。
pub struct ZoneManager;

impl ZoneManager {
    /// 创建管理器
    pub fn new() -> Self {
        tracing::debug!("ZoneManager 创建");
        ZoneManager
    }

    /// 注册区域到 WorldState（架构 3.2：register_zone）
    pub fn register_zone(
        world: &mut WorldState,
        owner: PlayerId,
        bounds: BoundingBox,
        rules_code_hash: String,
        created_tick: u64,
    ) -> ZoneId {
        let zone = Zone::new(owner, bounds, rules_code_hash, created_tick);
        let id = zone.id;
        tracing::info!(?id, "区域注册");
        world.zones.insert(zone.id, zone);
        id
    }

    /// 移除区域（架构 3.2：remove_zone）
    ///
    /// 移除成功返回 true，不存在返回 false。
    pub fn remove_zone(world: &mut WorldState, zone_id: ZoneId) -> bool {
        let removed = world.zones.remove(&zone_id).is_some();
        tracing::info!(?zone_id, removed, "区域移除");
        removed
    }

    /// 查找包含某坐标的所有区域 ID（架构 3.2：区域查询）
    pub fn find_zones_at(world: &WorldState, x: f64, y: f64) -> Vec<ZoneId> {
        let ids: Vec<ZoneId> = world
            .zones
            .values()
            .filter(|z| z.bounds.contains(x, y))
            .map(|z| z.id)
            .collect();
        tracing::debug!(x, y, count = ids.len(), "查询坐标所属区域");
        ids
    }

    /// 查询单个区域
    pub fn get_zone(world: &WorldState, zone_id: ZoneId) -> Option<&Zone> {
        world.zones.get(&zone_id)
    }

    /// 判断实体是否在某区域内（实体坐标在 zone bounds 内）
    ///
    /// 实体或区域不存在返回 false。
    pub fn entity_in_zone(world: &WorldState, entity_id: EntityId, zone_id: ZoneId) -> bool {
        let pos = match world.entities.get(&entity_id) {
            Some(e) => e.pos(),
            None => return false,
        };
        let zone = match world.zones.get(&zone_id) {
            Some(z) => z,
            None => return false,
        };
        let (x, y) = pos;
        let inside = zone.bounds.contains(x, y);
        tracing::debug!(?entity_id, ?zone_id, inside, "判断实体是否在区域内");
        inside
    }
}

impl Default for ZoneManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
    use codeworld_core::world::BoundingBox;
    use codeworld_core::world::WorldState;
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
    fn register_zone_adds_to_world() {
        // 读写：注册后 WorldState.zones 有该区域
        let mut w = WorldState::new();
        let owner = PlayerId(Uuid::from_u128(9));
        let id = ZoneManager::register_zone(
            &mut w,
            owner,
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            "rules".to_string(),
            0,
        );
        assert_eq!(w.zones.len(), 1);
        assert!(w.zones.contains_key(&id));
    }

    #[test]
    fn remove_zone_existing_returns_true() {
        let mut w = WorldState::new();
        let owner = PlayerId(Uuid::from_u128(9));
        let id = ZoneManager::register_zone(
            &mut w,
            owner,
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            "r".to_string(),
            0,
        );
        assert!(ZoneManager::remove_zone(&mut w, id));
        assert_eq!(w.zones.len(), 0);
    }

    #[test]
    fn remove_zone_missing_returns_false() {
        let mut w = WorldState::new();
        let id = codeworld_core::world::ZoneId::new();
        assert!(!ZoneManager::remove_zone(&mut w, id));
    }

    #[test]
    fn find_zones_at_returns_containing_zones() {
        // 读写：返回包含该坐标的所有区域 ID
        let mut w = WorldState::new();
        let owner = PlayerId(Uuid::from_u128(9));
        ZoneManager::register_zone(&mut w, owner, BoundingBox::new(0.0, 0.0, 10.0, 10.0), "a".to_string(), 0);
        ZoneManager::register_zone(&mut w, owner, BoundingBox::new(0.0, 0.0, 20.0, 20.0), "b".to_string(), 0);
        ZoneManager::register_zone(&mut w, owner, BoundingBox::new(100.0, 100.0, 200.0, 200.0), "c".to_string(), 0);
        let ids = ZoneManager::find_zones_at(&w, 5.0, 5.0);
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn find_zones_at_none_returns_empty() {
        // 边界：坐标不在任何区域内返回空
        let mut w = WorldState::new();
        let owner = PlayerId(Uuid::from_u128(9));
        ZoneManager::register_zone(&mut w, owner, BoundingBox::new(100.0, 100.0, 200.0, 200.0), "a".to_string(), 0);
        let ids = ZoneManager::find_zones_at(&w, 5.0, 5.0);
        assert_eq!(ids.len(), 0);
    }

    #[test]
    fn get_zone_returns_some_for_existing() {
        let mut w = WorldState::new();
        let owner = PlayerId(Uuid::from_u128(9));
        let id = ZoneManager::register_zone(
            &mut w,
            owner,
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            "r".to_string(),
            0,
        );
        let zone = ZoneManager::get_zone(&w, id);
        assert!(zone.is_some());
        assert_eq!(zone.unwrap().rules_code_hash, "r");
    }

    #[test]
    fn get_zone_returns_none_for_missing() {
        let w = WorldState::new();
        assert!(ZoneManager::get_zone(&w, codeworld_core::world::ZoneId::new()).is_none());
    }

    #[test]
    fn entity_in_zone_true_when_inside() {
        let mut w = WorldState::new();
        let owner = PlayerId(Uuid::from_u128(9));
        let e = make_entity(1, 5.0, 5.0);
        let eid = e.id;
        w.entities.insert(eid, e);
        let zid = ZoneManager::register_zone(&mut w, owner, BoundingBox::new(0.0, 0.0, 10.0, 10.0), "r".to_string(), 0);
        assert!(ZoneManager::entity_in_zone(&w, eid, zid));
    }

    #[test]
    fn entity_in_zone_false_when_outside() {
        let mut w = WorldState::new();
        let owner = PlayerId(Uuid::from_u128(9));
        let e = make_entity(1, 100.0, 100.0);
        let eid = e.id;
        w.entities.insert(eid, e);
        let zid = ZoneManager::register_zone(&mut w, owner, BoundingBox::new(0.0, 0.0, 10.0, 10.0), "r".to_string(), 0);
        assert!(!ZoneManager::entity_in_zone(&w, eid, zid));
    }

    #[test]
    fn entity_in_zone_false_when_entity_missing() {
        // 边界：实体不存在返回 false
        let mut w = WorldState::new();
        let owner = PlayerId(Uuid::from_u128(9));
        let zid = ZoneManager::register_zone(&mut w, owner, BoundingBox::new(0.0, 0.0, 10.0, 10.0), "r".to_string(), 0);
        assert!(!ZoneManager::entity_in_zone(&w, EntityId(Uuid::from_u128(999)), zid));
    }

    #[test]
    fn entity_in_zone_false_when_zone_missing() {
        // 边界：区域不存在返回 false
        let mut w = WorldState::new();
        let e = make_entity(1, 5.0, 5.0);
        let eid = e.id;
        w.entities.insert(eid, e);
        assert!(!ZoneManager::entity_in_zone(&w, eid, codeworld_core::world::ZoneId::new()));
    }
}
