//! 液体（D-009：liquid 出生声明家族的物理结算）
//!
//! decisions.md #053 立法：液体=造物出生声明家族新成员（与 radius/mass/binding/
//! motion/field 并列），"液体造物"作为特殊品类在法律上不存在，世界只认数字不认
//! 名字（油 0.75/水 1.0/蜂蜜 1.4/岩浆 2.5 是社区预设非内置清单，#011 反面教材
//! =MC 硬编码浮沉名单）。液体造物身体（格对齐 AABB）围成的矩形区域成为水域，
//! 域内实体按液体物理被结算——浮沉/阻尼/附加质量全公式涌现零特例（木浮铁沉
//! 自动出来，无清单）。
//!
//! 公式（全部定死，decisions.md #053）：
//! - 浸入分数 f（竖径夹逼近似）：`f = clamp((min(y_max, c.y+r) − max(y_min,
//!   c.y−r)) / (2r), 0.0, 1.0)`——精确弓形面积留 D-013 形状物理包时代；
//! - 浮力改写重力：`g_final = g_field × (1 − (ρ液/ρ物) × f)`，钳 [−98.0, +98.0]
//!   （浮力档对称十倍域，与力场档 [−98,0] 不同——浮力恰恰要向上）；ρ物 = m/(π·r²)
//!   （推导密度：密度已从出生参数删除，浮力用推导密度，m 下限 0.05 保证除法安全）；
//! - 水中阻尼（MC 次序）：`v ← v × k_eff + g_final`，k_eff = clamp(k, 0, 1)
//!   （声明域 [0,5] 合法但有效保留系数钳 [0,1]，不许凭空加速）；
//! - 附加质量：`m_eff = m + ρ液 × π × r² × f`（2D 圆解析解=排开流体质量，f 加权：
//!   半沉排开一半）；ρ物 计算用真实 m 不用 m_eff（防递归）；
//! - 多域取最高密度：实体同时接触多个液体域 → 取 ρ液 最高者结算，k 取同域配对值
//!   （ρ 与 k 是同一造物的配对声明）。
//!
//! 每拍登记制（照 collect_fields 先例）：每拍全量重算覆盖关系，无 enter/exit 缓存。

use codeworld_core::entity::EntityId;
use codeworld_core::world::WorldState;

/// 单个液体域在本拍的生效快照（main 每拍收集，喂给物理步）
///
/// 收集口径见 [`collect_liquid_domains`]；域内结算见 [`liquid_effect`]。
#[derive(Debug, Clone, PartialEq)]
pub struct LiquidDomain {
    /// 液体造物实体 id（域内实体清单不含它自己；判定不含所有权——账本不认老板）
    pub domain_id: EntityId,
    /// 液体 AABB 左下角（格对齐，creation_aabb 产物）
    pub min: (f64, f64),
    /// 液体 AABB 右上角
    pub max: (f64, f64),
    /// 液体密度 ρ（无量纲，水=1.0 参照系）
    pub density: f64,
    /// 水中阻尼 k（每拍速度保留系数声明值；结算时有效保留系数钳 [0,1]）
    pub drag_k: f64,
}

/// 液体结算产物（纯函数 [`liquid_effect`] 输出；f>0 即在水中，供日志与前端高亮判定）
#[derive(Debug, Clone, PartialEq)]
pub struct LiquidEffect {
    /// 浮力改写后的最终重力（域外=力场结算值原样；域内=g_field×(1−ρ比×f) 钳 ±98）
    pub g_final: f64,
    /// 有效保留系数（域外=0.0=无水中阻尼；域内=clamp(k, 0, 1)）
    pub k_eff: f64,
    /// 附加质量（域外=真实 m 原样；域内=m + ρ液·π·r²·f）
    pub m_eff: f64,
    /// 浸入分数（0=不接触/只擦边，1=全浸没，0.5=圆心在液面）
    pub f: f64,
    /// 结算所用的液体域（多域取最高密度者；None=域外）
    pub domain_id: Option<EntityId>,
}

/// 浮力改写后最终重力的钳制域（D-009 浮力档：对称十倍域 [−98, +98]——注意与
/// 力场档 [−98, 0] 不同，浮力恰恰要向上；钳制只作用于浮力改写后的最终值）
const BUOYANCY_G_MIN: f64 = -98.0;
const BUOYANCY_G_MAX: f64 = 98.0;

