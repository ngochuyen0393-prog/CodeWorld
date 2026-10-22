//! 碰撞判定（decisions.md #030 半径物理：格子降级为地址系统，实体不可重叠精确化为半径碰撞）
//!
//! 身体形状二分（Factorio 模型）：活动实体（player/npc）= 圆（radius，圆心即 pos）；
//! 静态造物（creation）= 格对齐 AABB（由占地格推导，MVP 单格 1×1，多格留接口）。
//! 判据约定：
//! - 重叠（挡移动/挡出生）：严格 <（恰好相切不算重叠，可以贴着站）
//! - 接触（开沙箱通道）：≤（相切即接触）
//! - 点命中（attack_at）：圆=距圆心 < r 严格；AABB=闭区间包含
//!
//! 全部用平方距离，免开方。

use crate::entity::Entity;

/// 创造物 code_hash 标识（process_spawns 写入，本模块/net.rs 共用同一常量）
pub const CREATION_CODE: &str = "creation";

/// 弹丸 code_hash 标识（历史实体兼容判定：#045/D-018 后世界不再产生 PROJECTILE_CODE
/// 实体，shoot import 已退役——该 code_hash 仅兼容历史快照反序列化，恢复即清场
///（弹丸不跨重启：main() 快照恢复按哑弹 retain 清场），运行时无活旧弹丸；
/// 本模块/net.rs 共用同一常量，kind=projectile 前端分支纯协议历史兼容）
pub const PROJECTILE_CODE: &str = "projectile";

/// 造物是否声明为全活 dynamic（D-003 motion 第 10 参：motion=1 的 creation
/// 身体=声明 radius 的圆，与生物同物理管线；static 造物仍格对齐 AABB）。
/// 判据与 is_projectile 同款集中在常量旁；kinematic 缓立不在此判。
pub fn is_dynamic_creation(e: &Entity) -> bool {
    e.code_hash == CREATION_CODE && e.motion() == crate::entity::MOTION_DYNAMIC
}

/// 按声明值判 dynamic 造物（出生预检伴生：spawn 阶段二实体未建，只有 motion 值；
/// 调用方语境已定 code_hash=creation，此处只比档位）
pub fn is_dynamic_creation_candidate(motion: i32) -> bool {
    motion == crate::entity::MOTION_DYNAMIC
}

/// 贴身刀判距冗余（#034 attack_at 接触化）：出手判距 = r攻 + r目 + 0.5。
/// 这 0.5 是"胳膊长度"物理判距常量，不是射程属性（射程不立法，全部物理涌现——
/// design-ballistics.md 否决清单：attack_at 加射程属性已被土豆否决）
pub const MELEE_REACH: f64 = 0.5;

/// 定装小弹半径（历史名 PROJECTILE_RADIUS，#034 弹丸退役 #045/D-018 后数值
/// 继续沿用：操控定装弹（#036 驾驶档）与投掷器模板第一课的"同款小弹"尺寸锚
/// ——0.15 不再是世界常数而是教学/定装口径；造物弹半径本由 spawn 第 7 参
/// 自定，此值=文档/解算器/测试共用的参照值）
pub const PROJECTILE_RADIUS: f64 = 0.15;

/// 实体是否是弹丸（历史实体兼容判定，D-018 后新世界不产生——仅兼容历史快照
/// 反序列化，恢复即清场，运行时无活旧弹丸；排除语义保留为防御性守卫+测试
/// 构造兼容锚：不可锁定/不可被撞/不参与接触）
pub fn is_projectile(e: &Entity) -> bool {
    e.code_hash == PROJECTILE_CODE
}

/// 贴身刀判据（#034 接触化）：dist(攻,目) < r攻 + r目 + MELEE_REACH，严格 <。
/// 恰好等于判距 = 落空（与重叠判据同款严格约定，审查锚定）
pub fn melee_in_reach(a_pos: (f64, f64), a_r: f64, b_pos: (f64, f64), b_r: f64) -> bool {
    let dx = a_pos.0 - b_pos.0;
    let dy = a_pos.1 - b_pos.1;
    let s = a_r + b_r + MELEE_REACH;
    dx * dx + dy * dy < s * s
}

/// 身体形状：世界里的实体要么是圆（活动实体），要么是格对齐方块（造物）
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    /// 圆：圆心即实体 pos，radius 为身体半径
    Circle { center: (f64, f64), radius: f64 },
    /// 格对齐 AABB：min 左下角、max 右上角（世界坐标）
    Aabb { min: (f64, f64), max: (f64, f64) },
}

