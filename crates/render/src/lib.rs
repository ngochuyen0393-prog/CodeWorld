//! render：ASCII 终端渲染模块
//!
//! 借鉴寒霜引擎 FrameGraph 架构（GDC 2017, Yuriy O'Donnell）的三阶段管线：
//! Setup（声明）-> Compile（裁剪+排序）-> Execute（绘制）。
//!
//! 架构文档 4.1 节 e 步定义渲染流程：从渲染队列取 RenderRequest，
//! 按图层排序后绘制（Background < Entity < Effect < UI，层内按 Y 轴深度排序）。

pub mod buffer;
pub mod graph;
pub mod module;
pub mod viewport;

pub use buffer::{Cell, TerminalBuffer};
pub use graph::{RenderGraph, RenderNode};
pub use module::{AsciiRenderModule, RenderModule};
pub use viewport::Viewport;

use codeworld_core::world::WorldState;

/// Renderer 编排器（借鉴寒霜 WorldRenderer 角色：只编排，不分配资源）
///
/// MVP 只有一个渲染模块（AsciiRenderModule）。
pub struct Renderer {
    graph: RenderGraph,
    buffer: TerminalBuffer,
    viewport: Viewport,
    module: AsciiRenderModule,
}

impl Renderer {
    /// 创建 Renderer，buffer 尺寸取自 viewport 的 term_rows/term_cols
    pub fn new(viewport: Viewport) -> Self {
        let (rows, cols) = (viewport.term_rows, viewport.term_cols);
        Self {
            graph: RenderGraph::new(),
            buffer: TerminalBuffer::new(rows, cols),
            viewport,
            module: AsciiRenderModule::new(),
        }
    }

    /// 完整渲染流程（三阶段），返回本帧与上帧的差异格子坐标列表
    ///
    /// 流程：① clear -> ② setup（声明）-> ③ compile（裁剪+排序）-> ④ execute（绘制）
    ///      -> ⑤ diff_cells（计算差异）-> ⑥ swap（flush 后调用）
    ///
    /// 注：步骤⑥ swap 语义为"flush 后调用"，MVP 步骤⑤"只计算不输出"即无真实 flush，
    /// 故暂不执行 swap——buffer.current 持有本帧画面供读取/测试。
    /// 接入真实终端 flush 时，在 flush 完成后调用 self.buffer.swap()。
    pub fn render(&mut self, world: &WorldState) -> Vec<(usize, usize)> {
        // ① 清空当前帧
        self.buffer.clear();
        // ② 声明：模块采集渲染请求
        self.module.setup(&mut self.graph, world, &self.viewport);
        // ③ 编译：视口裁剪 + 排序 + 屏幕坐标
        self.graph.compile(&self.viewport);
        // ④ 执行：按序写入 buffer（画家算法：后写覆盖先写）
        self.graph.execute(&mut self.buffer);
        // ⑤ 计算差异（MVP 只计算不输出，返回供测试验证）
        let diff = self.buffer.diff_cells();
        // ⑥ swap：flush 后调用；MVP 无真实输出，暂不交换（见上注）
        tracing::debug!(diff_count = diff.len(), "Renderer 渲染完成");
        diff
    }

    /// Flush：真实终端输出完成后调用，交换缓冲（供下帧 diff）
    ///
    /// MVP 无真实终端输出，render() 不调用本方法（buffer.current 持有本帧供读取/测试）。
    /// 接入真实终端 flush 时：先输出 buffer.current，再调用本方法交换缓冲。
    pub fn flush(&mut self) {
        self.buffer.swap();
    }

    /// 只读访问 buffer（测试验证用）
    pub fn buffer(&self) -> &TerminalBuffer {
        &self.buffer
    }

    /// 只读访问 graph（测试验证用）
    pub fn graph(&self) -> &RenderGraph {
        &self.graph
    }

    /// 只读访问 viewport
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{ActionState, Appearance, Entity, EntityId, PlayerId};
    use codeworld_core::render::{RenderLayer, RenderRequest};
    use uuid::Uuid;

    fn vp_10x10() -> Viewport {
        Viewport::new((5.0, 5.0), 5.0, 5.0, 10, 10)
    }