/// 液体结算（纯函数，浮沉涌现单一实现在此）
///
/// - 无液体世界快速路径：liquids 为空直接返回"不在水中"（g_final=力场结算值、
///   k_eff=0、m_eff=真实 m、f=0——零回归锚，现有物理测试全绿的根基）；
/// - 域影响判定：实体圆（圆心 pos、半径 radius）与液体 AABB 相交（水平方向
///   [c.x−r, c.x+r] 与 [min_x, max_x] 有重叠，竖直方向同理；恰擦边=不相交）。
///   相交才计算浸入分数；
/// - 浸入分数 f（竖径夹逼近似）：`f = clamp((min(y_max, c.y+r) − max(y_min,
///   c.y−r)) / (2r), 0.0, 1.0)`；f=0（只擦边）=不受液体影响；
/// - 多域覆盖取最高密度结算（k 取同域配对值）；
/// - 推导密度 ρ物 = m/(π·r²)（m 下限 0.05 保证除法安全；ρ物 ≤ 0 的病态防御：
///   跳过浮力改写并 warn）。
pub fn liquid_effect(
    pos: (f64, f64),
    radius: f64,
    mass: f64,
    velocity: (f64, f64),
    field_result: (f64, f64),
    liquids: &[LiquidDomain],
) -> LiquidEffect {
    // 无液体快速路径：逐位返回"不在水中"（零回归锚——不允许任何浮点疑虑）
    if liquids.is_empty() {
        return LiquidEffect {
            g_final: field_result.0,
            k_eff: 0.0,
            m_eff: mass,
            f: 0.0,
            domain_id: None,
        };
    }
    // 防御：半径非法（≤0 或非有限）无浸入可言——按域外结算（病态防御分支，
    // 正常实体半径钳 0.05..=25 不会走到）
    if radius <= 0.0 || !radius.is_finite() {
        return LiquidEffect {
            g_final: field_result.0,
            k_eff: 0.0,
            m_eff: mass,
            f: 0.0,
            domain_id: None,
        };
    }
    // 域判定 + 多域取最高密度（同密度并列取清单序在前者=EntityId 升序更小者，
    // 确定序；k 与 ρ 是同一造物的配对声明，取同域配对值）
    let mut best: Option<(&LiquidDomain, f64)> = None; // (域, 浸入分数)
    for d in liquids {
        let overlap_x = pos.0 + radius > d.min.0 && pos.0 - radius < d.max.0;
        let overlap_y = pos.1 + radius > d.min.1 && pos.1 - radius < d.max.1;
        if !(overlap_x && overlap_y) {
            continue; // 不相交（含恰擦边）
        }
        // 浸入分数（竖径夹逼近似）：竖直重叠高度 ÷ 圆直径
        let top = d.max.1.min(pos.1 + radius);
        let bottom = d.min.1.max(pos.1 - radius);
        let f = ((top - bottom) / (2.0 * radius)).clamp(0.0, 1.0);
        if f <= 0.0 {
            continue; // 只擦边（重叠高度为 0）=不受液体影响
        }
        let take = match best {
            None => true,
            Some((bd, _)) => d.density > bd.density,
        };
        if take {
            best = Some((d, f));
        }
    }
    let Some((d, f)) = best else {
        // 全部域都只擦边/不相交：域外结算（零改写）
        return LiquidEffect {
            g_final: field_result.0,
            k_eff: 0.0,
            m_eff: mass,
            f: 0.0,
            domain_id: None,
        };
    };
    // 推导密度 ρ物 = m/(π·r²)（真实 m 不用 m_eff——防递归；m 出生钳 0.05..=100
    // 保证除法安全，仍加防御：ρ物 ≤ 0 跳过浮力改写并 warn）
    let rho_obj = mass / (std::f64::consts::PI * radius * radius);
    // 浮力改写重力：g_final = g_field × (1 − (ρ液/ρ物) × f)，钳 [−98, +98]
    let g_final = if rho_obj > 0.0 && rho_obj.is_finite() {
        (field_result.0 * (1.0 - (d.density / rho_obj) * f)).clamp(BUOYANCY_G_MIN, BUOYANCY_G_MAX)
    } else {
        tracing::warn!(
            mass,
            radius,
            "[liquid] 推导密度非正（病态），跳过浮力改写（g_final=力场结算值）"
        );
        field_result.0
    };
    // 水中阻尼有效保留系数：声明域 [0,5] 合法，结算域钳 [0,1]（k>1 不许凭空加速）
    let k_eff = d.drag_k.clamp(0.0, 1.0);
    // 附加质量：m_eff = m + ρ液 × π × r² × f（2D 圆解析解=排开流体质量，f 加权）
    let m_eff = mass + d.density * std::f64::consts::PI * radius * radius * f;
    tracing::debug!(
        pos = ?pos,
        velocity = ?velocity,
        domain = %d.domain_id.0,
        density = d.density,
        f,
        g_final,
        k_eff,
        m_eff,
        "[liquid] 液体结算（ρ物={rho_obj:.4}）"
    );
    LiquidEffect {
        g_final,
        k_eff,
        m_eff,
        f,
        domain_id: Some(d.domain_id),
    }
}

