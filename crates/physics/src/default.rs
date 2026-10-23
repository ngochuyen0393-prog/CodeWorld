//! 默认物理规则：DefaultPhysicsEngine 每 tick 执行基础物理计算
//!
//! 架构文档 4.1 节 b 步 + 8.1 节（厚版本）。
//! #030 半径物理（decisions.md）：格子降级为地址系统，重力/落地精确化为半径碰撞——
//! 圆实体下落解析落点（地面=半径线 / 落在别的实体身上=接触即停，堆叠合法）；
//! 造物（creation，格对齐 AABB）直落地面，落点与身体重叠则悬停重试。
//! 悬停实体（aloft 集合，操控层飞行意图注入）本拍免重力——被挡≠坠落。

use codeworld_core::collision::{body_of, body_overlap, body_touching, creation_aabb, Body};
use codeworld_core::entity::{Entity, EntityId};
use codeworld_core::physics::{
    combine_friction, combine_restitution, DefaultPhysics, PhysicsChange, PhysicsReason,
};
use codeworld_core::terrain::{TERRAIN_SPACING, TERRAIN_X_MIN};
use codeworld_core::world::WorldState;
use std::collections::{HashMap, HashSet};

/// 低速闸阈值（D-010 碰撞响应刀细则④）：碰撞对相对法向速度低于此值（格/拍）
/// 按非弹性处理（e 生效值=0）——Box2D restitutionThreshold 1m/s / Unity
/// bounceThreshold 2 的拍制换算；慢拍（1拍/秒）下低速微弹=逐拍抖动，防抖必需。
/// 随世界常数批次重标定（不自动公式耦合）。
pub const RESTITUTION_LOW_SPEED: f64 = 1.0;

/// D-010 碰撞响应：碰撞瞬间一次性动量交换（#021 切割声明——撞完就分，非持续推挤）
///
/// 法向反弹 `v_n' = -e·v_n` + 切向摩擦 `v_t' = v_t·max(0, 1-f)`（简化库仑模型，
/// 组合后使用；组合前各分量已钳 [0,1] 故天然非负）——速度=法向+切向重组。
/// 反弹只改速度字段（位移下一拍物理步兑现——拍制世界的诚实形态：本拍停住，
/// 下拍弹走），停点/位置逻辑完全不动。
/// 调用方负责低速闸与"每对每拍一弹"闸（e 传 0 即非弹性，纯摩擦仍生效）。
fn bounce_response(vel: (f64, f64), normal: (f64, f64), e: f64, f: f64) -> (f64, f64) {
    let v_n = vel.0 * normal.0 + vel.1 * normal.1;
    let v_t = (vel.0 - v_n * normal.0, vel.1 - v_n * normal.1);
    let vn2 = -e * v_n;
    let keep = (1.0 - f).max(0.0);
    (
        vn2 * normal.0 + v_t.0 * keep,
        vn2 * normal.1 + v_t.1 * keep,
    )
}

use crate::fields::{effective_gravity_drag, FieldEffect};
use crate::liquid::{LiquidDomain, liquid_effect};

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

/// static 造物的地面高度（D-016）：格对齐 AABB 占地格 [cx, cx+1]，
/// 地面取该区间内高度数组峰值（全断点精确扫描——AABB 不能悬空：
/// 山坡半格悬空的塔不合法；分段线性面区间最大值必在断点或端点上，
/// 精确解无采样漏检）。纯查表无 IO、每拍热路径：日志豁免
fn terrain_ground_for_creation(world: &WorldState, x: f64) -> f64 {
    let (min, max) = creation_aabb((x, 0.0));
    terrain_ground_over(world, min.0, max.0)
}

