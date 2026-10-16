//! 下线处置权状态机（decisions.md #025，期五第一刀·中刀）
//!
//! 宪法："在线=操控独占；下线=玩家处置权"——登出三选一：
//! - take：角色带走（会话级离场，文件锚保留，重连认领即重新进场）
//! - stay_auto：角色留存+自主代码接管（on_tick 恢复，操控独占释放）
//! - stay_ai：角色留存+外部 AI 接管待连接（站桩，#014 端点协议留后）
//!
//! 触发源两条：① 主动登出（WS logout 消息，三选一即时落地）；
//! ② 意外断开（操控令牌释放翻转沿）→ 处置等待窗口（默认 30 拍，
//! CODEWORLD_DISPOSE_WAIT 可配，0=立即默认 take），超时默认 take。
//!
//! 两条下线路径并存不互相踩：文件锚删除=账号级下线（残影路径 #013），
//! 登出/断开=会话级下线（本模块）——文件删除时处置状态让位残影路径。
//!
//! 处置状态不持久化（serde 不触及；重启一律回在线态，"重启=新会话"）。

use codeworld_core::entity::EntityId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 处置等待默认拍数（宪法 #025：世界不等人，30 拍够玩家重开浏览器救场）
pub(crate) const DEFAULT_DISPOSE_WAIT_TICKS: u64 = 30;

/// 登出处置三选一（WS logout 上行消息的 disposition 值，net-protocol.md v1.7）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LogoutChoice {
    /// 带走：角色立即移除（默认语义）
    Take,
    /// 停留+自主接管：on_tick 恢复（pioneer 自己活）
    StayAuto,
    /// 停留+AI 接管待连接：站桩等外部 AI（本刀只立状态，端点协议是 #014 未来端点）
    StayAi,
}

impl LogoutChoice {
    /// 协议字段值（快照/日志用；"ai-await" 带连字符与 disposition 值域一致）
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            Self::Take => "take",
            Self::StayAuto => "stay_auto",
            Self::StayAi => "stay_ai",
        }
    }
}

/// 实体处置状态（会话级下线后的三种在场所处状态）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposeState {
    /// 处置等待：操控连接意外断开，倒计时到 deadline_tick，超时默认 take。
    /// on_tick 保持抑制（站着不动等处置），可被攻击（世界不等人）。
    Wait { deadline_tick: u64 },
    /// 停留+自主接管：on_tick 恢复（操控独占释放）；me 重声明+重连=抢回操控
    Auto,
    /// 停留+AI 接管待连接：站桩（on_tick 抑制，防守照跑=活角色）
    AiAwait,
}

impl DisposeState {
    /// 快照 disposition 字段值（net-protocol.md v1.7；字段缺席=online）
    pub(crate) fn wire_name(&self) -> &'static str {
        match self {
            Self::Wait { .. } => "dispose_wait",
            Self::Auto => "auto",
            Self::AiAwait => "ai-await",
        }
    }

    /// 是否抑制 on_tick（Wait/AiAwait 抑制=站着等；Auto 不抑制=自主恢复）
    pub(crate) fn suppresses(&self) -> bool {
        !matches!(self, Self::Auto)
    }

    /// 处置等待剩余拍数（仅 Wait 有；其余 None）
    pub(crate) fn wait_left(&self, now: u64) -> Option<u64> {
        match self {
            Self::Wait { deadline_tick } => Some(deadline_tick.saturating_sub(now)),
            _ => None,
        }
    }
}

/// begin_wait 结果（调用方据此打日志或立即清算）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitOutcome {
    /// 已进入等待（deadline = now + wait_ticks）
    Waiting,
    /// 等待期 0 拍：不落账，调用方立即 take
    TakeNow,
    /// 无需等待（玩家已有处置状态，不重复进入）
    Skipped,
}

/// 处置账本：玩家名 -> 处置状态（会话级，不持久化）
pub(crate) struct DisposeBook {
    states: HashMap<String, DisposeState>,
    wait_ticks: u64,
}

impl DisposeBook {
    pub(crate) fn new(wait_ticks: u64) -> Self {
        Self {
            states: HashMap::new(),
            wait_ticks,
        }
    }

    pub(crate) fn wait_ticks(&self) -> u64 {
        self.wait_ticks
    }

    /// 配置覆盖（main 启动时从 CODEWORLD_DISPOSE_WAIT 装配）
    pub(crate) fn set_wait_ticks(&mut self, ticks: u64) {
        self.wait_ticks = ticks;
    }

    /// 查询玩家处置状态
    pub(crate) fn get(&self, name: &str) -> Option<DisposeState> {
        self.states.get(name).copied()
    }

