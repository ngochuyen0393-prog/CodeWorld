//! 默认物理规则：DefaultPhysicsEngine 每 tick 执行基础物理计算
//!
//! 架构文档 4.1 节 b 步 + 8.1 节（厚版本）。
//! #030 半径物理（decisions.md）：格子降级为地址系统，重力/落地精确化为半径碰撞——
//! 圆实体下落解析落点（地面=半径线 / 落在别的实体身上=接触即停，堆叠合法）；
//! 造物（creation，格对齐 AABB）直落地面，落点与身体重叠则悬停重试。
//! 悬停实体（aloft 集合，操控层飞行意图注入）本拍免重力——被挡≠坠落。

use codeworld_core::collision::{body_of, body_overlap, body_touching, creation_aabb, Body};
use codeworld_core::entity::{Entity, EntityId};
use codeworld_core::physics::{DefaultPhysics, PhysicsChange, PhysicsReason};
use codeworld_core::world::WorldState;
use std::collections::{HashMap, HashSet};

/// 接触对（两个身体相触的实体，detect_touching 产出）
#[derive(Debug, Clone, PartialEq)]
pub struct CollisionPair {
    /// 实体 A（id 较小者）
    pub a: EntityId,
    /// 实体 B（id 较大者）
    pub b: EntityId,
    /// 接触位置（A 的坐标）
    pub pos: (f64, f64),
}

