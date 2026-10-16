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

/// 弹丸寿命上限（拍）。防永飞兜底：飞出 viewport 的弹丸沿此路径超时消亡。
/// 超时消亡 = 哑弹（INFO 日志在主程序事件消费侧打，物理步只产出事件）
pub const PROJECTILE_MAX_AGE_TICKS: u64 = 30;

/// 弹丸步进事件（物理步只检测，结算归主程序管线）
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectileEvent {
    /// 撞身体：命中目标（结算由主程序走 deliver_attacks 管线，弹丸照亡）
    HitBody {
        projectile: EntityId,
        target: EntityId,
        /// 命中点（目标坐标，主程序拿它过 find_hit_target 复用 #031 解析）
        point: (f64, f64),
    },
    /// 落地：触地消亡，payload 在落点做点命中（打不中任何身体 = 哑弹）
    Landed { projectile: EntityId, point: (f64, f64) },
    /// 超时哑弹：达到 PROJECTILE_MAX_AGE_TICKS 上限消亡
    Expired { projectile: EntityId },
}

/// 阻力定律（#034 立法第 3 条：阻力减速 = 阻力 ÷ 质量，除法是定律质量为分母）
///
/// v = v - drag * v / mass：线性阻力模型，方向不变模衰减。
/// 极端参数（质量 < 阻力）下裸公式会越过零点反向——钳到 0（方向永不翻转，
/// "方向不变模衰减"的物理诚实实现）。
/// MVP 只作用于弹丸（唯一有速度矢量的物体）；普通实体速度恒 (0,0)，
/// 公式为恒等变换，行为零变化（#034 零回归锚）。
pub fn apply_drag(v: (f64, f64), mass: f64, drag: f64) -> (f64, f64) {
    let factor = 1.0 - drag / mass;
    let decay = |comp: f64| -> f64 {
        let next = comp * factor;
        // 符号翻转 = 衰减越过零点 = 钳到零
        if comp != 0.0 && next.signum() != comp.signum() {
            0.0
        } else {
            next
        }
    };
    (decay(v.0), decay(v.1))
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
    /// 弹丸不参与（#034：弹丸撞身体的结算走弹道步的 HitBody 事件，
    /// 不开接触沙箱通道——弹丸是物体不是生物）。
    pub fn detect_touching(&self, entities: &HashMap<EntityId, Entity>) -> Vec<CollisionPair> {
        let mut pairs = Vec::new();
        let mut list: Vec<&Entity> = entities
            .values()
            .filter(|e| e.is_alive() && !codeworld_core::collision::is_projectile(e))
            .collect();
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

    /// 弹丸步进（#034 弹道系统：离体之物的世界物理）
    ///
    /// 每拍对每个弹丸（EntityId 升序确定序）执行：
    /// 1. 寿命检查：age（current_tick - created_tick）≥ PROJECTILE_MAX_AGE_TICKS
    ///    -> Expired 事件 + 消亡（哑弹，不再积分）
    /// 2. 重力：vy += gravity（弹丸吃满重力，无 aloft 豁免——它就是要落地的）
    /// 3. 阻力：apply_drag 定律（v = v - drag*v/mass，方向不变模衰减）
    /// 4. 位移：pos += v（一拍一步，与世界的拍节奏一致）
    /// 5. 撞身体：与所有活体做圆判据（严格 <，含造物 AABB；跳过发射者——
    ///    出生拍起豁免；跳过其他弹丸——互撞 MVP 不做，弹丸互穿）。
    ///    命中即 HitBody 事件 + 弹丸消亡（结算归主程序 deliver_attacks 管线）
    /// 6. 落地：y < radius 触地 -> Landed 事件 + 消亡（主程序对落点做点命中
    ///    #031 判据复用，打不中 = 哑弹）
    ///
    /// - `shooters`：弹丸 -> 发射者映射（主程序持有；出生拍起豁免发射者的数据源）
    /// - 返回事件按弹丸 id 升序（确定序，主程序结算不依赖遍历随机性）
    pub fn step_projectiles(
        &self,
        world: &mut WorldState,
        current_tick: u64,
        shooters: &HashMap<EntityId, EntityId>,
    ) -> Vec<ProjectileEvent> {
        let mut events = Vec::new();
        let mut ids: Vec<EntityId> = world
            .entities
            .iter()
            .filter(|(_, e)| e.is_alive() && codeworld_core::collision::is_projectile(e))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_by_key(|id| id.0);
        for pid in ids {
            // 先读弹丸状态（积分与事件检测需要）
            let (before, mut vel, mass, radius, birth) = {
                let Some(p) = world.entities.get(&pid) else {
                    continue;
                };
                (p.pos(), p.velocity(), p.mass(), p.radius(), p.created_tick)
            };
            // 1. 寿命检查（先于积分：寿命尽了不再飞）
            if current_tick.saturating_sub(birth) >= PROJECTILE_MAX_AGE_TICKS {
                if let Some(p) = world.entities.get_mut(&pid) {
                    p.kill();
                }
                tracing::debug!(projectile = %pid.0, age = current_tick.saturating_sub(birth), "[projectile] 超时哑弹");
                events.push(ProjectileEvent::Expired { projectile: pid });
                continue;
            }
            // 2. 重力（吃满，无 aloft 豁免）
            vel.1 += self.params.gravity;
            // 3. 阻力定律（质量为分母）
            vel = apply_drag(vel, mass, self.params.drag_coefficient);
            // 3.5 世界极速钳制（#034 修订一，双审 P1-A 修复）：积分后（重力+阻力）、
            // 位移前归一化压回——重力累积从此封顶，弹丸一拍位移不超过极速。
            // 视觉可读性常数：攻击永远看得见飞过来（发射端钳制见 arena shoot import，
            // 两处口径同源 config.max_speed / DefaultPhysics.max_speed）
            let speed2 = vel.0 * vel.0 + vel.1 * vel.1;
            let max_speed = self.params.max_speed;
            if speed2 > max_speed * max_speed {
                let scale = max_speed / speed2.sqrt();
                tracing::debug!(
                    projectile = %pid.0,
                    from_speed = speed2.sqrt(),
                    to_speed = max_speed,
                    "[projectile] 世界极速钳制（重力累积封顶）"
                );
                vel = (vel.0 * scale, vel.1 * scale);
            }
            // 4. 位移（一拍一步）
            let after = (before.0 + vel.0, before.1 + vel.1);
            if let Some(p) = world.entities.get_mut(&pid) {
                p.set_velocity(vel);
                p.set_pos(after);
            }
            // 5. CCD 路径检查（#034 修订二）：撞身体从"拍末点在身内"升级为
            //    "起点->终点线段扫过身体"（点到线段距离 < r弹+r身；造物 AABB 用
            //    线段-扩展 AABB 相交）——高速弹一拍跳过目标的隧穿修复，
            //    极速常数之内的任何速度都如实结算。
            //    多目标取线段参数 t 最小者（最先碰到），与触地 t 一起统一按真实
            //    先后定序（先撞体后落地不再靠检查顺序，消灭随机性）。
            //    跳过发射者（出生拍起豁免）/其他弹丸（互撞 MVP 不做）的口径不变。
            let shooter = shooters.get(&pid).copied();
            let mut best_t: Option<f64> = None;
            let mut hit: Option<EntityId> = None;
            let mut others: Vec<EntityId> = world
                .entities
                .iter()
                .filter(|(oid, e)| {
                    e.is_alive()
                        && **oid != pid
                        && !codeworld_core::collision::is_projectile(e)
                        && Some(**oid) != shooter
                })
                .map(|(oid, _)| *oid)
                .collect();
            others.sort_by_key(|id| id.0); // 确定序：t 并列时先扫到者保留（id 升序）
            for oid in others {
                let Some(target) = world.entities.get(&oid) else {
                    continue;
                };
                let contact_t = match body_of(target) {
                    Body::Circle { center, radius: tr } => {
                        codeworld_core::collision::segment_first_contact_circle(
                            before, after, center, radius + tr,
                        )
                    }
                    Body::Aabb { min, max } => {
                        codeworld_core::collision::segment_first_contact_expanded_aabb(
                            before, after, min, max, radius,
                        )
                    }
                };
                if let Some(t) = contact_t {
                    if best_t.is_none_or(|bt| t < bt) {
                        best_t = Some(t);
                        hit = Some(oid);
                    }
                }
            }
            // 触地参数 t（地面=半径线）：路径自上而下穿过半径线的交点
            let ground_t: Option<f64> = if after.1 < radius {
                if before.1 >= radius {
                    Some((before.1 - radius) / (before.1 - after.1))
                } else {
                    Some(0.0) // 起步已在半径线下（异常态兜底）：立即触地
                }
            } else {
                None
            };
            let body_first = match (best_t, ground_t) {
                (Some(bt), Some(gt)) => bt <= gt,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if body_first {
                let tid = hit.expect("body_first=true 时必有目标");
                let point = world.entities.get(&tid).map(|e| e.pos()).unwrap_or(after);
                if let Some(p) = world.entities.get_mut(&pid) {
                    p.kill();
                }
                tracing::debug!(projectile = %pid.0, target = %tid.0, "[projectile] 撞身体（CCD 路径扫掠）");
                events.push(ProjectileEvent::HitBody {
                    projectile: pid,
                    target: tid,
                    point,
                });
                continue;
            }
            // 6. 落地：路径先触地则结算于触地点——落点=线段与地面线的交点
            //（x 取交点插值、y=半径线），不再是拍末积分位置（高速弹穿地后拉回的
            // 旧口径同时退役）；P2 顺手修：尸体坐标同步钳到落点，地下异态不留在世界
            if let Some(gt) = ground_t {
                let land = (before.0 + (after.0 - before.0) * gt, radius);
                if let Some(p) = world.entities.get_mut(&pid) {
                    p.set_pos(land);
                    p.kill();
                }
                tracing::debug!(projectile = %pid.0, pos = ?land, "[projectile] 触地（CCD 地面交点）");
                events.push(ProjectileEvent::Landed {
                    projectile: pid,
                    point: land,
                });
            }
        }
        if !events.is_empty() {
            tracing::debug!(event_count = events.len(), "[projectile] 弹道步完成");
        }
        events
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
    /// - 弹丸（#034）：跳过——弹丸是速度积分制物体，由 step_projectiles 接管；
    ///   本步的位置重力制（落到半径线）会瞬移弹丸
    /// - 实体按 EntityId 升序处理（确定序，与移动裁决同源）
    pub fn step(&self, world: &mut WorldState, aloft: &HashSet<EntityId>) -> Vec<PhysicsChange> {
        let mut changes = Vec::new();
        // 实体 id 升序（裁决确定序）
        let mut ids: Vec<EntityId> = world.entities.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            // 先读实体数据（落下借再用），落点计算借 world 不可变引用
            let (alive, aloft_skip, is_creation, is_proj, before, me_r) = {
                let Some(entity) = world.entities.get(&id) else {
                    continue;
                };
                (
                    entity.is_alive(),
                    aloft.contains(&id),
                    entity.code_hash == codeworld_core::collision::CREATION_CODE,
                    codeworld_core::collision::is_projectile(entity),
                    entity.pos(),
                    entity.radius(),
                )
            };
            if !alive {
                continue; // 死亡实体不受物理影响
            }
            // #034：弹丸不进位置重力步（速度积分制，step_projectiles 全权接管）
            if is_proj {
                tracing::debug!(entity_id = %id.0, "[step] 弹丸由弹道步接管，跳过位置重力");
                continue;
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
            drag_coefficient: 0.1,
            max_speed: 25.0,
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

    // === #034 弹道系统：阻力定律 + 弹丸物理步 ===

    use crate::default::{ProjectileEvent, PROJECTILE_MAX_AGE_TICKS};
    use codeworld_core::collision::PROJECTILE_CODE;

    fn make_projectile(id_u: u128, pos: (f64, f64), vel: (f64, f64), created_tick: u64) -> Entity {
        let mut e = Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            codeworld_core::entity::PlayerId(Uuid::from_u128(7)),
            PROJECTILE_CODE.to_string(),
            pos,
            Appearance::new(".".to_string(), "red".to_string(), 1.0),
            created_tick,
            1_000_000,
        );
        e.set_radius(codeworld_core::collision::PROJECTILE_RADIUS);
        e.set_mass(1.0);
        e.set_velocity(vel);
        e
    }

    #[test]
    fn drag_law_scales_by_one_minus_drag_over_mass() {
        // 阻力定律（#034 立法第 3 条）：v = v - drag*v/mass（方向不变模衰减，线性模型）
        // mass=1.0, drag=0.05：每拍衰 5%
        let v = crate::default::apply_drag((4.0, -3.0), 1.0, 0.05);
        assert!((v.0 - 3.8).abs() < 1e-9 && (v.1 + 2.85).abs() < 1e-9);
    }

    #[test]
    fn drag_law_heavy_decays_less_light_decays_more() {
        // 质量为分母：重弹衰减少（弹道平直远），轻弹衰减多（短距弯垂）
        let heavy = crate::default::apply_drag((10.0, 0.0), 10.0, 0.05); // 衰 0.5%
        let light = crate::default::apply_drag((10.0, 0.0), 0.5, 0.05); // 衰 10%
        assert!(heavy.0 > 9.9);
        assert!(light.0 < 9.05);
        // 方向都不变（模衰减不是反向）
        assert!(heavy.0 > 0.0 && light.0 > 0.0);
    }

    #[test]
    fn drag_law_zero_clamped_when_decay_exceeds_velocity() {
        // 极端参数（质量低于阻力）：衰减会越过零点——钳到 0，方向永不翻转
        // mass=0.05, drag=0.1：factor = 1-2 = -1，裸公式会反向
        let v = crate::default::apply_drag((4.0, -3.0), 0.05, 0.1);
        assert_eq!(v, (0.0, 0.0), "衰减超过速度模长时钳到零，不反向");
    }

    #[test]
    fn projectile_gravity_parabola_two_ticks() {
        // 固定初速 (4, 9.8) mass 1.0 @ (0,10)：重力+阻力+位移逐拍断言（vy 先加 g 再阻力再位移）
        // tick1: vy=9.8-9.8=0 -> 阻力 x0.95 -> v=(3.8,0) -> pos=(3.8,10)
        // tick2: vy=-9.8 -> x0.95 -> v=(3.61,-9.31) -> pos=(7.41,0.69) 仍在飞
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_projectile(1, (0.0, 10.0), (4.0, 9.8), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert!(events.is_empty(), "前两拍无事件（还在飞）");
        assert!(near(world.entities.get(&pid).unwrap().pos(), (3.8, 10.0)));
        engine.step_projectiles(&mut world, 2, &HashMap::new());
        assert!(near(world.entities.get(&pid).unwrap().pos(), (7.41, 0.69)));
    }

    #[test]
    fn projectile_lands_at_ground_and_dies() {
        // 落地消亡：y < radius 触地 -> Landed 事件（落点钳在地面半径线）+ 弹丸标记死亡。
        // 审查修正：裸积分落点在地下深处，#031 落点点命中永远够不着地面身体——
        // 落点 y 钳回半径线（弹丸触地在地面），x 保积分值
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        // 一拍内必落地：vy=-9.8 从 y=0.2 直坠
        let p = make_projectile(1, (3.0, 0.2), (1.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert_eq!(events.len(), 1);
        match &events[0] {
            ProjectileEvent::Landed { projectile, point } => {
                assert_eq!(*projectile, pid);
                assert!(
                    (point.1 - codeworld_core::collision::PROJECTILE_RADIUS).abs() < 1e-9,
                    "落点钳在地面半径线 y=0.15，实际 {point:?}"
                );
            }
            other => panic!("应落地事件，实际 {other:?}"),
        }
        assert!(!world.entities.get(&pid).unwrap().is_alive(), "落地弹丸消亡");
    }

    #[test]
    fn projectile_expires_at_max_age_dumb_shell() {
        // 超时哑弹：age 达 PROJECTILE_MAX_AGE_TICKS 上限 -> Expired 事件 + 消亡（不再积分）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_projectile(1, (0.0, 10.0), (5.0, 9.8), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let events = engine.step_projectiles(&mut world, PROJECTILE_MAX_AGE_TICKS + 1, &HashMap::new());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], ProjectileEvent::Expired { projectile: pid });
        assert!(!world.entities.get(&pid).unwrap().is_alive());
        // 哑弹不积分：坐标保持原位
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (0.0, 10.0));
    }

    #[test]
    fn projectile_hits_body_and_dies_without_killing_target() {
        // 撞身体：飞行途中圆判据命中（严格 <）-> HitBody 事件（带目标）+ 弹丸消亡；
        // 结算（on_attack 询问/换血）是主程序管线的事，物理步只检测不动目标。
        // 弹道（重力逐拍 -9.8 很凶，用上抛抛物线设计两拍拦截）：
        //   v0=(1.8,19.6) @ (0,10)：tick1 v=(1.71,9.31) pos=(1.71,19.31)；
        //   tick2 v=(1.6245,-0.4655) pos=(3.3345,18.8445)——目标放 (3.7,19.21)：
        //   tick1 距 1.99 未中，tick2 距 0.517 < 0.65 命中
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let target = make_entity(2, 3.7, 19.21, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_projectile(1, (0.0, 10.0), (1.8, 19.6), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert!(events.is_empty(), "第一拍未接触");
        let events = engine.step_projectiles(&mut world, 2, &HashMap::new());
        assert_eq!(events.len(), 1, "第二拍接触命中");
        match &events[0] {
            ProjectileEvent::HitBody { projectile, target, .. } => {
                assert_eq!(*projectile, pid);
                assert_eq!(*target, tid);
            }
            other => panic!("应命中事件，实际 {other:?}"),
        }
        assert!(!world.entities.get(&pid).unwrap().is_alive(), "命中弹丸消亡");
        assert!(world.entities.get(&tid).unwrap().is_alive(), "物理步不动目标（结算归主程序）");
    }

    #[test]
    fn projectile_never_hits_its_shooter() {
        // 不撞 owner：出生拍起豁免发射者（往回打/穿过发射者都不命中）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let shooter = make_entity(9, 10.0, 10.0, 0);
        let sid = shooter.id;
        world.entities.insert(sid, shooter);
        // 弹丸从发射者身上出生、初速向后（穿过发射者）
        let p = make_projectile(1, (10.0, 10.0), (-2.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let mut shooters = HashMap::new();
        shooters.insert(pid, sid);
        for t in 1..=6 {
            let events = engine.step_projectiles(&mut world, t, &shooters);
            assert!(
                events.iter().all(|e| !matches!(e, ProjectileEvent::HitBody { .. })),
                "第 {t} 拍不应命中发射者"
            );
        }
    }

    #[test]
    fn projectiles_pass_through_each_other() {
        // 弹丸互撞 MVP 不做：两弹相向交叉飞行互不命中（穿透）。
        // vy0=+9.8 顶住重力让两弹在高空交叉两拍（重力逐拍 -9.8，平射一拍即坠）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p1 = make_projectile(1, (5.0, 10.0), (2.0, 9.8), 0);
        let p2 = make_projectile(2, (9.0, 10.0), (-2.0, 9.8), 0);
        world.entities.insert(p1.id, p1);
        world.entities.insert(p2.id, p2);
        for t in 1..=2 {
            let events = engine.step_projectiles(&mut world, t, &HashMap::new());
            assert!(events.is_empty(), "第 {t} 拍弹丸互穿不应有事件");
        }
        // 第三拍双双坠地（Landed），但仍无任何 HitBody（互穿语义）
        let events = engine.step_projectiles(&mut world, 3, &HashMap::new());
        assert!(
            events.iter().all(|e| !matches!(e, ProjectileEvent::HitBody { .. })),
            "弹丸互穿：坠地不算互撞，实际 {events:?}"
        );
    }

    #[test]
    fn step_projectiles_leaves_normal_entities_untouched() {
        // 回归锚：弹丸步只动弹丸——普通实体（生物/造物）坐标零变化
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let normal = make_entity(5, 8.0, 0.5, 0);
        let nid = normal.id;
        let creation = make_creation(6, 12.0, 0.0);
        let cid = creation.id;
        world.entities.insert(nid, normal);
        world.entities.insert(cid, creation);
        let p = make_projectile(1, (0.0, 10.0), (3.0, 0.0), 0);
        world.entities.insert(p.id, p);
        engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert_eq!(world.entities.get(&nid).unwrap().pos(), (8.0, 0.5), "普通实体不动");
        assert_eq!(world.entities.get(&cid).unwrap().pos(), (12.0, 0.0), "造物不动");
    }

    #[test]
    fn step_gravity_skips_projectiles() {
        // 回归锚：通用物理步不碰弹丸——弹丸是速度积分制（step_projectiles 接管），
        // 位置重力制（circle_landing_y 落到半径线）会瞬移弹丸，必须跳过
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_projectile(1, (0.0, 10.0), (3.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new());
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (0.0, 10.0),
            "通用物理步不应对弹丸做位置重力"
        );
    }

    #[test]
    fn projectile_range_calibration_default_drag() {
        // 射程调参判据（阻力默认值的标定锚）：默认参数（drag=0.05）下，
        // 标准上抛弹（vx=4, vy=+9.8, mass=1.0 地面发射——vy 顶一拍重力再坠）
        // 水平射程落在 3~6 格。
        // CCD（#034 修订二）量测口径修正：落点=路径与地面线交点，同弹实测约
        // 3.9 格；旧口径 7.4 格是"穿地后拉回拍末 x"的量测伪影（drag 常数本身
        // 未变，此锚随修订二重标定）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let x0 = 50.0; // 远离边界，排除 viewport 干扰
        let p = make_projectile(1, (x0, 0.5), (4.0, 9.8), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let mut range = None;
        for t in 1..=60 {
            let events = engine.step_projectiles(&mut world, t, &HashMap::new());
            if let Some(ProjectileEvent::Landed { point, .. }) = events.first() {
                range = Some(point.0 - x0);
                break;
            }
        }
        let range = range.expect("60 拍内必落地");
        assert!(
            (3.0..=6.0).contains(&range),
            "标准上抛弹射程应 3~6 格（CCD 地面交点口径），实际 {range}"
        );
    }

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    // === 世界极速·飞行中钳制（#034 修订一，双审 P1-A 修复）===

    fn zero_drag_engine() -> DefaultPhysicsEngine {
        // 无阻力引擎：隔离重力累积效应（阻力会抵消重力，掩盖钳制是否生效）
        DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        })
    }

    #[test]
    fn projectile_flight_speed_capped_at_world_max_speed() {
        // P1-A 复现锚：重弹 mass=100 自由落体（阻力 0 可忽略），重力逐拍累积
        // 修复前速度无封顶（29 拍 vy≈-284）；修复后速度模长封顶于世界极速 25
        let engine = zero_drag_engine();
        let mut world = WorldState::new();
        let p = make_projectile(1, (50.0, 5000.0), (0.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let mut peak = 0.0f64;
        for t in 1..=29 {
            engine.step_projectiles(&mut world, t, &HashMap::new());
            let (vx, vy) = world.entities.get(&pid).unwrap().velocity();
            let speed = (vx * vx + vy * vy).sqrt();
            peak = peak.max(speed);
            assert!(
                speed <= 25.0 + 1e-9,
                "第 {t} 拍速度应封顶世界极速 25，实际 {speed}"
            );
        }
        assert!(
            peak > 24.0,
            "重力应把速度推到接近极速（证明钳制真实在工作而非速度没涨），峰值 {peak}"
        );
    }

    #[test]
    fn projectile_flight_displacement_per_tick_capped() {
        // 钳制在位移之前（重力+阻力后、位移前）：一拍位移不超过极速——
        // 视觉可读性常数的世界保证：任何飞行物一拍最多走 25 格
        let engine = zero_drag_engine();
        let mut world = WorldState::new();
        let p = make_projectile(1, (50.0, 5000.0), (0.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        for t in 1..=29 {
            let before = world.entities.get(&pid).unwrap().pos();
            engine.step_projectiles(&mut world, t, &HashMap::new());
            let after = world.entities.get(&pid).unwrap().pos();
            let moved = ((after.0 - before.0).powi(2) + (after.1 - before.1).powi(2)).sqrt();
            assert!(
                moved <= 25.0 + 1e-9,
                "第 {t} 拍位移应不超过极速 25，实际 {moved}"
            );
        }
    }

    #[test]
    fn projectile_at_exact_max_speed_untouched() {
        // 边界：速度恰等于极速不钳（严格 > 判据）：无重力无阻力下 (25,0) 分毫不动
        let engine = DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            gravity: 0.0,
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        });
        let mut world = WorldState::new();
        let p = make_projectile(1, (0.0, 10.0), (25.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert_eq!(
            world.entities.get(&pid).unwrap().velocity(),
            (25.0, 0.0),
            "恰等于极速的速度不应被改动"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (25.0, 10.0),
            "一拍位移恰为极速 25 格"
        );
    }

    // === CCD 路径检查（#034 修订二：撞体/落地从点检查升级为线段扫掠）===

    fn straight_engine() -> DefaultPhysicsEngine {
        // 直线弹道引擎（零重力零阻力）：CCD 几何断言不受抛物线干扰
        DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            gravity: 0.0,
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        })
    }

    #[test]
    fn swept_path_hits_midway_target_point_check_would_miss() {
        // CCD 锚定用例：极速内高速弹一拍跨 20 格、目标在路径中点——必命中。
        // 旧点检查：拍末 (20,20) 距目标 (10,20) 有 10 格 → 漏（隧穿实锤）；
        // CCD：线段 (0,20)->(20,20) 扫过目标 → 命中
        let engine = straight_engine();
        let mut world = WorldState::new();
        let target = make_entity(2, 10.0, 20.0, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_projectile(1, (0.0, 20.0), (20.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert_eq!(events.len(), 1, "路径中点目标必须命中（点检查必漏的反例）");
        match &events[0] {
            ProjectileEvent::HitBody { target: t, .. } => assert_eq!(*t, tid),
            other => panic!("应命中事件，实际 {other:?}"),
        }
        assert!(!world.entities.get(&pid).unwrap().is_alive());
    }

    #[test]
    fn swept_multi_target_first_contact_wins() {
        // 多目标取线段参数 t 最小者（最先碰到）：路径上 x=5 与 x=15 两个目标 → 命中 x=5
        let engine = straight_engine();
        let mut world = WorldState::new();
        let far = make_entity(2, 15.0, 20.0, 0);
        let near = make_entity(3, 5.0, 20.0, 0);
        let fid = far.id;
        let nid = near.id;
        world.entities.insert(fid, far);
        world.entities.insert(nid, near);
        let p = make_projectile(1, (0.0, 20.0), (20.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert_eq!(events.len(), 1);
        match &events[0] {
            ProjectileEvent::HitBody { target, .. } => {
                assert_eq!(*target, nid, "应命中路径上最先碰到的（t 最小）目标")
            }
            other => panic!("应命中事件，实际 {other:?}"),
        }
    }

    #[test]
    fn landed_point_is_ground_crossing_not_tick_end() {
        // 落地=路径与地面线的交点（CCD 落地）：(0,1) 直线 vel(20,-2) 一拍末 (20,-1)，
        // 地面交点 t=(1-0.15)/2=0.425 → x=8.5。旧实现落点 (20,0.15)=穿地后拉回，位置错
        let engine = straight_engine();
        let mut world = WorldState::new();
        let p = make_projectile(1, (0.0, 1.0), (20.0, -2.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert_eq!(events.len(), 1);
        match &events[0] {
            ProjectileEvent::Landed { point, .. } => {
                assert!((point.0 - 8.5).abs() < 1e-9, "落点 x 应为地面交点 8.5，实际 {point:?}");
                assert!((point.1 - 0.15).abs() < 1e-9, "落点 y 应为半径线，实际 {point:?}");
            }
            other => panic!("应落地事件，实际 {other:?}"),
        }
    }

    #[test]
    fn landing_corpse_position_clamped_to_ground_line() {
        // P2 顺手修：落地弹尸体坐标=落点（半径线上），不再是地下积分值（异态不留世界）
        let engine = straight_engine();
        let mut world = WorldState::new();
        let p = make_projectile(1, (0.0, 1.0), (20.0, -2.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let _ = engine.step_projectiles(&mut world, 1, &HashMap::new());
        let corpse = world.entities.get(&pid).unwrap();
        assert!(
            (corpse.pos().1 - 0.15).abs() < 1e-9,
            "尸体 y 应钳回半径线 0.15，实际 {}",
            corpse.pos().1
        );
        assert!((corpse.pos().0 - 8.5).abs() < 1e-9, "尸体 x 应为地面交点");
    }

    #[test]
    fn body_hit_before_ground_wins_by_contact_t() {
        // 同拍路径既穿身体又触地：按 t 定序（先撞体后落地不再靠检查顺序，是真实先后）。
        // (0,0.3) vel(10,-1)：地面 t=0.15（x=1.5）；身体 (1,0.29) r0.5 首触 t≈0.035 → 撞体
        let engine = straight_engine();
        let mut world = WorldState::new();
        let target = make_entity(2, 1.0, 0.29, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_projectile(1, (0.0, 0.3), (10.0, -1.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new());
        assert_eq!(events.len(), 1);
        match &events[0] {
            ProjectileEvent::HitBody { target: t, .. } => assert_eq!(*t, tid, "先触身体应撞体结算"),
            other => panic!("应撞体事件（t 先于触地），实际 {other:?}"),
        }
    }
}
