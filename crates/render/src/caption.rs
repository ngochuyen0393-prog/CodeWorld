//! 战况字幕（caption）：终端第一行的实时战局条
//!
//! 架构定位：字幕 = 渲染层展示（同血条展示同一铁律层级），只读、不写回 world。
//! 数据来源全部由 host 侧喂入：tick/距离/存活数来自 world 快照，盾量来自
//! 玩家代码 env.log marker 自愿上报（见 main.rs parse_shield_markers）。
//! 字幕写 TerminalBuffer 第 0 行：真实视口（half_height=25）下 row 0 对应
//! 世界 y=-25 的地下空白区，永不与地面生物（y>=0 -> row>=25）抢格子。

/// 计算字幕行内容（纯函数，MVP 格式）
///
/// 格式（全 ASCII，理由见决策 #006）：`tick=12 | A->D dist=2.4 | D shield=7 | alive=3`
/// - dist=None（A 或 D 不在场/已死）-> `dist=--`
/// - shield=None（D 不在场或本局尚未上报）-> `shield=?`
/// - 距离保留 1 位小数
/// - ctrl=Some（操控层 v0：操控目标在线）-> 尾部追加 ` | ctrl:P`（玩家名字形）
pub fn format_caption(
    tick: u64,
    dist: Option<f64>,
    shield: Option<i32>,
    alive: usize,
    ctrl: Option<&str>,
) -> String {
    let dist_text = match dist {
        Some(d) => format!("{:.1}", d),
        None => "--".to_string(),
    };
    let shield_text = match shield {
        Some(s) => s.to_string(),
        None => "?".to_string(),
    };
    let base = format!(
        "tick={} | A->D dist={} | D shield={} | alive={}",
        tick, dist_text, shield_text, alive
    );
    match ctrl {
        Some(glyph) => format!("{base} | ctrl:{glyph}"),
        None => base,
    }
}

/// 字幕状态（渲染层持有，非游戏状态）
///
/// 字幕语义签名（距离 0.1 精度 / 盾量 / 存活数 / 操控目标字形）——"值变化才打日志"的判据
type CaptionSemantic = (Option<i64>, Option<i32>, usize, Option<String>);

/// `line`：当前字幕行内容，render 时写入终端第 0 行。
/// `shield`：最近一次上报的盾量（跨 tick 持久——D 只在拦截时上报，
///          两次上报之间保持上次值，不闪回占位符）。
/// `last_semantic`：上次有意义的战况值（见 [`CaptionSemantic`]），
/// 用于"值变化才打 info 日志"——tick 每拍必变，若按整行内容判变化会每 tick 刷屏。
pub struct CaptionState {
    line: String,
    shield: Option<i32>,
    last_semantic: Option<CaptionSemantic>,
}

impl CaptionState {
    /// 创建空字幕状态（行内容为空串，render 时不写任何格子）
    pub fn new() -> Self {
        Self {
            line: String::new(),
            shield: None,
            last_semantic: None,
        }
    }

    /// 更新字幕：合并盾量上报 + 重算行内容 + 战况值变化时打一条 info 日志（不每 tick 刷屏）
    ///
    /// - `d_present`：D 是否在场。D 已死则盾显示占位符（死人不报盾），
    ///   即使此前有过上报值；D 回不来，无需清理持久状态。
    /// - `shield_report`：本 tick 的盾量上报（None = 本 tick 无上报，保持上次值）。
    /// - `ctrl`：操控目标字形（操控层 v0；None = 无玩家在线，不显示 ctrl 段）。
    pub fn update(
        &mut self,
        tick: u64,
        dist: Option<f64>,
        d_present: bool,
        shield_report: Option<i32>,
        alive: usize,
        ctrl: Option<&str>,
    ) {
        if let Some(v) = shield_report {
            self.shield = Some(v);
        }
        let display_shield = if d_present { self.shield } else { None };
        self.line = format_caption(tick, dist, display_shield, alive, ctrl);
        // 距离按 0.1 精度取整判变化：微小浮点抖动（物理漂移）不算"战况变化"
        let dist_tenths = dist.map(|d| (d * 10.0).round() as i64);
        let semantic = (dist_tenths, display_shield, alive, ctrl.map(str::to_string));
        if self.last_semantic.as_ref() != Some(&semantic) {
            tracing::info!(caption = %self.line, "[caption] 战况字幕更新（值有变化）");
            self.last_semantic = Some(semantic);
        }
    }

    /// 当前字幕行内容（render 写 row 0 用）
    pub fn line(&self) -> &str {
        &self.line
    }
}

impl Default for CaptionState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_caption_full_line() {
        // 读写：全量输入 -> 完整字幕行（任务书样例内容，ASCII 化）
        let line = format_caption(12, Some(2.44), Some(7), 3, None);
        assert_eq!(line, "tick=12 | A->D dist=2.4 | D shield=7 | alive=3");
    }

    #[test]
    fn format_caption_dist_none_shows_placeholder() {
        // 负面路径：A 或 D 缺席（已死/被第三方击杀）-> 距离无意义，显示占位符
        let line = format_caption(20, None, Some(7), 2, None);
        assert_eq!(line, "tick=20 | A->D dist=-- | D shield=7 | alive=2");
    }

    #[test]
    fn format_caption_shield_unknown_shows_placeholder() {
        // 负面路径：盾量未知（D 不在场，或 D 尚未拦截上报过）-> 占位符
        let line = format_caption(1, Some(10.0), None, 3, None);
        assert_eq!(line, "tick=1 | A->D dist=10.0 | D shield=? | alive=3");
    }

    #[test]
    fn format_caption_last_survivor() {
        // 负面路径：全场剩 1 个（无敌人可扫描）-> 距离与盾全占位
        let line = format_caption(30, None, None, 1, None);
        assert_eq!(line, "tick=30 | A->D dist=-- | D shield=? | alive=1");
    }

    #[test]
    fn format_caption_zero_shield_displayed_as_zero() {
        // 边界：盾尽是合法上报值 0，不能与"未知"占位符混淆
        let line = format_caption(11, Some(1.0), Some(0), 2, None);
        assert_eq!(line, "tick=11 | A->D dist=1.0 | D shield=0 | alive=2");
    }

    #[test]
    fn format_caption_appends_ctrl_when_control_target_online() {
        // 操控层 v0：操控目标在线 -> 尾部追加 ctrl:玩家字形（任务 C.1）
        let line = format_caption(5, Some(2.4), Some(7), 3, Some("P"));
        assert_eq!(
            line,
            "tick=5 | A->D dist=2.4 | D shield=7 | alive=3 | ctrl:P",
            "ctrl 状态追加在 alive 之后"
        );
    }
}