/// 区间 [xa, xb] 内地形峰值（测试/造物落底共用）：精确口径 = 端点 +
/// 区间内全部 0.25 格断点取 max——分段线性函数在任意区间上的最大值必
/// 落在断点或区间端点（段内线性，内部无极值），全断点扫描即精确解。
/// 旧"端点+中点"三点采样会漏检 xa+0.25/xa+0.75 两个内部断点上的峰
///（R84 复审缺陷 2：全扫描实测最大差 0.375 格=Mountain 坡度钳制
/// max_delta，造物塔底可悬空至该差值）。纯查表无 IO：日志豁免
fn terrain_ground_over(world: &WorldState, xa: f64, xb: f64) -> f64 {
    // 端点先入 max（域外钳到域边高度；无断点子区间上线性，端点即极值）
    let mut peak = world.terrain.height(xa).max(world.terrain.height(xb));
    let samples = world.terrain.samples();
    if samples.len() >= 2 {
        // 断点采样下标区间 [i0, i1]；±1e-9 吸浮点噪声——恰落在断点上的
        // 端点不被 ceil/floor 因 ULP 排除（生产口径端点为整数格，除以
        // 0.25=2^-2 恰好精确，容差只为任意实数入参兜底）。
        // i0>i1（区间内无断点）时不补扫任何断点：区间外断点的高度不属于
        // 本区间，计入会高估峰值、破坏精确口径
        let n = samples.len() as i64;
        let i0 = ((((xa - TERRAIN_X_MIN) / TERRAIN_SPACING) - 1e-9).ceil() as i64).clamp(0, n - 1);
        let i1 = ((((xb - TERRAIN_X_MIN) / TERRAIN_SPACING) + 1e-9).floor() as i64).clamp(0, n - 1);
        if i0 <= i1 {
            for &h in &samples[i0 as usize..=i1 as usize] {
                peak = peak.max(h);
            }
        }
    }
    peak
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

    /// 线段扫掠首触身体检测（D-018 通用化：从 step_projectiles 迁移的 CCD 核，
    /// 对一切有速度实体生效）。
    ///
    /// 返回 Some((目标 id, t))：路径 [from, to] 上最先撞到的其他存活身体
    ///（t∈[0,1] 线段参数；目标按 EntityId 升序扫描，t 并列取先扫到者——
    /// 确定序，与退役前弹道步同口径）；None=路径不与任何身体相交。
    /// 圆目标判据 = segment_first_contact_circle（半径和窗口）；造物 AABB =
    /// segment_first_contact_expanded_aabb（按我的半径外扩）。
    /// 【#044 子步宪法锚】判据纯几何——只读位置/半径/身体形状，不读任何
    /// 机器数据（墙钟/CPU/帧率），同状态必然同结算。弹丸不作目标（is_projectile
    /// 历史实体不可被撞——互穿语义保留，D-002 弹弹相撞另立）。液体造物
    ///（liquid_density>0，D-009 布景）不作目标——wet 实体在水中浮沉穿越的
    /// 是"水"本身（泡在水里不撞水；实体撞实体由 detect_touching/接触通道管）。
    fn swept_first_body_contact(
        &self,
        world: &WorldState,
        me_id: EntityId,
        from: (f64, f64),
        to: (f64, f64),
        me_r: f64,
    ) -> Option<(EntityId, f64)> {
        let mut others: Vec<EntityId> = world
            .entities
            .iter()
            .filter(|(oid, e)| {
                e.is_alive()
                    && **oid != me_id
                    && !codeworld_core::collision::is_projectile(e)
                    && e.liquid_density() <= 0.0
            })
            .map(|(oid, _)| *oid)
            .collect();
        others.sort_by_key(|oid| oid.0); // 确定序：t 并列时先扫到者保留（id 升序）
        let mut best: Option<(EntityId, f64)> = None;
        for oid in others {
            let Some(target) = world.entities.get(&oid) else {
                continue;
            };
            let contact_t = match body_of(target) {
                Body::Circle { center, radius: tr } => {
                    codeworld_core::collision::segment_first_contact_circle(
                        from, to, center, me_r + tr,
                    )
                }
                Body::Aabb { min, max } => {
                    codeworld_core::collision::segment_first_contact_expanded_aabb(
                        from, to, min, max, me_r,
                    )
                }
            };
            if let Some(t) = contact_t {
                if best.as_ref().is_none_or(|(_, bt)| t < *bt) {
                    best = Some((oid, t));
                }
            }
        }
        best
    }

    /// 圆实体脚下高度（落点解析的纯几何核，与重力步长无关）：地面（terrain
    /// 高度插值，max(ground, r) 保脚贴地不陷入）与脚下可堆叠身体取最高——
    /// 落点面上有身体（水平方向与我有重叠部分的下方实体/造物）→ 落到恰好
    /// 接触的高度（圆-圆 sqrt(s²-dxh²) 解析贴面；圆-AABB 落到盒顶+贴面）。
    /// 候选只在 from_y 及以下取（不向上传送）。D-010a 速度积分路径与
    /// circle_landing_y 共用此核。
    fn circle_landing_surface(
        &self,
        world: &WorldState,
        me_id: EntityId,
        me_x: f64,
        me_r: f64,
        from_y: f64,
    ) -> f64 {
        // D-016 地形：地面=高度数组插值（max(ground, r) 保脚贴地——坑深时
        // 不陷入半径线以下，与旧平地口径 ground_y.max(r) 同构）
        let mut landing = world.terrain.height(me_x).max(me_r);
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
        landing
    }

    /// 圆实体落点解析：从 from_y 以重力步长下落，返回落点 y。
    /// max(terrain, r) 保底脚贴地不陷入；y=0 半径线是平地地形的特例）；
    /// 落点面上有身体→落到恰好接触的高度——堆叠合法，
    /// 底下的走了上面的自然下落。落点只在 from_y 及以下取（不向上传送）。
    /// `gravity`：本拍生效重力（D-012 力场域内=叠加 v2 结算值，域外=世界默认——
    /// 由调用方传 effective_gravity_drag 产物，本函数不做域判定）
    fn circle_landing_y(
        &self,
        world: &WorldState,
        me_id: EntityId,
        me_x: f64,
        me_r: f64,
        from_y: f64,
        gravity: f64,
    ) -> f64 {
        let fall_to = from_y + gravity; // 生效重力为负值（域钳制保证 ≤0）
        let landing = self.circle_landing_surface(world, me_id, me_x, me_r, from_y);
        if fall_to >= landing {
            fall_to
        } else {
            landing
        }
    }

    /// 执行完整默认物理步骤：重力落点解析 -> 速度限制（#030 半径物理）
    ///
    /// - `aloft`：本拍悬停实体集合（操控层飞行意图注入）——免重力（被挡≠坠落，
    ///   期四第五刀 bug 根治；升力是意图层持续作用，不随移动被挡取消）。
    ///   aloft 优先级高于场（D-012）：悬停声明本拍免重力，场不覆盖它——每拍
    ///   重申语义不变，悬停在世界重力前短路
    /// - `fields`：本拍场清单（main 每拍收集，登记制刷场）——域内实体按改后
    ///   参数被结算（叠加 v2 代数和+域钳制，见 fields::effective_gravity_drag）；
    ///   空清单=无场世界，行为与 D-012 前逐位一致（零回归锚）
    /// - 圆实体：落点解析（地面=半径线 / 落在身体上=接触即停，见 circle_landing_y）
    /// - 造物（creation，格对齐 AABB）：直落地面（y≥ground_y），落点身体重叠则
    ///   悬停原位下拍重试（造物运动罕见，MVP 不做 AABB 堆叠解析）
    /// - 带速度实体（生物/dynamic 造物，【D-018】含造物弹；存量旧弹丸仅历史
    ///   快照反序列化兼容——恢复即清场，运行时无活旧弹丸）：冲量
    ///   路径速度积分（重力→阻力→极速钳→中点位移→CCD 扫掠截断→落地清零）；
    ///   【历史注记】原 step_projectiles 弹丸专属管线（半隐式欧拉）随 #045 弹丸
    ///   退役并入本路径——一切有速度实体同一套弹道物理
    /// - 实体按 EntityId 升序处理（确定序，与移动裁决同源）
    pub fn step(
        &self,
        world: &mut WorldState,
        aloft: &HashSet<EntityId>,
        fields: &[FieldEffect],
    ) -> Vec<PhysicsChange> {
        // 无液体委托：空清单=世界默认，行为与 D-009 前逐位一致（零回归锚——
        // 现有全部物理测试经此路径照绿）
        self.step_with_liquids(world, aloft, fields, &[])
    }

    /// 执行完整默认物理步骤（带液体域，D-009：浮力改写+水中阻尼+附加质量+
    /// 零速入水速度制路由）
    ///
    /// - `liquids`：本拍液体域清单（main 每拍收集，登记制刷场）——域内实体
    ///   （生物+dynamic 造物）按液体物理被结算：先力场 effective_gravity_drag、
    ///   再浮力改写（正交改写代数叠加）、水中阻尼、附加质量进阻力项；空清单=
    ///   无液体世界，行为与 D-009 前逐位一致（零回归锚）。
    /// - 路由裁决（测试锚定 step_zero_velocity_entity_in_water_uses_velocity_regime）：
    ///   f>0 的实体一律走冲量积分路径（速度制），即使速度为零——阻尼定律
    ///   v×k 需要速度状态才成立；零速走位置制则每拍位移=全额 g_final，水面
    ///   附近会疯狂弹跳，"典型物体半沉"永远看不到。水面平衡=逐拍结算自然收敛。
    /// - static 造物/液体造物自己：不受液体影响（static 不吃物理现状照旧，
    ///   水不会动）。aloft 优先级高于浮力（短路在液体结算之前，与力场同款）。
    pub fn step_with_liquids(
        &self,
        world: &mut WorldState,
        aloft: &HashSet<EntityId>,
        fields: &[FieldEffect],
        liquids: &[LiquidDomain],
    ) -> Vec<PhysicsChange> {
        let mut changes = Vec::new();
        // 每对每拍一弹闸（D-010 细则⑤）：本拍已结算接触的无序对集合（按
        // (min_id, max_id) 键）——同对二次接触按非弹性处理
        let mut bounced_pairs: HashSet<(EntityId, EntityId)> = HashSet::new();
        // 实体 id 升序（裁决确定序）
        let mut ids: Vec<EntityId> = world.entities.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            // 先读实体数据（落下借再用），落点计算借 world 不可变引用
            // 【D-018 通用化】弹丸专属跳过分支随 step_projectiles 退役——旧
            // PROJECTILE_CODE 实体（历史存档/兼容判定，新世界不再产生）自此与
            // 一切有速度实体同管线：velocity≠0 走冲量路径（弹道物理），v≡0 走
            // 落点制。弹丸的"不可被锁定/不可被撞/不参与接触"语义保留在各自
            // 消费点（find_hit_target/detect_touching/CCD 目标过滤）
            let (alive, aloft_skip, is_creation, is_dyn_creation, before, me_r, me_mass, me_vel, me_e, me_f) = {
                let Some(entity) = world.entities.get(&id) else {
                    continue;
                };
                (
                    entity.is_alive(),
                    aloft.contains(&id),
                    entity.code_hash == codeworld_core::collision::CREATION_CODE,
                    codeworld_core::collision::is_dynamic_creation(entity),
                    entity.pos(),
                    entity.radius(),
                    entity.mass(),
                    entity.velocity(),
                    entity.restitution(),
                    entity.friction(),
                )
            };
            if !alive {
                continue; // 死亡实体不受物理影响
            }
            if aloft_skip {
                tracing::debug!(entity_id = %id.0, "[step] 悬停中，本拍免重力");
                continue;
            }
            // D-012 力场：本实体所处位置的生效重力/阻力（叠加 v2；空清单快速路径
            // 逐位返回默认值——无场世界行为与力场刀前一致）
            let (eff_gravity, eff_drag) = effective_gravity_drag(before, &self.params, fields);
            // D-009 液体结算（次序定死：①先力场 ②再浮力改写 ③水中阻尼 ④附加
            // 质量进阻力项——正交改写代数叠加）。static 造物不走液体结算（不吃
            // 物理现状照旧；液体造物自己=static 布景）；aloft 已在上面短路
            //（悬停免重力免浮力免阻尼，优先级高于浮力）
            let wet = if liquids.is_empty() || (is_creation && !is_dyn_creation) {
                None
            } else {
                let le = liquid_effect(before, me_r, me_mass, me_vel, (eff_gravity, eff_drag), liquids);
                (le.f > 0.0).then_some(le)
            };
            if let Some(le) = &wet {
                // 实体入水 INFO（带 f 值，任务书数据流两端日志；登记制无缓存
                // 故"出水"无独立边沿日志——本行缺席即出水信号，前端高亮判定同源）
                tracing::info!(
                    entity_id = %id.0,
                    f = le.f,
                    g_final = le.g_final,
                    k_eff = le.k_eff,
                    m_eff = le.m_eff,
                    "[liquid] 实体在水中（浮沉结算：入水=本行出现，出水=本行缺席）"
                );
            }
            // 分支选择（顺序固定，测试锚定）：static 造物→直落地面；带速度实体
            //（生物+dynamic 造物）或水中实体（f>0，即使零速——速度制路由裁决）
            // → D-010a 冲量路径（速度积分）；零速域外→落点制（现状）。
            // 产物：new_pos（本拍新坐标）+ ballistic_vel（Some=冲量路径要写回的速度）
            let (new_pos, ballistic_vel) = if is_creation && !is_dyn_creation {
                // static 造物：直落地面（D-016：地面=terrain.height(x)，格对齐 AABB
                // 以其覆盖列的最高地面为准——占地格不悬空），落点与身体重叠则
                // 原位悬停（下拍重试）
                let ground_here = terrain_ground_for_creation(world, before.0);
                let target = (
                    before.0,
                    (before.1 + eff_gravity).max(ground_here),
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
                    (before, None)
                } else {
                    (target, None)
                }
            } else if wet.is_some() || me_vel != (0.0, 0.0) {
                // D-010a 冲量路径（带速度生物/dynamic 造物速度积分，一拍一步）。
                // 【#021 切割声明】一次性冲量非持续推挤：已退役的"推挤"（持续重叠
                // 互相挤开→连体 bug）是每拍重复施加的位置推力；本路径是冲量入速度
                // 后的一次性动量积分（撞完就分），与任何重叠检测零耦合。
                // 积分次序（测试锚定）：冲量入速度（阶段二 thrust 应用）→ 重力入
                // 速度 → 阻力 → 世界极速钳制 → 中点位移 → 落地清零。中点位移 =
                // 速度吃整拍定律、位移取冲量后速度与结算后速度的均值（匀加速一拍
                // 的解析解 x += v·dt + g·dt²/2，dt=1）——若照抄弹丸半隐式欧拉（重力
                // 后速度直接位移），地面起跳 v0=7.7 < |g|=9.8 拍 1 位移为负，起跳会
                // 被落地解析当场吃掉跳不起来；中点法拍 1 位移 2.85，落在"跳 ≤3 格高"
                // 锚点内（thrust_ballistic_jump_peak_within_three_grid_anchor 锚定）。
                let mut vel = me_vel;
                // 重力入速度（空中累积；落地清零见下）。D-009 水中：阻尼+净加速
                // 一步完成（v ← v×k_eff + g_final，替代重力入速度步——MC 三步
                // 乘法加法同构，结算次序定死：①力场 ②浮力 ③水中阻尼 ④附加质量）
                if let Some(le) = &wet {
                    vel = (vel.0 * le.k_eff, vel.1 * le.k_eff + le.g_final);
                } else {
                    vel.1 += eff_gravity;
                }
                // 阻力定律（#045 通用化口径：一切有速度实体，质量为分母；域内用
                // 生效阻力——护弹道场在生物/dynamic 造物上的直接作用点）。
                // D-009 附加质量独门立法：水中阻力分母=m_eff（排开流体质量）——
                // "水下加速钝"从阻力响应涌现；ρ物 计算用真实 m 不用 m_eff（防递归）
                let drag_mass = wet.as_ref().map_or(me_mass, |le| le.m_eff);
                vel = apply_drag(vel, drag_mass, eff_drag);
                // 世界极速钳制（与弹丸/dynamic 造物同款：积分后归一化压回，方向不变）
                let speed2 = vel.0 * vel.0 + vel.1 * vel.1;
                let max_speed = self.params.max_speed;
                if speed2 > max_speed * max_speed {
                    let scale = max_speed / speed2.sqrt();
                    tracing::debug!(
                        entity_id = %id.0,
                        from_speed = speed2.sqrt(),
                        to_speed = max_speed,
                        "[step] 世界极速钳制（冲量路径）"
                    );
                    vel = (vel.0 * scale, vel.1 * scale);
                }
                // 中点位移：本拍位移 = (冲量后速度 + 定律结算后速度) / 2
                let after = (
                    before.0 + (me_vel.0 + vel.0) * 0.5,
                    before.1 + (me_vel.1 + vel.1) * 0.5,
                );
                // 【D-018 通用化】CCD 路径检查（原 step_projectiles 弹道步的扫掠
                // 逻辑，通用化为一切有速度实体——高速弹丸造物/高速跳跃不再隧穿）：
                // 起点→中点位移终点的线段扫过其他身体或地形面时，截断位移到
                // 首触点 + 速度清零（物理层"撞上即停"；规则层投递交给接触通道
                // ——截断点=恰好相切，detect_touching 判接触 → on_contact 通知
                // → 代码自攻（#045 替代链定案：命中=接触通道代码攻）。多目标取
                // 线段参数 t 最小者（最先碰到），与触地 t 统一定序；判据纯几何
                //（#044 子步宪法：只读位置/速度/身体，同状态必然同结算）。
                // 【积分器双轨注记】本路径保留中点位移（历史锚：跳跃
                // thrust_ballistic_jump_peak_within_three_grid_anchor，v0=7.7
                // 起跳 2.85 格）——与退役前的弹丸半隐式欧拉（重力后速度直接
                // 位移）数字上差半拍；D-018 后半隐式轨随弹丸专属管线一并退役，
                // 弹道手感锚（H 抛射/操控解算器镜像）已按中点口径换算重标
                //（见 bin solve_launch_vector 注释）。
                let swept_body = self.swept_first_body_contact(world, id, before, after, me_r);
                let swept_ground = world.terrain.segment_first_contact(before, after, me_r);
                let truncated = match (swept_body, swept_ground) {
                    (Some((_, bt)), Some(gt)) => {
                        if bt <= gt {
                            Some(false) // 先撞体
                        } else {
                            Some(true) // 先触地
                        }
                    }
                    (Some(_), None) => Some(false),
                    (None, Some(_)) => Some(true),
                    (None, None) => None,
                };
                if let Some(hit_ground) = truncated {
                    if hit_ground {
                        // 触地截断：落点=路径与地形面的交点（弹心贴面）。D-010 碰撞
                        // 响应：撞=动量交换——法向反弹（e=组合(实体,地面)）+切向摩擦
                        //（组合后使用）；位置停点逻辑完全不动（弹开发生在下一拍的
                        // 速度里——拍制世界的诚实形态：本拍停住，下拍弹走）。
                        let gt = swept_ground.expect("hit_ground=true 时必有触地 t");
                        let land_x = before.0 + (after.0 - before.0) * gt;
                        let land = (land_x, world.terrain.height(land_x) + me_r);
                        // n=地形面法线（D-010：触地=地形面法线，朝上）
                        let n = world.terrain.height_normal(land_x);
                        let e = combine_restitution(me_e, self.params.ground_restitution);
                        let f = combine_friction(me_f, self.params.ground_friction);
                        // 低速闸：地面静止，相对法向速度=入射法向分量
                        let v_n = vel.0 * n.0 + vel.1 * n.1;
                        let e_eff = if v_n.abs() < RESTITUTION_LOW_SPEED { 0.0 } else { e };
                        let bounced = bounce_response(vel, n, e_eff, f);
                        if e_eff > 0.0 {
                            tracing::info!(
                                entity_id = %id.0,
                                ground = true,
                                e = e_eff,
                                f,
                                v_n,
                                to_vel = ?bounced,
                                "[bounce] 触地反弹（D-010：法向反弹+切向摩擦，位移下拍兑现）"
                            );
                        } else {
                            tracing::debug!(
                                entity_id = %id.0,
                                ground = true,
                                e = e_eff,
                                f,
                                "[step] CCD 触地截断（低速闸/零弹性：法向清零+切向摩擦）"
                            );
                        }
                        (land, Some(bounced))
                    } else {
                        // 撞体截断：位移停在首触点（恰好相切），接触交接触通道结算
                        //（on_contact 代码攻）。D-010 碰撞响应：撞=动量交换——撞者
                        // 速度法向反弹+切向摩擦；【单写者原则】只改自己的速度，不动
                        // 目标——目标在自己拍的 CCD 里处理自己的碰撞（实体按 id 升序
                        // 定序，每个动体一轮冲量路径；本步代写目标速度会双写冲突）。
                        // t 推进 1cm 等效量：首触点数学上=恰相切（距目标圆心=r 和），
                        // 但浮点下停点恰在 detect_touching 的 ≤ 边界上（实测恰等
                        // 0.6500 时判不到——边界值浮点向上漂 1 ULP 即漏）。沿路径
                        // 向前推进 1cm，停点落在窗口内（接触语义内的真实重叠，视觉
                        // 不可感知），接触事实当拍成立。
                        let (tid, bt) = swept_body.expect("hit_ground=false 时必有撞体 t");
                        let disp = ((after.0 - before.0).powi(2) + (after.1 - before.1).powi(2))
                            .sqrt();
                        // 上限 1.0 + 1e-2/disp（D-018b 端点相切，P2-7）：首触恰为
                        // 线段末端（bt∈[1-1e-9,1]）时旧上限 1.0 把推进量吞掉——
                        // 停点=末端=数学恰切点，浮点距离漂到 0.65 之上即漏接触
                        //（实测 1 ULP 即发生，锚测试
                        // ccd_endpoint_tangency_advances_into_contact_window）。
                        // 1+ε 处线性外推 = 末端再沿位移方向 1cm（与中途首触的
                        // 推进语义同族）；首触在中途时 min 仍取 bt+ε 不受影响
                        let bt = if disp > 1e-12 {
                            (bt + 1e-2 / disp).min(1.0 + 1e-2 / disp)
                        } else {
                            bt
                        };
                        let stop = (
                            before.0 + (after.0 - before.0) * bt,
                            before.1 + (after.1 - before.1) * bt,
                        );
                        // D-010 撞体反弹：n=两圆心连线单位向量（MVP 口径——AABB 造物
                        // 的圆心=pos，面法线级反弹归 D-008 形状物理包）；组合=Box2D 款
                        let (t_e, t_f, n) = match world.entities.get(&tid) {
                            Some(t) => {
                                let (tx, ty) = t.pos();
                                let dx = tx - before.0;
                                let dy = ty - before.1;
                                let dlen = (dx * dx + dy * dy).sqrt();
                                let n = if dlen > 1e-12 {
                                    (dx / dlen, dy / dlen)
                                } else {
                                    (0.0, 1.0)
                                };
                                (t.restitution(), t.friction(), n)
                            }
                            None => (0.0, 0.6, (0.0, 1.0)), // 目标瞬失兜底（不可能路径）
                        };
                        // 每对每拍一弹闸（D-010 细则⑤）：同拍内同一对实体只结算一次
                        // 接触——已弹过的对按非弹性（e 生效=0，摩擦照常）。本拍先到者
                        // （id 升序定序）吃弹性，后到者非弹性清零；防将来的多次接触源
                        let pair = if id.0 < tid.0 { (id, tid) } else { (tid, id) };
                        let pair_hit = !bounced_pairs.insert(pair);
                        let e = if pair_hit {
                            0.0
                        } else {
                            combine_restitution(me_e, t_e)
                        };
                        let f = combine_friction(me_f, t_f);
                        // 低速闸：相对法向速度（目标速度参与——相向/同向影响入射强度；
                        // 目标速度取结算时刻实况，#044：定序已锁 id 升序，同状态同结算）
                        let (tvx, tvy) = world
                            .entities
                            .get(&tid)
                            .map(|t| t.velocity())
                            .unwrap_or((0.0, 0.0));
                        let rel_n = (vel.0 - tvx) * n.0 + (vel.1 - tvy) * n.1;
                        let e_eff = if rel_n.abs() < RESTITUTION_LOW_SPEED { 0.0 } else { e };
                        let bounced = bounce_response(vel, n, e_eff, f);
                        if e_eff > 0.0 {
                            tracing::info!(
                                entity_id = %id.0,
                                target_id = %tid.0,
                                e = e_eff,
                                f,
                                rel_n,
                                to_vel = ?bounced,
                                "[bounce] 撞体反弹（D-010：只改自己速度，目标下拍自理；位移下拍兑现）"
                            );
                        } else {
                            tracing::debug!(
                                entity_id = %id.0,
                                target = %tid.0,
                                pos = ?stop,
                                e = e_eff,
                                f,
                                "[step] CCD 撞体截断（低速闸/零弹性/同对已弹：法向清零+切向摩擦，接触交接触通道结算）"
                            );
                        }
                        (stop, Some(bounced))
                    }
                } else {
                    // 落地解析（脚下高度核复用 circle_landing_y 的几何）：位移后不高于
                    // 脚下高度即落地——贴面 + D-010 语义升级：法向清零+切向×(1-f)
                    //（旧口径速度全清零是因为"落地=贴面"；摩擦的正确语义=滑动衰减，
                    // 默认 f=0.6 时落地水平分量×0.4 逐拍衰减，贴地滑行数拍内自停）。
                    // n=地形面法线（落地解析落点即贴面点）；e=组合(实体,地面)，
                    // 低速闸天然把慢速落地吞成非弹性（v_n<1 时 e=0=现状清零语义）。
                    let surface = self.circle_landing_surface(world, id, after.0, me_r, after.1);
                    if after.1 <= surface {
                        let n = world.terrain.height_normal(after.0);
                        let e = combine_restitution(me_e, self.params.ground_restitution);
                        let f = combine_friction(me_f, self.params.ground_friction);
                        let v_n = vel.0 * n.0 + vel.1 * n.1;
                        let e_eff = if v_n.abs() < RESTITUTION_LOW_SPEED { 0.0 } else { e };
                        let bounced = bounce_response(vel, n, e_eff, f);
                        if e_eff > 0.0 {
                            tracing::info!(
                                entity_id = %id.0,
                                ground = true,
                                e = e_eff,
                                f,
                                v_n,
                                to_vel = ?bounced,
                                "[bounce] 落地反弹（D-010：法向反弹+切向摩擦）"
                            );
                        } else {
                            tracing::debug!(
                                entity_id = %id.0,
                                from = ?before,
                                to = ?(after.0, surface),
                                e = e_eff,
                                f,
                                "[step] 冲量路径落地贴面（低速闸/零弹性：法向清零+切向摩擦）"
                            );
                        }
                        ((after.0, surface), Some(bounced))
                    } else {
                        (after, Some(vel))
                    }
                }
            } else {
                // 生物 + dynamic 造物（D-003 全活）：零速走落点制（现状管线）
                let ly = self.circle_landing_y(world, id, before.0, me_r, before.1, eff_gravity);
                ((before.0, ly), None)
            };
            // D-003 dynamic 造物速度矢量定律（v≡0 时零效果）：阻力衰减 + 世界极速
            // 钳制（#045 口径）。注意：带速度的 dynamic 造物上面已走冲量路径（定律
            // 在路径内统一结算），此处只处理零速残留（恒等变换，行为零变化）
            if is_dyn_creation && ballistic_vel.is_none() {
                let mut vel = apply_drag(me_vel, me_mass, eff_drag);
                let speed2 = vel.0 * vel.0 + vel.1 * vel.1;
                let max_speed = self.params.max_speed;
                if speed2 > max_speed * max_speed {
                    let scale = max_speed / speed2.sqrt();
                    tracing::debug!(
                        entity_id = %id.0,
                        from_speed = speed2.sqrt(),
                        to_speed = max_speed,
                        "[step] dynamic 造物世界极速钳制"
                    );
                    vel = (vel.0 * scale, vel.1 * scale);
                }
                if vel != me_vel {
                    if let Some(entity) = world.entities.get_mut(&id) {
                        entity.set_velocity(vel);
                    }
                }
            }
            if new_pos != before {
                if let Some(entity) = world.entities.get_mut(&id) {
                    entity.set_pos(new_pos);
                }
                tracing::debug!(entity_id = %id.0, from = ?before, to = ?new_pos, "重力落点应用");
                changes.push(PhysicsChange::new(id, new_pos, PhysicsReason::Gravity, None));
            }
            // D-010a 冲量路径速度写回（空中=定律结算后的速度；落地=清零）
            if let Some(bvel) = ballistic_vel {
                if let Some(entity) = world.entities.get_mut(&id) {
                    entity.set_velocity(bvel);
                }
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
            // D-010：地面常数对继承默认（e=0/f=0.6——本测试只断言 gravity/
            // max_velocity，地面常数不被覆盖即与默认逐位一致）
            ..DefaultPhysics::default_values()
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
        engine.step(&mut world, &HashSet::new(), &[]);
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
        engine.step(&mut world, &HashSet::new(), &[]);
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
        engine.step(&mut world, &HashSet::new(), &[]);
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
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(world.entities.get(&id_h).unwrap().pos(), (10.0, 1.5), "D 在脚下时 H 稳定");
        let mut d = world.entities.remove(&id_d).unwrap();
        d.set_pos((20.0, 0.5)); // D 挪走
        world.entities.insert(id_d, d);
        engine.step(&mut world, &HashSet::new(), &[]);
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
        engine.step(&mut world, &HashSet::new(), &[]);
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
        engine.step(&mut world, &HashSet::new(), &[]);
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
        engine.step(&mut world, &aloft, &[]);
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
        engine.step(&mut world, &HashSet::new(), &[]);
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
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(world.entities.get(&id_t).unwrap().pos(), (10.0, 5.0), "落点重叠悬停原位");
    }

    // === D-003 motion 出生声明（dynamic 造物=圆物理管线）===

    /// dynamic 造物构造器：code_hash=creation + motion=1
    fn make_dyn_creation(id_u: u128, x: f64, y: f64) -> Entity {
        let mut e = make_creation(id_u, x, y);
        e.set_motion(codeworld_core::entity::MOTION_DYNAMIC);
        e
    }

    #[test]
    fn step_dynamic_creation_falls_to_radius_line() {
        // D-003 全活造物：空中出生走圆物理管线（circle_landing_y 一拍到底），
        // 落到地面=半径线 y=radius（脚贴地）——与生物同款，不是 static 的 y=0 落地
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let t = make_dyn_creation(1, 10.0, 5.0);
        assert!(codeworld_core::collision::is_dynamic_creation(&t));
        let id = t.id;
        world.entities.insert(id, t);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(
            world.entities.get(&id).unwrap().pos(),
            (10.0, 0.5),
            "dynamic 造物一拍落到半径线（生物同款管线）"
        );
    }

    #[test]
    fn step_dynamic_creation_stacks_on_static_tower_top() {
        // dynamic 造物可落在 static 塔顶（圆-AABB 落点面），物理涌现"造物当桥"
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let tower = make_creation(1, 10.0, 0.0); // AABB [10,11]×[0,1]
        let ball = make_dyn_creation(2, 10.5, 5.0); // r=0.5，正上方
        let id_t = tower.id;
        let id_b = ball.id;
        world.entities.insert(id_t, tower);
        world.entities.insert(id_b, ball);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(
            world.entities.get(&id_b).unwrap().pos(),
            (10.5, 1.5),
            "dynamic 造物落在塔顶（盒顶 1.0 + 半径 0.5）"
        );
    }

    #[test]
    fn step_dynamic_creation_velocity_drag_and_speed_limit() {
        // D-003 速度矢量定律 + D-010a 速度积分（语义演进：本测试锚定速度字段从
        // "只存不动"演进为"积分进位置"——速度积分进位置原待 D-010，现落地）：
        // v≠0 走冲量路径——重力入速度 → 阻力衰减（v=10 衰 5% → x 分量 9.5，
        // mass=1）→ 世界极速钳制（60 → 25 归一化）→ 中点位移进位置。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut a = make_dyn_creation(1, 0.0, 100.0); // 高空，落地解析不干扰断言
        a.set_velocity((10.0, 0.0));
        let mut b = make_dyn_creation(2, 50.0, 100.0);
        b.set_velocity((60.0, 0.0));
        let id_a = a.id;
        let id_b = b.id;
        world.entities.insert(id_a, a);
        world.entities.insert(id_b, b);
        engine.step(&mut world, &HashSet::new(), &[]);
        let (avx, avy) = world.entities.get(&id_a).unwrap().velocity();
        assert!(
            (avx - 9.5).abs() < 1e-9,
            "阻力定律 v=10 衰 5% → x 分量 9.5（mass=1），实际 {avx}"
        );
        assert!(
            (avy - (0.0 - 9.8) * 0.95).abs() < 1e-9,
            "重力先入速度再吃阻力 → vy=(0-9.8)×0.95，实际 {avy}"
        );
        assert!(
            (world.entities.get(&id_a).unwrap().pos().0 - 9.75).abs() < 1e-9,
            "中点位移应进位置（x 位移 = (10+9.5)/2 = 9.75），实际 {}",
            world.entities.get(&id_a).unwrap().pos().0
        );
        let (bx, by) = world.entities.get(&id_b).unwrap().velocity();
        let b_speed = (bx * bx + by * by).sqrt();
        assert!(
            (b_speed - 25.0).abs() < 1e-9,
            "超极速 60 应归一化压回 25，实际 {b_speed}"
        );
    }

    #[test]
    fn step_static_creation_velocity_untouched_zero_regression() {
        // 零回归锚：static 造物无速度矢量物理（速度字段原样不动），落地仍 y=0
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut t = make_creation(1, 10.0, 5.0);
        t.set_velocity((99.0, 0.0)); // 即使被硬塞速度，static 造物也不吃物理
        let id = t.id;
        world.entities.insert(id, t);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(
            world.entities.get(&id).unwrap().pos(),
            (10.0, 0.0),
            "static 造物仍直落地面 y=0（现状不变）"
        );
        assert_eq!(
            world.entities.get(&id).unwrap().velocity(),
            (99.0, 0.0),
            "static 造物速度字段不被 step 触碰"
        );
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
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 0.5), "id 小者落地");
        assert!(
            (world.entities.get(&id2).unwrap().pos().1 - 2.2).abs() < 1e-9,
            "e2 自由落到 e1 上方（浮点容差断言）"
        );
        engine.step(&mut world, &HashSet::new(), &[]);
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
        let changes = engine.step(&mut world, &HashSet::new(), &[]);
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
        let changes = engine.step(&mut world, &HashSet::new(), &[]);
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
        let changes = engine.step(&mut world, &HashSet::new(), &[]);
        assert!(
            changes.iter().any(|c| c.reason == PhysicsReason::Gravity),
            "应产生重力变更"
        );
        assert_eq!(world.entities.get(&id).unwrap().pos(), (5.0, 90.2), "单拍 -9.8");
        for _ in 0..20 {
            engine.step(&mut world, &HashSet::new(), &[]);
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

    // === D-018 通用弹道：一切有速度实体的物理（#045 弹丸退役后的通用化管线）===

    /// 末态快照元组（#044 对照锚用：id/坐标/速度/存亡）——复杂类型别名
    type ShellFin = (EntityId, (f64, f64), (f64, f64), bool);
    //
    // 退役前的 step_projectiles（弹丸专属：半隐式欧拉 + CCD 扫掠 + 寿命超时）整段
    // 并入 step 的冲量路径（重力→阻力→极速钳→中点位移→CCD 扫掠截断→落地清零）。
    // 本区测试全部以"dynamic 造物 + 出生初速"同态构造（原 push 弹丸构造 → 造物
    // set_velocity），断言语义保留、数字按中点口径重标（积分器双轨注记见 step：
    // 半隐式轨随弹丸专属管线退役；中点位移 = 匀加速一拍的解析解，弹道手感锚已
    // 按中点口径换算重标——bin solve_launch_vector 镜像同步）。

    use codeworld_core::collision::PROJECTILE_CODE;

    /// 构造旧弹丸实体（历史兼容测试用：D-018 后世界不再产生 PROJECTILE_CODE
    /// 实体——仅历史快照反序列化兼容且恢复即清场，运行时无活旧弹丸；本构造器
    /// 仅供测试复现历史形态，服务 find_nearest 排除/step 通用路径兼容锚）
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

    /// 构造带初速的 dynamic 造物弹（D-018 造物弹时代的标准构造：spawn 第 10/11/12
    /// 参落库后的世界形态——code_hash=creation + motion=1 + radius/mass/velocity）
    fn make_shell(id_u: u128, pos: (f64, f64), vel: (f64, f64), radius: f64, mass: f64) -> Entity {
        let mut e = make_dyn_creation(id_u, pos.0, pos.1);
        e.set_radius(radius);
        e.set_mass(mass);
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
    fn ballistic_shell_parabola_two_ticks() {
        // 造物弹抛物线（原弹丸测试同态改写，数字按中点口径重标）：
        // 固定初速 (4, 9.8) mass 1.0 @ (0,10)：重力→阻力→极速钳→中点位移。
        // tick1: vy=9.8-9.8=0 -> 阻力 x0.95 -> v=(3.8,0)；中点位移=((4+3.8)/2,(9.8+0)/2)
        //        -> pos=(3.9,14.9)
        // tick2: vy=-9.8 -> x0.95 -> v=(3.61,-9.31)；中点位移+=(3.705,-4.655)
        //        -> pos=(7.605,10.245) 仍在飞
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_shell(1, (0.0, 10.0), (4.0, 9.8), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert!(near(world.entities.get(&pid).unwrap().pos(), (3.9, 14.9)));
        engine.step(&mut world, &HashSet::new(), &[]);
        assert!(near(world.entities.get(&pid).unwrap().pos(), (7.605, 10.245)));
    }

    #[test]
    fn ballistic_shell_lands_and_stops() {
        // 造物弹落地 = CCD 触地截断（落点贴地形面 ground+r）。
        // 【D-010 语义升级】落地速度语义从"全清零"升级为"法向清零+切向×(1-f)"
        //（默认 f=0.6：切向=水平分量 ×0.4——摩擦的正确语义，旧全清零是因为
        // "落地=贴面"时代没有摩擦概念）。与退役弹丸的"Landed 事件+消亡"不同：
        // 造物弹是造物，落地停驻是物理事实，消亡归寿命自管（代码自杀/燃料尽）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_shell(1, (3.0, 0.2), (1.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(
            (shell.pos().1 - 0.15).abs() < 1e-9,
            "落点应贴地面半径线 y=0.15，实际 {shell:?}"
        );
        // D-010：积分后 vx=0.95（阻力后）→切向×(1-0.6)=0.38；vy 法向清零
        assert!(
            (shell.velocity().0 - 0.38).abs() < 1e-9 && shell.velocity().1 == 0.0,
            "触地截断=法向清零+切向×0.4（D-010 摩擦语义），实际 {:?}",
            shell.velocity()
        );
        assert!(shell.is_alive(), "造物弹落地停驻不消亡（寿命自管）");
    }

    #[test]
    fn ballistic_shell_hits_body_stops_without_harming() {
        // 造物弹 CCD 撞体 = 位移截断到首触点 + 速度清零 + 存活；目标不动
        //（物理步不动目标身体，结算归接触通道 on_contact 代码攻——#045 替代链）。
        // 场景：贴地平射小弹 (0,0.65) vel(20,4)，站地稳定目标 (10,0.5)（vel=0
        // 落点制恰在半径线不移动）——一拍路径扫过目标，撞体先于触地
        //（t≈0.48 < 0.66），截断点距目标圆心恰在接触窗口 0.65 上
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let target = make_entity(2, 10.0, 0.5, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_shell(1, (0.0, 0.65), (20.0, 4.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        let (tx, ty) = world.entities.get(&tid).unwrap().pos();
        let dx = shell.pos().0 - tx;
        let dy = shell.pos().1 - ty;
        let dist = (dx * dx + dy * dy).sqrt();
        assert!(
            dist <= 0.65 + 1e-9,
            "撞体截断应停在目标接触窗口（0.65）内，实际距离 {dist}"
        );
        assert!(
            shell.pos().1 > 0.15,
            "撞体截断先于触地（未落地面线），实际 {:?}",
            shell.pos()
        );
        // 【D-010 语义升级】撞体截断速度从"全清零"升级为"法向清零+切向×(1-f)"
        //（默认 e=0/f=0.6 组合=非弹性+摩擦）：斜入射（vel(20,4)）在**入射连线
        // 法向 n**（=目标圆心-弹起点，反弹分解用的几何）上的分量清零，切向分量
        // ×0.4 保留——单写者原则：只改撞者，目标自理
        {
            let (bx, by) = (0.0, 0.65); // 弹起点（n_impl 的入射几何，非截断后停点）
            let dx = tx - bx;
            let dy = ty - by;
            let dl = (dx * dx + dy * dy).sqrt();
            let (nx, ny) = (dx / dl, dy / dl);
            let (vx, vy) = shell.velocity();
            let v_n = vx * nx + vy * ny;
            assert!(
                v_n.abs() < 1e-6,
                "撞体截断法向分量应清零（D-010 e=0 组合），实际 {v_n}"
            );
            let speed = (vx * vx + vy * vy).sqrt();
            assert!(
                speed > 0.0,
                "撞体截断切向应剩摩擦衰减分量（D-010 ×0.4），实际 {speed}"
            );
        }
        assert!(shell.is_alive(), "造物弹撞体停驻不消亡（结算归接触通道）");
        assert!(world.entities.get(&tid).unwrap().is_alive(), "物理步不动目标");
    }

    #[test]
    fn ccd_endpoint_tangency_advances_into_contact_window() {
        // P2-7 端点相切缺口（D-018b）：首触点恰为线段末端（bt∈[1-1e-9,1]）时，
        // 旧实现 (bt + 1e-2/disp).min(1.0) 把推进量钳回 1.0——停点=末端，悬在
        // detect_touching 的 ≤ 边界（0.65）上靠浮点运气（边界值向上漂 1 ULP
        // 即漏接触）。修法：上限放宽到 1.0 + 1e-2/disp——bt 参数化在 1+ε 处
        // 线性外推=末端+沿位移方向 1cm，与中途首触的推进语义同族。
        // 固定坐标（算死禁随机，ULP 实测定标）：重力 0 引擎、目标圆心 (10,0.5)
        // r 和 0.65、弹中点位移 9.75（初速 10 → 阻力后 9.5）。端点取 9.35 下方
        // 1 ULP（ulp(9.35)=2^-49）= 9.349999999999998：数学上端点已在接触圆内
        //（swept 找到首触 bt≈1），但浮点下停点距离=0.6500000000000002 > 0.65
        // ——旧实现 (bt+1e-2/disp).min(1.0) 把推进量钳回 1.0，停点=端点，
        // detect_touching 的 ≤ 边界判不到接触（漏接触，实测探针 k=-1 档）
        let engine = DefaultPhysicsEngine::new_with_params(codeworld_core::physics::DefaultPhysics {
            gravity: 0.0,
            ground_y: 0.0,
            collision_enabled: true,
            max_velocity: 100.0,
            drag_coefficient: 0.05,
            max_speed: 25.0,
            ..codeworld_core::physics::DefaultPhysics::default_values() // D-010 地面常数继承默认
        });
        let mut world = WorldState::new();
        let target = make_entity(2, 10.0, 0.5, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let endpoint_x = 9.349999999999998_f64; // 恰切点 9.35 下方 1 ULP
        let before_x = endpoint_x - 9.75;
        let p = make_shell(1, (before_x, 0.5), (10.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        let (tx, ty) = world.entities.get(&tid).unwrap().pos();
        let dx = shell.pos().0 - tx;
        let dy = shell.pos().1 - ty;
        let dist = (dx * dx + dy * dy).sqrt();
        assert!(
            dist <= 0.65,
            "端点相切也必须推进落进接触窗口（≤0.65），实际 {dist}"
        );
        assert_eq!(shell.velocity(), (0.0, 0.0), "撞体截断速度清零");
        assert!(shell.is_alive(), "停驻存活");
        // 接触通道 payoff：停点落进窗口 → detect_touching 判接触成立
        let pairs = engine.detect_touching(&world.entities);
        assert!(
            pairs
                .iter()
                .any(|pr| (pr.a == pid && pr.b == tid) || (pr.a == tid && pr.b == pid)),
            "停点应被 detect_touching 判为接触，实际 {pairs:?}"
        );
    }

    #[test]
    fn ballistic_shell_muzzle_clearance_no_self_stop() {
        // 发射点外推出膛不撞发射者（投掷器模板第一课的教学锚）：出生点沿发射
        // 方向前移 (r_self + r_bullet + 0.1)（发射者 r0.5 + 弹 r0.15 + 余量 0.1
        // = 外推 0.75），首拍路径向外飞离——CCD 不与发射者截断（旧弹丸的 shooter
        // 豁免退役：造物弹无特权，外推就是全部答案）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let shooter = make_entity(9, 10.0, 10.0, 0);
        let sid = shooter.id;
        world.entities.insert(sid, shooter);
        let p = make_shell(1, (10.75, 10.0), (2.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(
            shell.pos().0 > 10.75,
            "出膛即飞离发射者（无自截断），实际 {:?}",
            shell.pos()
        );
        assert!(shell.is_alive(), "出膛弹存活");
    }

    #[test]
    fn ballistic_shell_range_calibration_default_drag() {
        // 射程调参判据（阻力默认值的标定锚，中点口径重标）：默认参数（drag=0.05）
        // 下，标准上抛造物弹（vx=4, vy=+9.8, mass=1.0 地面发射，r=0.15 小弹）
        // 水平射程。退役前弹丸半隐式口径 ≈3.9 格；中点位移（=匀加速解析解，
        // 重力响应提前半拍）口径实测 ≈7.89 格——锚区间 [7.0, 8.5]。
        // 手算（factor 0.95）：t1 位移 (3.9,4.9)；t2 位移 (3.705,-4.655)
        // -> pos=(7.605,1.245)；t3 vy=(-9.31-9.8)*0.95=-18.1545，路径触地
        // t=(1.245-0.15)/13.732=0.0798，落点 x=7.605+3.51975*0.0798≈7.886
        // 注：起点 y=0.5（r=0.15 不贴地面线 0.15）——贴地出生（y=ground+r）会
        // 被触地查表的 A=0 判 t=0 立即截断（地形 CCD 的"起步即在地面"口径）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let x0 = 50.0; // 远离边界，排除 viewport 干扰
        let p = make_shell(1, (x0, 0.5), (4.0, 9.8), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let mut range = None;
        for _ in 0..60 {
            engine.step(&mut world, &HashSet::new(), &[]);
            let shell = world.entities.get(&pid).unwrap();
            // D-010 摩擦语义：落地后切向速度 ×(1-f) 逐拍衰减（0.4^n），f64 精确
            // 零判定永不成立（0.4^n 无精确零点）——停驻判定改近似阈值（模长<1e-9
            // 即物理停驻；60 拍内 0.4^20≈1e-8 量级，判据可达）
            let (vx, vy) = shell.velocity();
            if vx * vx + vy * vy < 1e-18 {
                range = Some(shell.pos().0 - x0);
                break;
            }
        }
        let range = range.expect("60 拍内必落地");
        assert!(
            (7.0..=8.5).contains(&range),
            "标准上抛造物弹射程应 7~8.5 格（中点口径，精确 ≈7.89），实际 {range}"
        );
    }

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    // === 世界极速·飞行中钳制（#034 修订一，双审 P1-A 修复；D-018 通用化到造物弹）===

    fn zero_drag_engine() -> DefaultPhysicsEngine {
        // 无阻力引擎：隔离重力累积效应（阻力会抵消重力，掩盖钳制是否生效）
        DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        })
    }

    #[test]
    fn shell_flight_speed_capped_at_world_max_speed() {
        // P1-A 复现锚（造物弹版）：mass=100 高速下坠（阻力 0 可忽略），重力逐拍
        // 累积；速度模长封顶于世界极速 25。初速非零（vel=0 的实体走落点制位置
        // 直落、速度字段不动——那是另一条管线，见
        // ballistic_zero_velocity_entities_keep_positional_fall_regime）
        let engine = zero_drag_engine();
        let mut world = WorldState::new();
        let p = make_shell(1, (50.0, 5000.0), (0.0, -20.0), 0.15, 100.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let mut peak = 0.0f64;
        for _ in 0..29 {
            engine.step(&mut world, &HashSet::new(), &[]);
            let (vx, vy) = world.entities.get(&pid).unwrap().velocity();
            let speed = (vx * vx + vy * vy).sqrt();
            peak = peak.max(speed);
            assert!(
                speed <= 25.0 + 1e-9,
                "速度应封顶世界极速 25，实际 {speed}"
            );
        }
        assert!(
            peak > 24.0,
            "重力应把速度推到接近极速（证明钳制真实在工作而非速度没涨），峰值 {peak}"
        );
    }

    #[test]
    fn shell_flight_displacement_per_tick_capped() {
        // 钳制在位移之前（重力+阻力后、位移前）：一拍位移不超过极速——
        // 视觉可读性常数的世界保证：任何飞行物一拍最多走 25 格
        let engine = zero_drag_engine();
        let mut world = WorldState::new();
        let p = make_shell(1, (50.0, 5000.0), (0.0, -20.0), 0.15, 100.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        for _ in 0..29 {
            let before = world.entities.get(&pid).unwrap().pos();
            engine.step(&mut world, &HashSet::new(), &[]);
            let after = world.entities.get(&pid).unwrap().pos();
            let moved = ((after.0 - before.0).powi(2) + (after.1 - before.1).powi(2)).sqrt();
            assert!(
                moved <= 25.0 + 1e-9,
                "单拍位移应不超过极速 25，实际 {moved}"
            );
        }
    }

    #[test]
    fn shell_at_exact_max_speed_untouched() {
        // 边界：速度恰等于极速不钳（严格 > 判据）：无重力无阻力下 (25,0) 分毫不动
        let engine = DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            gravity: 0.0,
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        });
        let mut world = WorldState::new();
        let p = make_shell(1, (0.0, 10.0), (25.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(
            world.entities.get(&pid).unwrap().velocity(),
            (25.0, 0.0),
            "恰等于极速的速度不应被改动"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (25.0, 10.0),
            "一拍位移恰为极速 25 格（中点=v0 因速度无变化）"
        );
    }

    // === CCD 路径检查（D-018 通用化：一切有速度实体的线段扫掠截断）===

    fn straight_engine() -> DefaultPhysicsEngine {
        // 直线弹道引擎（零重力零阻力）：CCD 几何断言不受抛物线干扰
        DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            gravity: 0.0,
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        })
    }

    #[test]
    fn ccd_sweep_truncates_midway_target_point_check_would_miss() {
        // CCD 锚定用例（原弹丸测试同态改写）：极速内高速弹一拍跨 20 格、目标在
        // 路径中点——位移截断停在首触点（旧点检查会隧穿；CCD 纯几何必拦）
        // 截断点 = (0 + 20*(10-0.65)/20, 20) = (9.35, 20)
        let engine = straight_engine();
        let mut world = WorldState::new();
        let target = make_entity(2, 10.0, 20.0, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_shell(1, (0.0, 20.0), (20.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(
            near(shell.pos(), (9.36, 20.0)),
            "应停在首触点 (9.36,20)（含 1cm 推进），实际 {:?}",
            shell.pos()
        );
        assert_eq!(shell.velocity(), (0.0, 0.0), "撞体截断速度清零");
        assert!(shell.is_alive(), "造物弹截断停驻不消亡");
        assert!(world.entities.get(&tid).unwrap().is_alive(), "物理步不动目标");
    }

    #[test]
    fn ccd_sweep_multi_target_first_contact_wins() {
        // 多目标取线段参数 t 最小者（最先碰到）：路径上 x=5 与 x=15 两个目标 →
        // 截断在近目标首触点 (5-0.65+0.01, 20) = (4.36, 20)（含 1cm 推进）
        let engine = straight_engine();
        let mut world = WorldState::new();
        let far = make_entity(2, 15.0, 20.0, 0);
        let near_target = make_entity(3, 5.0, 20.0, 0);
        let fid = far.id;
        let nid = near_target.id;
        world.entities.insert(fid, far);
        world.entities.insert(nid, near_target);
        let p = make_shell(1, (0.0, 20.0), (20.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(
            near(shell.pos(), (4.36, 20.0)),
            "应停在最先碰到的（t 最小）目标首触点，实际 {:?}",
            shell.pos()
        );
        let _ = nid;
    }

    #[test]
    fn ccd_landing_truncates_at_ground_crossing() {
        // 落地=路径与地面线的交点（CCD 触地截断，原弹丸同语义迁移）：
        // (0,1) 直线 vel(20,-2) 一拍末 (20,-1)，地面交点 t=(1-0.15)/2=0.425
        // → 落点 (8.5, 0.15)。旧"穿地后拉回拍末 x"口径在退役前已修，此处锚定
        // 通用路径延续该口径
        let engine = straight_engine();
        let mut world = WorldState::new();
        let p = make_shell(1, (0.0, 1.0), (20.0, -2.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(near(shell.pos(), (8.5, 0.15)), "落点应为地面交点 (8.5,0.15)，实际 {:?}", shell.pos());
        // 【D-010 语义升级】触地截断：法向清零（e=0 默认组合，|v_n|=2>1 过低速闸
        // 但零弹性不反弹）+切向×(1-0.6)（vx 20→8）——摩擦语义非全清零
        assert!(
            near(shell.velocity(), (8.0, 0.0)),
            "触地截断=法向清零+切向×0.4（D-010），实际 {:?}",
            shell.velocity()
        );
        assert!(shell.is_alive(), "造物弹落地停驻（尸体坐标=落点，异态不留世界）");
    }

    #[test]
    fn ccd_body_hit_before_ground_wins_by_contact_t() {
        // 同拍路径既穿身体又触地：按 t 定序（真实先后，不再靠检查顺序）。
        // 弹 (0,0.5) vel(10,0)（同高平射悬空站桩目标 (1,0.5)——vel=0 落点制
        // 恰在半径线稳定不动）：地面 t=0.0752；身体 (1,0.5) r0.5 首触 t=0.0385
        // → 撞体先：停点距目标圆心恰在接触窗口 0.65 上且未到地面
        let engine = straight_engine();
        let mut world = WorldState::new();
        let target = make_entity(2, 1.0, 0.5, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_shell(1, (0.0, 0.5), (10.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        let (tx, ty) = world.entities.get(&tid).unwrap().pos();
        let dist = ((shell.pos().0 - tx).powi(2) + (shell.pos().1 - ty).powi(2)).sqrt();
        assert!(
            dist <= 0.65 + 1e-9,
            "应撞体截断（先触身体按 t 定序），实际距目标 {dist}"
        );
        assert!(
            shell.pos().1 > 0.15,
            "撞体截断先于触地（未落地面线），实际 {:?}",
            shell.pos()
        );
        assert_eq!(shell.velocity(), (0.0, 0.0), "截断速度清零");
    }

    // ===== #044 子步宪法：确定性测试锚（D-015 法典化；D-018 通用化到 step 冲量路径）=====
    //
    // 宪法要求（decisions #044）：N=世界状态的纯函数——同状态必然同 N 同结算；
    // 红线=绝不用墙钟/CPU/帧率选 N。CCD 扫掠并入 step 冲量路径后，锚形态不变：
    // 同状态两跑 step 末态逐位一致。

    /// 第一拍末态快照（确定性自检用：id + 坐标 + 速度）
    type MidState = Vec<(u128, (f64, f64), (f64, f64))>;

    #[test]
    fn ballistic_step_same_state_same_result() {
        // #044 纯函数锚：同一初始状态（实体集合+速度+造物弹 CCD 截断混合场景）
        // 跑两遍 step——末态（坐标/速度/存亡）与物理变更序列必须逐位一致。
        // 场景：一条平射造物弹撞中途目标（CCD 撞体截断）+ 一条上抛造物弹落地
        //（CCD 触地截断），覆盖两个截断分支。
        let engine = DefaultPhysicsEngine::new();
        let run_world = || -> (Vec<_>, WorldState, MidState) {
            let mut world = WorldState::new();
            for (id, pos, vel) in [
                (1u128, (0.0, 20.0), (12.0, 0.0)),  // 平射→路径扫过 (5.85,17.67) 目标
                (2u128, (30.0, 1.0), (3.0, 6.0)),   // 上抛→t2 触地截断
            ] {
                let s = make_shell(id, pos, vel, 0.15, 1.0);
                world.entities.insert(s.id, s);
            }
            for (id, pos) in [(3u128, (5.85, 17.67)), (4u128, (50.0, 20.0))] {
                let t = make_entity(id, pos.0, pos.1, 0);
                world.entities.insert(t.id, t);
            }
            // 两拍：弹1 t1 撞体截断；弹2 t2 触地截断（t1 尚在升弧）。
            // D-010 自检取第一拍末态（截断事实在 t1——t2 后重力重新加速，
            // 末态速度不再是截断语义的观测点）
            let mut changes = engine.step(&mut world, &HashSet::new(), &[]);
            let mid: MidState = [1u128, 2u128]
                .iter()
                .map(|&id| {
                    let e = world.entities.get(&EntityId(Uuid::from_u128(id))).unwrap();
                    (id, e.pos(), e.velocity())
                })
                .collect();
            changes.extend(engine.step(&mut world, &HashSet::new(), &[]));
            (changes, world, mid)
        };
        let (ch_a, wa, mid_a) = run_world();
        let (ch_b, wb, mid_b) = run_world();
        // PhysicsChange 未实现 PartialEq：比较变更条数 + 目标/位置逐位（等价口径）
        assert_eq!(ch_a.len(), ch_b.len(), "同状态必然同变更条数（#044）");
        for (ca, cb) in ch_a.iter().zip(ch_b.iter()) {
            assert_eq!(ca.entity, cb.entity, "变更目标序一致（#044）");
            assert_eq!(ca.new_pos, cb.new_pos, "变更位置逐位一致（#044）");
        }
        let mut ids: Vec<EntityId> = wa.entities.keys().copied().collect();
        ids.sort_by_key(|i| i.0);
        for id in ids {
            let (a, b) = (wa.entities.get(&id).unwrap(), wb.entities.get(&id).unwrap());
            assert_eq!(a.is_alive(), b.is_alive(), "存亡必须一致");
            assert_eq!(a.pos(), b.pos(), "坐标必须逐位一致（#044）");
            assert_eq!(a.velocity(), b.velocity(), "速度必须逐位一致（#044）");
        }
        // 场景自检（锚不空转）：第一拍末态——
        // 【D-010 语义升级】弹1 撞体截断：停点落目标接触窗口 + 速度法向清零+
        // 切向×0.4（迎头入射法向占主导→显著降速 <初速 60%）；弹2 t1 尚在升弧
        // 未截断（速度≈重力积分后的 (2.85, 5.7)，不做速度断言）
        {
            let s1 = mid_a.iter().find(|(id, _, _)| *id == 1).unwrap();
            let (_, p1, v1) = s1;
            let dist = ((p1.0 - 5.85).powi(2) + (p1.1 - 17.67).powi(2)).sqrt();
            assert!(
                dist <= 0.65 + 1e-6,
                "弹1 第一拍应撞体截断停在接触窗口，实际距离 {dist} @ {p1:?}"
            );
            let speed1 = (v1.0 * v1.0 + v1.1 * v1.1).sqrt();
            assert!(
                speed1 < 12.0 * 0.6,
                "弹1 截断拍应法向清零+切向摩擦显著降速（D-010），实际 {speed1}"
            );
            assert_eq!(mid_a, mid_b, "第一拍末态两跑逐位一致（#044）");
        }
    }

    #[test]
    fn ballistic_step_result_independent_of_hashmap_iteration_order() {
        // #044 机器数据红线锚（对照组）：同一场景构造序反过来——先插目标后插
        // 造物弹、id 收集后统一排序——结算必须与正序构造完全一致。
        // step 内部对实体按 EntityId 升序定序（确定序），CCD 目标扫描同序：
        // 结算与 HashMap 遍历序无关，即"N 的决定规则只读世界状态"在实现层成立。
        let engine = DefaultPhysicsEngine::new();
        let run = |insert_targets_first: bool| -> Vec<ShellFin> {
            let mut world = WorldState::new();
            let targets: Vec<Entity> = [(3u128, 6.0, 20.0), (4u128, 50.0, 20.0)]
                .into_iter()
                .map(|(id, x, y)| make_entity(id, x, y, 0))
                .collect();
            let shells: Vec<Entity> = [
                (1u128, (0.0, 20.0), (12.0, 9.8)),
                (2u128, (30.0, 1.0), (3.0, 6.0)),
            ]
            .into_iter()
            .map(|(id, pos, vel)| make_shell(id, pos, vel, 0.15, 1.0))
            .collect();
            if insert_targets_first {
                for e in targets {
                    world.entities.insert(e.id, e);
                }
                for e in shells {
                    world.entities.insert(e.id, e);
                }
            } else {
                for e in shells {
                    world.entities.insert(e.id, e);
                }
                for e in targets {
                    world.entities.insert(e.id, e);
                }
            }
            engine.step(&mut world, &HashSet::new(), &[]);
            let mut fin: Vec<ShellFin> = world
                .entities
                .iter()
                .map(|(id, e)| (*id, e.pos(), e.velocity(), e.is_alive()))
                .collect();
            fin.sort_by_key(|(id, _, _, _)| id.0);
            fin
        };
        let fin1 = run(false);
        let fin2 = run(true);
        assert_eq!(fin1, fin2, "末态与构造序无关（EntityId 定序，#044：结算只读状态）");
    }

    #[test]
    fn ccd_sweep_check_pure_geometry_no_wall_clock() {
        // #044 红线锚：CCD 扫掠判据=纯几何——同一（位置，速度）对在极速内
        // 取任意两档速度，截断判定只由几何决定且单调（路径穿过目标必截断，
        // 与跑多少次/何时跑无关）。高速 20 格/拍与慢速 4 格/拍都截断在目标
        // 接触窗口（旧点检查在 20 格/拍会漏——隧穿）。
        let engine = straight_engine();
        for speed in [4.0f64, 20.0f64] {
            let mut world = WorldState::new();
            let target = make_entity(2, 2.0, 20.0, 0);
            let tid = target.id;
            world.entities.insert(tid, target);
            let p = make_shell(1, (0.0, 20.0), (speed, 0.0), 0.15, 1.0);
            let pid = p.id;
            world.entities.insert(pid, p);
            engine.step(&mut world, &HashSet::new(), &[]);
            let shell = world.entities.get(&pid).unwrap();
            let (tx, _) = world.entities.get(&tid).unwrap().pos();
            let dist = (shell.pos().0 - tx).abs();
            assert!(
                dist <= 0.65 + 1e-9 && shell.pos().1 == 20.0,
                "速度 {speed} 格/拍：路径上的目标必截断（CCD 纯几何判据），实际 {dist}"
            );
        }
    }

    // === 历史弹丸实体兼容（D-018：PROJECTILE_CODE 实体退役后走通用路径）===

    #[test]
    fn step_legacy_projectile_now_uses_generic_ballistic_path() {
        // 兼容锚（测试构造历史形态）：旧 PROJECTILE_CODE 实体新世界不再产生
        //（仅历史快照反序列化兼容，恢复即清场——本测试用构造器复现历史形态，
        // 验证 step 不再按 kind 跳过：velocity≠0 走冲量路径，弹道物理通用化
        // 覆盖一切实体）。排除语义保留为防御性守卫：find_hit_target 不可锁定/
        // detect_touching 不接触/CCD 目标过滤不撞弹丸
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_projectile(1, (0.0, 10.0), (3.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let (vx, vy) = world.entities.get(&pid).unwrap().velocity();
        assert!(
            (vx - 3.0 * 0.95).abs() < 1e-9 && (vy + 9.8 * 0.95).abs() < 1e-9,
            "历史形态弹丸（测试构造）应走通用冲量路径（重力+阻力），实际 ({vx},{vy})"
        );
        assert!(
            world.entities.get(&pid).unwrap().pos().0 > 0.0,
            "位置应随速度积分位移"
        );
    }

    // === D-016 地形起伏：物理四件套③（落地钳制/CCD 落点查表）===

    #[test]
    fn d016_step_entity_lands_on_hill() {
        // D-016：生物落在山地上——落点=地形面（ground(x)+贴面），不再钳 y=0 半径线
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Mountain,
            8,
        ));
        // 找最高点，从其上空 10 格垂直下落
        let t = &world.terrain;
        let (peak_i, &peak_h) = t
            .samples()
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_x = codeworld_core::terrain::TERRAIN_X_MIN
            + peak_i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
        let e = make_entity(1, peak_x, peak_h + 10.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        // 多拍下落到稳定（落点解析：落点=max(ground(x), r)——山顶尖上圆心=peak_h+r）
        for _ in 0..5 {
            engine.step(&mut world, &HashSet::new(), &[]);
        }
        let final_y = world.entities.get(&id).unwrap().pos().1;
        let expect = peak_h.max(0.5);
        assert!(
            (final_y - expect).abs() < 1e-9,
            "峰顶落点应为 ground+半径（{expect}），实际 {final_y}"
        );
    }

    #[test]
    fn d016_step_entity_rests_in_valley() {
        // D-016：坑负全合法——谷底生物落点=谷底地面（y=0 退位为基准线）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Mountain,
            8,
        ));
        let t = &world.terrain;
        let (valley_i, &valley_h) = t
            .samples()
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let valley_x = codeworld_core::terrain::TERRAIN_X_MIN
            + valley_i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
        assert!(valley_h < 0.0, "山地应有负高度坑（海平面语义），实际 {valley_h}");
        let e = make_entity(1, valley_x, valley_h + 5.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        for _ in 0..5 {
            engine.step(&mut world, &HashSet::new(), &[]);
        }
        let final_y = world.entities.get(&id).unwrap().pos().1;
        let expect = valley_h.max(0.5);
        assert!(
            (final_y - expect).abs() < 1e-9,
            "谷底落点应为 ground（{expect}），实际 {final_y}"
        );
    }

    #[test]
    fn d016_flat_terrain_physics_zero_regression() {
        // D-016 零回归锚：平地地形（全零数组）下物理行为与旧常数地面逐位一致
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new(); // 默认 terrain=flat
        let e = make_entity(1, 5.0, 10.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(
            world.entities.get(&id).unwrap().pos(),
            (5.0, 0.5),
            "平地落点=半径线（旧口径零回归）"
        );
    }

    #[test]
    fn d016_shell_lands_on_mountain_slope() {
        // D-016 + D-018：造物弹 CCD 落点查表——撞山坡的弹截断贴地形面（y=ground+r）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Mountain,
            8,
        ));
        let t = &world.terrain;
        let (peak_i, &peak_h) = t
            .samples()
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_x = codeworld_core::terrain::TERRAIN_X_MIN
            + peak_i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
        // 从峰左侧高处向右下斜射（必撞上山坡）
        let start = (peak_x - 4.0, peak_h + 6.0);
        let p = make_shell(1, start, (10.0, -12.0), 0.15, 1.0);
        let pid = p.id;
        let r = p.radius();
        world.entities.insert(pid, p);
        // 【D-010 语义升级】撞坡停驻 → 撞坡截断+切向摩擦：截断拍停点仍贴地形面
        //（停点逻辑不动），但切向速度 ×(1-f) 保留——斜坡上速度非零的实体会沿坡
        // 稳态滑行（重力沿坡分量每拍重新加速 vs 摩擦×0.4 衰减，动摩擦模型无静摩
        // 擦：球在坡上会滚下来，物理涌现；贴坡静止实体 vel=0 走落点制不动=零回归）。
        // 本测试锚定第一拍的截断事实：位置贴坡+未过峰（原"永久停驻"语义随摩擦
        // 立法退役）
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        let (lx, ly) = shell.pos();
        let ground = world.terrain.height(lx);
        assert!(
            (ly - ground - r).abs() < 1e-6,
            "截断点弹心应贴地形面（ground+{r}），实际 ({lx},{ly}) ground={ground}"
        );
        assert!(lx < peak_x + 1.0, "应在山坡上截断（未过峰），实际 lx={lx}");
    }

    #[test]
    fn d016_terrain_ground_over_matches_all_breakpoint_peak_reference() {
        // D-016 R84 复审返工锚：分段线性地面在区间内的最大值必落在 0.25 格断点
        // 或区间端点上——xa/中点/xb 三点采样漏检内部断点峰值（复审口径实测
        // 最大差 0.1741 格；本测试的 Hills/Mountain×seed 0..49 全扫描口径实测
        // 最大差 0.375 格=Mountain max_delta，static 造物 AABB 底边可悬空到
        // 该量级）。参考=直接扫高度数组的全断点独立实现，不调被测函数。
        use codeworld_core::terrain::{Terrain, TerrainPreset, TERRAIN_SPACING, TERRAIN_X_MIN};
        let all_breakpoint_peak = |t: &Terrain, xa: f64, xb: f64| -> f64 {
            let mut peak = t.height(xa).max(t.height(xb));
            for (i, &h) in t.samples().iter().enumerate() {
                let x = TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING;
                if x >= xa && x <= xb {
                    peak = peak.max(h);
                }
            }
            peak
        };
        let mut world = WorldState::new();
        // 复审同口径全扫描：Hills/Mountain × seed 0..49 × 全部格对齐 1 格区间
        //（与 creation_aabb 生产口径一致：[floor(x), floor(x)+1]）
        let mut max_diff = 0.0f64;
        let mut worst_miss = 0.0f64;
        for preset in [TerrainPreset::Hills, TerrainPreset::Mountain] {
            for seed in 0..50u64 {
                world.set_terrain(Terrain::new(preset, seed));
                let t = &world.terrain;
                let n = t.sample_count();
                for i in 0..n - 4 {
                    let xa = TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING;
                    let xb = xa + 1.0;
                    let exact = all_breakpoint_peak(t, xa, xb);
                    // 三点口径（旧实现）：内部断点成峰时必然低估
                    let three_pt = t
                        .height(xa)
                        .max(t.height((xa + xb) / 2.0))
                        .max(t.height(xb));
                    worst_miss = worst_miss.max(exact - three_pt);
                    let got = terrain_ground_over(&world, xa, xb);
                    max_diff = max_diff.max(exact - got);
                }
            }
        }
        // 负面场景真实性自检：扫描范围内确有内部断点成峰且被三点口径漏检
        //（复审实测 0.1741）——防锚测试扫到无峰地形变空转
        assert!(
            worst_miss > 0.1,
            "前提自检失败：扫描应存在内部断点峰值被三点口径漏检（实测 {worst_miss}）"
        );
        assert!(
            max_diff < 1e-12,
            "terrain_ground_over 应=全断点精确峰值（最大差 {max_diff}）"
        );
        // 边界：区间内无断点的亚格窄区间——极值只能在两端点（区间外断点
        // 不计入，计入即高估），与全断点参考仍须逐位一致
        world.set_terrain(Terrain::new(TerrainPreset::Mountain, 8));
        for k in 0..400i64 {
            let xa = TERRAIN_X_MIN + (k as f64 + 0.1) * TERRAIN_SPACING;
            let xb = xa + 0.1 * TERRAIN_SPACING; // 宽 0.025 格，恒在单一格内
            let exact = all_breakpoint_peak(&world.terrain, xa, xb);
            let got = terrain_ground_over(&world, xa, xb);
            assert!(
                (got - exact).abs() < 1e-12,
                "亚格窄区间 k={k} 应=精确峰值：got={got} exact={exact}"
            );
        }
    }

    #[test]
    fn d016_creation_lands_on_terrain_no_float() {
        // D-016：static 造物落在起伏地形上不悬空——落地稳定后，AABB 底边
        // 不高于占地格内地形峰值（塔脚总有着落点），且多拍稳定不抖动
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Hills,
            11,
        ));
        // 在有起伏处放造物（找一个左右高差 >0.3 的 x：占地格跨坡）
        let t = &world.terrain;
        let mut spot = None;
        for i in 1..t.sample_count() - 5 {
            let x = codeworld_core::terrain::TERRAIN_X_MIN
                + i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
            let g0 = t.height(x);
            let g1 = t.height(x + 1.0);
            if (g1 - g0).abs() > 0.3 {
                spot = Some(x);
                break;
            }
        }
        let x = spot.expect("丘陵 0.25 采样必有跨坡格");
        let c = make_creation(1, x, 8.0);
        let cid = c.id;
        world.entities.insert(cid, c);
        let mut last = None;
        for _ in 0..8 {
            engine.step(&mut world, &HashSet::new(), &[]);
            last = Some(world.entities.get(&cid).unwrap().pos().1);
        }
        let final_y = last.unwrap();
        // 落地稳定（连续两拍同位）
        engine.step(&mut world, &HashSet::new(), &[]);
        let stable_y = world.entities.get(&cid).unwrap().pos().1;
        assert_eq!(final_y, stable_y, "造物落地应稳定不抖动");
        // 不悬空：AABB 底边 floor(y) 不高于占地格地形峰值
        //（分层锚定注记：此处 ground_peak 由被测函数 terrain_ground_over
        // 计算；其精确性的独立参考锚由
        // d016_terrain_ground_over_matches_all_breakpoint_peak_reference 覆盖）
        let (min, max) = creation_aabb((x, final_y));
        let ground_peak = terrain_ground_over(&world, min.0, max.0);
        assert!(
            min.1 <= ground_peak + 1e-9,
            "AABB 底边 {min_1} 应不高于地形峰值 {ground_peak}（塔脚有着落）",
            min_1 = min.1
        );
        assert!(final_y <= 0.0 + 1e-9 || min.1 >= ground_peak - 1.0 - 1e-9);
    }

    // === D-010a 冲量小刀：带速度实体的速度积分路径（跳跃弹道锚） ===

    /// 判断浮点近似相等（弹道数值断言用；半格/格级锚点用 1e-9 即可）
    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn thrust_ballistic_jump_peak_within_three_grid_anchor() {
        // 跳跃锚点（D-010a Gate）：地面生物带冲量速度 (0, 7.7) 进 physics.step，
        // 拍 1 升空高度 ≈3 格（±0.5 容差）；次拍落地贴半径线、速度清零。
        // 积分次序锚定：冲量入速度（阶段二 thrust 应用）→ 重力入速度 → 中点位移
        // → 落地清零。中点位移 = v0 + g/2 = 7.7 - 4.9 = 2.8，落在锚区间内。
        // 【积分器双轨】此锚是 step 冲量路径保留中点位移的历史证据——
        // D-018 通用化后本路径就是世界唯一弹道管线，锚继续生效。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut e = make_entity(1, 0.0, 0.5, 0); // r=0.5 站半径线（地面 y=0）
        let id = e.id;
        e.set_velocity((0.0, 7.7)); // 模拟阶段二 thrust 应用后的速度字段
        world.entities.insert(id, e);
        // 拍 1：升空
        engine.step(&mut world, &HashSet::new(), &[]);
        let e = world.entities.get(&id).unwrap();
        let peak_height = e.pos().1 - 0.5; // 相对地面高度
        assert!(
            (2.5..=3.5).contains(&peak_height),
            "跳跃最高点应 ≈3 格（±0.5 容差），实测 {peak_height}"
        );
        assert!(
            approx(e.velocity().1, (7.7 - 9.8) * 0.95) && approx(e.velocity().0, 0.0),
            "拍 1 重力入速度后吃阻力（(7.7-9.8)×0.95），实测 {:?}",
            e.velocity()
        );
        // 拍 2：落地清零
        engine.step(&mut world, &HashSet::new(), &[]);
        let e = world.entities.get(&id).unwrap();
        assert!(
            approx(e.pos().1, 0.5) && approx(e.pos().0, 0.0),
            "落地应贴半径线 (0, 0.5)，实测 {:?}",
            e.pos()
        );
        assert_eq!(e.velocity(), (0.0, 0.0), "落地速度清零（vy 清零规则）");
    }

    #[test]
    fn ballistic_midair_accumulates_gravity_no_teleport() {
        // 空中不清（vy 清零规则另一半）：拍 1 升空后拍 2 空中 vy 继续累积重力
        //（-2.1 → -11.9）；全程逐拍位移 ≤ 世界极速 25（非瞬移）。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut e = make_entity(1, 0.0, 0.5, 0);
        let id = e.id;
        e.set_velocity((0.0, 7.7));
        world.entities.insert(id, e);
        // 拍 1
        engine.step(&mut world, &HashSet::new(), &[]);
        let (p0, v0) = {
            let e = world.entities.get(&id).unwrap();
            (e.pos(), e.velocity())
        };
        assert!(approx(v0.1, -1.995), "拍 1 vy 应为 (7.7-9.8)×0.95，实测 {v0:?}");
        // 拍 2（空中）：vy 累积 = -2.1 + -9.8 = -11.9
        engine.step(&mut world, &HashSet::new(), &[]);
        let (p1, v1) = {
            let e = world.entities.get(&id).unwrap();
            (e.pos(), e.velocity())
        };
        assert!(
            p1.1 < p0.1,
            "拍 2 在下落（弧线回落），实测 p0={p0:?} p1={p1:?}"
        );
        // 落地拍位移（中点 = -2.1 + -9.8)/2 = -7.0）与起跳拍位移（+2.8）都 ≤ 极速 25
        assert!(
            (p1.1 - p0.1).abs() <= 25.0 && (p0.1 - 0.5).abs() <= 25.0,
            "逐拍位移不得超过世界极速（非瞬移锚）"
        );
        assert!(
            approx(v1.0, 0.0) && approx(v1.1, 0.0),
            "拍 2 应已落地清零，实测 {v1:?}"
        );
    }

    #[test]
    fn ballistic_zero_velocity_entities_keep_positional_fall_regime() {
        // 回归锚：速度恒 (0,0) 的实体走原落点制（circle_landing_y，一拍落 9.8），
        // 不进冲量路径——空中静止实体（悬停丢失后自由落体）现状行为零变化。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e = make_entity(1, 0.0, 5.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        engine.step(&mut world, &HashSet::new(), &[]);
        let e = world.entities.get(&id).unwrap();
        // 落点制：fall_to = 5.0 - 9.8 = -4.8 < 地面 0.5 → 落地半径线
        assert!(
            approx(e.pos().1, 0.5),
            "零速实体应走落点制落地，实测 {:?}",
            e.pos()
        );
        assert_eq!(e.velocity(), (0.0, 0.0), "落点制不动速度字段");
    }

    #[test]
    fn ballistic_dynamic_creation_with_velocity_now_integrates() {
        // D-018③ 先行半步：dynamic 造物出生初速此前只存不动（位置走落点制），
        // 冲量路径起造物带速度也走速度积分（被撞飞/抛射的地基）。
        // r=0.15 小弹形态（贴地出生 y=ground+r 会被触地查表 A=0 判立即截断——
        // 发射点外推教学点，见 docs/newbie-first-thrower.md）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut c = make_dyn_creation(1, 0.0, 0.5);
        let id = c.id;
        c.set_radius(0.15);
        c.set_velocity((7.7, 7.7));
        world.entities.insert(id, c);
        engine.step(&mut world, &HashSet::new(), &[]);
        let c = world.entities.get(&id).unwrap();
        assert!(
            c.pos().0 > 0.5,
            "dynamic 造物带速度应横向位移（速度积分生效），实测 {:?}",
            c.pos()
        );
    }

    // === D-012 力场：物理步接入（域内实体按改后参数被结算）===

    #[test]
    fn step_entity_in_zero_gravity_field_does_not_fall() {
        // 场影响生物落点：零重力场（scale=0）域内生物本拍不落（重力被场改写为 0）；
        // 对照组域外同况实体照常下落
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut in_field = make_entity(1, 5.0, 10.0, 0);
        in_field.set_velocity((0.5, 0.0)); // 微速度走冲量路径（重力在速度上累积）
        let mut outside = make_entity(2, 50.0, 10.0, 0);
        outside.set_velocity((0.5, 0.0));
        let id_in = in_field.id;
        let id_out = outside.id;
        world.entities.insert(id_in, in_field);
        world.entities.insert(id_out, outside);
        let fields = [crate::fields::FieldEffect {
            source_id: EntityId(Uuid::from_u128(9)),
            pos: (5.0, 10.0),
            radius: 3.0,
            gravity_scale: 0.0,
            drag_delta: 0.0,
        }];
        engine.step(&mut world, &HashSet::new(), &fields);
        let (vx_in, vy_in) = world.entities.get(&id_in).unwrap().velocity();
        assert!(
            vy_in.abs() < 1e-12,
            "零重力场域内速度 vy 不应累积重力，实际 {vy_in}"
        );
        let (_, vy_out) = world.entities.get(&id_out).unwrap().velocity();
        assert!(
            (vy_out - (0.0 - 9.8) * 0.95).abs() < 1e-9,
            "域外实体照常吃世界重力（阻力后 -9.31），实际 {vy_out}"
        );
        let _ = vx_in;
    }

    #[test]
    fn step_entity_outside_field_unaffected_vs_baseline() {
        // 域外实体不受影响锚：与无场基线逐位一致（无场世界行为不变）
        let engine = DefaultPhysicsEngine::new();
        let mut base = WorldState::new();
        let mut plain = make_entity(1, 5.0, 10.0, 0);
        plain.set_velocity((1.0, 0.0));
        let id_p = plain.id;
        base.entities.insert(id_p, plain);
        engine.step(&mut base, &HashSet::new(), &[]);
        // 场世界：同一实体挪到远处的场外（域判定只看距离，同引擎同参数）
        let mut fworld = WorldState::new();
        let mut shadow = make_entity(1, 5.0, 10.0, 0);
        shadow.set_velocity((1.0, 0.0));
        let id_s = shadow.id;
        fworld.entities.insert(id_s, shadow);
        let fields = [crate::fields::FieldEffect {
            source_id: EntityId(Uuid::from_u128(9)),
            pos: (40.0, 40.0),
            radius: 2.0,
            gravity_scale: 0.0,
            drag_delta: 2.0,
        }];
        engine.step(&mut fworld, &HashSet::new(), &fields);
        assert_eq!(
            base.entities.get(&id_p).unwrap().pos(),
            fworld.entities.get(&id_s).unwrap().pos(),
            "域外实体位置应与无场基线逐位一致"
        );
        assert_eq!(
            base.entities.get(&id_p).unwrap().velocity(),
            fworld.entities.get(&id_s).unwrap().velocity(),
            "域外实体速度应与无场基线逐位一致"
        );
    }

    #[test]
    fn step_shell_in_zero_gravity_field_keeps_straight_trajectory() {
        // 弹道场第一消费者锚（D-018 造物弹版）：零重力场域内造物弹不吃重力
        //（vy 不累积，直线飞行），域外对照造物弹照常下坠——护弹道场的直接作用
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let in_shell = make_shell(1, (5.0, 10.0), (3.0, 0.0), 0.15, 1.0);
        let out_shell = make_shell(2, (60.0, 10.0), (3.0, 0.0), 0.15, 1.0);
        let id_in = in_shell.id;
        let id_out = out_shell.id;
        world.entities.insert(id_in, in_shell);
        world.entities.insert(id_out, out_shell);
        let fields = [crate::fields::FieldEffect {
            source_id: EntityId(Uuid::from_u128(9)),
            pos: (5.0, 10.0),
            radius: 3.0,
            gravity_scale: 0.0,
            drag_delta: 0.0,
        }];
        engine.step(&mut world, &HashSet::new(), &fields);
        assert!(
            (world.entities.get(&id_in).unwrap().velocity().1).abs() < 1e-12,
            "域内造物弹 vy 不累积（零重力场）"
        );
        let vy_out = world.entities.get(&id_out).unwrap().velocity().1;
        assert!(
            (vy_out + 9.8 * 0.95).abs() < 1e-12,
            "域外造物弹照常吃重力（阻力后 -9.8×0.95），实际 {vy_out}"
        );
    }

    // === D-009 水体：物理步液体接入（浮力改写/水中阻尼/附加质量/速度制路由）===

    use crate::liquid::{LiquidDomain, liquid_effect};

    /// 深水液体域（y ∈ [-100, 100] 全覆盖，圆心 y<100 全浸没）
    fn deep_water(id_u: u128, density: f64, drag_k: f64) -> LiquidDomain {
        LiquidDomain {
            domain_id: EntityId(Uuid::from_u128(id_u)),
            min: (0.0, -100.0),
            max: (100.0, 100.0),
            density,
            drag_k,
        }
    }

    #[test]
    fn step_water_damping_drops_sixty_five_percent_first_tick() {
        // 水中阻尼锚（D-009 定案观感锚）：入水一拍速度掉 65%——v=(-0, -10)
        // 的实体进入 k=0.35 等密度水（g_final=0：m=π×r² 使 ρ物=ρ液=1.0）：
        // 水中一步 v ← v×0.35（-3.5）→ 再吃 apply_drag（m_eff=1+π×0.25≈1.7854，
        // 衰 2.8%）→ ≈-3.402。对照组：空气中同实体吃重力入速度 (-10-9.8)×0.95
        // = -18.81。（y=30 起跳：一拍位移 ~-6.7 够不着湖底，落地清零不干扰速度断言）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut wet = make_entity(1, 5.0, 30.0, 0);
        wet.set_mass(std::f64::consts::PI * 0.25); // ρ物=1.0 等密度
        wet.set_velocity((0.0, -10.0));
        let mut dry = make_entity(2, 150.0, 30.0, 0); // 水域 x≤100 域外（对照组）
        dry.set_velocity((0.0, -10.0));
        let id_wet = wet.id;
        let id_dry = dry.id;
        world.entities.insert(id_wet, wet);
        world.entities.insert(id_dry, dry);
        let liquids = [deep_water(9, 1.0, 0.35)];
        engine.step_with_liquids(&mut world, &HashSet::new(), &[], &liquids);
        let vy_wet = world.entities.get(&id_wet).unwrap().velocity().1;
        // m=π/4 等密度下 m_eff = m + ρ液·π·r²·f = π/4 × 2 = π/2
        let m_eff = std::f64::consts::PI * 0.5;
        let expect = (-10.0 * 0.35) * (1.0 - 0.05 / m_eff);
        assert!(
            (vy_wet - expect).abs() < 1e-9,
            "入水一拍应 v×0.35 再吃附加质量阻力（≈{expect}，掉 65%），实际 {vy_wet}"
        );
        let vy_dry = world.entities.get(&id_dry).unwrap().velocity().1;
        assert!(
            (vy_dry - (-10.0 - 9.8) * 0.95).abs() < 1e-9,
            "空气中对照组照常重力入速度（-18.81），实际 {vy_dry}"
        );
    }

    #[test]
    fn step_water_added_mass_sluggish_drag_response() {
        // 附加质量锚（独门立法）：水下阻力响应更钝——同 v=(10,0) 实体在水
        //（m_eff=1+π×0.25≈1.7854）阻力衰 2.8%（→9.7200），在空气（m=1）衰 5%
        //（→9.5）——"水下加速钝"从阻力响应涌现，零新参数。
        //（水配 k=1.0=速度完全保留的无阻尼档——隔离附加质量单变量，k 阻尼另测；
        // 注：k 保留系数语义下 k=0=每拍速度清零最粘档，非"无阻尼"）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut wet = make_entity(1, 5.0, 30.0, 0);
        wet.set_velocity((10.0, 0.0));
        let mut dry = make_entity(2, 150.0, 30.0, 0); // 水域 x≤100 域外（对照组）
        dry.set_velocity((10.0, 0.0));
        let id_wet = wet.id;
        let id_dry = dry.id;
        world.entities.insert(id_wet, wet);
        world.entities.insert(id_dry, dry);
        let liquids = [deep_water(9, 1.0, 1.0)];
        engine.step_with_liquids(&mut world, &HashSet::new(), &[], &liquids);
        let vx_wet = world.entities.get(&id_wet).unwrap().velocity().0;
        let vx_dry = world.entities.get(&id_dry).unwrap().velocity().0;
        let expect_wet = 10.0 * (1.0 - 0.05 / (1.0 + std::f64::consts::PI * 0.25));
        assert!(
            (vx_wet - expect_wet).abs() < 1e-9,
            "水下 vx 应=m_eff 阻力响应 9.72，实际 {vx_wet}"
        );
        assert!(
            vx_wet > vx_dry,
            "水下阻力响应应更钝（{vx_wet} > 空气 {vx_dry}）"
        );
    }

    #[test]
    fn step_zero_velocity_entity_in_water_uses_velocity_regime() {
        // 物理步路由裁决锚：f>0 的零速实体一律走冲量积分路径（速度制）——
        // 铁球全浸一拍位移=|g_final|/2 的小步进（速度从 0 涨起），而域外零速
        // 实体走落点制一拍摔 9.8 格。零速走位置制则每拍位移=全额 g_final，
        // 水面附近会疯狂弹跳，"典型物体半沉"永远看不到。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let iron = make_entity(1, 5.0, 50.0, 0); // r=0.5 m=1 → ρ物≈1.27>1 沉
        let id = iron.id;
        world.entities.insert(id, iron);
        let liquids = [deep_water(9, 1.0, 0.35)];
        engine.step_with_liquids(&mut world, &HashSet::new(), &[], &liquids);
        let e = world.entities.get(&id).unwrap();
        let drop = 50.0 - e.pos().1;
        assert!(
            drop > 0.5 && drop < 9.8,
            "零速入水应走速度制小步下沉（|g_final|/2 量级），实际下坠 {drop}"
        );
        assert!(
            e.velocity().1 < 0.0,
            "水中速度制应写回速度（下拍继续累积），实际 {:?}",
            e.velocity()
        );
    }

    #[test]
    fn step_half_submerged_neutral_body_stays_put() {
        // 半沉平衡锚（"典型物体半沉"涌现）：m=0.5×π×r² → ρ物=0.5、圆心在
        // 液面 f=0.5、ρ液=1.0=2ρ物 → g_final=0，零速实体位置逐拍不动
        //（阻尼 k<1 的线性系统自然收敛，不写平衡点求解器）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut body = make_entity(1, 5.0, 100.0, 0);
        body.set_mass(0.5 * std::f64::consts::PI * 0.25); // ρ物=0.5
        let id = body.id;
        world.entities.insert(id, body);
        // 水面在 y=100（域 max.1）
        let liquids = [deep_water(9, 1.0, 0.35)];
        for _ in 0..5 {
            engine.step_with_liquids(&mut world, &HashSet::new(), &[], &liquids);
        }
        let y = world.entities.get(&id).unwrap().pos().1;
        assert!(
            (y - 100.0).abs() < 1e-9,
            "半沉平衡体应原地悬浮（g_final=0），实际 y={y}"
        );
    }

    #[test]
    fn step_multi_liquid_heaviest_domain_floats_faster() {
        // 多域最高密度集成锚：木球同时泡在轻液（ρ=1）与重液（ρ=2.5）里，
        // 取 ρ 最高者结算——重液上浮推力更大，一拍上浮位移更大
        let engine = DefaultPhysicsEngine::new();
        let run = |densities: [f64; 2]| -> f64 {
            let mut world = WorldState::new();
            let mut wood = make_entity(1, 5.0, 50.0, 0);
            wood.set_mass(0.4); // ρ物≈0.51<1 木
            let id = wood.id;
            world.entities.insert(id, wood);
            let liquids = [
                deep_water(9, densities[0], 0.35),
                deep_water(8, densities[1], 0.35),
            ];
            engine.step_with_liquids(&mut world, &HashSet::new(), &[], &liquids);
            world.entities.get(&id).unwrap().pos().1 - 50.0
        };
        let rise_light = run([0.0, 1.0]);
        let rise_heavy = run([1.0, 2.5]);
        assert!(rise_light > 0.0, "轻液中木球应上浮，实际 {rise_light}");
        assert!(
            rise_heavy > rise_light,
            "重液上浮推力更大（ρ比 4.9 vs 1.96），实际 {rise_heavy} vs {rise_light}"
        );
    }

    #[test]
    fn step_entity_far_from_liquid_matches_no_liquid_baseline() {
        // 域外零回归锚：远处水域不影响域外实体——与无液体基线逐位一致
        //（位置+速度双双对照，无液体世界全测试照绿的显式形态）
        let engine = DefaultPhysicsEngine::new();
        let mut base = WorldState::new();
        let mut plain = make_entity(1, 5.0, 10.0, 0);
        plain.set_velocity((1.0, 0.0));
        let id_p = plain.id;
        base.entities.insert(id_p, plain);
        engine.step(&mut base, &HashSet::new(), &[]);
        let mut wet_world = WorldState::new();
        let mut shadow = make_entity(1, 5.0, 10.0, 0);
        shadow.set_velocity((1.0, 0.0));
        let id_s = shadow.id;
        wet_world.entities.insert(id_s, shadow);
        // 水域在 (60..61, 0..1)，实体在 (5,10)——域外
        let liquids = [LiquidDomain {
            domain_id: EntityId(Uuid::from_u128(9)),
            min: (60.0, 0.0),
            max: (61.0, 1.0),
            density: 1.0,
            drag_k: 0.35,
        }];
        engine.step_with_liquids(&mut wet_world, &HashSet::new(), &[], &liquids);
        assert_eq!(
            base.entities.get(&id_p).unwrap().pos(),
            wet_world.entities.get(&id_s).unwrap().pos(),
            "域外实体位置应与无液体基线逐位一致"
        );
        assert_eq!(
            base.entities.get(&id_p).unwrap().velocity(),
            wet_world.entities.get(&id_s).unwrap().velocity(),
            "域外实体速度应与无液体基线逐位一致"
        );
    }

    #[test]
    fn step_aloft_entity_in_water_ignores_buoyancy() {
        // aloft 优先级高于浮力锚（与力场同款）：悬停声明本拍免重力免浮力免
        // 阻尼——悬停在水中的实体坐标分毫不动
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut hover = make_entity(1, 5.0, 50.0, 0);
        hover.set_velocity((0.0, -10.0)); // 即使带速度
        let id = hover.id;
        world.entities.insert(id, hover);
        let mut aloft = HashSet::new();
        aloft.insert(id);
        let liquids = [deep_water(9, 1.0, 0.35)];
        engine.step_with_liquids(&mut world, &aloft, &[], &liquids);
        assert_eq!(
            world.entities.get(&id).unwrap().pos(),
            (5.0, 50.0),
            "悬停实体在水中也应分毫不动（aloft 短路在液体结算之前）"
        );
        assert_eq!(
            world.entities.get(&id).unwrap().velocity(),
            (0.0, -10.0),
            "悬停实体速度不被水中阻尼触碰"
        );
    }

    #[test]
    fn step_static_creation_ignores_liquids() {
        // static 造物（含液体造物自己）不吃液体物理锚：static 不吃物理现状
        // 照旧——直落地面分支，落点/速度字段都不被液体改写（水不会动）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut water_block = make_creation(1, 5.0, 5.0);
        water_block.set_liquid_density(1.0); // 液体造物=static 布景
        water_block.set_liquid_drag(0.35);
        let id = water_block.id;
        world.entities.insert(id, water_block);
        let liquids = [deep_water(9, 1.0, 0.35)];
        engine.step_with_liquids(&mut world, &HashSet::new(), &[], &liquids);
        assert_eq!(
            world.entities.get(&id).unwrap().pos(),
            (5.0, 0.0),
            "static 造物照旧直落地面（液体域不改写 static 分支）"
        );
    }

    #[test]
    fn step_projectile_in_water_slows_and_softens_fall() {
        // 弹丸入水改弹道锚（本刀可见效果之一；【D-018】迁移为通用路径——历史
        // 形态弹丸（测试构造）走 step 冲量路径，wet 路由同样命中）：标准弹（r=0.15/m=1 →
        // ρ物≈14.1 很稠）入水（ρ=1, k=0.5）——水平速度被阻尼减半后吃更钝
        // 阻力、下坠被浮力托缓（|g_final|<9.8）：弹道变短变缓
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut wet = make_entity(1, 50.0, 50.0, 0);
        wet.set_velocity((3.0, 0.0));
        wet.set_radius(codeworld_core::collision::PROJECTILE_RADIUS);
        wet.code_hash = codeworld_core::collision::PROJECTILE_CODE.to_string();
        let mut dry = make_entity(2, 150.0, 50.0, 0); // 水域 x≤100 域外（对照组）
        dry.set_velocity((3.0, 0.0));
        dry.set_radius(codeworld_core::collision::PROJECTILE_RADIUS);
        dry.code_hash = codeworld_core::collision::PROJECTILE_CODE.to_string();
        let id_wet = wet.id;
        let id_dry = dry.id;
        world.entities.insert(id_wet, wet);
        world.entities.insert(id_dry, dry);
        let liquids = [deep_water(9, 1.0, 0.5)];
        engine.step_with_liquids(&mut world, &HashSet::new(), &[], &liquids);
        let w = world.entities.get(&id_wet).unwrap();
        let d = world.entities.get(&id_dry).unwrap();
        assert!(
            w.velocity().0 < d.velocity().0,
            "弹丸水中水平速度应低于空气对照（阻尼减半=弹道显著变缓），实际 {} vs {}",
            w.velocity().0,
            d.velocity().0
        );
        let m_eff_proj = 1.0 + std::f64::consts::PI * 0.15 * 0.15; // 全浸 f=1
        let expect_vx = 3.0 * 0.5 * (1.0 - 0.05 / m_eff_proj);
        assert!(
            (w.velocity().0 - expect_vx).abs() < 1e-9,
            "水中 vx 应=v×k=1.5 再吃 m_eff 阻力（≈{expect_vx}），实际 {}",
            w.velocity().0
        );
        assert!(
            w.velocity().1 > d.velocity().1,
            "水中下坠应被浮力托缓（g_final 钳后仍小于空气重力），实际 {} vs {}",
            w.velocity().1,
            d.velocity().1
        );
    }

    #[test]
    fn liquid_effect_is_pure_helper_importable() {
        // 依赖接线锚：default.rs 经 crate::liquid::liquid_effect 结算（单一实现），
        // 全浸悬浮输入给出 g_final=0（防物理步里手抄公式分叉）
        let liquids = [deep_water(9, 1.0, 0.35)];
        let m = std::f64::consts::PI * 0.25; // ρ物=1.0
        let e = liquid_effect((5.0, 3.0), 0.5, m, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert!((e.g_final - 0.0).abs() < 1e-12, "等密度全浸应悬浮");
    }

    // === D-010 碰撞响应刀：撞=动量交换（法向反弹+切向摩擦+低速闸+每对每拍一弹）===
    //
    // 测试用无重力无阻力引擎隔离干扰（gravity=0/drag=0 先例
    // shell_at_exact_max_speed_untouched）；数字全手算（容差 1e-6 分量近似断言）。

    /// 无重力无阻力引擎（D-010 反弹测试隔离干扰：重力会拉速度、阻力会衰减，
    /// 纯几何反弹需要定律恒等）
    fn bounce_engine() -> DefaultPhysicsEngine {
        DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            gravity: 0.0,
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        })
    }

    #[test]
    fn high_speed_bounce_reflects_normal() {
        // 两实体相向运动（A→右 10，B→左 1），e=max(0.5,0.5)=0.5、f=√(0.25)=0.5，
        // 相对法向速度 11>1 过低速闸：撞者 A 的速度=法向反弹 -0.5×10=-5 +
        // 切向摩擦 ×0.5（vy 3→1.5）——撞者单侧反射，目标 B 不由本步代写
        //（目标在自己拍的 CCD 里处理自己的碰撞；同对已入"每对每拍一弹"闸，
        // B 的二次接触按非弹性清零——闸语义见
        // same_pair_bounces_once_per_tick_second_contact_inelastic）
        let engine = bounce_engine();
        let mut world = WorldState::new();
        let mut a = make_shell(1, (5.0, 5.0), (10.0, 3.0), 0.5, 1.0);
        a.set_restitution(0.5);
        a.set_friction(0.5);
        let mut b = make_shell(2, (8.0, 5.0), (-1.0, 0.0), 0.5, 1.0);
        b.set_restitution(0.5);
        b.set_friction(0.5);
        let (aid, bid) = (a.id, b.id);
        world.entities.insert(aid, a);
        world.entities.insert(bid, b);
        engine.step(&mut world, &HashSet::new(), &[]);
        let a = world.entities.get(&aid).unwrap();
        // n=(1,0)（A→B 两圆心连线）：v_n=10 → -0.5×10=-5；v_t=(0,3)→×(1-0.5)=(0,1.5)
        assert!(
            (a.velocity().0 - (-5.0)).abs() < 1e-6 && (a.velocity().1 - 1.5).abs() < 1e-6,
            "撞者 A 应法向反弹 -5 + 切向×0.5=1.5，实际 {:?}",
            a.velocity()
        );
        // 停点：斜线路径首触（与 B 圆心距离恰 r 和 1.0）+1cm 推进→距 B 圆心
        // ≈0.995（1cm 推进语义）；停点逻辑与 D-018b 前逐位一致（D-010 不动位置）
        let (sx, sy) = a.pos();
        let dist_to_b = ((sx - 8.0).powi(2) + (sy - 5.0).powi(2)).sqrt();
        assert!(
            dist_to_b > 0.98 && dist_to_b < 1.01,
            "A 应停在接触窗口（r 和 1.0 的 1cm 推进语义），实际距离 {dist_to_b} @ {:?}",
            a.pos()
        );
        // B：同对已弹过（闸）→e 生效=0（摩擦照常）；A 停点与 B 起点距 0.995<1.0
        // 起步即接触（t=0 截断）——n 为 A 停点/B 起点连线斜方向（反弹入射几何），
        // B 初速 (-1,0) 的法向分量清零、切向分量 ×(1-0.5) 摩擦衰减
        let b = world.entities.get(&bid).unwrap();
        let (ax, ay) = a.pos();
        let (bx, by) = (8.0, 5.0); // B 结算输入的起点（n 用 before 几何，非截断后停点）
        let (dx, dy) = (ax - bx, ay - by);
        let dl = (dx * dx + dy * dy).sqrt();
        let bvn = b.velocity().0 * (dx / dl) + b.velocity().1 * (dy / dl);
        assert!(
            bvn.abs() < 1e-6,
            "同对二次接触不二次反弹（每对每拍一弹闸）：B 法向分量应清零，实际 {bvn}"
        );
        let bspeed = (b.velocity().0 * b.velocity().0 + b.velocity().1 * b.velocity().1).sqrt();
        assert!(
            bspeed > 0.1 && bspeed < 0.6,
            "B 切向应剩摩擦衰减分量（×0.5，模长 ≈0.343），实际 {bspeed}"
        );
    }

    #[test]
    fn low_speed_collision_treated_as_inelastic() {
        // 低速闸（RESTITUTION_LOW_SPEED=1.0）：相对法向速度 0.9<1 时即使 e=0.9
        // 也按非弹性处理（法向清零）——拍制世界防低速微弹逐拍抖动
        let engine = bounce_engine();
        let mut world = WorldState::new();
        let mut a = make_shell(1, (5.0, 5.0), (0.8, 0.0), 0.5, 1.0);
        a.set_restitution(0.9);
        let mut b = make_shell(2, (5.6, 5.0), (-0.1, 0.0), 0.5, 1.0);
        b.set_restitution(0.9);
        let (aid, bid) = (a.id, b.id);
        world.entities.insert(aid, a);
        world.entities.insert(bid, b);
        engine.step(&mut world, &HashSet::new(), &[]);
        let a = world.entities.get(&aid).unwrap();
        // 起步即接触（距 0.6<1.0）→t=0 截断；相对法向 0.9<1 → e 生效=0 → v_n'=0，
        // 纯法向入射无切向 → 速度清零（e=0.9 的反弹被闸吞掉）
        assert_eq!(
            a.velocity(),
            (0.0, 0.0),
            "低速相对法向速度<1 应按非弹性（不反弹），实际 {:?}",
            a.velocity()
        );
    }

    #[test]
    fn ground_bounce_uses_ground_constants() {
        // 实体 e=0.6 落地（ground_e=0.0）：组合 max=0.6，垂直速度 -10 反弹 ×(-0.6)
        // →+6（高速入射前提：|v_n|=10>1 过低速闸）——地面常数对与实体声明组合
        //（Box2D"弹力球不需要弹力地板"的实体 vs 地面应用）
        let engine = bounce_engine();
        let mut world = WorldState::new();
        let mut a = make_shell(1, (50.0, 5.0), (0.0, -10.0), 0.15, 1.0);
        a.set_restitution(0.6);
        let aid = a.id;
        world.entities.insert(aid, a);
        engine.step(&mut world, &HashSet::new(), &[]);
        let a = world.entities.get(&aid).unwrap();
        assert!(
            (a.pos().1 - 0.15).abs() < 1e-9,
            "应停在地面半径线 y=0.15，实际 {:?}",
            a.pos()
        );
        assert!(
            (a.velocity().0 - 0.0).abs() < 1e-6 && (a.velocity().1 - 6.0).abs() < 1e-6,
            "垂直速度应反弹 ×(-0.6)=+6（ground_e=0 组合 max=0.6），实际 {:?}",
            a.velocity()
        );
    }

    #[test]
    fn same_pair_bounces_once_per_tick_second_contact_inelastic() {
        // 每对每拍一弹闸（D-010 细则⑤）：同拍内同一对实体（无序对键）只施加一次
        // 弹性冲量——闸防的是将来的多次接触源；本拍 A 反弹入闸后 B 的二次接触
        // 按非弹性处理（法向清零+切向×(1-f)）
        let engine = bounce_engine();
        let mut world = WorldState::new();
        let mut a = make_shell(1, (5.0, 5.0), (10.0, 0.0), 0.5, 1.0);
        a.set_restitution(0.8);
        a.set_friction(0.5);
        let mut b = make_shell(2, (8.0, 5.0), (-10.0, 0.0), 0.5, 1.0);
        b.set_restitution(0.8);
        b.set_friction(0.5);
        let (aid, bid) = (a.id, b.id);
        world.entities.insert(aid, a);
        world.entities.insert(bid, b);
        engine.step(&mut world, &HashSet::new(), &[]);
        let a = world.entities.get(&aid).unwrap();
        let b = world.entities.get(&bid).unwrap();
        // A 先结算（id 升序）：反弹 -0.8×10=-8 入闸
        assert!(
            (a.velocity().0 - (-8.0)).abs() < 1e-6,
            "A 应反弹 ×(-0.8)，实际 {:?}",
            a.velocity()
        );
        // B 二次接触（同对已在闸）：非弹性——相对法向速度 20>1 但闸吞掉 e，
        // 纯法向入射 → 速度清零（不许同拍二弹）
        assert_eq!(
            b.velocity(),
            (0.0, 0.0),
            "同对同拍二次接触应按非弹性（闸），实际 {:?}",
            b.velocity()
        );
    }

    #[test]
    fn same_state_same_impulse_result() {
        // #044 确定性锚（D-010 冲量路径形态，照 ballistic_step_same_state_same_result
        // 先例）：碰撞反弹场景（撞体+触地双分支）深拷贝世界跑两遍物理步——末态
        //（坐标/速度/存亡）逐位一致。反弹公式只读仿真状态（#044 子步宪法）。
        let engine = bounce_engine();
        let run_world = || -> WorldState {
            let mut world = WorldState::new();
            for (id, pos, vel, e, f) in [
                (1u128, (5.0, 5.0), (10.0, 3.0), 0.5, 0.5),
                (2u128, (8.0, 5.0), (-1.0, 0.0), 0.5, 0.5),
                (3u128, (50.0, 5.0), (0.0, -10.0), 0.6, 0.6),
            ] {
                let mut s = make_shell(id, pos, vel, 0.5, 1.0);
                s.set_restitution(e);
                s.set_friction(f);
                world.entities.insert(s.id, s);
            }
            // 一拍：弹1 撞体反弹；弹3 触地反弹（反弹位移下一拍兑现，两拍会飞离
            // 停点——单拍锚定反弹瞬间的诚实形态）
            engine.step(&mut world, &HashSet::new(), &[]);
            world
        };
        let wa = run_world();
        let wb = run_world();
        let mut ids: Vec<EntityId> = wa.entities.keys().copied().collect();
        ids.sort_by_key(|i| i.0);
        for id in ids {
            let (a, b) = (wa.entities.get(&id).unwrap(), wb.entities.get(&id).unwrap());
            assert_eq!(a.is_alive(), b.is_alive(), "存亡必须一致");
            assert_eq!(a.pos(), b.pos(), "坐标必须逐位一致（#044）");
            assert_eq!(a.velocity(), b.velocity(), "速度必须逐位一致（#044）");
        }
        // 场景自检：撞体/触地反弹都真实发生（锚不空转）
        let s1 = wa
            .entities
            .get(&EntityId(Uuid::from_u128(1)))
            .unwrap();
        assert!(
            s1.velocity().0 < 0.0,
            "弹1 应撞体反弹（vx 反向），实际 {:?}",
            s1.velocity()
        );
        let s3 = wa
            .entities
            .get(&EntityId(Uuid::from_u128(3)))
            .unwrap();
        assert!(
            s3.velocity().1 > 0.0 && (s3.pos().1 - 0.5).abs() < 1e-9,
            "弹3 应触地反弹停在半径线（r=0.5），实际 {:?} @ {:?}",
            s3.velocity(),
            s3.pos()
        );
    }
}
