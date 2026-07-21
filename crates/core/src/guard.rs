//! 指令配额（InstructionGuard）
//!
//! 纯技术保护，非游戏数值。防止玩家代码跑死服务器（死循环/恶意吃CPU）。
//! 规则（架构文档 2.4 节）：超额=本tick跳过该实体（不死亡，下次tick继续）。

use serde::{Deserialize, Serialize};

/// 指令配额（每tick每实体的指令消耗上限）
///
/// 不是"能量条"/"蓝量"，纯技术保护。超额=本tick跳过该实体，不死亡。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstructionGuard {
    /// 每tick每实体最大指令数（如100万条）
    per_tick_limit: u64,
    /// 本tick已消耗指令数
    consumed_this_tick: u64,
}

impl InstructionGuard {
    /// 创建指令配额（指定每tick上限，初始消耗0）
    pub fn new(per_tick_limit: u64) -> Self {
        let g = Self {
            per_tick_limit,
            consumed_this_tick: 0,
        };
        tracing::debug!(per_tick_limit, "指令配额创建");
        g
    }

    /// 重置配额：每tick开始时调用，把本tick消耗清零
    pub fn reset(&mut self) {
        tracing::debug!(
            before = self.consumed_this_tick,
            per_tick_limit = self.per_tick_limit,
            "指令配额重置"
        );
        self.consumed_this_tick = 0;
    }

    /// 消耗 n 条指令。返回 `true` 表示仍未超额（可继续），`false` 表示已超额（本tick跳过）。
    ///
    /// 无论是否超额都记录消耗（saturating，不 panic）。
    pub fn consume(&mut self, n: u64) -> bool {
        self.consumed_this_tick = self.consumed_this_tick.saturating_add(n);
        let within = !self.is_exhausted();
        tracing::debug!(
            consumed = n,
            total_consumed = self.consumed_this_tick,
            per_tick_limit = self.per_tick_limit,
            within_budget = within,
            "指令配额消耗"
        );
        within
    }

    /// 是否超额（consumed >= limit）
    pub fn is_exhausted(&self) -> bool {
        self.consumed_this_tick >= self.per_tick_limit
    }

    /// 每tick上限（只读）
    pub fn per_tick_limit(&self) -> u64 {
        self.per_tick_limit
    }

    /// 本tick已消耗（只读）
    pub fn consumed_this_tick(&self) -> u64 {
        self.consumed_this_tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_zero_consumed_and_not_exhausted() {
        // 创建：新配额消耗为0，未超额
        let g = InstructionGuard::new(1_000_000);
        assert_eq!(g.per_tick_limit(), 1_000_000);
        assert_eq!(g.consumed_this_tick(), 0);
        assert!(!g.is_exhausted());
    }

    #[test]
    fn reset_clears_consumed() {
        // reset：消耗一些后重置，consumed归0，不再超额
        let mut g = InstructionGuard::new(100);
        g.consume(80);
        g.reset();
        assert_eq!(g.consumed_this_tick(), 0);
        assert!(!g.is_exhausted());
    }

    #[test]
    fn consume_increases_consumed_and_returns_true_when_within_budget() {
        // consume：消耗后consumed增加，未超额时返回true
        let mut g = InstructionGuard::new(100);
        let ok = g.consume(30);
        assert_eq!(g.consumed_this_tick(), 30);
        assert!(ok);
        assert!(!g.is_exhausted());
    }

    #[test]
    fn consume_accumulates_across_calls() {
        // 读写：多次消耗累加
        let mut g = InstructionGuard::new(100);
        g.consume(30);
        g.consume(30);
        assert_eq!(g.consumed_this_tick(), 60);
        assert!(!g.is_exhausted());
    }

    #[test]
    fn consume_to_limit_is_exhausted() {
        // 边界：消耗刚好等于上限 -> 超额（>=limit），consume返回false
        let mut g = InstructionGuard::new(100);
        let ok = g.consume(100);
        assert_eq!(g.consumed_this_tick(), 100);
        assert!(!ok);
        assert!(g.is_exhausted());
    }

    #[test]
    fn consume_over_limit_is_exhausted() {
        // 边界：消耗超过上限 -> 超额
        let mut g = InstructionGuard::new(100);
        g.consume(101);
        assert!(g.is_exhausted());
    }

    #[test]
    fn consume_huge_amount_does_not_panic() {
        // 边界：消耗极大值不 panic（saturating），判定为超额
        let mut g = InstructionGuard::new(100);
        let ok = g.consume(u64::MAX);
        assert!(!ok);
        assert!(g.is_exhausted());
    }

    #[test]
    fn consume_zero_is_noop_success() {
        // 边界：消耗0不改变consumed，返回未超额
        let mut g = InstructionGuard::new(100);
        let ok = g.consume(0);
        assert_eq!(g.consumed_this_tick(), 0);
        assert!(ok);
        assert!(!g.is_exhausted());
    }

    #[test]
    fn reset_after_exhaustion_allows_new_tick() {
        // 读写：超额后reset，新tick可继续消耗
        let mut g = InstructionGuard::new(100);
        g.consume(100);
        assert!(g.is_exhausted());
        g.reset();
        g.consume(50);
        assert_eq!(g.consumed_this_tick(), 50);
        assert!(!g.is_exhausted());
    }

    #[test]
    fn guard_serialize_roundtrip() {
        // 读写：序列化往返保留 limit 与 consumed
        let mut g = InstructionGuard::new(1_000_000);
        g.consume(123);
        let json = serde_json::to_string(&g).unwrap();
        let back: InstructionGuard = serde_json::from_str(&json).unwrap();
        assert_eq!(back.per_tick_limit(), 1_000_000);
        assert_eq!(back.consumed_this_tick(), 123);
    }
}
