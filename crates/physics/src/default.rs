//! 默认物理规则：DefaultPhysicsEngine 每 tick 执行基础物理计算
//!
//! 架构文档 4.1 节 b 步 + 8.1 节（厚版本）。
//! 流程：重力 -> 地面约束 -> 速度限制（#021 推挤退役，同格只检不推）。
//! 占位制：重力/地面约束落被占格 → 悬停上一格（占位判定与移动/spawn 同源，
//! codeworld_core::occupancy）。

use codeworld_core::entity::{Entity, EntityId};
use codeworld_core::physics::{DefaultPhysics, PhysicsChange, PhysicsReason};
use codeworld_core::world::WorldState;
use std::collections::HashMap;

/// 碰撞对（两个坐标重叠的实体）
#[derive(Debug, Clone, PartialEq)]
pub struct CollisionPair {
    /// 实体 A
    pub a: EntityId,
    /// 实体 B
    pub b: EntityId,
    /// 重叠坐标
    pub pos: (f64, f64),
}

/// 默认物理规则引擎（架构文档 4.1 b 步 + 8.1 节）
///
/// 持有默认物理参数，每 tick 对实体执行重力/地面/速度/碰撞计算。
pub struct DefaultPhysicsEngine {
    params: DefaultPhysics,
}

impl DefaultPhysicsEngine {
    /// 创建引擎（用 DefaultPhysics::default()）
    pub fn new() -> Self {
        Self::new_with_params(DefaultPhysics::default())
    }

    /// 创建引擎（指定参数）
    pub fn new_with_params(params: DefaultPhysics) -> Self {
        tracing::debug!(
            gravity = params.gravity,
            ground_y = params.ground_y,
            "DefaultPhysicsEngine 创建"
        );
        Self { params }
    }

    /// 默认物理参数
    pub fn params(&self) -> &DefaultPhysics {
        &self.params
    }

    /// 应用重力：空中实体（y > ground_y）y 减少 gravity 值，返回产生的变更
    pub fn apply_gravity(&self, entity: &mut Entity) -> Option<PhysicsChange> {
        let (x, y) = entity.pos();
        if y > self.params.ground_y {
            let new_y = y + self.params.gravity; // gravity 为负值
            let new_pos = (x, new_y);
            entity.set_pos(new_pos);
            tracing::debug!(?entity.id, from = y, to = new_y, "重力应用");
            Some(PhysicsChange::new(
                entity.id,
                new_pos,
                PhysicsReason::Gravity,
                None,
            ))
        } else {
            None
        }
    }

    /// 地面约束：y < ground_y 时拉回 ground_y
    pub fn apply_ground_constraint(&self, entity: &mut Entity) -> Option<PhysicsChange> {
        let (x, y) = entity.pos();
        if y < self.params.ground_y {
            let new_pos = (x, self.params.ground_y);
            entity.set_pos(new_pos);
            tracing::debug!(?entity.id, from = y, to = self.params.ground_y, "地面约束");
            Some(PhysicsChange::new(
                entity.id,
                new_pos,
                PhysicsReason::Gravity,
                None,
            ))
        } else {
            None
        }
    }

    /// 速度限制：超出 max_velocity 时截断
    ///
    /// 截断 velocity 但不产生位置 PhysicsChange（速度限制改速度不改位置）。
    ///
    /// 返回 None：PhysicsChange 记录位置变更，速度限制不改位置故无变更。
    /// 签名保留 Option<PhysicsChange> 与其他 apply_* 一致（任务签名要求）。
    pub fn apply_velocity_limit(&self, entity: &mut Entity) -> Option<PhysicsChange> {
        let (vx, vy) = entity.velocity();
        let speed = (vx * vx + vy * vy).sqrt();
        if speed > self.params.max_velocity && speed > 0.0 {
            let scale = self.params.max_velocity / speed;
            let new_v = (vx * scale, vy * scale);
            entity.set_velocity(new_v);
            tracing::debug!(
                ?entity.id,
                from_speed = speed,
                to_speed = self.params.max_velocity,
                "速度限制"
            );
        }
        None
    }

