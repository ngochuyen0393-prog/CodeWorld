//! 实体占位判定（#021 实体占位制：一格一物，Screeps 阻挡制口径）
//!
//! 【#030 已退役】半径物理取代格子占位：移动/重力/spawn 三处判据改走
//! collision.rs 身体重叠（circle/AABB）。本模块保留仅作历史记录，
//! 旧测试保留其行为文档价值（与 physics resolve_collision_archived 同例）。

use crate::entity::{Entity, EntityId};
use std::collections::HashMap;

/// 查询 pos 所在格（floor(x),floor(y)）是否被其他**存活**实体占据
///
/// 返回占据者 EntityId（异常多占据时取 EntityId 最小者，保证确定序）；
/// 空格 / 仅被 exclude 自己占据 / 占据者已死 → None。
#[deprecated(
    since = "0.1.0",
    note = "#030 半径物理：格子占位退役，改用 collision::body_overlap 身体裁决"
)]
pub fn cell_occupied_by(
    entities: &HashMap<EntityId, Entity>,
    pos: (f64, f64),
    exclude: EntityId,
) -> Option<EntityId> {
    let cell = (pos.0.floor(), pos.1.floor());
    let mut occupant: Option<EntityId> = None;
    for (id, e) in entities {
        if *id == exclude || !e.is_alive() {
            continue;
        }
        let p = e.pos();
        if (p.0.floor(), p.1.floor()) == cell {
            let better = match occupant {
                None => true,
                Some(cur) => id.0 < cur.0,
            };
            if better {
                occupant = Some(*id);
            }
        }
    }
    tracing::debug!(cell = ?cell, occupant = ?occupant, "[occupancy] 占位判定");
    occupant
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::entity::{Appearance, Entity, PlayerId};
    use uuid::Uuid;

    fn make_entity(id_u: u128, pos: (f64, f64)) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "test".to_string(),
            pos,
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    #[test]
    fn cell_occupied_by_returns_occupant() {
        // 目标格 (6,0) 被其他实体占据（查询坐标取整后命中同一格）→ 返回占据者
        let mut entities = HashMap::new();
        let e1 = make_entity(1, (6.0, 0.0));
        let e2 = make_entity(2, (5.0, 0.0));
        let id1 = e1.id;
        let id2 = e2.id;
        entities.insert(id1, e1);
        entities.insert(id2, e2);
        // 查询 (6.5, 0.3)：floor 后仍落在格 (6,0)
        let occ = cell_occupied_by(&entities, (6.5, 0.3), id2);
        assert_eq!(occ, Some(EntityId(Uuid::from_u128(1))), "格 (6,0) 应被实体1占据");
    }

    #[test]
    fn cell_occupied_by_excludes_self() {
        // 查询自己所在格：仅被自己占据 → 不算占据（允许原地"移动"）
        let mut entities = HashMap::new();
        let e1 = make_entity(1, (6.0, 0.0));
        let id1 = e1.id;
        entities.insert(id1, e1);
        assert_eq!(cell_occupied_by(&entities, (6.0, 0.0), id1), None, "自己占据的格不算被挡");
    }

    #[test]
    fn cell_occupied_by_ignores_dead() {
        // 占据者已死（待死亡清理移除）→ 不挡移动
        let mut entities = HashMap::new();
        let mut e1 = make_entity(1, (6.0, 0.0));
        e1.kill();
        entities.insert(e1.id, e1);
        let e2 = make_entity(2, (5.0, 0.0));
        assert_eq!(cell_occupied_by(&entities, (6.0, 0.0), e2.id), None, "死者不占格");
    }

    #[test]
    fn cell_occupied_by_empty_cell_returns_none() {
        // 空格 → None（照常应用移动）
        let mut entities = HashMap::new();
        let e1 = make_entity(1, (6.0, 0.0));
        let id1 = e1.id;
        entities.insert(id1, e1);
        assert_eq!(cell_occupied_by(&entities, (7.0, 0.0), id1), None, "空格不挡");
    }

    #[test]
    fn cell_occupied_by_negative_floor() {
        // 负坐标取整：floor(-0.5) = -1；实体在格 (0,-1)，查询落 -1 格命中，落 0/-2 格不命中
        let mut entities = HashMap::new();
        let e1 = make_entity(1, (0.0, -1.0));
        let id1 = e1.id;
        entities.insert(id1, e1);
        let e2 = make_entity(2, (5.0, 5.0));
        assert_eq!(cell_occupied_by(&entities, (0.3, -0.5), e2.id), Some(id1), "floor(-0.5)=-1 应命中");
        assert_eq!(cell_occupied_by(&entities, (0.3, -1.5), e2.id), None, "floor(-1.5)=-2 应不命中");
        assert_eq!(cell_occupied_by(&entities, (0.3, 0.5), e2.id), None, "floor(0.5)=0 应不命中");
    }

    #[test]
    fn cell_occupied_by_lowest_id_on_multi_occupancy() {
        // 异常多占据（同格两实体）：取 EntityId 最小者（确定序，日志可重现）
        let mut entities = HashMap::new();
        let e2 = make_entity(2, (6.0, 0.0));
        let e3 = make_entity(3, (6.0, 0.0));
        entities.insert(e2.id, e2);
        entities.insert(e3.id, e3);
        let e1 = make_entity(1, (5.0, 0.0));
        let occ = cell_occupied_by(&entities, (6.0, 0.0), e1.id);
        assert_eq!(occ, Some(EntityId(Uuid::from_u128(2))), "多占据应返回最小 EntityId");
    }
}
