//! render：ASCII 终端渲染模块
//!
//! 借鉴寒霜引擎 FrameGraph 架构（GDC 2017, Yuriy O'Donnell）的三阶段管线：
//! Setup（声明）-> Compile（裁剪+排序）-> Execute（绘制）。
//!
//! 架构文档 4.1 节 e 步定义渲染流程：从渲染队列取 RenderRequest，
//! 按图层排序后绘制（Background < Entity < Effect < UI，层内按 Y 轴深度排序）。

pub mod ansi;
pub mod buffer;
pub mod caption;
pub mod graph;
pub mod module;
pub mod viewport;

pub use ansi::{color_to_ansi, enter_terminal, exit_terminal};
pub use buffer::{Cell, TerminalBuffer};
pub use caption::{CaptionState, format_caption};
pub use graph::{RenderGraph, RenderNode};
pub use module::{AsciiRenderModule, RenderModule};
pub use viewport::Viewport;

use codeworld_core::entity::EntityId;
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
    /// 上一帧 render() 算出的 diff，供 flush() 复用（避免 flush_diff 重复扫描全缓冲）
    last_diff: Vec<(usize, usize)>,
    /// 战况字幕状态（第 0 行，渲染层展示，非游戏状态）
    caption: CaptionState,
    /// 操控目标（#022 操控层 v0：被操控实体渲染白色高亮；渲染层特判，
    /// 不改 core 数据结构；None = 无玩家在线不高亮）
    controlled: Option<EntityId>,
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
            last_diff: Vec::new(),
            caption: CaptionState::new(),
            controlled: None,
        }
    }

    /// 设置操控目标（#022：host 每 tick 从操控路由喂入；None = 无玩家在线）
    ///
    /// 高亮跟随"在线的操控目标"而非"正在按键"——回合制下按高亮闪现会让玩家
    /// 丢失自己角色（roguelike 惯例：你的 @ 始终视觉可辨）。
    pub fn set_controlled(&mut self, controlled: Option<EntityId>) {
        self.controlled = controlled;
    }

    /// 观测当前操控目标（期四第一刀：WS 快照组装在 tick 外部读；同 caption_line 先例）
    pub fn controlled(&self) -> Option<EntityId> {
        self.controlled
    }

    /// 更新战况字幕（host 每 tick 喂入只读战况快照）
    ///
    /// - tick/距离/存活数来自 world；盾量来自玩家代码 env.log marker 上报（host 解析）。
    /// - `d_present`：防御者是否在场（不在场则盾显示占位符）。
    /// - `shield_report`：本 tick 的盾量上报，None = 无新上报（保持上次值）。
    /// - `ctrl`：操控目标字形（操控层 v0；None = 无玩家在线，不显示 ctrl 段）。
    /// - 战况值（距离 0.1 精度/盾量/存活数/操控状态）变化时打一条 info 日志，不每 tick 刷屏。
    /// - 实际写 buffer 发生在 render()（本帧 diff 计算之前，保证变化进 diff）。
    pub fn update_caption(
        &mut self,
        tick: u64,
        dist: Option<f64>,
        d_present: bool,
        shield_report: Option<i32>,
        alive: usize,
        ctrl: Option<&str>,
    ) {
        self.caption
            .update(tick, dist, d_present, shield_report, alive, ctrl);
    }

    /// 完整渲染流程（三阶段），返回本帧与上帧的差异格子坐标列表
    ///
    /// 流程：① clear -> ② setup（声明）-> ③ compile（裁剪+排序）-> ④ execute（绘制）
    ///      -> ⑤ 字幕写第 0 行 -> ⑥ diff_cells（计算差异）；swap 在 flush() 中完成
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
        // ④5 操控高亮（#022 操控层 v0）：被操控实体 glyph 白色渲染（渲染层特判，
        //    不改 core 数据结构）。目标不在场/不在视口 = 无高亮；后写覆盖实体原色。
        if let Some(id) = self.controlled {
            if let Some(e) = world.entities.get(&id).filter(|e| e.is_alive()) {
                if let Some((row, col)) = self.viewport.world_to_screen(e.pos()) {
                    self.buffer.write(row, col, &e.appearance.glyph, "white");
                }
            }
        }
        // ⑤ 字幕写第 0 行（UI 层语义，后写覆盖战场格子；真实视口下 row 0 = 世界
        //    y=-25 的地下空白区，正常布局不与生物重叠）
        for (col, ch) in self.caption.line().chars().take(self.viewport.term_cols).enumerate() {
            self.buffer.write(0, col, &ch.to_string(), "yellow");
        }
        // ⑥ 计算差异（缓存供 flush 复用，避免 flush_diff 重复扫描全缓冲）
        let diff = self.buffer.diff_cells();
        self.last_diff = diff.clone();
        // swap 在 flush() 中完成：输出 diff 后再 swap
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
        // 注：此时 flush_to 已成功输出并 swap。若此处 out.flush() 失败，本帧画面
        // 可能未真正上屏（视觉丢失），但缓冲已 swap、数据一致性不受影响（下帧 diff
        // 基于已 swap 状态计算）；仅记日志不回滚。
        if let Err(e) = out.flush() {
            tracing::warn!(error = %e, "[flush] stdout flush 失败");
        }
    }

    /// Flush 到任意 writer（测试用 Vec<u8> 验证 ANSI 输出）
    ///
    /// 输出 buffer 的 diff 格子（flush_diff），成功后 swap 缓冲。
    /// 输出失败则不 swap（保留 diff 状态供下帧重试）。
    pub fn flush_to<W: std::io::Write>(&mut self, out: &mut W) -> std::io::Result<()> {
        self.buffer.flush_diff(out, &self.last_diff)?;
        self.buffer.swap();
        tracing::debug!("[flush_to] 渲染输出完成并已 swap");
        Ok(())
    }

    /// 只读访问 buffer（测试验证用）
    pub fn buffer(&self) -> &TerminalBuffer {
        &self.buffer
    }

    /// 当前字幕行内容（测试验证用；flush swap 后 buffer current 不再持有本帧，
    /// 字幕内容的稳定观测点是 CaptionState 保存的行字符串）
    pub fn caption_line(&self) -> &str {
        self.caption.line()
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

    // === 战况字幕：update_caption + render 写第 0 行 ===

    /// 读取 buffer 第 0 行拼成字符串（截掉尾部空格），字幕内容断言用
    fn row0_line(r: &Renderer) -> String {
        let mut s = String::new();
        for col in 0..r.viewport().term_cols {
            s.push_str(r.buffer().get(0, col).unwrap().glyph.as_str());
        }
        s.trim_end().to_string()
    }

    /// 宽屏渲染器（对齐真实配置 50x80：字幕 47 字符完整放下，不触发截断）
    fn renderer_80cols() -> Renderer {
        Renderer::new(Viewport::new((0.0, 0.0), 50.0, 25.0, 50, 80))
    }

    #[test]
    fn controlled_accessor_returns_last_set_value() {
        // 读写：controlled() 观测访问器（期四快照组装用，同 caption_line 先例）
        // 返回最近一次 set_controlled 喂入的值，供 tick 外部（WS 推送）观测
        let mut r = renderer_80cols();
        assert_eq!(r.controlled(), None, "初始无操控目标");
        let id = EntityId(Uuid::from_u128(9));
        r.set_controlled(Some(id));
        assert_eq!(r.controlled(), Some(id), "访问器应返回最近喂入值");
        r.set_controlled(None);
        assert_eq!(r.controlled(), None, "下线 latch 观战态应可见");
    }

    #[test]
    fn update_caption_render_writes_row0() {
        // 读写：update_caption 存字幕行，render 后第 0 行显示字幕；diff 含 row 0 格子
        let mut r = renderer_80cols();
        let world = WorldState::new();
        r.update_caption(12, Some(2.44), true, Some(7), 3, None);
        let diff = r.render(&world);
        assert_eq!(
            row0_line(&r),
            "tick=12 | A->D dist=2.4 | D shield=7 | alive=3",
            "第 0 行应为字幕内容"
        );
        assert!(diff.contains(&(0, 0)), "字幕首格应在 diff 中");
    }

    #[test]
    fn caption_change_appears_in_next_render_diff() {
        // 修改：字幕内容变化 -> 下帧 diff 含 row 0（随 tick 更新的依据：换行即重绘）
        let mut r = renderer_80cols();
        let world = WorldState::new();
        r.update_caption(1, Some(9.0), true, None, 2, None);
        r.render(&world);
        let mut out = Vec::new();
        r.flush_to(&mut out).unwrap(); // swap
        r.update_caption(2, Some(8.0), true, None, 2, None);
        let diff = r.render(&world);
        assert!(
            diff.iter().any(|&(row, _)| row == 0),
            "字幕变化后 diff 应含第 0 行格子，实际 {:?}",
            diff
        );
    }

    #[test]
    fn caption_tick_only_change_limits_diff_to_tick_digits() {
        // 边界：语义值（距离/盾/存活）不变、仅 tick 前进 -> 只有 tick 数字格重绘，
        // 语义段（col>=10，"| A->D..." 起）不产生 diff（画面不闪烁，日志不刷屏同理）
        let mut r = renderer_80cols();
        let world = WorldState::new();
        r.update_caption(1, Some(9.0), true, None, 2, None);
        r.render(&world);
        let mut out = Vec::new();
        r.flush_to(&mut out).unwrap(); // swap
        r.update_caption(2, Some(9.0), true, None, 2, None); // 仅 tick 变化
        let diff = r.render(&world);
        assert!(
            diff.iter().all(|&(_, col)| col < 10),
            "仅 tick 变化时 diff 应局限在 tick 数字区（col<10），实际 {:?}",
            diff
        );
    }

    #[test]
    fn caption_truncated_to_terminal_cols() {
        // 边界：字幕行超过终端列数 -> 截断到 cols，不越界、不影响其他行
        let mut r = Renderer::new(Viewport::new((5.0, 5.0), 5.0, 5.0, 10, 10));
        let world = WorldState::new();
        r.update_caption(1, Some(1.0), true, Some(1), 1, None);
        r.render(&world);
        let line = row0_line(&r);
        assert!(
            line.chars().count() <= 10,
            "字幕应截断到 10 列，实际 {} 字符",
            line.chars().count()
        );
    }

    #[test]
    fn caption_overwrites_entity_at_row0() {
        // 边界：实体恰好映射到 row 0（10x10 视口下 y=0）-> 字幕后写覆盖（画家算法，
        // 字幕=UI 层语义）。战场区在真实视口位于 row>=25，此覆盖仅极端小视口发生。
        let mut r = renderer_10x10();
        let mut world = WorldState::new();
        world.entities.insert(
            EntityId(Uuid::from_u128(1)),
            make_entity(1, (5.0, 0.0), "@", true), // y=0 -> row 0
        );
        r.update_caption(1, Some(1.0), true, None, 1, None);
        r.render(&world);
        assert_eq!(
            r.buffer().get(0, 0).unwrap().glyph,
            "t",
            "row 0 首格应是字幕字符（覆盖实体）"
        );
    }

    #[test]
    fn caption_shield_persists_between_reports() {
        // 读写：盾量只在 D 上报时更新，两次上报之间保持上次值（不闪回占位符）
        let mut r = renderer_80cols();
        let world = WorldState::new();
        r.update_caption(1, Some(9.0), true, Some(9), 2, None);
        r.render(&world);
        assert!(row0_line(&r).contains("shield=9"), "上报后应显示盾 9");
        // 下一 tick 无新上报（report=None）-> 保持 9
        r.update_caption(2, Some(8.0), true, None, 2, None);
        r.render(&world);
        assert!(
            row0_line(&r).contains("shield=9"),
            "无新上报应保持上次盾量，实际 {:?}",
            row0_line(&r)
        );
    }

    #[test]
    fn caption_shield_hidden_when_defender_absent() {
        // 负面路径：D 已死（d_present=false）-> 盾显示占位符（死人不报盾）
        let mut r = renderer_80cols();
        let world = WorldState::new();
        r.update_caption(1, Some(9.0), true, Some(9), 2, None);
        r.render(&world);
        r.update_caption(2, None, false, None, 1, None); // D 不在场
        r.render(&world);
        assert!(
            row0_line(&r).contains("shield=?"),
            "D 不在场时盾应显示占位符，实际 {:?}",
            row0_line(&r)
        );
    }

    #[test]
    fn update_caption_ctrl_online_appends_ctrl_segment() {
        // 操控层 v0：操控目标在线 -> 字幕尾部 ctrl:字形
        let mut r = renderer_80cols();
        let world = WorldState::new();
        r.update_caption(5, Some(2.4), true, None, 3, Some("P"));
        r.render(&world);
        assert!(
            row0_line(&r).ends_with("ctrl:P"),
            "字幕应含 ctrl:P，实际 {:?}",
            row0_line(&r)
        );
    }

    // === 操控高亮（#022 操控层 v0）：被操控实体 glyph 白色渲染（渲染层特判）===

    #[test]
    fn controlled_entity_rendered_white() {
        // 读写：set_controlled(Some(id)) 后 render -> 该实体格子颜色覆盖为 white（字形不变）
        let mut r = renderer_80cols();
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world
            .entities
            .insert(id, make_entity(1, (25.0, 12.0), "@", true));
        r.set_controlled(Some(id));
        r.render(&world);
        let (row, col) = r.viewport().world_to_screen((25.0, 12.0)).unwrap();
        let cell = r.buffer().get(row, col).unwrap();
        assert_eq!(cell.glyph, "@", "字形不变");
        assert_eq!(
            cell.color, "white",
            "被操控实体应白色高亮（原色 red 被覆盖）"
        );
    }

    #[test]
    fn uncontrolled_entity_keeps_original_color() {
        // 反向护栏：未设置操控目标 -> 渲染色保持实体原色
        let mut r = renderer_80cols();
        let mut world = WorldState::new();
        world
            .entities
            .insert(EntityId(Uuid::from_u128(1)), make_entity(1, (25.0, 12.0), "@", true));
        r.render(&world);
        let (row, col) = r.viewport().world_to_screen((25.0, 12.0)).unwrap();
        assert_eq!(
            r.buffer().get(row, col).unwrap().color, "red",
            "无操控目标保持原色"
        );
    }

    #[test]
    fn controlled_entity_outside_viewport_no_panic() {
        // 边界：操控目标在视口外（world_to_screen=None）-> 忽略高亮不 panic
        let mut r = renderer_80cols();
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world
            .entities
            .insert(id, make_entity(1, (500.0, 500.0), "@", true));
        r.set_controlled(Some(id));
        let _ = r.render(&world); // 不 panic 即通过
    }

    #[test]
    fn controlled_dead_entity_not_highlighted() {
        // 边界：操控目标已死 -> 不高亮（尸体不是玩家的在场角色）
        let mut r = renderer_80cols();
        let mut world = WorldState::new();
        let mut e = make_entity(1, (25.0, 12.0), "@", true);
        e.kill();
        let id = e.id;
        world.entities.insert(id, e);
        r.set_controlled(Some(id));
        r.render(&world);
        let (row, col) = r.viewport().world_to_screen((25.0, 12.0)).unwrap();
        assert_eq!(
            r.buffer().get(row, col).unwrap().glyph, " ",
            "死者不渲染也不高亮"
        );
    }
}