    fn renderer_10x10() -> Renderer {
        Renderer::new(Viewport::new((5.0, 5.0), 5.0, 5.0, 10, 10))
    }

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
    fn render_draws_entities_to_buffer() {
        // 集成：多实体 render() 后 buffer 中有正确的字符
        let mut r = renderer_10x10();
        let mut world = WorldState::new();
        world.entities.insert(EntityId(Uuid::from_u128(1)), make_entity(1, (5.0, 5.0), "@", true));
        world.entities.insert(EntityId(Uuid::from_u128(2)), make_entity(2, (3.0, 3.0), "#", true));
        let diff = r.render(&world);
        // 两实体都画到各自位置
        assert_eq!(r.buffer().get(5, 5).unwrap().glyph, "@");
        assert_eq!(r.buffer().get(3, 3).unwrap().glyph, "#");
        // 首帧 previous 全空格 -> 两个写入格都在 diff 中
        assert!(diff.contains(&(5, 5)));
        assert!(diff.contains(&(3, 3)));
    }

    #[test]
    fn render_skips_dead_entities() {
        // 集成：死亡实体不渲染
        let mut r = renderer_10x10();
        let mut world = WorldState::new();
        world.entities.insert(EntityId(Uuid::from_u128(1)), make_entity(1, (5.0, 5.0), "@", false));
        r.render(&world);
        assert_eq!(r.buffer().get(5, 5).unwrap().glyph, " "); // 死亡不画，保持空格
    }

    #[test]
    fn render_skips_outside_viewport() {
        // 集成：视口外实体不渲染（compile 裁剪）
        let mut r = renderer_10x10();
        let mut world = WorldState::new();
        world.entities.insert(EntityId(Uuid::from_u128(1)), make_entity(1, (100.0, 5.0), "@", true));
        r.render(&world);
        assert_eq!(r.graph().node_count(), 0); // 视口外被裁剪
        assert_eq!(r.buffer().cell_at(5, 5).glyph, " ");
    }

    #[test]
    fn render_empty_world_no_panic() {
        // 集成：空世界 render() 不 panic
        let mut r = renderer_10x10();
        let world = WorldState::new();
        let diff = r.render(&world);
        assert!(diff.is_empty()); // 无实体 -> 无写入 -> 无 diff
    }

    #[test]
    fn render_y_sort_keeps_both_entities() {
        // 集成：同图层不同 Y 的两实体都渲染到各自位置（Y 排序不丢实体）
        let mut r = renderer_10x10();
        let mut world = WorldState::new();
        world.entities.insert(EntityId(Uuid::from_u128(1)), make_entity(1, (5.0, 2.0), "L", true));
        world.entities.insert(EntityId(Uuid::from_u128(2)), make_entity(2, (5.0, 8.0), "H", true));
        r.render(&world);
        assert_eq!(r.buffer().get(2, 5).unwrap().glyph, "L"); // y=2 -> row 2
        assert_eq!(r.buffer().get(8, 5).unwrap().glyph, "H"); // y=8 -> row 8
    }

    #[test]
    fn layer_order_background_overridden_by_entity() {
        // 集成：图层排序正确（Background 先画，Entity 后画覆盖）
        // AsciiRenderModule 只产 Entity 层，故此处直接驱动 graph+buffer 验证管线图层排序
        let vp = vp_10x10();
        let mut graph = RenderGraph::new();
        let q = vec![
            RenderRequest::new(
                RenderLayer::Background,
                5.0,
                Appearance::new("#".to_string(), "gray".to_string(), 1.0),
                (5.0, 5.0),
                ActionState::Idle,
                0,
            ),
            RenderRequest::new(
                RenderLayer::Entity,
                5.0,
                Appearance::new("@".to_string(), "red".to_string(), 1.0),
                (5.0, 5.0),
                ActionState::Idle,
                0,
            ),
        ];
        graph.setup(&q);
        graph.compile(&vp);
        let mut buf = TerminalBuffer::new(10, 10);
        graph.execute(&mut buf);
        assert_eq!(buf.get(5, 5).unwrap().glyph, "@"); // Entity 覆盖 Background
    }

    #[test]
    fn viewport_accessor_returns_reference() {
        // 读写：viewport() 只读访问
        let r = renderer_10x10();
        assert_eq!(r.viewport().term_rows, 10);
        assert_eq!(r.viewport().term_cols, 10);
    }
}
