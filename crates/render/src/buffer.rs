//! 终端缓冲区（TerminalBuffer + Cell）：双缓冲 + diff flush
//!
//! MVP 不做真实 ANSI 输出，buffer 只做数据层面操作（渲染设计文档 5.4）。
//! diff 检测用于将来优化终端刷新（只重绘变化的格子）。

use std::io::Write;
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

    /// 把 current 与 previous 的差异格子以 ANSI 转义序列写入 output
    ///
    /// 只重绘变化格子（diff 优化）：对每个 diff 格子输出
    /// `\x1b[{row+1};{col+1}H`（光标移动，buffer 0-based 转 ANSI 1-based）
    /// + `\x1b[{code}m`（颜色码）+ glyph。
    ///
    /// 不交换缓冲：swap 由调用方在输出完成后调用（见 Renderer::flush），
    /// 以便"输出失败时不破坏 diff 状态"并可单独测试输出内容。
    pub fn flush_diff<W: Write>(&self, output: &mut W) -> std::io::Result<()> {
        let diffs = self.diff_cells();
        let count = diffs.len();
        for (row, col) in diffs {
            let cell = self.cell_at(row, col);
            let code = crate::ansi::color_to_ansi(&cell.color);
            // 光标定位(1-based) + 设颜色 + 写字符
            write!(output, "\x1b[{};{}H\x1b[{}m{}", row + 1, col + 1, code, cell.glyph)?;
        }
        tracing::debug!(diff_count = count, "[flush_diff] ANSI 输出完成");
        Ok(())
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

    // === flush_diff：ANSI diff 输出 ===

    #[test]
    fn flush_diff_writes_ansi_for_changed_cell() {
        // 读写：单个变化格子 -> 光标移动(1-based) + 颜色码 + 字符
        let mut b = TerminalBuffer::new(3, 3);
        b.write(0, 0, "@", "red");
        let mut out = Vec::new();
        b.flush_diff(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        // (row=0,col=0) -> 光标 1;1，red->31
        assert_eq!(s, "\x1b[1;1H\x1b[31m@");
    }

    #[test]
    fn flush_diff_empty_when_no_diff() {
        // 边界：current==previous（新缓冲全空格）-> 无 diff -> 无输出
        let b = TerminalBuffer::new(3, 3);
        let mut out = Vec::new();
        b.flush_diff(&mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn flush_diff_writes_multiple_cells_in_row_major_order() {
        // 读写：多个变化格子按行主序输出（row 0 先于 row 2）
        let mut b = TerminalBuffer::new(3, 3);
        b.write(0, 0, "A", "red");
        b.write(2, 1, "B", "blue");
        let mut out = Vec::new();
        b.flush_diff(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        // (0,0)->1;1 red 31 A ; (2,1)->3;2 blue 34 B
        assert_eq!(s, "\x1b[1;1H\x1b[31mA\x1b[3;2H\x1b[34mB");
    }

    #[test]
    fn flush_diff_unknown_color_uses_default_code() {
        // 边界：未知颜色当 default（码 0）
        let mut b = TerminalBuffer::new(2, 2);
        b.write(1, 1, "X", "purple");
        let mut out = Vec::new();
        b.flush_diff(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "\x1b[2;2H\x1b[0mX");
    }

    #[test]
    fn flush_diff_does_not_swap_buffers() {
        // 修改：flush_diff 只输出不交换（swap 由调用方负责）
        // 输出后 current 仍持有本帧、diff 仍非空（证明未 swap）
        let mut b = TerminalBuffer::new(2, 2);
        b.write(0, 0, "@", "red");
        let mut out = Vec::new();
        b.flush_diff(&mut out).unwrap();
        assert_eq!(b.get(0, 0).unwrap().glyph, "@"); // current 未变
        assert!(b.diff_cells().contains(&(0, 0))); // 未 swap -> diff 仍在
    }
}