    /// 检测所有坐标重叠的实体对
    pub fn detect_collisions(&self, entities: &HashMap<EntityId, Entity>) -> Vec<CollisionPair> {
        let mut pairs = Vec::new();
        let mut list: Vec<&Entity> = entities.values().collect();
        list.sort_by_key(|e| e.id.0); // 按 EntityId(Uuid) 排序，保证碰撞检测顺序确定（可重现）
        for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                // MVP：坐标完全相同（精确相等）判定碰撞，符合验收"坐标完全相同"。
                // 真实物理引擎用 epsilon 容差处理浮点精度，MVP 简化。
                if list[i].pos() == list[j].pos() {
                    tracing::debug!(a = ?list[i].id, b = ?list[j].id, pos = ?list[i].pos(), "检测到碰撞");
                    pairs.push(CollisionPair {
                        a: list[i].id,
                        b: list[j].id,
                        pos: list[i].pos(),
                    });
                }
            }
        }
        pairs
    }

    /// 解决碰撞（MVP 策略：后创建的实体挤到旁边一格 (x+1, y)）
    ///
    /// 注意：MVP 不处理级联碰撞（挤到新位置后若与第三实体重叠，本 tick 不重新检测）。
    ///
    /// #021 已退役：本函数不再被 tick 流程调用（物理步与主程序均不再调用）。
    /// 退役原因：挤压式解决产生"永久连体推挤"——两个实体同格后每拍被推 x+1
    /// 环游世界（实测 P/T 连体 bug 实锤）。占位制（移动被挡+spawn 预检+占位重力）
    /// 让同格在正常路径下不可能发生，挤压失去存在意义。函数保留仅作历史记录，
    /// 旧测试 resolve_collision_moves_later_created 保留其行为文档价值。
    #[deprecated(
        since = "0.1.0",
        note = "#021 推挤退役：占位制取代挤压，tick 流程不再调用"
    )]
    pub fn resolve_collision(
        &self,
        pair: CollisionPair,
        entities: &mut HashMap<EntityId, Entity>,
    ) -> Vec<PhysicsChange> {
        let a_tick = entities.get(&pair.a).map(|e| e.created_tick);
        let b_tick = entities.get(&pair.b).map(|e| e.created_tick);
        let to_move = match (a_tick, b_tick) {
            (Some(a), Some(b)) => {
                if a >= b {
                    pair.a
                } else {
                    pair.b
                }
            }
            (Some(_), None) => pair.a,
            (None, Some(_)) => pair.b,
            (None, None) => return Vec::new(),
        };
        let mut changes = Vec::new();
        if let Some(entity) = entities.get_mut(&to_move) {
            let (x, y) = entity.pos();
            let new_pos = (x + 1.0, y);
            entity.set_pos(new_pos);
            tracing::debug!(?to_move, from = ?(x, y), to = ?new_pos, "碰撞解决（挤压）");
            changes.push(PhysicsChange::new(
                to_move,
                new_pos,
                PhysicsReason::Collision,
                None,
            ));
        }
        changes
    }

    /// 执行完整默认物理步骤：重力 -> 地面约束 -> 速度限制（#021 推挤已退役）
    ///
    /// #021 实体占位制：重力/地面约束产生的 y 变更同样过占位检查——落到被占格
    /// → 停在上一格（本拍不动，下拍重试）。实体按 EntityId 升序处理（占位裁决
    /// 确定序，与主程序移动裁决同源）。
    ///
    /// 碰撞检测保留为防御性观测：占位三闸（移动被挡/spawn 预检/占位重力）之下
    /// 同格理论不可能发生；若仍检出属异常路径，记 warn 让世界状态可见，不推挤。
    pub fn step(&self, world: &mut WorldState) -> Vec<PhysicsChange> {
        let mut changes = Vec::new();
        // 实体 id 升序（占位裁决确定序）
        let mut ids: Vec<EntityId> = world.entities.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            // 重力意图：先算后查占位，被挡则回滚坐标（本拍不动，下拍重试）
            let gravity_intent = {
                let Some(entity) = world.entities.get_mut(&id) else {
                    continue;
                };
                if !entity.is_alive() {
                    continue; // 死亡实体不受物理影响
                }
                let before = entity.pos();
                self.apply_gravity(entity).map(|c| (before, c))
            };
            if let Some((before, c)) = gravity_intent {
                if let Some(occupant) =
                    codeworld_core::occupancy::cell_occupied_by(&world.entities, c.new_pos, id)
                {
                    tracing::warn!(
                        entity_id = %id.0,
                        occupant = %occupant.0,
                        from = ?before,
                        target = ?c.new_pos,
                        "[step] 重力下落被挡（目标格被占，悬停上一格）"
                    );
                    if let Some(entity) = world.entities.get_mut(&id) {
                        entity.set_pos(before); // 回滚：apply_gravity 已改坐标
                    }
                } else {
                    changes.push(c);
                }
            }
            // 地面约束意图：同理过占位检查（回拉目标格被占 → 本拍不回拉）
            let ground_intent = {
                let Some(entity) = world.entities.get_mut(&id) else {
                    continue;
                };
                if !entity.is_alive() {
                    continue;
                }
                let before = entity.pos();
                self.apply_ground_constraint(entity).map(|c| (before, c))
            };
            if let Some((before, c)) = ground_intent {
                if let Some(occupant) =
                    codeworld_core::occupancy::cell_occupied_by(&world.entities, c.new_pos, id)
                {
                    tracing::warn!(
                        entity_id = %id.0,
                        occupant = %occupant.0,
                        from = ?before,
                        target = ?c.new_pos,
                        "[step] 地面回拉被挡（目标格被占）"
                    );
                    if let Some(entity) = world.entities.get_mut(&id) {
                        entity.set_pos(before); // 回滚：apply_ground_constraint 已改坐标
                    }
                } else {
                    changes.push(c);
                }
            }
            // 速度限制（改速度不改坐标，无占位问题）
            if let Some(entity) = world.entities.get_mut(&id) {
                if entity.is_alive() {
                    self.apply_velocity_limit(entity);
                }
            }
        }
        // 防御性同格观测（#021 推挤退役：只检不推）
        if self.params.collision_enabled {
            let pairs = self.detect_collisions(&world.entities);
            for pair in pairs {
                tracing::warn!(
                    a = ?pair.a,
                    b = ?pair.b,
                    pos = ?pair.pos,
                    "[step] 异常：仍检出同格重叠（占位制下不应发生，不推挤）"
                );
            }
        }
        tracing::info!(change_count = changes.len(), "默认物理 step 完成");
        changes
    }
}

