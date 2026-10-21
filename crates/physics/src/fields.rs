//! 力场（D-012：field 出生声明家族的物理结算）
//!
//! decisions.md #040 立法：场=造物的一种出生声明（与 radius/mass/binding/motion
//! 并列），"场造物"作为特殊品类在法律上不存在。场源=物理身体，效果=以身体为锚
//! 向外延伸的参数改写区（无形体不占位不拦截碰撞）。
//!
//! 叠加 v2（土豆否决 v1"取最强"）：**代数叠加+域钳制+抵消原则**——
//! - 生效重力 = 世界重力 × 域内各场 gravity_scale 乘积（缩放叠乘是缩放的代数
//!   形式：两场 0.5×0.5=0.25，负向抵消靠 scale>1 实现）；
//! - 生效阻力 = 世界阻力 + Σ(域内各场 drag_delta)（加法式 delta，正负互吃：
//!   +2 与 -2 归零，对冲场是真实战术）；
//! - 域钳制兜底：生效阻力钳 [0.0, 25.0]、生效重力钳 [-98.0, 0.0]——堆场买不到
//!   定律外胜利（不许向上飞的重力、不许超过十倍）；
//! - 优先级不实现（降级为边角工具）；账本不认老板（无所有权标签，全部入账）。
//!
//! 每拍登记制（0.1s tick 刷场判例）：每拍全量重算覆盖关系，无 enter/exit 缓存。
//! 成型期（FIELD_FORMING_TICKS）内场不结算物理但已可见（给进攻方反应窗，
//! Kinetic Field 判例）。

use std::collections::HashMap;

use codeworld_core::entity::EntityId;
use codeworld_core::physics::DefaultPhysics;
use codeworld_core::world::WorldState;

/// 单个场在本拍的生效快照（main 每拍收集，喂给物理步）
///
/// 收集口径见 [`collect_fields`]；叠加结算见 [`effective_gravity_drag`]。
#[derive(Debug, Clone, PartialEq)]
pub struct FieldEffect {
    /// 场源实体（域内实体清单不含它自己；判定不含所有权——账本不认老板）
    pub source_id: EntityId,
    /// 场源位置（本拍收集时刻快照；域判定=圆心距 < radius）
    pub pos: (f64, f64),
    /// 场域半径
    pub radius: f64,
    /// 重力缩放（乘法式：多场叠乘）
    pub gravity_scale: f64,
    /// 阻力改写量（加法式：多场代数和，正负互吃）
    pub drag_delta: f64,
}

/// 生效重力与生效阻力（纯函数，叠加 v2 单一实现在此）
///
/// - 无场世界快速路径：fields 为空直接返回世界默认值（逐位一致，零回归锚——
///   现有 physics 测试全部照绿的根基）；
/// - 域判定：pos 距场源圆心距 < radius（严格域内，恰在圆周上不算域内——
///   简单确定，与身体碰撞的"相切即接触"口径无关）；
/// - 多场覆盖取并集合账：先乘所有覆盖场的 gravity_scale，再加所有覆盖场的
///   drag_delta，最后域钳制兜底。
///
/// 返回 `(effective_gravity, effective_drag)`。
pub fn effective_gravity_drag(
    pos: (f64, f64),
    defaults: &DefaultPhysics,
    fields: &[FieldEffect],
) -> (f64, f64) {
    // 无场快速路径：逐位返回默认值（零回归锚——不允许 x×1.0 的任何浮点疑虑）
    if fields.is_empty() {
        return (defaults.gravity, defaults.drag_coefficient);
    }
    let mut scale_prod = 1.0_f64;
    let mut delta_sum = 0.0_f64;
    let mut covered = false;
    for f in fields {
        let dx = pos.0 - f.pos.0;
        let dy = pos.1 - f.pos.1;
        // 域判定：圆心距 < radius（严格域内，恰在圆周上不算——平方比较免开方）
        if dx * dx + dy * dy < f.radius * f.radius {
            covered = true;
            scale_prod *= f.gravity_scale; // 缩放叠乘（缩放的代数形式）
            delta_sum += f.drag_delta; // 阻力代数和（正负互吃）
        }
    }
    if !covered {
        return (defaults.gravity, defaults.drag_coefficient);
    }
    // 域钳制兜底：堆场买不到定律外胜利（重力 [-98, 0]：不许向上飞、不超十倍；
    // 阻力 [0, 25]：永不负值，负阻力=凭空加速不合法）
    let gravity = (defaults.gravity * scale_prod).clamp(
        codeworld_core::entity::FIELD_GRAVITY_EFFECTIVE_MIN,
        0.0,
    );
    let drag = (defaults.drag_coefficient + delta_sum)
        .clamp(codeworld_core::entity::FIELD_DRAG_EFFECTIVE_MIN, codeworld_core::entity::FIELD_DRAG_EFFECTIVE_MAX);
    tracing::debug!(
        pos = ?pos,
        fields_total = fields.iter().len(),
        gravity,
        drag,
        "[field] 生效参数结算（叠加 v2 代数和+域钳制；fields_total=清单总数非覆盖数）"
    );
    (gravity, drag)
}

