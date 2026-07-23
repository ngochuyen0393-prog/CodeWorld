//! 视口（Viewport）：世界坐标 <-> 终端行列映射 + 视口裁剪
//!
//! 借鉴寒霜 FrameGraph 的 view 概念（渲染设计文档 5.3）。
//! 视口以世界坐标中心 + 半宽半高定义可见范围，并把世界坐标线性映射到终端行列。
//! 世界 y 增大方向 = 终端行号增大方向（屏幕下方），与"y 大=近"的画家算法约定一致。

/// 视口（世界坐标 -> 终端行列映射）
#[derive(Debug, Clone)]
pub struct Viewport {
    /// 视口中心（世界坐标）
    pub center: (f64, f64),
    /// 视口半宽（世界坐标）
    pub half_width: f64,
    /// 视口半高（世界坐标）
    pub half_height: f64,
    /// 终端行数
    pub term_rows: usize,
    /// 终端列数
    pub term_cols: usize,
}

impl Viewport {
    /// 创建视口
    pub fn new(
        center: (f64, f64),
        half_width: f64,
        half_height: f64,
        term_rows: usize,
        term_cols: usize,
    ) -> Self {
        let vp = Self {
            center,
            half_width,
            half_height,
            term_rows,
            term_cols,
        };
        tracing::debug!(
            center = ?vp.center,
            rows = vp.term_rows,
            cols = vp.term_cols,
            "视口创建"
        );
        vp
    }

    /// 世界坐标是否在视口内（含边界）
    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (min_x, max_x) = self.world_x_range();
        let (min_y, max_y) = self.world_y_range();
        x >= min_x && x <= max_x && y >= min_y && y <= max_y
    }

    /// 世界坐标 -> 终端行列，视口外返回 None
    ///
    /// 映射：以视口左下角为原点，世界坐标偏移按比例换算到终端行列。
    /// 边缘点（刚好在右/上边界）clamp 到最后一格，避免越界。
    pub fn world_to_screen(&self, world: (f64, f64)) -> Option<(usize, usize)> {
        let (min_x, max_x) = self.world_x_range();
        let (min_y, max_y) = self.world_y_range();
        // 视口外返回 None（含边界判定，避免再调 contains 重复计算范围）
        if world.0 < min_x || world.0 > max_x || world.1 < min_y || world.1 > max_y {
            return None;
        }
        let dx = world.0 - min_x;
        let dy = world.1 - min_y;
        let width = self.half_width * 2.0;
        let height = self.half_height * 2.0;
        // 视口内 dx/dy >= 0，转 usize 安全
        let col = (dx / width * self.term_cols as f64) as usize;
        let row = (dy / height * self.term_rows as f64) as usize;
        // 边缘 clamp：右/上边界时 col/row 可能等于 term_cols/term_rows
        let col = col.min(self.term_cols.saturating_sub(1));
        let row = row.min(self.term_rows.saturating_sub(1));
        Some((row, col))
    }

    /// 视口世界坐标 X 范围 (min, max)
    pub fn world_x_range(&self) -> (f64, f64) {
        (self.center.0 - self.half_width, self.center.0 + self.half_width)
    }

    /// 视口世界坐标 Y 范围 (min, max)
    pub fn world_y_range(&self) -> (f64, f64) {
        (self.center.1 - self.half_height, self.center.1 + self.half_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 视口：center=(5,5), half_width=5, half_height=5, 终端 10x10
    // -> x_range=(0,10), y_range=(0,10)
    fn vp_10x10() -> Viewport {
        Viewport::new((5.0, 5.0), 5.0, 5.0, 10, 10)
    }

    #[test]
    fn world_to_screen_maps_interior_to_grid() {
        // 读写：视口内坐标正确映射到终端行列
        let vp = vp_10x10();
        // 左下角 -> (0,0)
        assert_eq!(vp.world_to_screen((0.0, 0.0)), Some((0, 0)));
        // 中心 -> (5,5)
        assert_eq!(vp.world_to_screen((5.0, 5.0)), Some((5, 5)));
    }

    #[test]
    fn world_to_screen_outside_returns_none() {
        // 边界：视口外坐标返回 None
        let vp = vp_10x10();
        assert_eq!(vp.world_to_screen((11.0, 5.0)), None); // x 超出
        assert_eq!(vp.world_to_screen((5.0, -0.1)), None); // y 超出
        assert_eq!(vp.world_to_screen((-1.0, -1.0)), None);
    }

    #[test]
    fn world_to_screen_edge_clamps_to_last_cell() {
        // 边界：刚好在视口边缘映射到最后一格（不越界）
        let vp = vp_10x10();
        // 右上角 (10,10) -> col=10/row=10，clamp 到 (9,9)
        assert_eq!(vp.world_to_screen((10.0, 10.0)), Some((9, 9)));
    }

    #[test]
    fn world_to_screen_scales_with_terminal_size() {
        // 边界：不同终端尺寸映射比例正确（中心仍映射到中间）
        let vp = Viewport::new((5.0, 5.0), 5.0, 5.0, 20, 20);
        // 中心 (5,5) -> 20 的中点 (10,10)
        assert_eq!(vp.world_to_screen((5.0, 5.0)), Some((10, 10)));
        // 左下角 -> (0,0)
        assert_eq!(vp.world_to_screen((0.0, 0.0)), Some((0, 0)));
    }

    #[test]
    fn contains_matches_range() {
        // 读写：contains 与世界范围一致（含边界）
        let vp = vp_10x10();
        assert!(vp.contains(0.0, 0.0));
        assert!(vp.contains(10.0, 10.0));
        assert!(vp.contains(5.0, 5.0));
        assert!(!vp.contains(10.1, 5.0));
        assert!(!vp.contains(5.0, -0.1));
    }

    #[test]
    fn world_ranges_centered() {
        // 读写：世界坐标范围以 center 为中心
        let vp = vp_10x10();
        assert_eq!(vp.world_x_range(), (0.0, 10.0));
        assert_eq!(vp.world_y_range(), (0.0, 10.0));
    }
}
