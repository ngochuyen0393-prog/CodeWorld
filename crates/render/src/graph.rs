//! 渲染图（RenderGraph + RenderNode）：三阶段管线
//!
//! 借鉴寒霜 FrameGraph 三阶段（渲染设计文档 5.1/5.2）：
//! - Setup（声明）：每个 RenderRequest 创建一个 RenderNode
//! - Compile（编译）：视口裁剪 + 排序 + 计算屏幕坐标
//! - Execute（执行）：按序写入终端缓冲区（画家算法：后写覆盖先写）
//!
//! 排序规则（架构文档 4.1e）：
//! - 图层升序：Background < Entity < Effect < UI
//! - 同图层内 Y 升序：Y 值大的排后面（后画=近的遮远的）
//!   注：设计文档 5.2 措辞"Y 降序"与"Y 大排后面"矛盾，
//!   以画家算法物理语义（Y 大=近=后画=数组后面=升序）为准。
//! - 同图层同 Y 内 priority 升序：priority 小的排前面

use std::cmp::Ordering;

use codeworld_core::render::RenderRequest;

use crate::buffer::TerminalBuffer;
use crate::viewport::Viewport;

/// 渲染图节点（一个渲染操作 = 画一个实体）
#[derive(Debug, Clone)]
pub struct RenderNode {
    /// 自增序号（内部追踪用）
    pub id: usize,
    /// 源渲染请求副本
    pub request: RenderRequest,
    /// 编译阶段计算的终端坐标
    pub screen_pos: Option<(usize, usize)>,
}

/// 渲染图
pub struct RenderGraph {
    nodes: Vec<RenderNode>,
    next_id: usize,
}

impl RenderGraph {
    /// 创建空渲染图
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            next_id: 0,
        }
    }

    /// ① Setup（声明阶段）：遍历 queue，每个 RenderRequest 创建一个 RenderNode
    ///
    /// screen_pos 初始化为 None（compile 阶段填）。
    pub fn setup(&mut self, queue: &[RenderRequest]) {
        self.nodes.clear();
        self.next_id = 0;
        for req in queue {
            let id = self.next_id;
            self.next_id += 1;
            self.nodes.push(RenderNode {
                id,
                request: req.clone(),
                screen_pos: None,
            });
        }
        tracing::debug!(count = self.nodes.len(), "渲染图 Setup 完成");
    }

    /// ② Compile（编译阶段）：视口裁剪 + 排序 + 计算屏幕坐标
    ///
    /// a. retain 只保留 pos 在视口内的节点
    /// b. 排序：layer 升序 -> y 升序（Y 大排后面）-> priority 升序
    /// c. 计算屏幕坐标
    pub fn compile(&mut self, viewport: &Viewport) {
        // a. 视口裁剪
        self.nodes
            .retain(|n| viewport.contains(n.request.pos.0, n.request.pos.1));

        // b. 排序：图层升序 -> Y 升序 -> priority 升序
        self.nodes.sort_by(|a, b| {
            a.request
                .layer
                .value()
                .cmp(&b.request.layer.value())
                .then_with(|| {
                    a.request
                        .y_sort_value
                        .partial_cmp(&b.request.y_sort_value)
                        .unwrap_or(Ordering::Equal)
                })
                .then_with(|| a.request.priority.cmp(&b.request.priority))
        });

        // c. 计算屏幕坐标
        for n in &mut self.nodes {
            n.screen_pos = viewport.world_to_screen(n.request.pos);
        }
        tracing::debug!(remaining = self.nodes.len(), "渲染图 Compile 完成");
    }

    /// ③ Execute（执行阶段）：按序写入 buffer（后写覆盖先写）
    pub fn execute(&self, buffer: &mut TerminalBuffer) {
        for n in &self.nodes {
            if let Some((row, col)) = n.screen_pos {
                buffer.write(row, col, &n.request.appearance.glyph, &n.request.appearance.color);
            }
        }
        tracing::debug!(written = self.nodes.len(), "渲染图 Execute 完成");
    }

    /// 清空节点
    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// 节点数量
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 只读访问节点（测试用）
    pub fn nodes(&self) -> &[RenderNode] {
        &self.nodes
    }
}

