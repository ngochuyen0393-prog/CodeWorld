//! 渲染相关数据结构（RenderLayer / RenderRequest / VisibleEntity / AttackResult）
//!
//! 图层只管渲染顺序，不影响物理逻辑（架构文档 2.2 节）。

use serde::{Deserialize, Serialize};

use crate::entity::{ActionState, Appearance, EntityId};

/// 渲染图层（参考 Unity Sorting Layer / Unreal Translucency Sort）
///
/// 图层只管渲染顺序，不影响物理逻辑。
/// 同图层内按 Y 轴排序（画家算法：Y值大的后画=近的遮远的）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(i32)]
pub enum RenderLayer {
    /// 世界层：地形、地面、背景
    Background = 0,
    /// 实体层：玩家、NPC、可交互物体（默认）
    Entity = 1,
    /// 效果层：特效、粒子、时空扭曲可视化
    Effect = 2,
    /// 界面层：叠加UI（MVP不用）
    UI = 3,
}

impl RenderLayer {
    /// 图层数值（用于排序：值小的先画）
    pub fn value(self) -> i32 {
        self as i32
    }
}

impl Default for RenderLayer {
    /// 默认图层=Entity（玩家代码不指定时默认实体层）
    fn default() -> Self {
        RenderLayer::Entity
    }
}

/// 渲染请求（实体提交给渲染队列的完整数据）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderRequest {
    /// 目标图层
    pub layer: RenderLayer,
    /// Y轴排序值（通常=实体y坐标，用于层内深度排序）
    pub y_sort_value: f64,
    /// 外观数据
    pub appearance: Appearance,
    /// 屏幕坐标
    pub pos: (f64, f64),
    /// 当前动作（影响外观帧）
    pub action_state: ActionState,
    /// 同图层同Y值时的微调优先级（默认0）
    pub priority: i32,
}

impl RenderRequest {
    /// 创建渲染请求
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        layer: RenderLayer,
        y_sort_value: f64,
        appearance: Appearance,
        pos: (f64, f64),
        action_state: ActionState,
        priority: i32,
    ) -> Self {
        let req = Self {
            layer,
            y_sort_value,
            appearance,
            pos,
            action_state,
            priority,
        };
        tracing::debug!(
            layer = req.layer.value(),
            y_sort_value,
            priority,
            "渲染请求提交"
        );
        req
    }
}

/// 视口内可见实体（感知接口返回）
///
/// 只包含"眼睛能看到"的信息，不包含代码层面信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibleEntity {
    /// 实体ID
    pub id: EntityId,
    /// 坐标（可见所以可知）
    pub pos: (f64, f64),
    /// 外观（可见所以可知）
    pub appearance: Appearance,
}

impl VisibleEntity {
    /// 创建可见实体
    pub fn new(id: EntityId, pos: (f64, f64), appearance: Appearance) -> Self {
        tracing::debug!(entity_id = %id.0, ?pos, "可见实体采集");
        Self { id, pos, appearance }
    }
}

/// 攻击结果（架构文档 2.2 / 4.2 节）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttackResult {
    /// 是否命中（坐标上有实体）
    pub hit: bool,
    /// 是否被 on_attack 拦截
    pub intercepted: bool,
    /// 是否击杀成功（life() 被覆写为 false）
    pub success: bool,
}

impl AttackResult {
    /// 任意组合构造
    pub fn new(hit: bool, intercepted: bool, success: bool) -> Self {
        let r = Self { hit, intercepted, success };
        tracing::debug!(hit, intercepted, success, "攻击结果产生");
        r
    }

    /// 攻击落空（坐标上无实体，miss）
    pub fn miss() -> Self {
        Self::new(false, false, false)
    }

    /// 命中且击杀成功（life() 被覆写为 false）
    pub fn killed() -> Self {
        Self::new(true, false, true)
    }