/// creation 身体推导：由其占地格生成 1×1 格对齐 AABB（pos 所在格 floor(x)..floor(x)+1）。
/// 多格造物（N×M 占地）留接口：届时由占地格集合推导并集，MVP 塔=1×1。
pub fn creation_aabb(pos: (f64, f64)) -> ((f64, f64), (f64, f64)) {
    let cx = pos.0.floor();
    let cy = pos.1.floor();
    ((cx, cy), (cx + 1.0, cy + 1.0))
}

/// 实体 -> 身体（creation：static=格对齐 AABB / dynamic（D-003）=声明 radius 的圆；
/// 其余=圆）
pub fn body_of(e: &Entity) -> Body {
    if e.code_hash == CREATION_CODE && !is_dynamic_creation(e) {
        let (min, max) = creation_aabb(e.pos());
        Body::Aabb { min, max }
    } else {
        Body::Circle {
            center: e.pos(),
            radius: e.radius(),
        }
    }
}

/// 圆-圆重叠：dist² < (a_r+b_r)²（恰好相切=距离恰为半径和，不算重叠）
pub fn circle_overlap(a_pos: (f64, f64), a_r: f64, b_pos: (f64, f64), b_r: f64) -> bool {
    let dx = a_pos.0 - b_pos.0;
    let dy = a_pos.1 - b_pos.1;
    let d2 = dx * dx + dy * dy;
    let s = a_r + b_r;
    d2 < s * s
}

/// 圆-AABB 重叠（MDN 教科书法）：钳制圆心到盒内最近点，距离 < r 即重叠
pub fn circle_aabb_overlap(
    c_pos: (f64, f64),
    r: f64,
    aabb_min: (f64, f64),
    aabb_max: (f64, f64),
) -> bool {
    let (dx, dy) = clamp_point_to_aabb_dist(c_pos, aabb_min, aabb_max);
    dx * dx + dy * dy < r * r
}

/// AABB-AABB 重叠（严格：恰好贴边不算重叠）
pub fn aabb_overlap(
    a_min: (f64, f64),
    a_max: (f64, f64),
    b_min: (f64, f64),
    b_max: (f64, f64),
) -> bool {
    a_min.0 < b_max.0 && b_min.0 < a_max.0 && a_min.1 < b_max.1 && b_min.1 < a_max.1
}

/// 任意两身体是否重叠（移动裁决/出生预检判据，严格 <）
pub fn body_overlap(a: &Body, b: &Body) -> bool {
    match (a, b) {
        (
            Body::Circle {
                center: ca,
                radius: ra,
            },
            Body::Circle {
                center: cb,
                radius: rb,
            },
        ) => circle_overlap(*ca, *ra, *cb, *rb),
        (
            Body::Circle {
                center: c,
                radius: r,
            },
            Body::Aabb { min, max },
        )
        | (
            Body::Aabb { min, max },
            Body::Circle {
                center: c,
                radius: r,
            },
        ) => circle_aabb_overlap(*c, *r, *min, *max),
        (Body::Aabb { min: amin, max: amax }, Body::Aabb { min: bmin, max: bmax }) => {
            aabb_overlap(*amin, *amax, *bmin, *bmax)
        }
    }
}

/// 圆-圆相触：dist² ≤ (a_r+b_r)²（相切即接触，接触通道判据）
pub fn circle_touching(a_pos: (f64, f64), a_r: f64, b_pos: (f64, f64), b_r: f64) -> bool {
    let dx = a_pos.0 - b_pos.0;
    let dy = a_pos.1 - b_pos.1;
    let d2 = dx * dx + dy * dy;
    let s = a_r + b_r;
    d2 <= s * s
}

/// 圆-AABB 相触：钳制点距离 ≤ r
pub fn circle_aabb_touching(
    c_pos: (f64, f64),
    r: f64,
    aabb_min: (f64, f64),
    aabb_max: (f64, f64),
) -> bool {
    let (dx, dy) = clamp_point_to_aabb_dist(c_pos, aabb_min, aabb_max);
    dx * dx + dy * dy <= r * r
}

