//! 生存点数（SurvivalPoints）
//!
//! 框架管理的唯一游戏内硬数值。玩家代码不能直接修改，只能通过框架接口操作。
//! 规则（架构文档 2.3 节）：初始3天，每 86400 tick（=1天）自动 -1，归零死亡。

use serde::{Deserialize, Serialize};

/// 1天对应的 tick 数（架构文档 2.3 节：每 86400 tick 扣减1天）
pub const TICKS_PER_DAY: u64 = 86_400;

/// 生存点数（单位：天）
///
/// 框架管理的唯一硬数值。初始3天，每天衰减1点，归零即死亡。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurvivalPoints {
    /// 当前生存点数（天）
    points: u64,
    /// 创建时间 tick（用于计算每日消耗）
    created_tick: u64,
    /// 上次扣减的 tick
    last_decay_tick: u64,
}

impl SurvivalPoints {
    /// 创建初始生存点数（3天）
    ///
    /// `created_tick` 为实体出生时的全局 tick。
    pub fn initial(created_tick: u64) -> Self {
        let sp = Self {
            points: 3,
            created_tick,
            last_decay_tick: created_tick,
        };
        tracing::debug!(
            points = sp.points,
            created_tick,
            "生存点数初始化"
        );
        sp
    }

    /// 计算衰减：每经过 86400 tick 扣减1点，归零后稳定在0
    ///
    /// 内部推进 `last_decay_tick` 到最近整天，重复用同一 tick 调用是幂等的。
    /// 传入比 `last_decay_tick` 更小的 tick 视为时间倒流，安全无操作。
    pub fn decay(&mut self, current_tick: u64) {
        // 时间倒流保护：current_tick < last_decay_tick 时不衰减
        let elapsed = match current_tick.checked_sub(self.last_decay_tick) {
            Some(e) => e,
            None => {
                tracing::warn!(
                    current_tick,
                    last_decay_tick = self.last_decay_tick,
                    "decay 收到早于上次扣减的tick，忽略"
                );
                return;
            }
        };
        let days = elapsed / TICKS_PER_DAY;
        if days == 0 {
            return; // 不足1天，不衰减
        }
        let before = self.points;
        self.points = self.points.saturating_sub(days);
        self.last_decay_tick += days * TICKS_PER_DAY;
        tracing::debug!(
            before,
            after = self.points,
            days,
            current_tick,
            last_decay_tick = self.last_decay_tick,
            "生存点数衰减"
        );
    }

    /// 是否归零死亡
    pub fn is_dead(&self) -> bool {
        self.points == 0
    }

    /// 转移 n 点给 `to`。点数不足则失败且双方不变。
    ///
    /// 转移全部点数会让捐赠方归零死亡（合法牺牲）。
    pub fn transfer(&mut self, to: &mut SurvivalPoints, n: u64) -> bool {
        if self.points < n {
            tracing::debug!(
                from_points = self.points,
                requested = n,
                "生存点数转移失败：余额不足"
            );
            return false;
        }
        self.points -= n;
        to.points += n;
        tracing::info!(
            transferred = n,
            from_after = self.points,
            to_after = to.points,
            "生存点数转移成功"
        );
        true
    }

    /// 当前点数（只读）
    pub fn points(&self) -> u64 {
        self.points
    }

    /// 创建 tick（只读）
    pub fn created_tick(&self) -> u64 {
        self.created_tick
    }

