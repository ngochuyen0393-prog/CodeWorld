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
use codeworld_core::terrain::{TERRAIN_SPACING, TERRAIN_X_MIN};
use codeworld_core::world::WorldState;
use std::collections::{HashMap, HashSet};

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

    /// 弹丸步进（#034 弹道系统：离体之物的世界物理）
    ///
    /// 【#044 子步宪法锚】N=世界状态的纯函数——本函数的"子步"形态是 CCD 扫掠：
    /// 每弹丸每拍恒定一整步积分（重力→阻力→极速钳→位移，N=1），随速度状态
    /// 自适应的是"检查精度"而非"步数"（线段扫掠精确到首次接触参数 t，极速
    /// 常数内任何速度如实结算，无需为高速体降步长）。这是宪法速度 tier 的
    /// 已然形态：精度由弹丸速度状态决定（状态驱动），同一状态必然同一结算
    /// ——两台机器看同一个世界必然同样结果（回放仲裁根基）。将来自适应真
    /// 子步（tier>1）时必须沿用本宪法：tier 由量化阈值算+滞回带防抖+换档
    /// 只在拍边界并记日志入回放流；红线=绝不用墙钟/CPU/帧率选 N（机器数据
    /// =结果分叉=回放仲裁全毁）。docs/decisions.md #044 / dev-plan D-015。
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
    /// - `fields`：本拍场清单（D-012：弹丸是场的第一消费者——护弹道场域内弹丸
    ///   按改后重力/阻力被积分；域外/空清单=世界默认，零回归锚）
    /// - 返回事件按弹丸 id 升序（确定序，主程序结算不依赖遍历随机性）
    pub fn step_projectiles(
        &self,
        world: &mut WorldState,
        current_tick: u64,
        shooters: &HashMap<EntityId, EntityId>,
        fields: &[FieldEffect],
    ) -> Vec<ProjectileEvent> {
        // 无液体委托：空清单=世界默认，弹丸行为与 D-009 前逐位一致（零回归锚）
        self.step_projectiles_with_liquids(world, current_tick, shooters, fields, &[])
    }

    /// 弹丸步进（带液体域，D-009）：域内弹丸按液体物理被结算——浮力托缓下坠
    /// （g_final）、水中阻尼（k_eff，替代重力入速度步）、附加质量（m_eff 进阻力
    /// 分母）。浸入判定用弹丸圆心+半径（f 竖径夹逼近似）；ρ物=m/πr²（标准弹
    /// r=0.15/m=1 → ρ物≈14 很稠，任何液体都缓沉不浮，物理诚实）。空清单=
    /// 无液体世界，行为与 D-009 前逐位一致（零回归锚）。
    pub fn step_projectiles_with_liquids(
        &self,
        world: &mut WorldState,
        current_tick: u64,
        shooters: &HashMap<EntityId, EntityId>,
        fields: &[FieldEffect],
        liquids: &[LiquidDomain],
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
            // 2. 重力（吃满，无 aloft 豁免；D-012 力场域内用生效重力）
            //    D-009 水中：阻尼+净加速度一步完成（v ← v×k_eff + g_final，
            //    替代重力入速度步——MC 三步乘法加法同构），附加质量进阻力分母
            // 3. 阻力定律（质量为分母；域内用生效阻力——护弹道场第一消费者）
            let (eff_gravity, eff_drag) = effective_gravity_drag(before, &self.params, fields);
            if liquids.is_empty() {
                vel.1 += eff_gravity;
                vel = apply_drag(vel, mass, eff_drag);
            } else {
                let le = liquid_effect(before, radius, mass, vel, (eff_gravity, eff_drag), liquids);
                if le.f > 0.0 {
                    tracing::debug!(
                        projectile = %pid.0,
                        f = le.f,
                        g_final = le.g_final,
                        k_eff = le.k_eff,
                        m_eff = le.m_eff,
                        "[projectile] 弹丸在水中（浮力托缓+阻尼+附加质量）"
                    );
                    vel = (vel.0 * le.k_eff, vel.1 * le.k_eff + le.g_final);
                    vel = apply_drag(vel, le.m_eff, eff_drag);
                } else {
                    vel.1 += eff_gravity;
                    vel = apply_drag(vel, mass, eff_drag);
                }
            }
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
            //    【#044 子步宪法锚·CCD 现状】这里的"扫掠检查"就是速度 tier 的
            //    已然形态：高速体自动获得与低速体同级的命中精度（精确到 t），
            //    且判据是纯几何——只读弹丸速度/位置/目标身体，不读任何机器
            //    数据（墙钟/CPU/帧率），同状态必然同结算（#044 纯函数要求，
            //    同拍同状态锚测试 step_projectiles_same_state_same_result）。
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
            // 触地判定（D-016 地形版）：弹道路径与地形面（高度数组线性插值）
            // 的首次接触——逐段解析查表 O(路径跨格数)，替代旧"半径线 y=r"
            // 平面交点（平地是该查表的退化特例：全零高度数组）。起步已在
            // 地下（异常态兜底）由查表返回 t=0 立即触地。
            let ground_t: Option<f64> = world.terrain.segment_first_contact(before, after, radius);
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
            // 6. 落地：路径先触地则结算于触地点——落点=路径与地形面的交点
            //（x 取交点插值、y=ground(x)+radius 弹心贴面），不再是拍末积分位置
            //（高速弹穿地后拉回的旧口径同时退役）；P2 顺手修沿用：尸体坐标同步
            // 钳到落点，地下异态不留在世界
            if let Some(gt) = ground_t {
                let land_x = before.0 + (after.0 - before.0) * gt;
                let land = (land_x, world.terrain.height(land_x) + radius);
                if let Some(p) = world.entities.get_mut(&pid) {
                    p.set_pos(land);
                    p.kill();
                }
                tracing::debug!(projectile = %pid.0, pos = ?land, "[projectile] 触地（CCD 地形交点，D-016）");
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
    /// - 弹丸（#034）：跳过——弹丸是速度积分制物体，由 step_projectiles 接管；
    ///   本步的位置重力制（落到半径线）会瞬移弹丸
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
        // 实体 id 升序（裁决确定序）
        let mut ids: Vec<EntityId> = world.entities.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            // 先读实体数据（落下借再用），落点计算借 world 不可变引用
            let (alive, aloft_skip, is_creation, is_dyn_creation, is_proj, before, me_r, me_mass, me_vel) = {
                let Some(entity) = world.entities.get(&id) else {
                    continue;
                };
                (
                    entity.is_alive(),
                    aloft.contains(&id),
                    entity.code_hash == codeworld_core::collision::CREATION_CODE,
                    codeworld_core::collision::is_dynamic_creation(entity),
                    codeworld_core::collision::is_projectile(entity),
                    entity.pos(),
                    entity.radius(),
                    entity.mass(),
                    entity.velocity(),
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
                // 落地解析（脚下高度核复用 circle_landing_y 的几何）：位移后不高于
                // 脚下高度即落地——贴面 + 速度全清零（vy 清零规则：落地时清、空中
                // 不清；水平分量一并清——贴地滑行白拿位移不合法，想平移走 move_by）
                let surface = self.circle_landing_surface(world, id, after.0, me_r, after.1);
                if after.1 <= surface {
                    tracing::debug!(
                        entity_id = %id.0,
                        from = ?before,
                        to = ?(after.0, surface),
                        "[step] 冲量路径落地贴面（速度清零）"
                    );
                    ((after.0, surface), Some((0.0, 0.0)))
                } else {
                    (after, Some(vel))
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
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
        assert!(events.is_empty(), "前两拍无事件（还在飞）");
        assert!(near(world.entities.get(&pid).unwrap().pos(), (3.8, 10.0)));
        engine.step_projectiles(&mut world, 2, &HashMap::new(), &[]);
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
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
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
        let events = engine.step_projectiles(&mut world, PROJECTILE_MAX_AGE_TICKS + 1, &HashMap::new(), &[]);
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
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
        assert!(events.is_empty(), "第一拍未接触");
        let events = engine.step_projectiles(&mut world, 2, &HashMap::new(), &[]);
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
            let events = engine.step_projectiles(&mut world, t, &shooters, &[]);
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
            let events = engine.step_projectiles(&mut world, t, &HashMap::new(), &[]);
            assert!(events.is_empty(), "第 {t} 拍弹丸互穿不应有事件");
        }
        // 第三拍双双坠地（Landed），但仍无任何 HitBody（互穿语义）
        let events = engine.step_projectiles(&mut world, 3, &HashMap::new(), &[]);
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
        engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
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
        engine.step(&mut world, &HashSet::new(), &[]);
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
            let events = engine.step_projectiles(&mut world, t, &HashMap::new(), &[]);
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
            engine.step_projectiles(&mut world, t, &HashMap::new(), &[]);
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
            engine.step_projectiles(&mut world, t, &HashMap::new(), &[]);
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
        engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
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
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
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
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
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
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
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
        let _ = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
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
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ProjectileEvent::HitBody { target: t, .. } => assert_eq!(*t, tid, "先触身体应撞体结算"),
            other => panic!("应撞体事件（t 先于触地），实际 {other:?}"),
        }
    }

    // ===== #044 子步宪法：确定性测试锚（D-015 法典化）=====
    //
    // 宪法要求（decisions #044）：N=世界状态的纯函数——同状态必然同 N 同结算；
    // 红线=绝不用墙钟/CPU/帧率选 N。现状三处子步形态（CCD 扫掠/预览采样/弹道
    // 积分）全部是"状态驱动"实现，以下锚测试把宪法条款钉进可执行断言。

    #[test]
    fn step_projectiles_same_state_same_result() {
        // #044 纯函数锚：同一初始状态（实体集合+速度+拍号一致，HashMap 遍历序
        // 不同）跑两遍弹道步——事件序列与末态坐标必须逐位一致。
        // 这是"N=世界状态纯函数"的可执行形态：结算只依赖世界状态，不依赖
        // 容器遍历序/机器/时间。将来 tier>1 自适应子步落地时本锚仍然必须绿
        // （tier 只能读上拍状态，两遍跑看到同一状态必然同 tier）。
        let engine = DefaultPhysicsEngine::new();
        let mut world_a = WorldState::new();
        let mut world_b = WorldState::new();
        // 多弹丸多目标混合场景：一条平射弹道（vy=9.8 恰抵消重力，本拍水平
        // 穿越）撞中途目标 + 一条上抛弹道落地，覆盖 HitBody/Landed 分支与
        // EntityId 定序
        for (id, pos, vel) in [
            (1u128, (0.0, 20.0), (12.0, 9.8)),  // 平射→撞 (6,20) 目标
            (2u128, (30.0, 1.0), (3.0, 6.0)),   // 上抛→落地
        ] {
            let a = make_projectile(id, pos, vel, 0);
            let b = make_projectile(id, pos, vel, 0);
            world_a.entities.insert(a.id, a);
            world_b.entities.insert(b.id, b);
        }
        for (id, pos) in [(3u128, (6.0, 20.0)), (4u128, (50.0, 20.0))] {
            let a = make_entity(id, pos.0, pos.1, 0);
            let b = make_entity(id, pos.0, pos.1, 0);
            world_a.entities.insert(a.id, a);
            world_b.entities.insert(b.id, b);
        }
        // shooters 表两遍共用（实现只 .get 点查不遍历，无关序）
        let mut shooters = HashMap::new();
        shooters.insert(EntityId(Uuid::from_u128(1)), EntityId(Uuid::from_u128(9)));
        let ev_a = engine.step_projectiles(&mut world_a, 1, &shooters, &[]);
        let ev_b = engine.step_projectiles(&mut world_b, 1, &shooters, &[]);
        // 事件序列逐位一致（ProjectileEvent 派生 PartialEq；含命中点坐标）
        assert_eq!(ev_a, ev_b, "同状态必然同事件序列（#044 纯函数要求）");
        assert!(!ev_a.is_empty(), "场景应产生事件（撞体/落地），空则锚退化");
        // 末态逐位一致：每个实体坐标与存亡
        let mut ids: Vec<EntityId> = world_a.entities.keys().copied().collect();
        ids.sort_by_key(|i| i.0);
        for id in ids {
            let (a, b) = (
                world_a.entities.get(&id).unwrap(),
                world_b.entities.get(&id).unwrap(),
            );
            assert_eq!(a.is_alive(), b.is_alive(), "存亡必须一致");
            assert_eq!(a.pos(), b.pos(), "坐标必须逐位一致（#044）");
        }
    }

    #[test]
    fn step_projectiles_result_independent_of_hashmap_iteration_order() {
        // #044 机器数据红线锚（对照组）：同上测试的场景构造序反过来——
        // 先插目标后插弹丸、ids 收集后统一排序——结算必须与正序构造完全一致。
        // step_projectiles 内部对弹丸/目标均按 EntityId 升序定序（确定序），
        // 实体表是 HashMap，插入序≠遍历序：本锚证明结算与遍历序无关，
        // 即"N 的决定规则只读世界状态"在实现层成立。
        let engine = DefaultPhysicsEngine::new();
        let run = |insert_targets_first: bool| -> (Vec<ProjectileEvent>, Vec<(EntityId, (f64, f64), bool)>) {
            let mut world = WorldState::new();
            let targets: Vec<Entity> = [(3u128, 6.0, 20.0), (4u128, 50.0, 20.0)]
                .into_iter()
                .map(|(id, x, y)| make_entity(id, x, y, 0))
                .collect();
            let projs: Vec<Entity> = [
                (1u128, (0.0, 20.0), (12.0, 9.8)),
                (2u128, (30.0, 1.0), (3.0, 6.0)),
            ]
            .into_iter()
            .map(|(id, pos, vel)| make_projectile(id, pos, vel, 0))
            .collect();
            if insert_targets_first {
                for e in targets {
                    world.entities.insert(e.id, e);
                }
                for e in projs {
                    world.entities.insert(e.id, e);
                }
            } else {
                for e in projs {
                    world.entities.insert(e.id, e);
                }
                for e in targets {
                    world.entities.insert(e.id, e);
                }
            }
            let ev = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
            let mut fin: Vec<(EntityId, (f64, f64), bool)> = world
                .entities
                .iter()
                .map(|(id, e)| (*id, e.pos(), e.is_alive()))
                .collect();
            fin.sort_by_key(|(id, _, _)| id.0);
            (ev, fin)
        };
        let (ev1, fin1) = run(false);
        let (ev2, fin2) = run(true);
        assert_eq!(ev1, ev2, "事件序列与构造序无关（EntityId 定序）");
        assert_eq!(fin1, fin2, "末态与构造序无关（#044：结算只读状态）");
        assert!(!ev1.is_empty(), "场景应产生事件，空则锚退化");
    }

    #[test]
    fn step_projectiles_swept_check_pure_geometry_no_wall_clock() {
        // #044 红线锚：CCD 扫掠判据=纯几何——同一（位置，速度）对在极速内
        // 取任意两档速度，命中判定只由几何决定且单调（路径穿过目标必命中，
        // 与跑多少次/何时跑无关）。本锚用"高速漏检 Impossible"钉死：
        // 极速内慢速 4 格/拍命中路径中点目标，高速 20 格/拍同样必命中
        // （旧点检查在 20 格/拍会漏——隧穿；CCD 扫掠两档都中）。
        // 若有人把判据改成读机器数据（帧率/负载），两档结果会出现非几何
        // 相关的漂移，本锚（连同上面的同状态锚）会红。
        let engine = straight_engine();
        for speed in [4.0f64, 20.0f64] {
            let mut world = WorldState::new();
            let target = make_entity(2, 2.0, 20.0, 0);
            let tid = target.id;
            world.entities.insert(tid, target);
            let p = make_projectile(1, (0.0, 20.0), (speed, 0.0), 0);
            world.entities.insert(p.id, p);
            let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &[]);
            assert_eq!(
                events.len(),
                1,
                "速度 {speed} 格/拍：路径上的目标必命中（CCD 纯几何判据）"
            );
            assert!(
                matches!(&events[0], ProjectileEvent::HitBody { target: t, .. } if *t == tid),
                "速度 {speed} 应命中目标"
            );
        }
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
    fn d016_projectile_lands_on_mountain_slope() {
        // D-016：弹道 CCD 落点查表——撞山坡的弹丸落点贴地形面（y=ground+r）
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
        let p = make_projectile(1, start, (10.0, -12.0), 0);
        let pid = p.id;
        let r = p.radius(); // PROJECTILE_RADIUS 常量
        world.entities.insert(pid, p);
        let mut landed = None;
        for tick in 1..=30 {
            let events = engine.step_projectiles(&mut world, tick, &HashMap::new(), &[]);
            if let Some(ProjectileEvent::Landed { point, .. }) = events.first() {
                landed = Some(*point);
                break;
            }
        }
        let (lx, ly) = landed.expect("30 拍内必撞坡落地");
        let ground = world.terrain.height(lx);
        assert!(
            (ly - ground - r).abs() < 1e-6,
            "落点弹心应贴地形面（ground+{r}），实际 ({lx},{ly}) ground={ground}"
        );
        assert!(lx < peak_x + 1.0, "应在山坡上落地（未过峰），实际 lx={lx}");
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
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut c = make_dyn_creation(1, 0.0, 0.5);
        let id = c.id;
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
    fn step_projectiles_field_changes_trajectory() {
        // 弹道场第一消费者锚：零重力场域内弹丸不吃重力（vy 不累积），对照弹丸
        //（域外）照常下坠——护弹道场的直接作用
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut in_proj = make_entity(1, 5.0, 10.0, 0);
        in_proj.set_velocity((3.0, 0.0));
        in_proj.set_radius(0.15);
        in_proj.code_hash = codeworld_core::collision::PROJECTILE_CODE.to_string();
        let mut out_proj = make_entity(2, 60.0, 10.0, 0);
        out_proj.set_velocity((3.0, 0.0));
        out_proj.set_radius(0.15);
        out_proj.code_hash = codeworld_core::collision::PROJECTILE_CODE.to_string();
        let id_in = in_proj.id;
        let id_out = out_proj.id;
        world.entities.insert(id_in, in_proj);
        world.entities.insert(id_out, out_proj);
        let fields = [crate::fields::FieldEffect {
            source_id: EntityId(Uuid::from_u128(9)),
            pos: (5.0, 10.0),
            radius: 3.0,
            gravity_scale: 0.0,
            drag_delta: 0.0,
        }];
        let events = engine.step_projectiles(&mut world, 1, &HashMap::new(), &fields);
        assert!(
            events.is_empty(),
            "两弹丸域内/域外都不应撞地/撞体，实际 {events:?}"
        );
        assert!(
            (world.entities.get(&id_in).unwrap().velocity().1).abs() < 1e-12,
            "域内弹丸 vy 不累积（零重力场）"
        );
        let vy_out = world.entities.get(&id_out).unwrap().velocity().1;
        assert!(
            (vy_out + 9.8 * 0.95).abs() < 1e-12,
            "域外弹丸照常吃重力（阻力后 -9.8×0.95），实际 {vy_out}"
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
        // 弹丸入水改弹道锚（本刀可见效果之一）：标准弹（r=0.15/m=1 →
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
        let events = engine.step_projectiles_with_liquids(&mut world, 1, &HashMap::new(), &[], &liquids);
        assert!(events.is_empty(), "两弹都还在飞，实际 {events:?}");
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
}
