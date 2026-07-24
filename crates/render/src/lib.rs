//! render：ASCII 终端渲染模块
//!
//! 借鉴寒霜引擎 FrameGraph 架构（GDC 2017, Yuriy O'Donnell）的三阶段管线：
//! Setup（声明）-> Compile（裁剪+排序）-> Execute（绘制）。
//!
//! 架构文档 4.1 节 e 步定义渲染流程：从渲染队列取 RenderRequest，
//! 按图层排序后绘制（Background < Entity < Effect < UI，层内按 Y 轴深度排序）。

pub mod ansi;
pub mod buffer;
pub mod graph;
pub mod module;
pub mod viewport;

pub use ansi::{color_to_ansi, enter_terminal, exit_terminal};
pub use buffer::{Cell, TerminalBuffer};
pub use graph::{RenderGraph, RenderNode};
pub use module::{AsciiRenderModule, RenderModule};
pub use viewport::Viewport;

use codeworld_core::world::WorldState;
use std::io::Write;

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
    ///      -> ⑤ diff_cells（计算差异）；⑥ swap 在 flush() 中完成（输出 ANSI 后 swap）
    ///
    /// 注：render() 本身不交换缓冲，buffer.current 持有本帧画面供 flush 读取。
    /// flush() 输出 diff 后再 swap（供下帧 diff）；render() 不调用 swap。
    pub fn render(&mut self, world: &WorldState) -> Vec<(usize, usize)> {
        // ① 清空当前帧
        self.buffer.clear();
        // ② 声明：模块采集渲染请求
        self.module.setup(&mut self.graph, world, &self.viewport);
        // ③ 编译：视口裁剪 + 排序 + 屏幕坐标
        self.graph.compile(&self.viewport);
        // ④ 执行：按序写入 buffer（画家算法：后写覆盖先写）
        self.graph.execute(&mut self.buffer);
        // ⑤ 计算差异（返回供调用方判断是否 flush）
        let diff = self.buffer.diff_cells();
        // ⑥ swap 在 flush() 中完成：输出 diff 后再 swap
        tracing::debug!(diff_count = diff.len(), "Renderer 渲染完成");
        diff
    }

    /// Flush：把本帧 diff 以 ANSI 转义序列输出到 stdout，再交换缓冲（供下帧 diff）
    ///
    /// 仅当 render() 返回非空 diff 时调用（main.rs tick 第 e 步）。
    /// 输出失败仅记日志不 panic（终端不可写不应崩主循环），且不 swap 以便下帧重试未绘变更。
    pub fn flush(&mut self) {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        if let Err(e) = self.flush_to(&mut out) {
            tracing::warn!(error = %e, "[flush] 渲染输出到 stdout 失败");
            return;
        }
        // 显式 flush：stdout 行缓冲，ANSI 转义无换行需手动冲刷才即时显示
        if let Err(e) = out.flush() {
            tracing::warn!(error = %e, "[flush] stdout flush 失败");
        }
    }

    /// Flush 到任意 writer（测试用 Vec<u8> 验证 ANSI 输出）
    ///
    /// 输出 buffer 的 diff 格子（flush_diff），成功后 swap 缓冲。
    /// 输出失败则不 swap（保留 diff 状态供下帧重试）。
    pub fn flush_to<W: std::io::Write>(&mut self, out: &mut W) -> std::io::Result<()> {
        self.buffer.flush_diff(out)?;
        self.buffer.swap();
        tracing::debug!("[flush_to] 渲染输出完成并已 swap");
        Ok(())
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

    // === flush_to：Renderer ANSI 输出 ===

    #[test]
    fn flush_to_outputs_ansi_for_rendered_entities() {
        // 读写：render 后 flush_to 输出实体的 ANSI 序列（光标+颜色+字符）
        let mut r = renderer_10x10();
        let mut world = WorldState::new();
        world.entities.insert(
            EntityId(Uuid::from_u128(1)),
            make_entity(1, (5.0, 5.0), "@", true),
        );
        let diff = r.render(&world);
        assert!(!diff.is_empty(), "首帧应有 diff");
        let mut out = Vec::new();
        r.flush_to(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        // 世界 (5,5) -> 屏幕 (5,5) -> ANSI 光标 6;6；red->31；glyph @
        assert!(
            s.contains("\x1b[6;6H\x1b[31m@"),
            "应输出实体 ANSI，实际 {:?}",
            s
        );
    }

    #[test]
    fn flush_to_swaps_so_next_render_has_no_diff() {
        // 修改：flush_to 输出后 swap -> 同状态再 render 无 diff（证明 swap 生效）
        let mut r = renderer_10x10();
        let mut world = WorldState::new();
        world.entities.insert(
            EntityId(Uuid::from_u128(1)),
            make_entity(1, (5.0, 5.0), "@", true),
        );
        let first = r.render(&world);
        assert!(!first.is_empty(), "首帧应有 diff");
        let mut out = Vec::new();
        r.flush_to(&mut out).unwrap();
        let second = r.render(&world);
        assert!(
            second.is_empty(),
            "flush_to 应 swap 使下帧无 diff，实际 {:?}",
            second
        );
    }
}