/// 收集本拍场清单（登记制 tick 刷场：每拍全量重算，无 enter/exit 缓存）
///
/// 口径：
/// - alive 且 field_radius > 0 的实体（场源=物理身体，死了场即消失）；
/// - 成型期过滤：`current_tick - created_tick >= forming_ticks` 才入账（成型期
///   内场不结算物理，已可见——前端画虚线环，给进攻方反应窗）；
/// - 按 EntityId 升序（确定序，与全世界的结算定序同源）；
/// - `overrides`：on_field 询问协议的本拍应答修正（场有表达权——非 0 返回值
///   临时改写本拍 drag_delta；世界有结算权——域钳制与代数和最终裁定），
///   无应答用出生声明的 field_drag_delta。
pub fn collect_fields(
    world: &WorldState,
    current_tick: u64,
    forming_ticks: u64,
    overrides: &HashMap<EntityId, f64>,
) -> Vec<FieldEffect> {
    let mut fields: Vec<FieldEffect> = world
        .entities
        .iter()
        .filter(|(_, e)| {
            e.is_alive()
                && e.field_radius() > 0.0
                && current_tick.saturating_sub(e.created_tick) >= forming_ticks
        })
        .map(|(id, e)| FieldEffect {
            source_id: *id,
            pos: e.pos(),
            radius: e.field_radius(),
            gravity_scale: e.field_gravity_scale(),
            // 应答覆盖：on_field 本拍返回非 0 → 临时改写本拍 delta；无应答用声明值
            drag_delta: overrides.get(id).copied().unwrap_or(e.field_drag_delta()),
        })
        .collect();
    fields.sort_by_key(|f| f.source_id.0);
    if !fields.is_empty() {
        tracing::debug!(tick = current_tick, count = fields.len(), "[field] 本拍场清单收集");
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, PlayerId};
    use uuid::Uuid;

    fn defaults() -> DefaultPhysics {
        DefaultPhysics::default_values()
    }

    /// 构造一个场（源 id 固定 u128，便于排序断言）
    fn field(id_u: u128, pos: (f64, f64), radius: f64, scale: f64, delta: f64) -> FieldEffect {
        FieldEffect {
            source_id: EntityId(Uuid::from_u128(id_u)),
            pos,
            radius,
            gravity_scale: scale,
            drag_delta: delta,
        }
    }

    /// 构造世界实体（场源/域内实体的载体；code_hash 用 "creation" 直读性更强）
    fn make_entity(id_u: u128, pos: (f64, f64), created_tick: u64) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "creation".to_string(),
            pos,
            Appearance::new("F".to_string(), "cyan".to_string(), 1.0),
            created_tick,
            1_000_000,
        )
    }

    // === effective_gravity_drag 五锚（验收清单：无场/单场重力/单场阻力/双场
    // === 代数抵消/域钳制兜底）===

    #[test]
    fn effective_no_fields_matches_defaults_exactly() {
        // 无场锚：空清单逐位返回世界默认值（零回归根基——现有物理测试全绿的
        // 前提是无场世界行为与 D-012 前逐字节一致）
        let d = defaults();
        let (g, drag) = effective_gravity_drag((5.0, 3.0), &d, &[]);
        assert_eq!(g, d.gravity, "无场重力应逐位等于默认值");
        assert_eq!(drag, d.drag_coefficient, "无场阻力应逐位等于默认值");
    }

    #[test]
    fn effective_single_field_multiplies_gravity() {
        // 单场重力锚：域内实体生效重力 = 世界重力 × scale（0.5 → -4.9）
        let d = defaults();
        let fields = [field(1, (0.0, 0.0), 10.0, 0.5, 0.0)];
        let (g, drag) = effective_gravity_drag((3.0, 2.0), &d, &fields);
        assert!((g - (-9.8 * 0.5)).abs() < 1e-12, "重力应乘 0.5，实际 {g}");
        assert_eq!(drag, d.drag_coefficient, "delta=0 阻力不变");
    }

    #[test]
    fn effective_single_field_adds_drag_delta() {
        // 单场阻力锚：域内实体生效阻力 = 世界阻力 + delta（加法式，不是缩放）
        let d = defaults();
        let fields = [field(1, (0.0, 0.0), 10.0, 1.0, 2.0)];
        let (g, drag) = effective_gravity_drag((0.0, 0.0), &d, &fields);
        assert_eq!(g, d.gravity, "scale=1 重力不变");
        assert!((drag - (0.05 + 2.0)).abs() < 1e-12, "阻力应加 delta，实际 {drag}");
    }

    #[test]
    fn effective_two_fields_algebraic_cancellation() {
        // 双场代数抵消锚：+2 与 -2 归零（对冲场是真实战术）；缩放 0.5×2.0=1.0
        // 重力复原——叠加 v2：力只能叠加，正负互吃
        let d = defaults();
        let fields = [
            field(1, (0.0, 0.0), 10.0, 0.5, 2.0),
            field(2, (0.0, 0.0), 10.0, 2.0, -2.0),
        ];
        let (g, drag) = effective_gravity_drag((1.0, 1.0), &d, &fields);
        assert!((g - d.gravity).abs() < 1e-12, "0.5×2.0 应复原重力，实际 {g}");
        assert!((drag - d.drag_coefficient).abs() < 1e-12, "+2 与 -2 应归零，实际 {drag}");
    }

    #[test]
    fn effective_domain_clamp_bottoms_out() {
        // 域钳制兜底锚：两场 scale 10×10 → 重力 -980 钳 -98.0（不许超过十倍）；
        // delta -1（下限）→ 阻力 -0.95 钳 0.0（阻力永不负——负阻力=凭空加速）
        let d = defaults();
        let heavy = [
            field(1, (0.0, 0.0), 10.0, 10.0, 0.0),
            field(2, (0.0, 0.0), 10.0, 10.0, 0.0),
        ];
        let (g, _) = effective_gravity_drag((0.0, 0.0), &d, &heavy);
        assert!((g - (-98.0)).abs() < 1e-12, "十倍叠乘应钳 -98.0，实际 {g}");
        let buoyant = [field(3, (0.0, 0.0), 10.0, 1.0, -1.0)];
        let (_, drag) = effective_gravity_drag((0.0, 0.0), &d, &buoyant);
        assert_eq!(drag, 0.0, "负净阻力应钳 0.0（定律外胜利买不到），实际 {drag}");
    }

    #[test]
    fn effective_pos_outside_field_unaffected() {
        // 域外实体不受影响锚：pos 距场源 >= radius（含恰在圆周上）不吃场效果
        let d = defaults();
        let fields = [field(1, (0.0, 0.0), 5.0, 0.0, 2.0)];
        let (g, drag) = effective_gravity_drag((5.0, 0.0), &d, &fields);
        assert_eq!(g, d.gravity, "恰在圆周上=域外（严格 < 判定）");
        assert_eq!(drag, d.drag_coefficient, "域外阻力不变");
        let inside = effective_gravity_drag((4.999, 0.0), &d, &fields);
        assert!((inside.1 - 2.05).abs() < 1e-12, "域内即生效");
    }

    // === collect_fields（登记制 tick 刷场）===

    #[test]
    fn collect_fields_excludes_forming_fields() {
        // 成型期锚：诞生后 forming_ticks 内不入账（不结算物理但已可见——
        // 前端画虚线环，给进攻方反应窗，Kinetic Field 判例）
        let mut world = WorldState::new();
        let mut e = make_entity(1, (5.0, 0.0), 10);
        e.set_field_radius(3.0);
        e.set_field_gravity_scale(0.5);
        let id = e.id;
        world.entities.insert(id, e);
        // 成型期内（age=2 < 3）：不入账
        let empty = HashMap::new();
        let fields = collect_fields(&world, 12, 3, &empty);
        assert!(fields.is_empty(), "成型期内场不应入账，实际 {fields:?}");
        // 成型期满（age=3 >= 3）：入账
        let fields = collect_fields(&world, 13, 3, &empty);
        assert_eq!(fields.len(), 1, "成型期满应入账");
        assert_eq!(fields[0].radius, 3.0);
        assert_eq!(fields[0].gravity_scale, 0.5);
        assert_eq!(fields[0].source_id, id);
    }

    #[test]
    fn collect_fields_skips_dead_and_no_field_entities() {
        // 口径锚：死亡场源不入账（失联即灭）+ field_radius=0 的普通造物不入账
        let mut world = WorldState::new();
        let mut dead = make_entity(1, (5.0, 0.0), 0);
        dead.set_field_radius(3.0);
        dead.kill();
        let dead_id = dead.id;
        world.entities.insert(dead_id, dead);
        let mut plain = make_entity(2, (6.0, 0.0), 0); // 无场声明
        plain.set_field_gravity_scale(0.5); // 有缩放无半径 = 无场
        let plain_id = plain.id;
        world.entities.insert(plain_id, plain);
        let fields = collect_fields(&world, 10, 3, &HashMap::new());
        assert!(fields.is_empty(), "死者/无场实体不应入账，实际 {fields:?}");
    }

    #[test]
    fn collect_fields_sorted_by_id_and_applies_overrides() {
        // 定序+应答锚：多场按 EntityId 升序（确定序）；overrides 命中者用应答
        // delta（场有表达权世界有结算权），未命中者用出生声明 delta
        let mut world = WorldState::new();
        let mut a = make_entity(2, (5.0, 0.0), 0);
        a.set_field_radius(3.0);
        a.set_field_drag_delta(1.0);
        let mut b = make_entity(1, (20.0, 0.0), 0);
        b.set_field_radius(4.0);
        b.set_field_drag_delta(0.5);
        let a_id = a.id;
        let b_id = b.id;
        world.entities.insert(a_id, a);
        world.entities.insert(b_id, b);
        let mut overrides = HashMap::new();
        overrides.insert(b_id, -0.5);
        let fields = collect_fields(&world, 10, 3, &overrides);
        assert_eq!(fields.len(), 2);
        assert_eq!(
            fields[0].source_id,
            EntityId(Uuid::from_u128(1)),
            "应按 EntityId 升序"
        );
        assert_eq!(fields[0].drag_delta, -0.5, "on_field 应答覆盖本拍 delta");
        assert_eq!(fields[1].source_id, EntityId(Uuid::from_u128(2)));
        assert_eq!(fields[1].drag_delta, 1.0, "无应答用出生声明 delta");
    }
}
