//! 终端缓冲区（TerminalBuffer + Cell）：双缓冲 + diff flush
//!
//! MVP 不做真实 ANSI 输出，buffer 只做数据层面操作（渲染设计文档 5.4）。
//! diff 检测用于将来优化终端刷新（只重绘变化的格子）。

use std::sync::OnceLock;

/// 单个终端格子
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// 字符
    pub glyph: String,
    /// 颜色
    pub color: String,
}

impl Default for Cell {
    fn default() -> Self {
        // 默认空格 + 默认颜色
        Self {
            glyph: " ".to_string(),
            color: "default".to_string(),
        }
    }
}

/// 终端缓冲区（双缓冲）
pub struct TerminalBuffer {
    /// 行数
    pub rows: usize,
    /// 列数
    pub cols: usize,
    /// 当前帧
    current: Vec<Vec<Cell>>,
    /// 上一帧（diff 用）
    previous: Vec<Vec<Cell>>,
}

impl TerminalBuffer {
    /// 创建缓冲区，current/previous 均初始化为空格
    pub fn new(rows: usize, cols: usize) -> Self {
        let current = vec![vec![Cell::default(); cols]; rows];
        let previous = vec![vec![Cell::default(); cols]; rows];
        tracing::debug!(rows, cols, "终端缓冲区创建");
        Self {
            rows,
            cols,
            current,
            previous,
        }
    }

    /// 写入格子（后写覆盖先写=画家算法），越界静默忽略
    pub fn write(&mut self, row: usize, col: usize, glyph: &str, color: &str) {
        if let Some(r) = self.current.get_mut(row) {
            if let Some(cell) = r.get_mut(col) {
                cell.glyph = glyph.to_string();
                cell.color = color.to_string();
            }
        }
    }

    /// 清空 current 帧（填空格）
    pub fn clear(&mut self) {
        for row in &mut self.current {
            for cell in row.iter_mut() {
                *cell = Cell::default();
            }
        }
    }

    /// 返回 current 和 previous 不同的格子坐标列表
    pub fn diff_cells(&self) -> Vec<(usize, usize)> {
        let mut diff = Vec::new();
        for (row_idx, (cur_row, prev_row)) in self
            .current
            .iter()
            .zip(self.previous.iter())
            .enumerate()
        {
            for (col_idx, (cur, prev)) in cur_row.iter().zip(prev_row.iter()).enumerate() {
                if cur != prev {
                    diff.push((row_idx, col_idx));
                }
            }
        }
        diff
    }

    /// 交换 current 和 previous（flush 后调用）
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.current, &mut self.previous);
    }

    /// 读取格子（越界返回 None）
    pub fn get(&self, row: usize, col: usize) -> Option<&Cell> {
        self.current.get(row).and_then(|r| r.get(col))
    }

    /// 读取格子（越界返回空格 Cell 的引用，不 panic）
    pub fn cell_at(&self, row: usize, col: usize) -> &Cell {
        // 越界时返回全局空格 Cell 的引用（Cell 含 String 不能用 const，用 OnceLock）
        static EMPTY: OnceLock<Cell> = OnceLock::new();
        self.current
            .get(row)
            .and_then(|r| r.get(col))
            .unwrap_or_else(|| EMPTY.get_or_init(Cell::default))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_get_returns_value() {
        // 读写：写入后读取正确
        let mut b = TerminalBuffer::new(5, 5);
        b.write(1, 2, "A", "red");
        let cell = b.get(1, 2).unwrap();
        assert_eq!(cell.glyph, "A");
        assert_eq!(cell.color, "red");
    }

    #[test]
    fn write_overwrites_previous() {
        // 修改：后写覆盖先写（画家算法）
        let mut b = TerminalBuffer::new(3, 3);
        b.write(0, 0, "A", "red");
        b.write(0, 0, "B", "blue");
        let cell = b.get(0, 0).unwrap();
        assert_eq!(cell.glyph, "B");
        assert_eq!(cell.color, "blue");
    }

    #[test]
    fn write_out_of_bounds_does_not_panic() {
        // 边界：越界写入不 panic，且不影响缓冲
        let mut b = TerminalBuffer::new(2, 2);
        b.write(5, 5, "X", "red");
        b.write(0, 5, "X", "red");
        assert_eq!(b.get(0, 0).unwrap().glyph, " "); // 未被越界写入影响
    }

    #[test]
    fn diff_cells_no_diff_when_identical() {
        // 边界：新缓冲 current==previous（都空格），无 diff
        let b = TerminalBuffer::new(3, 3);
        assert!(b.diff_cells().is_empty());
    }

    #[test]
    fn diff_cells_detects_changes() {
        // 读写：不同内容有 diff
        let mut b = TerminalBuffer::new(3, 3);
        b.write(0, 0, "A", "red");
        b.write(2, 2, "B", "blue");
        let mut diff = b.diff_cells();
        diff.sort();
        assert_eq!(diff, vec![(0, 0), (2, 2)]);
    }

    #[test]
    fn clear_resets_current_to_spaces() {
        // 修改：clear 后 current 全空格
        let mut b = TerminalBuffer::new(2, 2);
        b.write(0, 0, "A", "red");
        b.clear();
        let cell = b.get(0, 0).unwrap();
        assert_eq!(cell.glyph, " ");
        assert_eq!(cell.color, "default");
    }

    #[test]
    fn swap_makes_previous_equal_old_current() {
        // 修改：swap 后 previous 变成之前的 current
        let mut b = TerminalBuffer::new(2, 2);
        b.write(0, 0, "A", "red");
        // swap 前：current[0][0]=A，previous[0][0]=空格 -> diff 含 (0,0)
        assert!(b.diff_cells().contains(&(0, 0)));
        b.swap();
        // swap 后：current[0][0]=空格（旧 previous）
        assert_eq!(b.get(0, 0).unwrap().glyph, " ");
        // current(空格) vs previous(A) 仍不同 -> 证明 previous 现在是 A
        assert!(b.diff_cells().contains(&(0, 0)));
    }

    #[test]
    fn cell_at_in_bounds_returns_cell() {
        // 读写：cell_at 范围内返回对应格子
        let mut b = TerminalBuffer::new(2, 2);
        b.write(0, 1, "Z", "green");
        assert_eq!(b.cell_at(0, 1).glyph, "Z");
    }

    #[test]
    fn cell_at_out_of_bounds_returns_space_no_panic() {
        // 边界：cell_at 越界返回空格，不 panic
        let b = TerminalBuffer::new(2, 2);
        let cell = b.cell_at(10, 10);
        assert_eq!(cell.glyph, " ");
    }
}