/// AABB-AABB 相触：区间 ≤（贴边即接触）
pub fn aabb_touching(
    a_min: (f64, f64),
    a_max: (f64, f64),
    b_min: (f64, f64),
    b_max: (f64, f64),
) -> bool {
    a_min.0 <= b_max.0 && b_min.0 <= a_max.0 && a_min.1 <= b_max.1 && b_min.1 <= a_max.1
}

/// 任意两身体是否相触（接触通道判据，≤）
pub fn body_touching(a: &Body, b: &Body) -> bool {
    match (a, b) {
        (
            Body::Circle {
                center: ca,
                radius: ra,
            },
            Body::Circle {
                center: cb,
                radius: rb,
            },
        ) => circle_touching(*ca, *ra, *cb, *rb),
        (
            Body::Circle {
                center: c,
                radius: r,
            },
            Body::Aabb { min, max },
        )
        | (
            Body::Aabb { min, max },
            Body::Circle {
                center: c,
                radius: r,
            },
        ) => circle_aabb_touching(*c, *r, *min, *max),
        (Body::Aabb { min: amin, max: amax }, Body::Aabb { min: bmin, max: bmax }) => {
            aabb_touching(*amin, *amax, *bmin, *bmax)
        }
    }
}

/// 点在体内（attack_at #031 点命中判据）：圆=距圆心 < r 严格；AABB=闭区间包含（点在边上也算）
pub fn point_in_body(p: (f64, f64), body: &Body) -> bool {
    match body {
        Body::Circle { center, radius } => {
            let dx = p.0 - center.0;
            let dy = p.1 - center.1;
            dx * dx + dy * dy < radius * radius
        }
        Body::Aabb { min, max } => {
            min.0 <= p.0 && p.0 <= max.0 && min.1 <= p.1 && p.1 <= max.1
        }
    }
}

/// 半径校验钳制（spawn 第 7 参口径）：0.05 ≤ radius ≤ 25.0，超界 warn 并钳制；
/// 非有限值（NaN/无穷）回退默认 0.5 并 warn。
pub fn clamp_radius(r: f64) -> f64 {
    const MIN: f64 = 0.05;
    const MAX: f64 = 25.0;
    const DEFAULT: f64 = 0.5;
    if !r.is_finite() {
        tracing::warn!(value = r, "[radius] 非有限值，回退默认 0.5");
        return DEFAULT;
    }
    if r < MIN {
        tracing::warn!(value = r, clamped = MIN, "[radius] 低于下限，钳制");
        return MIN;
    }
    if r > MAX {
        tracing::warn!(value = r, clamped = MAX, "[radius] 超过上限，钳制");
        return MAX;
    }
    r
}

/// 质量校验钳制（#034 spawn 第 8 参 / shoot 第 3 参口径）：0.05 ≤ mass ≤ 100，
/// 超界 warn 并钳制；非有限值（NaN/无穷）回退默认 1.0 并 warn。
/// 这是除零护栏（阻力定律 mass 做分母），不是玩法裁定（#034 立法第 4 条）。
pub fn clamp_mass(m: f64) -> f64 {
    const MIN: f64 = 0.05;
    const MAX: f64 = 100.0;
    const DEFAULT: f64 = 1.0;
    if !m.is_finite() {
        tracing::warn!(value = m, "[mass] 非有限值，回退默认 1.0");
        return DEFAULT;
    }
    if m < MIN {
        tracing::warn!(value = m, clamped = MIN, "[mass] 低于下限，钳制");
        return MIN;
    }
    if m > MAX {
        tracing::warn!(value = m, clamped = MAX, "[mass] 超过上限，钳制");
        return MAX;
    }
    m
}

/// 圆心到 AABB 的最近点偏移（dx, dy）——圆-AABB 判定的共享实现：
/// 圆心在盒内 → (0,0)（必然重叠）；盒外 → 最近点与圆心的差向量。
fn clamp_point_to_aabb_dist(
    c_pos: (f64, f64),
    aabb_min: (f64, f64),
    aabb_max: (f64, f64),
) -> (f64, f64) {
    let cx = c_pos.0.clamp(aabb_min.0, aabb_max.0);
    let cy = c_pos.1.clamp(aabb_min.1, aabb_max.1);
    (c_pos.0 - cx, c_pos.1 - cy)
}