    /// 上次扣减 tick（只读）
    pub fn last_decay_tick(&self) -> u64 {
        self.last_decay_tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_has_three_days_and_not_dead() {
        // 创建：初始生存点数=3天，未死亡
        let s = SurvivalPoints::initial(0);
        assert_eq!(s.points(), 3);
        assert_eq!(s.created_tick(), 0);
        assert_eq!(s.last_decay_tick(), 0);
        assert!(!s.is_dead());
    }

    #[test]
    fn decay_one_day_reduces_to_two() {
        // 衰减：经过1天(86400 tick)后点数从3降到2
        let mut s = SurvivalPoints::initial(0);
        s.decay(86400);
        assert_eq!(s.points(), 2);
        assert!(!s.is_dead());
    }

    #[test]
    fn decay_partial_day_does_not_reduce() {
        // 边界：不足1天(86399 tick)不衰减
        let mut s = SurvivalPoints::initial(0);
        s.decay(86399);
        assert_eq!(s.points(), 3);
    }

    #[test]
    fn decay_three_days_reaches_zero_and_dead() {
        // 边界：3天后点数归零，is_dead=true
        let mut s = SurvivalPoints::initial(0);
        s.decay(86400 * 3);
        assert_eq!(s.points(), 0);
        assert!(s.is_dead());
    }

    #[test]
    fn decay_clamps_at_zero() {
        // 边界：衰减超过剩余点数不会下溢，稳定在0
        let mut s = SurvivalPoints::initial(0);
        s.decay(86400 * 5);
        assert_eq!(s.points(), 0);
        assert!(s.is_dead());
    }

    #[test]
    fn decay_advances_last_decay_tick() {
        // 读写：衰减后 last_decay_tick 推进到整天数，再调同tick不会重复衰减
        let mut s = SurvivalPoints::initial(0);
        s.decay(86400);
        assert_eq!(s.last_decay_tick(), 86400);
        // 同tick再调一次：无新增衰减
        s.decay(86400);
        assert_eq!(s.points(), 2);
    }

    #[test]
    fn decay_handles_nonzero_start_tick() {
        // 边界：从非0 tick创建，按经过的真实天数衰减
        let mut s = SurvivalPoints::initial(1000);
        s.decay(1000 + 86400 * 2);
        assert_eq!(s.points(), 1); // 3 - 2 = 1
    }

    #[test]
    fn decay_with_earlier_tick_is_noop() {
        // 边界：传入比 last_decay_tick 更小的 tick 不应 panic、不应增加点数
        let mut s = SurvivalPoints::initial(86400);
        s.decay(100); // 时间倒流（异常输入），应安全无操作
        assert_eq!(s.points(), 3);
    }

    #[test]
    fn transfer_moves_points_between_two_owners() {
        // 转移：A转1点给B，A=2 B=4
        let mut a = SurvivalPoints::initial(0);
        let mut b = SurvivalPoints::initial(0);
        assert!(a.transfer(&mut b, 1));
        assert_eq!(a.points(), 2);
        assert_eq!(b.points(), 4);
    }

    #[test]
    fn transfer_insufficient_points_fails_and_unchanged() {
        // 边界：点数不足时转移失败，双方不变
        let mut a = SurvivalPoints::initial(0); // 3
        let mut b = SurvivalPoints::initial(0); // 3
        assert!(!a.transfer(&mut b, 5));
        assert_eq!(a.points(), 3);
        assert_eq!(b.points(), 3);
    }

    #[test]
    fn transfer_all_points_leaves_donor_dead() {
        // 边界：转全部点数后捐赠方归零死亡
        let mut a = SurvivalPoints::initial(0); // 3
        let mut b = SurvivalPoints::initial(0); // 3
        assert!(a.transfer(&mut b, 3));
        assert_eq!(a.points(), 0);
        assert!(a.is_dead());
        assert_eq!(b.points(), 6);
    }

    #[test]
    fn transfer_zero_is_noop_success() {
        // 边界：转0点成功但双方不变
        let mut a = SurvivalPoints::initial(0);
        let mut b = SurvivalPoints::initial(0);
        assert!(a.transfer(&mut b, 0));
        assert_eq!(a.points(), 3);
        assert_eq!(b.points(), 3);
    }

    #[test]
    fn survivalpoints_serialize_roundtrip() {
        // 读写：序列化往返保留点数与时间字段
        let s = SurvivalPoints::initial(12345);
        let json = serde_json::to_string(&s).unwrap();
        let back: SurvivalPoints = serde_json::from_str(&json).unwrap();
        assert_eq!(s.points(), back.points());
        assert_eq!(s.created_tick(), back.created_tick());
        assert_eq!(s.last_decay_tick(), back.last_decay_tick());
    }
}