    /// 命中但被 on_attack 拦截
    pub fn intercepted() -> Self {
        Self::new(true, true, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{ActionState, Appearance, EntityId};
    use uuid::Uuid;

    // === RenderLayer ===

    #[test]
    fn render_layer_values_match_doc() {
        // 创建：图层优先级数值符合架构文档
        assert_eq!(RenderLayer::Background.value(), 0);
        assert_eq!(RenderLayer::Entity.value(), 1);
        assert_eq!(RenderLayer::Effect.value(), 2);
        assert_eq!(RenderLayer::UI.value(), 3);
    }

    #[test]
    fn render_layer_ordering() {
        // 边界：图层优先级 Background < Entity < Effect < UI
        assert!(RenderLayer::Background.value() < RenderLayer::Entity.value());
        assert!(RenderLayer::Entity.value() < RenderLayer::Effect.value());
        assert!(RenderLayer::Effect.value() < RenderLayer::UI.value());
    }

    #[test]
    fn render_layer_default_is_entity() {
        // 默认图层=Entity（玩家代码不指定时默认实体层）
        assert_eq!(RenderLayer::default(), RenderLayer::Entity);
    }

    #[test]
    fn render_layer_serialize_roundtrip() {
        let l = RenderLayer::Effect;
        let json = serde_json::to_string(&l).unwrap();
        let back: RenderLayer = serde_json::from_str(&json).unwrap();
        assert_eq!(l, back);
    }

    // === RenderRequest ===

    fn sample_appearance() -> Appearance {
        Appearance::new("@".to_string(), "red".to_string(), 1.0)
    }

    #[test]
    fn render_request_new_stores_fields() {
        // 创建：渲染请求字段正确存取
        let req = RenderRequest::new(
            RenderLayer::Entity,
            5.0,
            sample_appearance(),
            (10.0, 20.0),
            ActionState::Walking,
            0,
        );
        assert_eq!(req.layer, RenderLayer::Entity);
        assert_eq!(req.y_sort_value, 5.0);
        assert_eq!(req.appearance.glyph, "@");
        assert_eq!(req.pos, (10.0, 20.0));
        assert_eq!(req.action_state, ActionState::Walking);
        assert_eq!(req.priority, 0);
    }

    #[test]
    fn render_request_default_priority_is_zero() {
        // 边界：priority 默认0（同图层同Y值时微调）
        let req = RenderRequest::new(
            RenderLayer::Background,
            0.0,
            sample_appearance(),
            (0.0, 0.0),
            ActionState::Idle,
            0,
        );
        assert_eq!(req.priority, 0);
    }

    #[test]
    fn render_request_serialize_roundtrip() {
        let req = RenderRequest::new(
            RenderLayer::Effect,
            3.5,
            sample_appearance(),
            (1.0, 2.0),
            ActionState::Custom("cast".to_string()),
            -1,
        );
        let json = serde_json::to_string(&req).unwrap();
        let back: RenderRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.layer, RenderLayer::Effect);
        assert_eq!(back.priority, -1);
        assert_eq!(back.action_state, ActionState::Custom("cast".to_string()));
    }

    // === VisibleEntity ===

    #[test]
    fn visible_entity_new_stores_fields() {
        // 创建：可见实体只含"眼睛能看到"的信息
        let id = EntityId::new();
        let ve = VisibleEntity::new(id, (7.0, 8.0), sample_appearance());
        assert_eq!(ve.id, id);
        assert_eq!(ve.pos, (7.0, 8.0));
        assert_eq!(ve.appearance.glyph, "@");
    }

    #[test]
    fn visible_entity_serialize_roundtrip() {
        let ve = VisibleEntity::new(
            EntityId(Uuid::from_u128(5)),
            (1.0, 2.0),
            sample_appearance(),
        );
        let json = serde_json::to_string(&ve).unwrap();
        let back: VisibleEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(ve.id, back.id);
        assert_eq!(ve.pos, back.pos);
    }

    // === AttackResult ===

    #[test]
    fn attack_result_miss_is_all_false() {
        // 边界：攻击落空（坐标上无实体）hit=false
        let r = AttackResult::miss();
        assert!(!r.hit);
        assert!(!r.intercepted);
        assert!(!r.success);
    }

    #[test]
    fn attack_result_killed_is_hit_and_success() {
        // 命中且击杀成功
        let r = AttackResult::killed();
        assert!(r.hit);
        assert!(!r.intercepted);
        assert!(r.success);
    }

    #[test]
    fn attack_result_intercepted_is_hit_but_no_success() {
        // 命中但被拦截
        let r = AttackResult::intercepted();
        assert!(r.hit);
        assert!(r.intercepted);
        assert!(!r.success);
    }

    #[test]
    fn attack_result_custom_combo() {
        // 读写：任意组合可通过 new 构造
        let r = AttackResult::new(true, false, false); // 命中但未击杀
        assert!(r.hit);
        assert!(!r.intercepted);
        assert!(!r.success);
    }

    #[test]
    fn attack_result_serialize_roundtrip() {
        let r = AttackResult::killed();
        let json = serde_json::to_string(&r).unwrap();
        let back: AttackResult = serde_json::from_str(&json).unwrap();
        assert!(back.hit);
        assert!(back.success);
    }
}