/// CCD 线段-圆首触参数（#034 修订二：路径扫掠）。
/// 线段 a->b 与圆（center, r）的扫掠重叠判据：路径上任一点距圆心 < r。
/// 返回 Some(t)：首触的线段参数（0=起点即重叠，1=终点恰触）；
/// None：路径未扫过身体。
/// 起点已在重叠区：**仅当正在接近**（速度朝圆心，f·d<0）才返回 0——
/// 重叠但分离中（D-009 浮体上浮穿过沉体的涌现场景）不拦截，否则浮沉
/// 物理被截断卡死（D-018 撞体截断接入 step 后实测：木球贴铁球出生、
/// 上浮每拍被 Some(0.0) 清零速度，浮沉涌现消失）。
pub fn segment_first_contact_circle(
    a: (f64, f64),
    b: (f64, f64),
    center: (f64, f64),
    r: f64,
) -> Option<f64> {
    // 解 |P(t) - C|² = r²，P(t) = a + t·d：标准二次方程
    let d = (b.0 - a.0, b.1 - a.1);
    let f = (a.0 - center.0, a.1 - center.1);
    let qa = d.0 * d.0 + d.1 * d.1;
    let qb = 2.0 * (f.0 * d.0 + f.1 * d.1);
    let qc = f.0 * f.0 + f.1 * f.1 - r * r;
    if qc < 0.0 {
        // 起点已在重叠区（严格 < 判据下的内部）：严格分离中（f·d>0，远离圆心）
        // 放行；在内部横穿（f·d≤0，含零位移）=撞内壁，t=0
        return if qb > 0.0 && qa > 0.0 { None } else { Some(0.0) };
    }
    if qa <= 0.0 {
        // 零长度线段：起点不在重叠区（qc >= 0）则无接触
        return None;
    }
    let disc = qb * qb - 4.0 * qa * qc;
    if disc <= 0.0 {
        return None; // 不相交或恰好擦边（严格判据：擦边不算命中）
    }
    let t = (-qb - disc.sqrt()) / (2.0 * qa);
    if (0.0..=1.0).contains(&t) {
        Some(t)
    } else {
        None
    }
}

