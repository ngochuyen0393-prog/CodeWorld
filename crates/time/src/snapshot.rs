//! 快照管理：SnapshotManager 创建/查询/保留历史快照
//!
//! 架构文档 七节（持久化）+ 3.2 节（get_history_snapshot，MVP 只读）。
//! 基于 core 的 SnapshotStore 实现，MVP 不实现 WAL 增量日志（storage 模块负责）。

use codeworld_core::world::{SnapshotStore, WorldSnapshot, WorldState};
use thiserror::Error;

/// 快照操作错误
#[derive(Debug, Error)]
pub enum TimeError {
    /// 序列化世界状态失败
    #[error("序列化世界状态失败: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// 快照保留策略（架构文档 7.1 节）
///
/// MVP 只实现 [`KeepAll`](Self::KeepAll)；分级保留（7天/30天）Phase 2 实现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RetirePolicy {
    /// 保留所有快照，不过期删除（MVP 默认）
    #[default]
    KeepAll,
}

/// 快照管理器（架构文档 七节 + 3.2 节）
///
/// 基于 core 的 [`SnapshotStore`] 实现快照创建/查询。MVP 保留所有快照。
pub struct SnapshotManager {
    store: SnapshotStore,
    retire_policy: RetirePolicy,
}

impl SnapshotManager {
    /// 创建空管理器（默认 KeepAll 策略）
    pub fn new() -> Self {
        tracing::debug!(retire_policy = ?RetirePolicy::KeepAll, "SnapshotManager 创建");
        Self {
            store: SnapshotStore::new(),
            retire_policy: RetirePolicy::KeepAll,
        }
    }

    /// 创建并存储世界状态快照，返回快照对应的 tick
    ///
    /// 序列化 WorldState 为字节存入 SnapshotStore。同 tick 重复创建会覆盖。
    ///
    /// 注意：序列化整个 WorldState（含 `snapshots` 字段）。MVP 运行时该字段为空；
    /// Phase 2 从存储加载世界后若 `world.snapshots` 非空，会嵌套历史快照致数据膨胀，
    /// 届时需改为只序列化当前状态子集。
    pub fn create_snapshot(&mut self, world: &WorldState) -> Result<u64, TimeError> {
        let tick = world.tick();
        let data = serde_json::to_vec(world)?;
        tracing::info!(tick, size = data.len(), "快照创建并存储");
        self.store.store(tick, data);
        Ok(tick)
    }

    /// 按 tick 查找历史快照（架构 3.2：get_history_snapshot，MVP 只读）
    pub fn get_snapshot(&self, tick: u64) -> Option<&WorldSnapshot> {
        let snap = self.store.get(tick);
        tracing::debug!(tick, found = snap.is_some(), "查询历史快照");
        snap
    }

    /// 最近的快照（最大 tick）
    pub fn latest_snapshot(&self) -> Option<&WorldSnapshot> {
        self.store.latest()
    }

    /// 快照数量
    pub fn snapshot_count(&self) -> usize {
        self.store.count()
    }

    /// 当前保留策略
    pub fn retire_policy(&self) -> RetirePolicy {
        self.retire_policy
    }
}

impl Default for SnapshotManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::world::WorldState;

    #[test]
    fn new_is_empty() {
        // 创建：新管理器为空
        let m = SnapshotManager::new();
        assert_eq!(m.snapshot_count(), 0);
        assert!(m.latest_snapshot().is_none());
        assert!(m.get_snapshot(0).is_none());
    }

    #[test]
    fn create_snapshot_stores_and_returns_tick() {
        // 读写：创建快照返回 tick，可按 tick 取回
        let mut m = SnapshotManager::new();
        let world = WorldState::new(); // tick=0
        let tick = m.create_snapshot(&world).unwrap();
        assert_eq!(tick, 0);
        assert_eq!(m.snapshot_count(), 1);
        let snap = m.get_snapshot(0).unwrap();
        assert_eq!(snap.tick, 0);
        assert!(!snap.data.is_empty(), "快照数据不应为空");
    }

    #[test]
    fn create_snapshot_at_advanced_tick() {
        // 读写：推进 tick 后创建，返回推进后的 tick
        let mut m = SnapshotManager::new();
        let mut world = WorldState::new();
        world.advance_tick(); // tick=1
        let tick = m.create_snapshot(&world).unwrap();
        assert_eq!(tick, 1);
        assert_eq!(m.snapshot_count(), 1);
    }

    #[test]
    fn latest_snapshot_returns_highest_tick() {
        // 边界：latest 返回最大 tick
        let mut m = SnapshotManager::new();
        let mut world = WorldState::new();
        for _ in 0..3 {
            world.advance_tick();
        }
        m.create_snapshot(&world).unwrap(); // tick 3
        for _ in 0..2 {
            world.advance_tick();
        }
        m.create_snapshot(&world).unwrap(); // tick 5
        assert_eq!(m.snapshot_count(), 2);
        let latest = m.latest_snapshot().unwrap();
        assert_eq!(latest.tick, 5);
    }

    #[test]
    fn get_snapshot_missing_returns_none() {
        // 边界：取不存在的 tick 返回 None
        let m = SnapshotManager::new();
        assert!(m.get_snapshot(999).is_none());
    }

    #[test]
    fn create_snapshot_overwrites_same_tick() {
        // 边界：同 tick 重复创建覆盖（数量不变）
        let mut m = SnapshotManager::new();
        let world = WorldState::new();
        m.create_snapshot(&world).unwrap();
        m.create_snapshot(&world).unwrap(); // 同 tick=0 覆盖
        assert_eq!(m.snapshot_count(), 1);
    }

    #[test]
    fn default_retire_policy_is_keep_all() {
        // 创建：默认保留策略 KeepAll
        let m = SnapshotManager::new();
        assert_eq!(m.retire_policy(), RetirePolicy::KeepAll);
    }

    #[test]
    fn snapshot_data_roundtrips_to_worldstate() {
        // 读写：存入的 data 可反序列化回 WorldState（序列化正确性）
        let mut m = SnapshotManager::new();
        let mut world = WorldState::new();
        world.advance_tick();
        m.create_snapshot(&world).unwrap();
        let snap = m.latest_snapshot().unwrap();
        let restored: WorldState = serde_json::from_slice(&snap.data).unwrap();
        assert_eq!(restored.tick(), 1);
    }
}
