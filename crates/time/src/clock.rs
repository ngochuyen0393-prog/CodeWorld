//! tick 时钟：TimeEngine 推进 tick 并按间隔触发快照
//!
//! 架构文档 4.1 节：time-engine 每 tick 执行 tick++，每 100 tick 触发一次快照。
//! TimeEngine 只管 tick 计数与触发判定，不持有 WorldState（模块边界：主循环负责
//! 协调 WorldState.advance_tick）。

/// tick 主循环驱动器（架构文档 4.1 节）
///
/// 维护全局 tick 计数与快照触发间隔。每个 tick 调用 [`advance_tick`](Self::advance_tick)
/// 推进计数，达到间隔时返回 `true` 表示应触发快照。不持有 WorldState（模块边界：
/// time-engine 只管 tick 计数与触发判定，主循环负责协调 `WorldState.advance_tick`）。
#[derive(Debug, Clone)]
pub struct TimeEngine {
    /// 当前 tick 计数
    tick: u64,
    /// 快照触发间隔（每 N tick 触发一次）
    snapshot_interval: u64,
}

impl TimeEngine {
    /// 创建引擎：tick=0，默认快照间隔 100（架构文档 4.1：每 100 tick 快照）
    pub fn new() -> Self {
        tracing::debug!(tick = 0, snapshot_interval = 100, "TimeEngine 创建");
        Self {
            tick: 0,
            snapshot_interval: 100,
        }
    }

    /// 创建引擎：tick=0，自定义快照间隔
    pub fn new_with_interval(snapshot_interval: u64) -> Self {
        tracing::debug!(tick = 0, snapshot_interval, "TimeEngine 创建(自定义间隔)");
        Self {
            tick: 0,
            snapshot_interval,
        }
    }

    /// 推进一个 tick，返回是否到达快照触发点（`tick % interval == 0`）
    ///
    /// 间隔为 0 时永不触发（避免取模 panic）。
    pub fn advance_tick(&mut self) -> bool {
        self.tick = self.tick.saturating_add(1);
        let trigger = self.snapshot_interval > 0 && self.tick.is_multiple_of(self.snapshot_interval);
        tracing::debug!(tick = self.tick, interval = self.snapshot_interval, trigger, "tick 推进");
        trigger
    }

    /// 当前 tick
    pub fn current_tick(&self) -> u64 {
        self.tick
    }

    /// 快照触发间隔
    pub fn snapshot_interval(&self) -> u64 {
        self.snapshot_interval
    }
}

impl Default for TimeEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::world::WorldState;

    #[test]
    fn new_default_tick_zero_interval_100() {
        // 创建：new() tick=0，默认快照间隔 100
        let e = TimeEngine::new();
        assert_eq!(e.current_tick(), 0);
        assert_eq!(e.snapshot_interval(), 100);
    }

    #[test]
    fn new_with_interval_custom() {
        // 创建：new_with_interval 指定间隔
        let e = TimeEngine::new_with_interval(50);
        assert_eq!(e.current_tick(), 0);
        assert_eq!(e.snapshot_interval(), 50);
    }

    #[test]
    fn advance_tick_increments_and_no_trigger_before_interval() {
        // 修改：tick 递增，间隔内不触发
        let mut e = TimeEngine::new();
        for _ in 0..99 {
            assert!(!e.advance_tick(), "tick<100 不应触发快照");
        }
        assert_eq!(e.current_tick(), 99);
    }

    #[test]
    fn advance_tick_triggers_at_100_not_99_or_101() {
        // 边界：tick 100 触发，99/101 不触发
        let mut e = TimeEngine::new();
        for _ in 0..99 {
            e.advance_tick();
        }
        assert_eq!(e.current_tick(), 99);
        assert!(e.advance_tick(), "tick 100 应触发快照");
        assert_eq!(e.current_tick(), 100);
        assert!(!e.advance_tick(), "tick 101 不应触发");
    }

    #[test]
    fn advance_tick_triggers_at_200_and_300() {
        // 边界：100/200/300 触发，其余不触发
        let mut e = TimeEngine::new();
        for step in 1..=300 {
            let triggered = e.advance_tick();
            if step == 100 || step == 200 || step == 300 {
                assert!(triggered, "tick {} 应触发", step);
            } else {
                assert!(!triggered, "tick {} 不应触发", step);
            }
        }
        assert_eq!(e.current_tick(), 300);
    }

    #[test]
    fn custom_interval_triggers_correctly() {
        // 边界：自定义间隔 5，tick 5/10 触发
        let mut e = TimeEngine::new_with_interval(5);
        for _ in 0..4 {
            assert!(!e.advance_tick());
        }
        assert!(e.advance_tick(), "tick 5 应触发");
        assert!(!e.advance_tick(), "tick 6 不应触发");
        for _ in 0..3 {
            e.advance_tick();
        }
        assert!(e.advance_tick(), "tick 10 应触发");
    }

    #[test]
    fn time_engine_tick_aligns_with_world_state() {
        // 集成：主循环中 TimeEngine 与 WorldState.tick 保持一致（架构 4.1）
        let mut world = WorldState::new();
        let mut engine = TimeEngine::new();
        for _ in 0..150 {
            engine.advance_tick();
            world.advance_tick();
        }
        assert_eq!(engine.current_tick(), world.tick());
    }
}