/// 默认物理规则引擎（架构文档 4.1 b 步 + 8.1 节）
///
/// 持有默认物理参数，每 tick 对实体执行重力落点解析/速度限制计算。
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

    /// 接触扫描（#030：替代旧"同坐标精确相等"）：所有存活实体两两
    /// body_touching（≤，相切即接触）判定，O(n²)，pair 内 a.id < b.id（确定序）。
    /// collision_enabled=false 时调用方跳过本扫描。
    pub fn detect_touching(&self, entities: &HashMap<EntityId, Entity>) -> Vec<CollisionPair> {
        let mut pairs = Vec::new();
        let mut list: Vec<&Entity> = entities.values().filter(|e| e.is_alive()).collect();
        list.sort_by_key(|e| e.id.0); // EntityId(Uuid) 升序，pair 定向 + 扫描顺序确定
        for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                let a = body_of(list[i]);
                let b = body_of(list[j]);
                if body_touching(&a, &b) {
                    tracing::debug!(a = ?list[i].id, b = ?list[j].id, pos = ?list[i].pos(), "检测到接触");
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

    /// 圆实体落点解析：从 from_y 以重力步长下落，返回落点 y。
    ///
    /// 地面 = max(ground_y, 自身半径)（脚贴地，圆心即位置）；
    /// 落点面上有身体（水平方向与我有重叠部分的下方实体/造物）→ 落到恰好接触的高度
    /// （圆-圆用 sqrt(s²-dxh²) 解析贴面；圆-AABB 落到盒顶+贴面）——堆叠合法，
    /// 底下的走了上面的自然下落。落点只在 from_y 及以下取（不向上传送）。
    fn circle_landing_y(
        &self,
        world: &WorldState,
        me_id: EntityId,
        me_x: f64,
        me_r: f64,
        from_y: f64,
    ) -> f64 {
        let fall_to = from_y + self.params.gravity; // gravity 为负值
        let mut landing = self.params.ground_y.max(me_r);
        for (id, e) in &world.entities {
            if *id == me_id || !e.is_alive() {
                continue;
            }
            match body_of(e) {
                Body::Circle { center, radius } => {
                    // 只落在中心不高于自己的实体身上（上方的桥不挡下落）
                    if center.1 > from_y {
                        continue;
                    }
                    let dxh = (me_x - center.0).abs();
                    let s = me_r + radius;
                    if dxh < s {
                        let cand = center.1 + (s * s - dxh * dxh).sqrt();
                        landing = landing.max(cand.min(from_y));
                    }
                }
                Body::Aabb { min, max } => {
                    // 造物 AABB：盒顶不高于自己才可落；钳制 x 求贴面高度
                    if max.1 > from_y {
                        continue;
                    }
                    let cx = me_x.clamp(min.0, max.0);
                    let dxh = (me_x - cx).abs();
                    if dxh < me_r {
                        let cand = max.1 + (me_r * me_r - dxh * dxh).sqrt();
                        landing = landing.max(cand.min(from_y));
                    }
                }
            }
        }
        if fall_to >= landing {
            fall_to
        } else {
            landing
        }
    }

    /// 执行完整默认物理步骤：重力落点解析 -> 速度限制（#030 半径物理）
    ///
    /// - `aloft`：本拍悬停实体集合（操控层飞行意图注入）——免重力（被挡≠坠落，
    ///   期四第五刀 bug 根治；升力是意图层持续作用，不随移动被挡取消）
    /// - 圆实体：落点解析（地面=半径线 / 落在身体上=接触即停，见 circle_landing_y）
    /// - 造物（creation，格对齐 AABB）：直落地面（y≥ground_y），落点身体重叠则
    ///   悬停原位下拍重试（造物运动罕见，MVP 不做 AABB 堆叠解析）
    /// - 实体按 EntityId 升序处理（确定序，与移动裁决同源）
    pub fn step(&self, world: &mut WorldState, aloft: &HashSet<EntityId>) -> Vec<PhysicsChange> {
        let mut changes = Vec::new();
        // 实体 id 升序（裁决确定序）
        let mut ids: Vec<EntityId> = world.entities.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            // 先读实体数据（落下借再用），落点计算借 world 不可变引用
            let (alive, aloft_skip, is_creation, before, me_r) = {
                let Some(entity) = world.entities.get(&id) else {
                    continue;
                };
                (
                    entity.is_alive(),
                    aloft.contains(&id),
                    entity.code_hash == codeworld_core::collision::CREATION_CODE,
                    entity.pos(),
                    entity.radius(),
                )
            };
            if !alive {
                continue; // 死亡实体不受物理影响
            }
            if aloft_skip {
                tracing::debug!(entity_id = %id.0, "[step] 悬停中，本拍免重力");
                continue;
            }
            let new_pos = if is_creation {
                // 造物：直落地面，落点与身体重叠则原位悬停（下拍重试）
                let target = (
                    before.0,
                    (before.1 + self.params.gravity).max(self.params.ground_y),
                );
                let (tmin, tmax) = creation_aabb(target);
                let blocked = world.entities.iter().any(|(oid, e)| {
                    *oid != id
                        && e.is_alive()
                        && body_overlap(&Body::Aabb { min: tmin, max: tmax }, &body_of(e))
                });
                if blocked {
                    tracing::warn!(
                        entity_id = %id.0,
                        target = ?target,
                        "[step] 造物下落落点身体重叠，悬停原位（下拍重试）"
                    );
                    before
                } else {
                    target
                }
            } else {
                let ly = self.circle_landing_y(world, id, before.0, me_r, before.1);
                (before.0, ly)
            };
            if new_pos != before {
                if let Some(entity) = world.entities.get_mut(&id) {
                    entity.set_pos(new_pos);
                }
                tracing::debug!(entity_id = %id.0, from = ?before, to = ?new_pos, "重力落点应用");
                changes.push(PhysicsChange::new(id, new_pos, PhysicsReason::Gravity, None));
            }
            // 速度限制（改速度不改坐标）
            if let Some(entity) = world.entities.get_mut(&id) {
                self.apply_velocity_limit(entity);
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

/// 解决碰撞（MVP 策略：后创建的实体挤到旁边一格 (x+1, y)）
///
/// #021 已退役：本函数不再被 tick 流程调用（物理步与主程序均不再调用）。
/// 退役原因：挤压式解决产生"永久连体推挤"——两个实体同格后每拍被推 x+1
/// 环游世界（实测 P/T 连体 bug 实锤）。
/// #030 再退役"同格"概念本身（半径物理取代格子占位）。函数保留仅作历史记录，
/// 旧测试 resolve_collision_moves_later_created 保留其行为文档价值。
#[deprecated(
    since = "0.1.0",
    note = "#021 推挤退役、#030 半径物理：占位制与挤压均已退役，tick 流程不再调用"
)]
pub fn resolve_collision_archived(
    engine: &DefaultPhysicsEngine,
    pair: CollisionPair,
    entities: &mut HashMap<EntityId, Entity>,
) -> Vec<PhysicsChange> {
    let _ = engine;
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

    fn make_creation(id_u: u128, x: f64, y: f64) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "creation".to_string(),
            (x, y),
            Appearance::new("T".to_string(), "cyan".to_string(), 1.0),
            0,
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

    // === #030 半径物理：重力落点解析 ===

    #[test]
    fn step_gravity_lands_at_radius_line() {
        // 空中实体下落：地面=半径线（脚贴地，圆心即位置）——不再钳 0
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e = make_entity(1, 5.0, 10.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 0.5), "落点=半径线 y=0.5");
    }

    #[test]
    fn step_resting_on_ground_stable() {
        // 已在半径线上：重力落点解析回到原位，坐标 tick 间稳定
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e = make_entity(1, 5.0, 0.5, 0);
        let id = e.id;
        world.entities.insert(id, e);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 0.5));
    }

    #[test]
    fn step_gravity_stacks_on_entity_below() {
        // 堆叠合法：H 从 (10,8) 落到 D (10,0.5) 头上——接触即停，落点=恰好接触高度
        // D 顶 1.0，H 圆心落 1.5（r+r）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let d = make_entity(1, 10.0, 0.5, 0);
        let h = make_entity(2, 10.0, 8.0, 1);
        let id_d = d.id;
        let id_h = h.id;
        world.entities.insert(id_d, d);
        world.entities.insert(id_h, h);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id_h).unwrap().pos(), (10.0, 1.5), "落在 D 头上");
        assert_eq!(world.entities.get(&id_d).unwrap().pos(), (10.0, 0.5), "底下的不受影响");
    }

    #[test]
    fn step_falls_past_when_landing_surface_leaves() {
        // 底下的走了，上面的自然下落：H 悬在 D 头上，D 挪走后 H 落回地面
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let d = make_entity(1, 10.0, 0.5, 0);
        let h = make_entity(2, 10.0, 1.5, 1);
        let id_d = d.id;
        let id_h = h.id;
        world.entities.insert(id_d, d);
        world.entities.insert(id_h, h);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id_h).unwrap().pos(), (10.0, 1.5), "D 在脚下时 H 稳定");
        let mut d = world.entities.remove(&id_d).unwrap();
        d.set_pos((20.0, 0.5)); // D 挪走
        world.entities.insert(id_d, d);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id_h).unwrap().pos(), (10.0, 0.5), "D 走了 H 落地");
    }

    #[test]
    fn step_gravity_lands_on_tower_top() {
        // 圆落在造物 AABB 顶上：塔格对齐 [10,11]×[0,1]，圆心落 1+0.5=1.5
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let t = make_creation(1, 10.0, 0.0);
        let h = make_entity(2, 10.5, 8.0, 1);
        let id_t = t.id;
        let id_h = h.id;
        world.entities.insert(id_t, t);
        world.entities.insert(id_h, h);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id_h).unwrap().pos(), (10.5, 1.5), "落在塔顶");
    }

    #[test]
    fn step_gravity_falls_past_beside_body() {
        // 水平距离 >= 半径和：不算落点面，从旁边直接落过（恰好=r 不算重叠的延伸）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let d = make_entity(1, 10.0, 0.5, 0);
        let h = make_entity(2, 11.6, 8.0, 1); // dxh=1.6 > r+r=1.0
        let id_d = d.id;
        let id_h = h.id;
        world.entities.insert(id_d, d);
        world.entities.insert(id_h, h);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id_h).unwrap().pos(), (11.6, 0.5), "从旁边落过到地面");
        let _ = id_d;
    }

    #[test]
    fn step_aloft_entity_ignores_gravity() {
        // 悬停实体免重力（被挡≠坠落，升力独立）：aloft 集合内实体坐标不动
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e = make_entity(1, 5.0, 10.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        let mut aloft = HashSet::new();
        aloft.insert(id);
        engine.step(&mut world, &aloft);
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 10.0), "悬停中不落");
    }

    #[test]
    fn step_creation_falls_to_ground() {
        // 造物（AABB）从空中直落地面 y=0（占地格贴地）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let t = make_creation(1, 10.0, 5.0);
        let id = t.id;
        world.entities.insert(id, t);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id).unwrap().pos(), (10.0, 0.0), "造物落地 y=0");
    }

    #[test]
    fn step_creation_hover_when_landing_overlaps_body() {
        // 造物落点与身体重叠 → 悬停原位下拍重试（MVP 无 AABB 堆叠解析）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let d = make_entity(1, 10.0, 0.5, 0);
        let t = make_creation(2, 10.0, 5.0); // 落地 (10,0) 与 D 圆(10,0.5) 重叠
        let id_d = d.id;
        let id_t = t.id;
        world.entities.insert(id_d, d);
        world.entities.insert(id_t, t);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id_t).unwrap().pos(), (10.0, 5.0), "落点重叠悬停原位");
    }

    #[test]
    fn step_two_fallers_stack_by_id_order() {
        // 两个下落实体同点：id 小者先落地面，id 大者随后落在 id 小者头上
        //（每拍重力 -9.8：e2 从 12 先自由落到 2.2【还在 e1 上方】，下一拍落点被
        // e1 拦住堆到 1.5——落点解析拦的是"会穿过"的下落，不是瞬移）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e1 = make_entity(1, 6.0, 10.0, 0);
        let e2 = make_entity(2, 6.0, 12.0, 1);
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 0.5), "id 小者落地");
        assert!(
            (world.entities.get(&id2).unwrap().pos().1 - 2.2).abs() < 1e-9,
            "e2 自由落到 e1 上方（浮点容差断言）"
        );
        engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (6.0, 1.5), "e2 落在 e1 头上");
    }

    #[test]
    fn step_no_shove_on_overlap_anomaly() {
        // 异常重叠（出生同点等）：不推挤，坐标保留（异常可见不自动修复，#021 精神延续）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e1 = make_entity(1, 5.0, 0.5, 0);
        let e2 = make_entity(2, 5.0, 0.5, 1);
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let changes = engine.step(&mut world, &HashSet::new());
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.5), "异常重叠不推挤");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (5.0, 0.5), "异常重叠不推挤");
        assert!(
            changes.iter().all(|c| c.reason != PhysicsReason::Collision),
            "不应有 Collision 变更（推挤已退役）"
        );
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
        let changes = engine.step(&mut world, &HashSet::new());
        assert!(changes.is_empty(), "死亡实体不应产生物理变更");
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 100.0)); // y 未减
    }

    #[test]
    fn step_executes_full_pipeline() {
        // 集成：step 执行完整流程，产生重力变更（每拍 -9.8，10 拍内从 100 落到半径线）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e = make_entity(1, 5.0, 100.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        let changes = engine.step(&mut world, &HashSet::new());
        assert!(
            changes.iter().any(|c| c.reason == PhysicsReason::Gravity),
            "应产生重力变更"
        );
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 90.2), "单拍 -9.8");
        for _ in 0..20 {
            engine.step(&mut world, &HashSet::new());
        }
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 0.5), "最终落到半径线");
    }

    // === detect_touching（接触扫描，≤）===

    #[test]
    fn detect_touching_exact_touch_returns_pair() {
        // 恰好相切（距离=半径和）= 接触（≤），pair 内 a.id < b.id
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let e1 = make_entity(1, 14.0, 0.5, 0);
        let e2 = make_entity(2, 15.0, 0.5, 1);
        let id1 = e1.id;
        let id2 = e2.id;
        entities.insert(id1, e1);
        entities.insert(id2, e2);
        let pairs = engine.detect_touching(&entities);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].a, id1, "a=id 较小者");
        assert_eq!(pairs[0].b, id2);
    }

    #[test]
    fn detect_touching_separated_empty() {
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let e1 = make_entity(1, 5.0, 0.5, 0);
        let e2 = make_entity(2, 15.0, 0.5, 1);
        entities.insert(e1.id, e1);
        entities.insert(e2.id, e2);
        assert_eq!(engine.detect_touching(&entities).len(), 0);
    }

    #[test]
    fn detect_touching_circle_vs_tower_face() {
        // 圆贴塔西脸（距 AABB 边恰 0.5）= 接触
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let p = make_entity(1, 9.5, 0.5, 0);
        let t = make_creation(2, 10.0, 0.0);
        entities.insert(p.id, p);
        entities.insert(t.id, t);
        assert_eq!(engine.detect_touching(&entities).len(), 1);
    }

    #[test]
    fn detect_touching_ignores_dead() {
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let mut e1 = make_entity(1, 14.0, 0.5, 0);
        e1.kill();
        let e2 = make_entity(2, 15.0, 0.5, 1);
        entities.insert(e1.id, e1);
        entities.insert(e2.id, e2);
        assert_eq!(engine.detect_touching(&entities).len(), 0, "死者不参与接触");
    }

    // === 退役留档 ===

    #[test]
    #[allow(deprecated)]
    fn resolve_collision_moves_later_created() {
        // #021/#030 已退役的行为文档：后创建的实体被挤到旁边一格 (x+1, y)。
        // 函数不再被 tick 流程调用；本测试保留记录历史挤压语义（退役原因见
        // resolve_collision_archived 文档注释——永久连体推挤 bug）。
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
        let changes = resolve_collision_archived(&engine, pair, &mut entities);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].reason, PhysicsReason::Collision);
        assert_eq!(entities.get(&id2).unwrap().pos(), (6.0, 5.0)); // 后创建被挤
        assert_eq!(entities.get(&id1).unwrap().pos(), (5.0, 5.0)); // 先创建不动
    }
}
