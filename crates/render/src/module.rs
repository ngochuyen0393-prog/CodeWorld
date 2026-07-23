//! 渲染模块（RenderModule trait + AsciiRenderModule）
//!
//! 借鉴寒霜 FrameGraph 的 Features 模块概念（渲染设计文档 5.5）。
//! MVP 只有一个实现：AsciiRenderModule，遍历世界存活实体提交渲染请求。
//!
//! 注：MVP 阶段实体不主动提交渲染请求（没有 WASM 执行），
//! AsciiRenderModule 代替实体遍历世界状态生成渲染请求。
//! Phase 2 实体在 on_tick 中调 render() 自行提交。

use codeworld_core::render::{RenderLayer, RenderRequest};
use codeworld_core::world::WorldState;

use crate::graph::RenderGraph;
use crate::viewport::Viewport;

/// 渲染模块 trait（借鉴寒霜 Features 模块）
///
/// MVP 只有一个实现：AsciiRenderModule。
/// v2 可扩展：SpriteRenderModule / EffectRenderModule / UIRenderModule。
pub trait RenderModule {
    /// Setup：声明本模块的渲染节点（向 graph 提交渲染请求）
    fn setup(&self, graph: &mut RenderGraph, world: &WorldState, viewport: &Viewport);

    /// 模块名（用于 blackboard 注册）
    fn name(&self) -> &str;
}

/// ASCII 渲染模块（MVP 默认）
///
/// 遍历 world.entities 中所有存活实体，为每个实体创建 RenderRequest
/// （图层默认 Entity，y_sort_value = 实体 y 坐标，priority = 0）。
#[derive(Debug, Default)]
pub struct AsciiRenderModule;

impl AsciiRenderModule {
    /// 创建 ASCII 渲染模块
    pub fn new() -> Self {
        tracing::debug!("AsciiRenderModule 创建");
        Self
    }
}

impl RenderModule for AsciiRenderModule {
    fn setup(&self, graph: &mut RenderGraph, world: &WorldState, _viewport: &Viewport) {
        // MVP 不按视口筛选实体（graph.compile 会裁剪视口外的节点）
        let requests: Vec<RenderRequest> = world
            .entities
            .values()
            .filter(|e| e.is_alive())
            .map(|e| {
                RenderRequest::new(
                    RenderLayer::Entity, // 默认图层
                    e.pos().1,           // y_sort_value = 实体 y 坐标
                    e.appearance.clone(),
                    e.pos(),
                    e.action_state().clone(),
                    0, // priority 默认 0
                )
            })
            .collect();
        tracing::debug!(alive = requests.len(), "AsciiRenderModule 采集渲染请求");
        graph.setup(&requests);
    }

    fn name(&self) -> &str {
        "ascii"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{ActionState, Appearance, Entity, EntityId, PlayerId};
    use codeworld_core::render::RenderLayer;
    use uuid::Uuid;

    fn vp_10x10() -> Viewport {
        Viewport::new((5.0, 5.0), 5.0, 5.0, 10, 10)
    }

    // 构造实体：pos/glyph/存活可定制
    fn make_entity(id_u: u128, pos: (f64, f64), glyph: &str, alive: bool) -> Entity {
        let mut e = Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            format!("hash_{id_u}"),
            pos,
            Appearance::new(glyph.to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        );
        if !alive {
            e.kill();
        }
        e
    }

    #[test]
    fn setup_submits_request_per_alive_entity() {
        // 读写：遍历存活实体，每个生成一个请求
        let module = AsciiRenderModule::new();
        let mut world = WorldState::new();
        world.entities.insert(EntityId(Uuid::from_u128(1)), make_entity(1, (5.0, 5.0), "A", true));
        world.entities.insert(EntityId(Uuid::from_u128(2)), make_entity(2, (5.0, 6.0), "B", true));
        world.entities.insert(EntityId(Uuid::from_u128(3)), make_entity(3, (5.0, 7.0), "D", false));
        let vp = vp_10x10();
        let mut graph = RenderGraph::new();
        module.setup(&mut graph, &world, &vp);
        assert_eq!(graph.node_count(), 2); // 2 个存活
    }

    #[test]
    fn setup_skips_dead_entities() {
        // 边界：死亡实体不生成请求
        let module = AsciiRenderModule::new();
        let mut world = WorldState::new();
        world.entities.insert(EntityId(Uuid::from_u128(1)), make_entity(1, (5.0, 5.0), "A", false));
        let vp = vp_10x10();
        let mut graph = RenderGraph::new();
        module.setup(&mut graph, &world, &vp);
        assert_eq!(graph.node_count(), 0);
    }

    #[test]
    fn setup_request_fields_match_entity() {
        // 读写：生成的请求字段与实体一致
        let module = AsciiRenderModule::new();
        let mut world = WorldState::new();
        world.entities.insert(EntityId(Uuid::from_u128(1)), make_entity(1, (3.0, 7.0), "@", true));
        let vp = vp_10x10();
        let mut graph = RenderGraph::new();
        module.setup(&mut graph, &world, &vp);
        assert_eq!(graph.node_count(), 1);
        let req = &graph.nodes()[0].request;
        assert_eq!(req.layer, RenderLayer::Entity);
        assert_eq!(req.y_sort_value, 7.0); // = pos.y
        assert_eq!(req.pos, (3.0, 7.0));
        assert_eq!(req.priority, 0);
        assert_eq!(req.appearance.glyph, "@");
        assert_eq!(req.action_state, ActionState::Idle); // Entity 默认 Idle
    }

    #[test]
    fn ascii_module_name() {
        // 读写：模块名
        assert_eq!(AsciiRenderModule::new().name(), "ascii");
    }

    #[test]
    fn setup_empty_world_no_panic() {
        // 边界：空世界 setup 不 panic
        let module = AsciiRenderModule::new();
        let world = WorldState::new();
        let vp = vp_10x10();
        let mut graph = RenderGraph::new();
        module.setup(&mut graph, &world, &vp);
        assert_eq!(graph.node_count(), 0);
    }
}
