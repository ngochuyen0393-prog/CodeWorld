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

/// 实体 -> 身体（creation=格对齐 AABB，其余=圆）
pub fn body_of(e: &Entity) -> Body {
    if e.code_hash == CREATION_CODE {
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
}