/// CCD 线段-扩展 AABB 首触参数（#034 修订二：造物判据）。
/// 把 AABB 四周扩展 r（Minkowski 膨胀近似：角部以方角代替圆角，最大偏差
/// r·(√2-1)≈0.06 格，弹丸 r=0.15 尺度下不可感知），线段与扩展盒做 slab 相交，
/// 返回首触参数 t（0=起点即在内）；None=路径未扫到。
pub fn segment_first_contact_expanded_aabb(
    a: (f64, f64),
    b: (f64, f64),
    aabb_min: (f64, f64),
    aabb_max: (f64, f64),
    r: f64,
) -> Option<f64> {
    // 起点在扩展盒内 → 立即接触
    let inside = a.0 >= aabb_min.0 - r
        && a.0 <= aabb_max.0 + r
        && a.1 >= aabb_min.1 - r
        && a.1 <= aabb_max.1 + r;
    if inside {
        return Some(0.0);
    }
    // slab 法：两轴分别解进入/离开参数区间，交集非空 = 相交
    let d = (b.0 - a.0, b.1 - a.1);
    let mut tmin = 0.0f64;
    let mut tmax = 1.0f64;
    for axis in 0..2 {
        let (p, dd, lo, hi) = match axis {
            0 => (a.0, d.0, aabb_min.0 - r, aabb_max.0 + r),
            _ => (a.1, d.1, aabb_min.1 - r, aabb_max.1 + r),
        };
        if dd.abs() < f64::EPSILON {
            // 该轴无位移：起点不在该轴区间内则永不相交
            if p < lo || p > hi {
                return None;
            }
        } else {
            let mut t1 = (lo - p) / dd;
            let mut t2 = (hi - p) / dd;
            if t1 > t2 {
                std::mem::swap(&mut t1, &mut t2);
            }
            tmin = tmin.max(t1);
            tmax = tmax.min(t2);
        }
    }
    if tmin <= tmax {
        Some(tmin)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{Appearance, Entity, PlayerId};
    use uuid::Uuid;

    fn make_entity(id_u: u128, code_hash: &str, pos: (f64, f64)) -> Entity {
        Entity::new(
            crate::entity::EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            code_hash.to_string(),
            pos,
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    // === circle_overlap（严格 <）===

    #[test]
    fn circle_overlap_separated_false() {
        // 分离：距离 2.0 远大于半径和 1.0
        assert!(!circle_overlap((0.0, 0.0), 0.5, (2.0, 0.0), 0.5));
    }

    #[test]
    fn circle_overlap_exact_touch_false() {
        // 边界：恰好相切（距离=半径和）不算重叠（严格 <，#030 定案）
        assert!(!circle_overlap((0.0, 0.0), 0.5, (1.0, 0.0), 0.5));
    }

    #[test]
    fn circle_overlap_slightly_overlapping_true() {
        assert!(circle_overlap((0.0, 0.0), 0.5, (0.9, 0.0), 0.5));
    }

    #[test]
    fn circle_overlap_contained_true() {
        // 包含：小圆完全在大圆内（距离远小于半径和）
        assert!(circle_overlap((0.0, 0.0), 3.0, (0.5, 0.0), 0.5));
    }

    #[test]
    fn circle_overlap_same_pos_true() {
        assert!(circle_overlap((1.0, 1.0), 0.5, (1.0, 1.0), 0.5));
    }

    #[test]
    fn circle_overlap_diagonal_uses_squared_distance() {
        // 斜向：距离 sqrt(2)≈1.414 < 1.5 重叠；dist² 用平方距离免开方
        assert!(circle_overlap((0.0, 0.0), 0.75, (1.0, 1.0), 0.75));
        assert!(!circle_overlap((0.0, 0.0), 0.7, (1.0, 1.0), 0.7));
    }

    // === circle_aabb_overlap（钳制点法）===

    #[test]
    fn circle_aabb_center_inside_true() {
        // 圆心在盒内：最近点=自身，距离 0 < r 必然重叠
        assert!(circle_aabb_overlap((0.5, 0.5), 0.5, (0.0, 0.0), (1.0, 1.0)));
    }

    #[test]
    fn circle_aabb_face_overlap_true() {
        // 正对一条边：圆心 (1.4, 0.5) 距盒右边缘 0.4 < 0.5
        assert!(circle_aabb_overlap((1.4, 0.5), 0.5, (0.0, 0.0), (1.0, 1.0)));
    }

    #[test]
    fn circle_aabb_corner_overlap_true() {
        // 角：圆心 (1.3, 1.3) 距角点 (1,1) 距离 sqrt(0.18)≈0.424 < 0.5
        assert!(circle_aabb_overlap((1.3, 1.3), 0.5, (0.0, 0.0), (1.0, 1.0)));
    }

    #[test]
    fn circle_aabb_exact_edge_touch_false() {
        // 边界：圆心在盒顶边正上方恰 0.5（(0.5,1.5) 距夹持点 (0.5,1.0) 恰 0.5）：
        // 不算重叠（严格 <），算接触（≤）。0.5 二进制精确；斜角恰切有浮点噪声不做恰切断言
        assert!(!circle_aabb_overlap((0.5, 1.5), 0.5, (0.0, 0.0), (1.0, 1.0)));
        assert!(circle_aabb_touching((0.5, 1.5), 0.5, (0.0, 0.0), (1.0, 1.0)));
    }

    #[test]
    fn circle_aabb_far_false() {
        assert!(!circle_aabb_overlap((5.0, 5.0), 0.5, (0.0, 0.0), (1.0, 1.0)));
    }

    // === aabb_overlap（严格）===

    #[test]
    fn aabb_overlap_separated_false() {
        assert!(!aabb_overlap((0.0, 0.0), (1.0, 1.0), (2.0, 0.0), (3.0, 1.0)));
    }

    #[test]
    fn aabb_overlap_edge_touch_false() {
        // 恰好贴边（x 区间 [0,1] 与 [1,2] 共享边界）不算重叠
        assert!(!aabb_overlap((0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (2.0, 1.0)));
    }

    #[test]
    fn aabb_overlap_real_true() {
        assert!(aabb_overlap((0.0, 0.0), (1.0, 1.0), (0.5, 0.5), (1.5, 1.5)));
    }

    // === body dispatch（creation=AABB / 其余=圆）===

    #[test]
    fn creation_body_is_cell_aligned_aabb() {
        // creation 身体=占地格 1×1：pos (10.0, 0.0) → AABB [10,11]×[0,1]
        let e = make_entity(1, "creation", (10.0, 0.0));
        assert_eq!(
            body_of(&e),
            Body::Aabb {
                min: (10.0, 0.0),
                max: (11.0, 1.0)
            }
        );
    }

    #[test]
    fn creation_body_floor_negative_coord() {
        // 负坐标取整：pos (-0.3, -0.7) → 格 (-1,-1)
        let e = make_entity(2, "creation", (-0.3, -0.7));
        assert_eq!(
            body_of(&e),
            Body::Aabb {
                min: (-1.0, -1.0),
                max: (0.0, 0.0)
            }
        );
    }

    #[test]
    fn activity_body_is_circle_with_radius() {
        let mut e = make_entity(3, "attacker", (5.0, 0.5));
        e.set_radius(0.7);
        assert_eq!(
            body_of(&e),
            Body::Circle {
                center: (5.0, 0.5),
                radius: 0.7
            }
        );
    }

    // === body dispatch（D-003：dynamic creation=声明 radius 的圆）===

    #[test]
    fn dynamic_creation_body_is_declared_radius_circle() {
        // D-003 motion=1：全活造物身体=声明 radius 的圆（连续坐标不取整，
        // 与生物同款），不再是格对齐 AABB
        let mut e = make_entity(4, "creation", (10.3, 2.7));
        e.set_radius(0.8);
        e.set_motion(crate::entity::MOTION_DYNAMIC);
        assert!(is_dynamic_creation(&e), "motion=1 的 creation 应判 dynamic");
        assert_eq!(
            body_of(&e),
            Body::Circle {
                center: (10.3, 2.7),
                radius: 0.8
            }
        );
    }

    #[test]
    fn static_creation_body_stays_aabb() {
        // 默认档（motion=0）造物身体不变：格对齐 AABB（现状零回归锚）
        let e = make_entity(5, "creation", (10.3, 2.7));
        assert!(!is_dynamic_creation(&e));
        assert_eq!(
            body_of(&e),
            Body::Aabb {
                min: (10.0, 2.0),
                max: (11.0, 3.0)
            }
        );
    }

    #[test]
    fn dynamic_creation_circle_overlaps_circle_not_aabb_semantics() {
        // 行为锚：同坐标下 dynamic 造物（r=0.5）与生物圆按圆-圆判——
        // 距 1.0 恰好相切不重叠（判据严格 <，两 r=0.5 的圆相切距离=r和=1.0），
        // 距 0.99 重叠；若误走 AABB 语义（1×1 盒）距 1.0 早已重叠，断言即红。
        let mut dc = make_entity(6, "creation", (0.0, 0.5));
        dc.set_motion(crate::entity::MOTION_DYNAMIC);
        let mut mob = make_entity(7, "attacker", (1.0, 0.5));
        dc.set_radius(0.5);
        mob.set_radius(0.5);
        assert!(!body_overlap(&body_of(&dc), &body_of(&mob)), "距 1.0=两圆恰相切，不重叠");
        mob.set_pos((0.99, 0.5));
        assert!(body_overlap(&body_of(&dc), &body_of(&mob)), "距 0.99<1.0 两圆重叠");
    }

    #[test]
    fn body_overlap_circle_vs_creation_aabb() {
        // 圆贴着塔西脸（圆心 9.5，塔 AABB [10,11]）：距离恰 0.5 不重叠
        let circle = Body::Circle {
            center: (9.5, 0.5),
            radius: 0.5,
        };
        let aabb = Body::Aabb {
            min: (10.0, 0.0),
            max: (11.0, 1.0),
        };
        assert!(!body_overlap(&circle, &aabb));
        // 再进半格即重叠
        let inside = Body::Circle {
            center: (9.8, 0.5),
            radius: 0.5,
        };
        assert!(body_overlap(&inside, &aabb));
        // 对称：参数顺序无关
        assert!(body_overlap(&aabb, &inside));
    }

    // === touching（≤，接触通道判据）===

    #[test]
    fn circle_touching_exact_touch_true() {
        // 相切即接触（≤），与重叠判据（<）的分界
        assert!(circle_touching((0.0, 0.0), 0.5, (1.0, 0.0), 0.5));
        assert!(!circle_touching((0.0, 0.0), 0.5, (1.01, 0.0), 0.5));
    }

    #[test]
    fn circle_aabb_touching_exact_true() {
        // 圆贴塔脸（距边恰 0.5）=接触
        assert!(circle_aabb_touching((9.5, 0.5), 0.5, (10.0, 0.0), (11.0, 1.0)));
        assert!(!circle_aabb_touching((9.49, 0.5), 0.5, (10.0, 0.0), (11.0, 1.0)));
    }

    #[test]
    fn aabb_touching_edge_true() {
        assert!(aabb_touching((0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (2.0, 1.0)));
    }

    // === point_in_body（#031 点命中判据）===

    #[test]
    fn point_in_circle_strict() {
        let body = Body::Circle {
            center: (5.0, 0.5),
            radius: 0.5,
        };
        assert!(point_in_body((5.2, 0.5), &body));
        assert!(!point_in_body((5.5, 0.5), &body), "恰在圆周上不算命中（严格 <）");
        assert!(!point_in_body((5.6, 0.5), &body));
    }

    #[test]
    fn point_in_aabb_inclusive() {
        let body = Body::Aabb {
            min: (10.0, 0.0),
            max: (11.0, 1.0),
        };
        assert!(point_in_body((10.5, 0.5), &body));
        assert!(point_in_body((10.0, 0.0), &body), "AABB 闭区间：角点也算在内");
        assert!(point_in_body((11.0, 1.0), &body));
        assert!(!point_in_body((11.1, 0.5), &body));
    }

    // === clamp_radius（spawn 第 7 参校验口径）===

    #[test]
    fn clamp_radius_passes_valid() {
        assert_eq!(clamp_radius(0.5), 0.5);
        assert_eq!(clamp_radius(0.05), 0.05);
        assert_eq!(clamp_radius(25.0), 25.0);
        assert_eq!(clamp_radius(2.0), 2.0);
    }

    #[test]
    fn clamp_radius_clamps_out_of_bounds() {
        assert_eq!(clamp_radius(0.01), 0.05, "低于下限钳到 0.05");
        assert_eq!(clamp_radius(100.0), 25.0, "超上限钳到 25.0");
    }

    #[test]
    fn clamp_radius_non_finite_falls_back() {
        assert_eq!(clamp_radius(f64::NAN), 0.5);
        assert_eq!(clamp_radius(f64::INFINITY), 0.5);
    }

    // === clamp_mass（#034 spawn/shoot 第 mass 参校验口径）===

    #[test]
    fn clamp_mass_passes_valid() {
        assert_eq!(clamp_mass(1.0), 1.0);
        assert_eq!(clamp_mass(0.05), 0.05, "下限含边界");
        assert_eq!(clamp_mass(100.0), 100.0, "上限含边界");
        assert_eq!(clamp_mass(0.5), 0.5);
    }

    #[test]
    fn clamp_mass_clamps_out_of_bounds() {
        assert_eq!(clamp_mass(0.01), 0.05, "低于下限钳到 0.05");
        assert_eq!(clamp_mass(1000.0), 100.0, "超上限钳到 100.0");
    }

    #[test]
    fn clamp_mass_non_finite_falls_back() {
        // 除零护栏：NaN/无穷回退默认 1.0（#034 铁律：mass 只做物理分母，非有限值进公式会污染世界）
        assert_eq!(clamp_mass(f64::NAN), 1.0);
        assert_eq!(clamp_mass(f64::INFINITY), 1.0);
        assert_eq!(clamp_mass(f64::NEG_INFINITY), 1.0);
    }

    // === melee_in_reach（#034 attack_at 接触化判据：dist < r1+r2+MELEE_REACH 严格）===

    #[test]
    fn melee_in_reach_strict_less_than() {
        // 两半径和 1.0 + 冗余 0.5 = 1.5：恰在 1.5 = 落空（严格 <，审查锚定）
        assert!(melee_in_reach((0.0, 0.0), 0.5, (1.49, 0.0), 0.5));
        assert!(!melee_in_reach((0.0, 0.0), 0.5, (1.5, 0.0), 0.5), "恰好等于判距=落空");
        assert!(!melee_in_reach((0.0, 0.0), 0.5, (1.51, 0.0), 0.5));
    }

    #[test]
    fn melee_in_reach_reach_is_arm_not_range() {
        // 冗余 0.5 是"胳膊"不是射程：大身体判距=半径和+0.5，够得着更远但仍是贴身语义
        assert!(melee_in_reach((0.0, 0.0), 0.5, (1.0, 0.0), 0.5));
        assert!(melee_in_reach((0.0, 0.0), 2.0, (2.9, 0.0), 0.5));
        assert!(!melee_in_reach((0.0, 0.0), 2.0, (3.0, 0.0), 0.5));
    }

    // === 弹丸身份（#034 离体之物识别：body_of 同款 code_hash 常量判据）===

    #[test]
    fn is_projectile_by_code_hash() {
        let p = make_entity(1, "projectile", (0.0, 5.0));
        assert!(is_projectile(&p), "code_hash=projectile 是弹丸");
        let b = make_entity(2, "attacker", (1.0, 0.5));
        assert!(!is_projectile(&b), "普通生物不是弹丸");
        let t = make_entity(3, "creation", (10.0, 0.0));
        assert!(!is_projectile(&t), "造物不是弹丸");
    }

    // === CCD 路径检查几何原语（#034 修订二：线段扫掠首触参数 t）===

    #[test]
    fn segment_circle_first_contact_midpoint() {
        // 直线穿过圆心：首触在半径和减去处（t=0.4，接触于圆的近侧）
        // 线段 (0,0)->(10,0)，圆心 (5,0) r=1，弹 r=0：接触距离 1 → t=(5-1)/10=0.4
        let t = segment_first_contact_circle((0.0, 0.0), (10.0, 0.0), (5.0, 0.0), 1.0);
        assert!((t.unwrap() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn segment_circle_miss_when_path_far() {
        // 路径离圆心 2.0 > 半径和 1.0 → 不命中
        let t = segment_first_contact_circle((0.0, 0.0), (10.0, 0.0), (5.0, 2.0), 1.0);
        assert!(t.is_none(), "路径未扫过身体应 None");
    }

    #[test]
    fn segment_circle_start_inside_returns_zero() {
        // 起点已在身体重叠区内（异常态兜底）：首触 t=0
        let t = segment_first_contact_circle((5.0, 0.0), (10.0, 0.0), (5.0, 0.0), 1.0);
        assert_eq!(t, Some(0.0), "起点已重叠应立即命中 t=0");
    }

    #[test]
    fn segment_circle_graze_beyond_segment_none() {
        // 圆在线段延长线之外（最接近点 t>1）：路径未扫到 → None
        let t = segment_first_contact_circle((0.0, 0.0), (10.0, 0.0), (15.0, 0.0), 1.0);
        assert!(t.is_none());
    }

    #[test]
    fn segment_circle_stationary_inside_returns_zero() {
        // 零长度线段（起点=终点）且在身内：t=0
        let t = segment_first_contact_circle((5.0, 0.0), (5.0, 0.0), (5.0, 0.0), 1.0);
        assert_eq!(t, Some(0.0));
    }

    #[test]
    fn segment_expanded_aabb_face_entry() {
        // 正对 AABB 左脸直线穿入：扩展盒（r=0.15）左缘 9.85，线段 (9,0.5)->(11,0.5)
        // → 首触 t=(9.85-9)/2=0.425
        let t = segment_first_contact_expanded_aabb(
            (9.0, 0.5),
            (11.0, 0.5),
            (10.0, 0.0),
            (11.0, 1.0),
            0.15,
        );
        assert!((t.unwrap() - 0.425).abs() < 1e-9);
    }

    #[test]
    fn segment_expanded_aabb_miss() {
        // 路径从盒上方掠过（间距 0.5 > r=0.15）→ None
        let t = segment_first_contact_expanded_aabb(
            (0.0, 1.65),
            (12.0, 1.65),
            (10.0, 0.0),
            (11.0, 1.0),
            0.15,
        );
        assert!(t.is_none(), "距盒顶 0.65 > 0.15 不命中");
    }

    #[test]
    fn segment_expanded_aabb_start_inside_returns_zero() {
        // 起点在扩展盒内：t=0
        let t = segment_first_contact_expanded_aabb(
            (10.5, 0.5),
            (12.0, 0.5),
            (10.0, 0.0),
            (11.0, 1.0),
            0.15,
        );
        assert_eq!(t, Some(0.0));
    }

    #[test]
    fn segment_expanded_aabb_vertical_axis_only() {
        // 纯垂直下落穿盒顶：x 恒 10.5（盒 x 区间内），y 从 2 落到 0.5
        // → 扩展顶 1.15，t=(2-1.15)/1.5≈0.5667
        let t = segment_first_contact_expanded_aabb(
            (10.5, 2.0),
            (10.5, 0.5),
            (10.0, 0.0),
            (11.0, 1.0),
            0.15,
        );
        let tv = t.unwrap();
        assert!((tv - (2.0 - 1.15) / 1.5).abs() < 1e-9);
    }
}
