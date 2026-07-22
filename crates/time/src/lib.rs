//! time-engine：tick 计数器 + 快照存储
//!
//! 架构文档 1.4 节时间模块子结构：
//! - tick 计数器（clock）-- 提供"现在第几 tick"
//! - 快照存储（snapshot）-- 存/恢复世界状态
//! - 时间篡改接口 -- Phase 2，MVP 不实现

pub mod clock;
pub mod snapshot;