impl Default for RenderGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{ActionState, Appearance};
    use codeworld_core::render::RenderLayer;

    // 视口：center=(5,5), half=5,5, 终端 10x10 -> 世界范围 (0..10, 0..10)
    fn vp_10x10() -> Viewport {
        Viewport::new((5.0, 5.0), 5.0, 5.0, 10, 10)
    }

    // 构造渲染请求：pos=(5, y)（x=5 在视口内），y_sort_value=y
    fn make_req(layer: RenderLayer, y: f64, priority: i32, glyph: &str) -> RenderRequest {
        RenderRequest::new(
            layer,
            y,
            Appearance::new(glyph.to_string(), "red".to_string(), 1.0),
            (5.0, y),
            ActionState::Idle,
            priority,
        )
    }

    #[test]
    fn setup_creates_node_per_request() {
        // 读写：N 个请求生成 N 个节点，screen_pos 初始 None，id 自增
        let mut g = RenderGraph::new();
        let q = vec![
            make_req(RenderLayer::Entity, 1.0, 0, "A"),
            make_req(RenderLayer::Entity, 2.0, 0, "B"),
        ];
        g.setup(&q);
        assert_eq!(g.node_count(), 2);
        assert!(g.nodes().iter().all(|n| n.screen_pos.is_none()));
        let ids: Vec<usize> = g.nodes().iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![0, 1]);
    }

    #[test]
    fn compile_culls_nodes_outside_viewport() {
        // 边界：视口外节点被裁剪
        let mut g = RenderGraph::new();
        let inside = make_req(RenderLayer::Entity, 5.0, 0, "A"); // pos(5,5) 视口内
        let outside = RenderRequest::new(
            RenderLayer::Entity,
            5.0,
            Appearance::new("B".to_string(), "red".to_string(), 1.0),
            (100.0, 5.0), // x=100 视口外
            ActionState::Idle,
            0,
        );
        g.setup(&[inside, outside]);
        g.compile(&vp_10x10());
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.nodes()[0].request.appearance.glyph, "A");
    }

    #[test]
    fn compile_sorts_by_layer_ascending() {
        // 读写：图层升序 Background < Entity < Effect < UI
        let mut g = RenderGraph::new();
        // 故意乱序提交
        let q = vec![
            make_req(RenderLayer::UI, 5.0, 0, "U"),
            make_req(RenderLayer::Background, 5.0, 0, "B"),
            make_req(RenderLayer::Effect, 5.0, 0, "E"),
            make_req(RenderLayer::Entity, 5.0, 0, "N"),
        ];
        g.setup(&q);
        g.compile(&vp_10x10());
        let glyphs: String = g
            .nodes()
            .iter()
            .map(|n| n.request.appearance.glyph.chars().next().unwrap())
            .collect();
        assert_eq!(glyphs, "BNEU");
    }

    #[test]
    fn compile_sorts_y_large_later_within_layer() {
        // 读写：同图层内 Y 值大的排后面（后画）
        let mut g = RenderGraph::new();
        let q = vec![
            make_req(RenderLayer::Entity, 8.0, 0, "H"), // Y 大
            make_req(RenderLayer::Entity, 2.0, 0, "L"), // Y 小
            make_req(RenderLayer::Entity, 5.0, 0, "M"),
        ];
        g.setup(&q);
        g.compile(&vp_10x10());
        let ys: Vec<f64> = g.nodes().iter().map(|n| n.request.y_sort_value).collect();
        // 升序：Y 小的排前面，Y 大的排后面
        assert_eq!(ys, vec![2.0, 5.0, 8.0]);
    }

    #[test]
    fn compile_sorts_priority_small_first_within_same_y() {
        // 读写：同图层同 Y 值 priority 小的排前面
        let mut g = RenderGraph::new();
        let q = vec![
            make_req(RenderLayer::Entity, 5.0, 10, "a"),
            make_req(RenderLayer::Entity, 5.0, 1, "b"),
            make_req(RenderLayer::Entity, 5.0, 5, "c"),
        ];
        g.setup(&q);
        g.compile(&vp_10x10());
        let pris: Vec<i32> = g.nodes().iter().map(|n| n.request.priority).collect();
        assert_eq!(pris, vec![1, 5, 10]); // priority 升序
    }

    #[test]
    fn compile_computes_screen_pos() {
        // 读写：screen_pos 正确计算（pos(5,5) -> 终端 (5,5)）
        let mut g = RenderGraph::new();
        let q = vec![make_req(RenderLayer::Entity, 5.0, 0, "A")];
        g.setup(&q);
        g.compile(&vp_10x10());
        assert_eq!(g.nodes()[0].screen_pos, Some((5, 5)));
    }

    #[test]
    fn execute_writes_in_order_later_overrides() {
        // 读写：按序写入 buffer，后写的覆盖先写的
        // 同 layer 同 y 同格 (5,5)，priority 升序 -> priority 1 在后覆盖 priority 0
        let mut g = RenderGraph::new();
        let q = vec![
            RenderRequest::new(
                RenderLayer::Entity,
                5.0,
                Appearance::new("A".to_string(), "red".to_string(), 1.0),
                (5.0, 5.0),
                ActionState::Idle,
                0,
            ),
            RenderRequest::new(
                RenderLayer::Entity,
                5.0,
                Appearance::new("B".to_string(), "blue".to_string(), 1.0),
                (5.0, 5.0),
                ActionState::Idle,
                1,
            ),
        ];
        g.setup(&q);
        g.compile(&vp_10x10());
        let mut buf = TerminalBuffer::new(10, 10);
        g.execute(&mut buf);
        assert_eq!(buf.get(5, 5).unwrap().glyph, "B"); // priority 1（后画）覆盖
    }

    #[test]
    fn execute_higher_layer_overrides_lower() {
        // 读写：图层排序正确（Background 先画，Entity 后画覆盖）
        let mut g = RenderGraph::new();
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
        g.setup(&q);
        g.compile(&vp_10x10());
        let mut buf = TerminalBuffer::new(10, 10);
        g.execute(&mut buf);
        assert_eq!(buf.get(5, 5).unwrap().glyph, "@"); // Entity 覆盖 Background
    }

    #[test]
    fn clear_empties_nodes() {
        // 修改：clear 后节点为空
        let mut g = RenderGraph::new();
        g.setup(&[make_req(RenderLayer::Entity, 1.0, 0, "A")]);
        assert_eq!(g.node_count(), 1);
        g.clear();
        assert_eq!(g.node_count(), 0);
    }
}
