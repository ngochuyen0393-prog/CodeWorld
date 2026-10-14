//! ANSI 终端转义（颜色映射 + 终端模式进/出）
//!
//! 零外部依赖：用标准库直接写 ANSI 转义序列到 stdout。
//! - color_to_ansi：Cell.color 字符串 -> ANSI 颜色码
//! - enter_terminal / exit_terminal：进入/退出全屏渲染模式（清屏/隐光标 <-> 重置/显光标）

use std::io::{self, Write};

/// Cell.color 字符串 -> ANSI 颜色码
///
/// 未知颜色（含大小写不匹配、空串）统一当 default 处理（码 0）。
/// 调用方用 `\x1b[{code}m` 设置前景色。
pub fn color_to_ansi(color: &str) -> u8 {
    let code = match color {
        "red" => 31,
        "green" => 32,
        "yellow" => 33,
        "blue" => 34,
        "magenta" => 35,
        "cyan" => 36,
        "gray" => 90,
        "white" => 97, // 亮白前景（操控层 v0：被操控实体高亮）
        "default" => 0,
        _ => 0, // 未知颜色当 default
    };
    tracing::trace!(color, code, "[color_to_ansi] 颜色映射");
    code
}

/// 进入终端渲染模式：隐藏光标 + 清屏 + 光标归位（左上角）
///
/// 写入任意 writer（stdout 或测试用 Vec<u8>），末尾 flush 保证立即生效。
pub fn enter_terminal<W: Write>(out: &mut W) -> io::Result<()> {
    tracing::info!("[enter_terminal] 进入终端渲染模式");
    // \x1b[?25l 隐光标；\x1b[2J 清屏；\x1b[H 光标归位
    write!(out, "\x1b[?25l\x1b[2J\x1b[H")?;
    out.flush()
}

/// 退出终端渲染模式：重置颜色 + 显示光标 + 换行
///
/// 换行让退出后的 shell 提示符落在新行，避免覆盖最后一行画面。
pub fn exit_terminal<W: Write>(out: &mut W) -> io::Result<()> {
    tracing::info!("[exit_terminal] 恢复终端");
    // \x1b[0m 重置所有属性；\x1b[?25h 显光标
    write!(out, "\x1b[0m\x1b[?25h")?;
    writeln!(out)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    // === color_to_ansi ===

    #[test]
    fn color_to_ansi_maps_known_colors() {
        // 读写：已知颜色映射到正确 ANSI 码
        assert_eq!(color_to_ansi("red"), 31);
        assert_eq!(color_to_ansi("green"), 32);
        assert_eq!(color_to_ansi("yellow"), 33);
        assert_eq!(color_to_ansi("blue"), 34);
        assert_eq!(color_to_ansi("magenta"), 35);
        assert_eq!(color_to_ansi("cyan"), 36);
        assert_eq!(color_to_ansi("gray"), 90);
        assert_eq!(color_to_ansi("default"), 0);
    }

    #[test]
    fn color_to_ansi_unknown_color_falls_back_to_default() {
        // 边界：未知颜色当 default（0）
        assert_eq!(color_to_ansi("purple"), 0);
        assert_eq!(color_to_ansi(""), 0);
        assert_eq!(color_to_ansi("RED"), 0); // 大小写敏感，未知
    }

    #[test]
    fn color_to_ansi_maps_white_for_control_highlight() {
        // 操控层 v0：white = 亮白前景（被操控实体高亮）
        assert_eq!(color_to_ansi("white"), 97);
    }

    // === enter_terminal ===

    #[test]
    fn enter_terminal_hides_cursor_and_clears_screen() {
        // 读写：进入渲染模式 -> 隐藏光标 + 清屏
        let mut out = Vec::new();
        enter_terminal(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\x1b[?25l"), "应隐藏光标，实际 {:?}", s);
        assert!(s.contains("\x1b[2J"), "应清屏，实际 {:?}", s);
    }

    // === exit_terminal ===

    #[test]
    fn exit_terminal_resets_color_and_shows_cursor() {
        // 读写：退出渲染模式 -> 重置颜色 + 显示光标
        let mut out = Vec::new();
        exit_terminal(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\x1b[0m"), "应重置颜色，实际 {:?}", s);
        assert!(s.contains("\x1b[?25h"), "应显示光标，实际 {:?}", s);
    }
}