/// 收集本拍液体域清单（登记制每拍全量重算，照 collect_fields 先例）
///
/// 口径：
/// - alive 且 liquid_density > 0 的实体（液体造物=世界实体，死了水域即消失）；
/// - 域=AABB：liquid 造物一律 static 格对齐（spawn 侧强制），creation_aabb 即域；
/// - 按 EntityId 升序（确定序，与全世界的结算定序同源）。
pub fn collect_liquid_domains(world: &WorldState) -> Vec<LiquidDomain> {
    let mut domains: Vec<LiquidDomain> = world
        .entities
        .iter()
        .filter(|(_, e)| e.is_alive() && e.liquid_density() > 0.0)
        .map(|(id, e)| {
            let (min, max) = codeworld_core::collision::creation_aabb(e.pos());
            LiquidDomain {
                domain_id: *id,
                min,
                max,
                density: e.liquid_density(),
                drag_k: e.liquid_drag(),
            }
        })
        .collect();
    domains.sort_by_key(|d| d.domain_id.0);
    if !domains.is_empty() {
        tracing::debug!(count = domains.len(), "[liquid] 本拍液体域清单收集");
    }
    domains
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, PlayerId};
    use std::f64::consts::PI;
    use uuid::Uuid;

    /// 构造液体域（id 固定 u128，便于排序断言）
    fn domain(id_u: u128, min: (f64, f64), max: (f64, f64), density: f64, drag_k: f64) -> LiquidDomain {
        LiquidDomain {
            domain_id: EntityId(Uuid::from_u128(id_u)),
            min,
            max,
            density,
            drag_k,
        }
    }

    /// 构造世界实体（液体造物/域内实体的载体）
    fn make_entity(id_u: u128, pos: (f64, f64), created_tick: u64) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "creation".to_string(),
            pos,
            Appearance::new("W".to_string(), "blue".to_string(), 1.0),
            created_tick,
            1_000_000,
        )
    }

    /// 木球参数锚：r=0.5、m=0.4 → ρ物 = 0.4/(π·0.25) ≈ 0.509 < 1（木，浮）
    const WOOD_MASS: f64 = 0.4;
    /// 铁球参数锚：r=0.5、m=10.0 → ρ物 ≈ 12.73 > 1（铁，沉）
    const IRON_MASS: f64 = 10.0;
    const R: f64 = 0.5;

    // === liquid_effect 七锚（验收清单：域外零回归/全浸悬浮/木浮/铁沉/
    // === 多域最高密度/浮力档钳制/半沉平衡点）===

    #[test]
    fn liquid_no_domains_matches_field_result_exactly() {
        // 域外零回归锚：空清单逐位返回"不在水中"（g_final=力场结算值、k_eff=0、
        // m_eff=真实 m——无液体世界行为与 D-009 前逐字节一致）
        let liquids: Vec<LiquidDomain> = Vec::new();
        let e = liquid_effect((5.0, 3.0), R, 1.0, (1.0, 2.0), (-9.8, 0.05), &liquids);
        assert_eq!(e.g_final, -9.8, "域外 g_final 应逐位等于力场结算值");
        assert_eq!(e.k_eff, 0.0);
        assert_eq!(e.m_eff, 1.0, "域外 m_eff 应逐位等于真实质量");
        assert_eq!(e.f, 0.0);
        assert_eq!(e.domain_id, None);
    }

    #[test]
    fn liquid_fully_submerged_equal_density_floats_neutral() {
        // 全浸悬浮锚：ρ物=ρ液 且 f=1 → g_final=0（悬浮，不沉不浮）
        // m = π·r² × 1.0 = π×0.25 ≈ 0.7854 → ρ物=1.0；深水 AABB 全包圆
        let m = PI * R * R;
        let liquids = [domain(1, (0.0, -100.0), (100.0, 100.0), 1.0, 0.35)];
        let e = liquid_effect((5.0, 3.0), R, m, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert!((e.g_final - 0.0).abs() < 1e-12, "等密度全浸应 g_final=0，实际 {}", e.g_final);
        assert!((e.f - 1.0).abs() < 1e-12, "全浸没 f=1，实际 {}", e.f);
        assert!((e.k_eff - 0.35).abs() < 1e-12, "k 取同域配对声明值（≤1 原样）");
        assert_eq!(e.domain_id, Some(EntityId(Uuid::from_u128(1))));
    }

    #[test]
    fn liquid_wood_floats_upward() {
        // 木浮锚：ρ液 > ρ物 → 乘数为负 → g_final 与 g 反向=向上推（正数）
        let liquids = [domain(1, (0.0, -100.0), (100.0, 100.0), 1.0, 0.35)];
        let e = liquid_effect((5.0, 3.0), R, WOOD_MASS, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert!(e.g_final > 0.0, "木球应被向上推（g_final>0），实际 {}", e.g_final);
    }

    #[test]
    fn liquid_iron_sinks_but_lighter_than_air_fall() {
        // 铁沉锚：ρ液 < ρ物 → g_final 仍负（继续沉）但 |g| 变小（浮力托了一把）
        let liquids = [domain(1, (0.0, -100.0), (100.0, 100.0), 1.0, 0.35)];
        let e = liquid_effect((5.0, 3.0), R, IRON_MASS, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert!(e.g_final < 0.0, "铁球仍向下（g_final<0），实际 {}", e.g_final);
        assert!(e.g_final > -9.8, "|g| 应小于空气下落 9.8（浮力托一把），实际 {}", e.g_final);
    }

    #[test]
    fn liquid_multi_domain_takes_highest_density() {
        // 多域取最高密度锚：两域重叠取 ρ 最高者结算，k 取同域配对值
        //（ρ 与 k 是同一造物的配对声明，不许拆开混搭）
        let light = [domain(1, (0.0, -100.0), (100.0, 100.0), 1.0, 0.9)];
        let heavy = [domain(2, (0.0, -100.0), (100.0, 100.0), 2.5, 0.1)];
        let both = [domain(1, (0.0, -100.0), (100.0, 100.0), 1.0, 0.9),
                    domain(2, (0.0, -100.0), (100.0, 100.0), 2.5, 0.1)];
        let wood_in_heavy = liquid_effect((5.0, 3.0), R, WOOD_MASS, (0.0, 0.0), (-9.8, 0.05), &heavy);
        assert!(wood_in_heavy.g_final > 0.0, "重液中木球上浮推力更大（ρ比=4.9），实际 {}", wood_in_heavy.g_final);
        assert!((wood_in_heavy.k_eff - 0.1).abs() < 1e-12, "k 应取重液配对值 0.1，实际 {}", wood_in_heavy.k_eff);
        let both_result = liquid_effect((5.0, 3.0), R, WOOD_MASS, (0.0, 0.0), (-9.8, 0.05), &both);
        assert_eq!(both_result, wood_in_heavy, "双域并存结算应与单取最高密度域逐位一致");
        let light_only = liquid_effect((5.0, 3.0), R, WOOD_MASS, (0.0, 0.0), (-9.8, 0.05), &light);
        assert!(light_only.g_final < both_result.g_final, "轻液上浮推力应小于重液");
    }

    #[test]
    fn liquid_buoyancy_clamp_bottoms_out_at_positive_ninety_eight() {
        // 浮力档钳制锚：极端 ρ液/ρ物 比值（最轻物 m=0.05 + 最重液 5.0）→
        // 裸公式上浮推力数百 → 钳 +98.0（对称十倍域，与力场档 [−98,0] 不同——
        // 浮力恰恰要向上）
        let liquids = [domain(1, (0.0, -100.0), (100.0, 100.0), 5.0, 0.35)];
        let e = liquid_effect((5.0, 3.0), R, 0.05, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert!((e.g_final - 98.0).abs() < 1e-12, "极端比值应钳 +98.0，实际 {}", e.g_final);
    }

    #[test]
    fn liquid_half_submerged_double_density_is_equilibrium() {
        // 半沉锚（"典型物体半沉"的平衡点）：圆心在液面 f=0.5、ρ液=2ρ物 →
        // 乘数 1−2×0.5=0 → g_final=0。m = 0.5×π×r² → ρ物=0.5，配水 ρ=1.0
        let m = 0.5 * PI * R * R; // ρ物 = 0.5
        let liquids = [domain(1, (0.0, 0.0), (100.0, 100.0), 1.0, 0.35)];
        // 圆心恰在液面 y=0（水在 [0,100]）：f = (min(100, 0.5) − max(0, -0.5))/1 = 0.5
        let e = liquid_effect((5.0, 0.0), R, m, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert!((e.f - 0.5).abs() < 1e-12, "圆心在液面应 f=0.5，实际 {}", e.f);
        assert!((e.g_final - 0.0).abs() < 1e-12, "ρ液=2ρ物 半沉应悬浮平衡（g_final=0），实际 {}", e.g_final);
    }

    #[test]
    fn liquid_touching_edge_only_is_not_in_water() {
        // 擦边锚：圆底恰触液面（c.y−r = y_max）→ 竖直方向无重叠 → f=0 不受影响
        //（浮沉判据的"只擦边不算"，与碰撞"相切即接触"口径无关）
        let liquids = [domain(1, (0.0, 0.0), (100.0, 10.0), 1.0, 0.35)];
        // 圆心 y=10.5、r=0.5：圆底 10.0 恰触液顶 10.0 → 不相交
        let e = liquid_effect((5.0, 10.5), R, 1.0, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert_eq!(e.f, 0.0, "擦边不应计浸入");
        assert_eq!(e.g_final, -9.8, "擦边重力不被改写");
        assert_eq!(e.m_eff, 1.0, "擦边质量不加附加");
    }

    #[test]
    fn liquid_added_mass_anchor_one_plus_pi_over_four() {
        // 附加质量数值锚：m=1、r=0.5、ρ液=1、f=1 → m_eff = 1 + π×0.25 ≈ 1.7854
        //（2D 圆解析解=排开流体质量；半沉 f=0.5 排开一半）
        let liquids = [domain(1, (0.0, -100.0), (100.0, 100.0), 1.0, 0.35)];
        let full = liquid_effect((5.0, 3.0), R, 1.0, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert!(
            (full.m_eff - (1.0 + PI * 0.25)).abs() < 1e-12,
            "全浸 m_eff 应=1+π×0.25，实际 {}",
            full.m_eff
        );
        let half = liquid_effect((5.0, 100.0), R, 1.0, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert!(
            (half.m_eff - (1.0 + PI * 0.25 * 0.5)).abs() < 1e-12,
            "半沉 m_eff 应=1+π×0.125（排开一半），实际 {}",
            half.m_eff
        );
    }

    #[test]
    fn liquid_drag_k_above_one_clamps_to_one() {
        // 有效保留系数钳 [0,1] 锚：k=3.0 的声明合法（域 [0,5]）但结算按 1
        //——阻尼不许凭空加速
        let liquids = [domain(1, (0.0, -100.0), (100.0, 100.0), 1.0, 3.0)];
        let e = liquid_effect((5.0, 3.0), R, 1.0, (0.0, 0.0), (-9.8, 0.05), &liquids);
        assert_eq!(e.k_eff, 1.0, "k>1 声明应按 1 结算（不许凭空加速）");
    }

    // === collect_liquid_domains（登记制每拍全量收集）===

    #[test]
    fn collect_liquid_domains_only_alive_with_density() {
        // 口径锚：死亡液体造物不入账（死了水域即消失）+ liquid_density=0 的
        // 普通实体不入账
        let mut world = WorldState::new();
        let mut dead = make_entity(1, (5.0, 0.0), 0);
        dead.set_liquid_density(1.0);
        dead.kill();
        let dead_id = dead.id;
        world.entities.insert(dead_id, dead);
        let mut plain = make_entity(2, (6.0, 0.0), 0); // 非液体声明
        let plain_id = plain.id;
        world.entities.insert(plain_id, plain);
        let domains = collect_liquid_domains(&world);
        assert!(domains.is_empty(), "死者/非液体实体不应入账，实际 {domains:?}");
    }

    #[test]
    fn collect_liquid_domains_aabb_from_position_sorted_by_id() {
        // AABB+定序锚：域=造物 pos 的格对齐 1×1 AABB（creation_aabb），清单按
        // EntityId 升序（确定序）；密度/阻尼随声明透传
        let mut world = WorldState::new();
        let mut b = make_entity(2, (12.0, 0.0), 0);
        b.set_liquid_density(1.0);
        b.set_liquid_drag(0.35);
        let mut a = make_entity(1, (10.5, 0.0), 0);
        a.set_liquid_density(2.5);
        let a_id = a.id;
        let b_id = b.id;
        world.entities.insert(b_id, b);
        world.entities.insert(a_id, a);
        let domains = collect_liquid_domains(&world);
        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0].domain_id, EntityId(Uuid::from_u128(1)), "应按 EntityId 升序");
        assert_eq!(domains[0].min, (10.0, 0.0), "AABB=格对齐（floor 对齐 1×1）");
        assert_eq!(domains[0].max, (11.0, 1.0));
        assert_eq!(domains[0].density, 2.5, "密度随声明透传");
        assert_eq!(domains[1].drag_k, 0.35, "阻尼随声明透传");
    }
}