impl Default for DefaultPhysicsEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
    use codeworld_core::physics::{DefaultPhysics, PhysicsReason};
    use codeworld_core::world::WorldState;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn make_entity(id_u: u128, x: f64, y: f64, created_tick: u64) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "h".to_string(),
            (x, y),
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            created_tick,
            1_000_000,
        )
    }

    #[test]
    fn new_uses_default_physics() {
        // 创建：new() 用 DefaultPhysics::default()
        let engine = DefaultPhysicsEngine::new();
        assert_eq!(engine.params().gravity, -9.8);
        assert_eq!(engine.params().ground_y, 0.0);
        assert!(engine.params().collision_enabled);
        assert_eq!(engine.params().max_velocity, 100.0);
    }

    #[test]
    fn new_with_params_custom() {
        let p = DefaultPhysics {
            gravity: -5.0,
            ground_y: 10.0,
            collision_enabled: false,
            max_velocity: 50.0,
        };
        let engine = DefaultPhysicsEngine::new_with_params(p);
        assert_eq!(engine.params().gravity, -5.0);
        assert_eq!(engine.params().max_velocity, 50.0);
    }

    #[test]
    fn apply_gravity_in_air_decreases_y() {
        // 修改：空中实体 y 减少 gravity 值
        let engine = DefaultPhysicsEngine::new();
        let mut e = make_entity(1, 5.0, 100.0, 0);
        let change = engine.apply_gravity(&mut e);
        assert!(change.is_some());
        assert_eq!(e.pos(), (5.0, 100.0 - 9.8));
        assert_eq!(change.unwrap().reason, PhysicsReason::Gravity);
    }

    #[test]
    fn apply_gravity_on_ground_no_change() {
        // 边界：在地面（y <= ground_y）不产生变更
        let engine = DefaultPhysicsEngine::new();
        let mut e = make_entity(1, 5.0, 0.0, 0);
        let change = engine.apply_gravity(&mut e);
        assert!(change.is_none());
        assert_eq!(e.pos(), (5.0, 0.0));
    }

    #[test]
    fn apply_ground_constraint_pulls_back() {
        // 修改：y < ground_y 时拉回 ground_y
        let engine = DefaultPhysicsEngine::new();
        let mut e = make_entity(1, 5.0, -10.0, 0);
        let change = engine.apply_ground_constraint(&mut e);
        assert!(change.is_some());
        assert_eq!(e.pos(), (5.0, 0.0));
        assert_eq!(change.unwrap().reason, PhysicsReason::Gravity);
    }

    #[test]
    fn apply_ground_constraint_no_change_when_above() {
        // 边界：y >= ground_y 不产生变更
        let engine = DefaultPhysicsEngine::new();
        let mut e = make_entity(1, 5.0, 10.0, 0);
        assert!(engine.apply_ground_constraint(&mut e).is_none());
        assert_eq!(e.pos(), (5.0, 10.0));
    }

    #[test]
    fn apply_velocity_limit_clamps_over_max() {
        // 修改：速度超过 max_velocity 截断
        let engine = DefaultPhysicsEngine::new(); // max=100
        let mut e = make_entity(1, 0.0, 0.0, 0);
        e.set_velocity((200.0, 0.0)); // speed=200 > 100
        engine.apply_velocity_limit(&mut e);
        let (vx, vy) = e.velocity();
        assert!(vx.abs() <= 100.0 + 0.001, "vx 应截断到 100，实际 {}", vx);
        assert_eq!(vy, 0.0);
    }

    #[test]
    fn apply_velocity_limit_no_change_under_max() {
        // 边界：速度 <= max_velocity 不截断
        let engine = DefaultPhysicsEngine::new();
        let mut e = make_entity(1, 0.0, 0.0, 0);
        e.set_velocity((50.0, 30.0)); // speed~58 < 100
        engine.apply_velocity_limit(&mut e);
        assert_eq!(e.velocity(), (50.0, 30.0));
    }

    #[test]
    fn detect_collisions_same_pos_returns_pair() {
        // 读写：坐标完全相同返回 CollisionPair
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let e1 = make_entity(1, 5.0, 5.0, 0);
        let e2 = make_entity(2, 5.0, 5.0, 1);
        entities.insert(e1.id, e1);
        entities.insert(e2.id, e2);
        let pairs = engine.detect_collisions(&entities);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].pos, (5.0, 5.0));
    }

    #[test]
    fn detect_collisions_different_pos_empty() {
        // 边界：坐标不同返回空
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let e1 = make_entity(1, 5.0, 5.0, 0);
        let e2 = make_entity(2, 10.0, 10.0, 1);
        entities.insert(e1.id, e1);
        entities.insert(e2.id, e2);
        let pairs = engine.detect_collisions(&entities);
        assert_eq!(pairs.len(), 0);
    }

    #[test]
    #[allow(deprecated)]
    fn resolve_collision_moves_later_created() {
        // #021 已退役的行为文档：后创建的实体被挤到旁边一格 (x+1, y)。
        // 函数不再被 tick 流程调用；本测试保留记录历史挤压语义（退役原因见
        // resolve_collision 文档注释——永久连体推挤 bug）。
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let e1 = make_entity(1, 5.0, 5.0, 0); // 先创建
        let e2 = make_entity(2, 5.0, 5.0, 10); // 后创建
        let id1 = e1.id;
        let id2 = e2.id;
        entities.insert(id1, e1);
        entities.insert(id2, e2);
        let pair = CollisionPair {
            a: id1,
            b: id2,
            pos: (5.0, 5.0),
        };
        let changes = engine.resolve_collision(pair, &mut entities);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].reason, PhysicsReason::Collision);
        assert_eq!(entities.get(&id2).unwrap().pos(), (6.0, 5.0)); // 后创建被挤
        assert_eq!(entities.get(&id1).unwrap().pos(), (5.0, 5.0)); // 先创建不动
    }

    #[test]
    fn step_skips_dead_entities() {
        // 边界：死亡实体不受物理影响（y 不变，无变更）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut e = make_entity(1, 5.0, 100.0, 0);
        e.kill();
        let id = e.id;
        world.entities.insert(id, e);
        let changes = engine.step(&mut world);
        assert!(changes.is_empty(), "死亡实体不应产生物理变更");
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 100.0)); // y 未减
    }

    #[test]
    fn step_skips_collision_when_disabled() {
        // 边界：collision_enabled=false 时不检测碰撞
        let params = DefaultPhysics {
            gravity: 0.0,
            ground_y: 0.0,
            collision_enabled: false,
            max_velocity: 100.0,
        };
        let engine = DefaultPhysicsEngine::new_with_params(params);
        let mut world = WorldState::new();
        let e1 = make_entity(1, 5.0, 0.0, 0);
        let e2 = make_entity(2, 5.0, 0.0, 1);
        world.entities.insert(e1.id, e1);
        world.entities.insert(e2.id, e2);
        let changes = engine.step(&mut world);
        assert!(
            changes
                .iter()
                .all(|c| c.reason != PhysicsReason::Collision),
            "collision_enabled=false 不应有碰撞变更"
        );
    }

    #[test]
    fn step_executes_full_pipeline() {
        // 集成：step 执行完整流程，产生重力变更
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e = make_entity(1, 5.0, 100.0, 0);
        world.entities.insert(e.id, e);
        let changes = engine.step(&mut world);
        assert!(
            changes
                .iter()
                .any(|c| c.reason == PhysicsReason::Gravity),
            "应产生重力变更"
        );
        // 实体 y 应减少（重力）
        let entity = world.entities.values().next().unwrap();
        assert!(entity.pos().1 < 100.0);
    }

    // === #021 实体占位制：物理步占位检查 + 推挤退役 ===

    #[test]
    fn step_gravity_stops_above_occupied_cell() {
        // 占位重力：下落目标格被占 → 悬停上一格（本拍不动，下拍重试）
        // E2@(5,10) 重力落点 y=10-9.8=0.2，格 (5,0) 被 E1 占据 → 本拍不动
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e1 = make_entity(1, 5.0, 0.0, 0);
        let e2 = make_entity(2, 5.0, 10.0, 1);
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let changes = engine.step(&mut world);
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (5.0, 10.0), "目标格被占应悬停原位");
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.0), "占据者不受影响");
        assert!(
            !changes.iter().any(|c| c.entity == id2),
            "被挡实体不应产生物理变更"
        );
    }

    #[test]
    fn step_gravity_falls_freely_when_cell_free() {
        // 反向护栏：目标格空闲 → 照常下落（占位检查不过度拦截）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e = make_entity(1, 5.0, 10.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        engine.step(&mut world);
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 10.0 - 9.8), "空格应照常下落");
    }

    #[test]
    fn step_ground_constraint_blocked_when_ground_cell_occupied() {
        // 占位地面约束：穿地回拉目标格被占 → 本拍不回拉（停在重力后的位置，下拍重试）
        // E2@(5,0.5)：重力落到 -9.3（格 (5,-10) 空闲放行），地面约束目标 (5,0) 被 E1 占 → 不回拉
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e1 = make_entity(1, 5.0, 0.0, 0);
        let e2 = make_entity(2, 5.0, 0.5, 1);
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        engine.step(&mut world);
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (5.0, 0.5 - 9.8), "地面格被占不回拉");
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.0), "占据者不受影响");
    }

    #[test]
    fn step_no_shove_on_same_cell_anomaly() {
        // #021 推挤退役：同格重叠（异常路径）→ 只记 warn 不推挤，坐标原样保留
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e1 = make_entity(1, 5.0, 0.0, 0);
        let e2 = make_entity(2, 5.0, 0.0, 1);
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let changes = engine.step(&mut world);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.0), "异常同格不推挤");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (5.0, 0.0), "异常同格不推挤");
        assert!(
            changes.iter().all(|c| c.reason != PhysicsReason::Collision),
            "退役后不应再有 Collision 变更"
        );
    }

    #[test]
    fn step_falling_adjudication_order_deterministic_by_id() {
        // 占位裁决确定序：两个下落实体同抢一格，EntityId 小者先落（先到先得）
        // E1@(6,10.5) 落点 0.7 / E2@(6,10.0) 落点 0.2，同格 (6,0)：
        // E1(id 小) 先落成功 → E2 目标格被占 → 悬停 10.0
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e1 = make_entity(1, 6.0, 10.5, 0);
        let e2 = make_entity(2, 6.0, 10.0, 1);
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        engine.step(&mut world);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 10.5 - 9.8), "id 小者先落成功");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (6.0, 10.0), "id 大者被挡悬停");
    }
}