    /// 是否有处置记录（控制解析屏蔽依据：处置中的玩家不被 me 声明重认领）
    pub(crate) fn has(&self, name: &str) -> bool {
        self.states.contains_key(name)
    }

    /// 是否抑制该玩家的 on_tick
    pub(crate) fn suppresses(&self, name: &str) -> bool {
        self.get(name).map(|s| s.suppresses()).unwrap_or(false)
    }

    /// 登出三选一落地（覆盖旧值；take 由调用方清算实体后调 clear）
    pub(crate) fn apply(&mut self, name: &str, state: DisposeState) {
        self.states.insert(name.to_string(), state);
    }

    /// 意外断开：开始处置等待。已有处置不重复进入；等待期 0 拍=立即 take 不落账。
    pub(crate) fn begin_wait(&mut self, name: &str, now: u64) -> WaitOutcome {
        if self.states.contains_key(name) {
            return WaitOutcome::Skipped;
        }
        if self.wait_ticks == 0 {
            return WaitOutcome::TakeNow;
        }
        self.states.insert(
            name.to_string(),
            DisposeState::Wait {
                deadline_tick: now + self.wait_ticks,
            },
        );
        WaitOutcome::Waiting
    }

    /// 等待超时清算：返回本拍到期（deadline <= now）的玩家名并摘除记录。
    /// 调用方对每个名字执行 take（kill 实体 + 离场标记）。
    pub(crate) fn expire_due(&mut self, now: u64) -> Vec<String> {
        let due: Vec<String> = self
            .states
            .iter()
            .filter(|(_, s)| {
                matches!(s, DisposeState::Wait { deadline_tick } if *deadline_tick <= now)
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in &due {
            self.states.remove(name);
        }
        due
    }

    /// 清除处置状态（死亡清理/文件锚让位用）；返回此前是否有记录（供条件日志）
    pub(crate) fn clear(&mut self, name: &str) -> bool {
        self.states.remove(name).is_some()
    }

    /// 重连认领：清除该玩家全部处置，返回原状态（供"接管者被顶掉/重连取消"沿日志）
    pub(crate) fn clear_all(&mut self, name: &str) -> Option<DisposeState> {
        self.states.remove(name)
    }

    /// 房间打扫：只留在线玩家的处置记录（防无界增长；离场标记的名字本就无记录）
    pub(crate) fn prune(&mut self, online: &HashMap<String, EntityId>) {
        self.states.retain(|name, _| online.contains_key(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // === LogoutChoice 协议值 ===

    #[test]
    fn logout_choice_wire_names_match_protocol_v17() {
        // 契约（net-protocol.md v1.7）：disposition 值域 take/stay_auto/stay_ai
        assert_eq!(LogoutChoice::Take.wire_name(), "take");
        assert_eq!(LogoutChoice::StayAuto.wire_name(), "stay_auto");
        assert_eq!(LogoutChoice::StayAi.wire_name(), "stay_ai");
    }

    #[test]
    fn logout_choice_parses_from_protocol_json() {
        // serde snake_case：协议 JSON 值直接反序列化（net.rs 解析层的域基础）
        let v: LogoutChoice = serde_json::from_str("\"stay_auto\"").unwrap();
        assert_eq!(v, LogoutChoice::StayAuto);
        let v: LogoutChoice = serde_json::from_str("\"take\"").unwrap();
        assert_eq!(v, LogoutChoice::Take);
        let v: LogoutChoice = serde_json::from_str("\"stay_ai\"").unwrap();
        assert_eq!(v, LogoutChoice::StayAi);
        assert!(serde_json::from_str::<LogoutChoice>("\"dance\"").is_err(), "非法值拒绝");
    }

    // === DisposeState 语义 ===

    #[test]
    fn dispose_state_wire_names_match_protocol_v17() {
        // 快照 disposition 值域：dispose_wait/auto/ai-await（字段缺席=online）
        assert_eq!(
            DisposeState::Wait { deadline_tick: 130 }.wire_name(),
            "dispose_wait"
        );
        assert_eq!(DisposeState::Auto.wire_name(), "auto");
        assert_eq!(DisposeState::AiAwait.wire_name(), "ai-await");
    }

    #[test]
    fn only_auto_unsuppresses_on_tick() {
        // #025 接管者语义：stay_auto=自主恢复（不抑制）；Wait/AiAwait=站着等（抑制）
        assert!(!DisposeState::Auto.suppresses(), "auto=自主恢复");
        assert!(DisposeState::AiAwait.suppresses(), "ai-await 站桩抑制");
        assert!(
            DisposeState::Wait { deadline_tick: 0 }.suppresses(),
            "处置等待抑制（站着不动等处置）"
        );
    }

    #[test]
    fn wait_left_counts_down_and_never_negative() {
        let w = DisposeState::Wait { deadline_tick: 130 };
        assert_eq!(w.wait_left(100), Some(30), "等待开始剩 30 拍");
        assert_eq!(w.wait_left(129), Some(1), "到期前一拍剩 1");
        assert_eq!(w.wait_left(130), Some(0), "到期拍剩 0（expire_due 同拍清算）");
        assert_eq!(w.wait_left(200), Some(0), "越界钳 0 不回绕");
        assert_eq!(DisposeState::Auto.wait_left(100), None, "非等待态无剩余拍数");
    }

    // === begin_wait / expire_due：等待窗口生命周期 ===

    #[test]
    fn begin_wait_then_expires_at_deadline() {
        // 规则：begin(100) 等待 30 拍 -> 129 未到期、130 到期清算
        let mut book = DisposeBook::new(30);
        assert_eq!(book.begin_wait("p1", 100), WaitOutcome::Waiting);
        assert_eq!(
            book.get("p1"),
            Some(DisposeState::Wait { deadline_tick: 130 }),
            "deadline = 开始拍 + 等待拍数"
        );
        assert!(book.expire_due(129).is_empty(), "未到期不清算");
        assert!(book.has("p1"));
        assert_eq!(book.expire_due(130), vec!["p1".to_string()], "到期拍清算");
        assert!(!book.has("p1"), "清算后摘除");
    }

    #[test]
    fn begin_wait_zero_ticks_takes_immediately_without_state() {
        // 规则（CODEWORLD_DISPOSE_WAIT=0）：立即默认 take——不落账，调用方直接清算
        let mut book = DisposeBook::new(0);
        assert_eq!(book.begin_wait("p1", 100), WaitOutcome::TakeNow);
        assert!(!book.has("p1"), "0 拍等待不落账（take 即时清算）");
    }

    #[test]
    fn begin_wait_skips_when_disposition_already_active() {
        // 防御：已有处置（Auto/AiAwait/Wait）的玩家不重复进入等待（不覆盖既有处置）
        let mut book = DisposeBook::new(30);
        book.apply("p1", DisposeState::Auto);
        assert_eq!(book.begin_wait("p1", 100), WaitOutcome::Skipped);
        assert_eq!(book.get("p1"), Some(DisposeState::Auto), "既有处置不被覆盖");
    }

    // === apply / clear / clear_all / prune ===

    #[test]
    fn apply_logout_overwrites_previous_state() {
        // 边界：先等待后主动登出（极端时序）——logout 三选一覆盖等待状态
        let mut book = DisposeBook::new(30);
        book.apply("p1", DisposeState::Wait { deadline_tick: 130 });
        book.apply("p1", DisposeState::Auto);
        assert_eq!(book.get("p1"), Some(DisposeState::Auto));
    }

    #[test]
    fn clear_returns_whether_a_record_existed() {
        let mut book = DisposeBook::new(30);
        book.apply("p1", DisposeState::AiAwait);
        assert!(book.clear("p1"), "有记录时 clear 返回 true（条件日志用）");
        assert!(!book.clear("p1"), "无记录时 false");
        assert!(!book.has("p1"));
    }

    #[test]
    fn clear_all_returns_previous_state_for_reclaim_logs() {
        // 重连认领：返回原状态供"接管者被顶掉（auto）/重连取消处置（其余）"沿日志
        let mut book = DisposeBook::new(30);
        book.apply("p1", DisposeState::Auto);
        assert_eq!(book.clear_all("p1"), Some(DisposeState::Auto));
        assert_eq!(book.clear_all("p1"), None, "二次认领无记录");
        assert!(!book.has("p1"));
    }

    #[test]
    fn prune_keeps_only_online_players() {
        // 房间打扫：处置记录只属于在场玩家；不在 online 表的名字清除（防无界增长）
        let mut online = HashMap::new();
        online.insert("p1".to_string(), EntityId(uuid::Uuid::from_u128(1)));
        let mut book = DisposeBook::new(30);
        book.apply("p1", DisposeState::Auto);
        book.apply("gone", DisposeState::AiAwait);
        book.prune(&online);
        assert!(book.has("p1"), "在线玩家的处置保留");
        assert!(!book.has("gone"), "不在场的处置清除");
    }

    #[test]
    fn default_wait_ticks_is_thirty() {
        // 宪法默认：30 拍处置等待（CODEWORLD_DISPOSE_WAIT 可覆盖，config 层测试）
        assert_eq!(DEFAULT_DISPOSE_WAIT_TICKS, 30);
        let book = DisposeBook::new(DEFAULT_DISPOSE_WAIT_TICKS);
        assert_eq!(book.wait_ticks(), 30);
    }
}
