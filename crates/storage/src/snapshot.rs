//! 快照序列化/反序列化 + 保留策略（架构文档 7.1 节）
//!
//! 负责将 [`WorldState`] 序列化为字节（存入 SQLite BLOB）与反序列化恢复，
//! 并按保留策略计算应清理的旧快照。MVP 用 tick 差值近似时间（1天 = 86400 tick）。

use anyhow::{Context, Result};
use codeworld_core::world::WorldState;

/// 快照保留策略（架构文档 7.1 节）
///
/// - 7 天内保留每 `recent_granularity` tick 的快照
/// - 7~30 天保留每 `medium_granularity` tick 的快照
/// - 30 天后保留每 `old_granularity` tick 的快照
#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    /// 每 N tick 做一次全量快照
    pub snapshot_interval: u64,
    /// 7 天内保留粒度（每 N tick）
    pub recent_granularity: u64,
    /// 7 天后保留粒度
    pub medium_granularity: u64,
    /// 30 天后保留粒度
    pub old_granularity: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            snapshot_interval: 100,
            recent_granularity: 100,
            medium_granularity: 1000,
            old_granularity: 10000,
        }
    }
}

/// 快照管理器（序列化 + 保留策略）
pub struct SnapshotManager {
    /// 保留策略
    retention: RetentionPolicy,
}

impl SnapshotManager {
    /// 默认保留策略（snapshot_interval=100, recent=100, medium=1000, old=10000）
    pub fn new() -> Self {
        Self {
            retention: RetentionPolicy::default(),
        }
    }

    /// 序列化 WorldState 为 JSON 字节
    pub fn serialize_world(world: &WorldState) -> Result<Vec<u8>> {
        serde_json::to_vec(world).context("序列化 WorldState 失败")
    }

    /// 反序列化
    pub fn deserialize_world(data: &[u8]) -> Result<WorldState> {
        serde_json::from_slice(data).context("反序列化 WorldState 失败")
    }

    /// 是否该做快照了（current_tick - last >= snapshot_interval）
    pub fn should_snapshot(&self, current_tick: u64, last_snapshot_tick: u64) -> bool {
        current_tick.saturating_sub(last_snapshot_tick) >= self.retention.snapshot_interval
    }

    /// 计算需要删除的快照 tick 列表
    ///
    /// 参数：`(tick, created_at_timestamp)` 列表。MVP 用 tick 差值近似时间
    /// （1 天 = 86400 tick，每秒 1 tick）：
    /// - 7 天内（age < 604800）：保留每 `recent_granularity` tick
    /// - 7~30 天（604800 <= age < 2592000）：保留每 `medium_granularity` tick
    /// - 30 天后（age >= 2592000）：保留每 `old_granularity` tick
    ///
    /// 返回应删除的 tick 列表（升序，保证确定性）。
    pub fn prune_snapshots(&self, snapshots: &[(u64, u64)], current_tick: u64) -> Vec<u64> {
        const SECS_PER_DAY: u64 = 86_400;
        let recent_window = 7 * SECS_PER_DAY; // 604800
        let medium_window = 30 * SECS_PER_DAY; // 2592000
        let mut to_delete = Vec::new();
        for &(tick, _created_at) in snapshots {
            let age = current_tick.saturating_sub(tick);
            let granularity = if age < recent_window {
                self.retention.recent_granularity
            } else if age < medium_window {
                self.retention.medium_granularity
            } else {
                self.retention.old_granularity
            };
            // 粒度为 0 视为全保留（防御）
            if granularity == 0 {
                continue;
            }
            // 非 granularity 倍数的快照删除
            if tick % granularity != 0 {
                to_delete.push(tick);
            }
        }
        to_delete.sort();
        to_delete
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
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
    use uuid::Uuid;

    /// 构造推进到指定 tick 的世界（WorldState.tick 私有，只能 advance 推进）
    fn world_at_tick(tick: u64) -> WorldState {
        let mut w = WorldState::new();
        for _ in 0..tick {
            w.advance_tick();
        }
        w
    }

    #[test]
    fn serialize_deserialize_empty_world_roundtrip() {
        // 读写：空世界序列化/反序列化往返一致
        let world = world_at_tick(5);
        let data = SnapshotManager::serialize_world(&world).unwrap();
        let back = SnapshotManager::deserialize_world(&data).unwrap();
        assert_eq!(back.tick(), 5);
        assert_eq!(back.entities.len(), 0);
        assert_eq!(back.default_physics.gravity, -9.8);
    }

    #[test]
    fn serialize_deserialize_world_with_entities_roundtrip() {
        // 读写：有实体的世界序列化/反序列化，实体坐标/外观/存活状态匹配
        let mut world = world_at_tick(10);
        let eid = EntityId(Uuid::from_u128(1));
        world.entities.insert(
            eid,
            Entity::new(
                eid,
                PlayerId(Uuid::from_u128(9)),
                "code_hash_abc".to_string(),
                (3.5, 7.2),
                Appearance::new("@".to_string(), "red".to_string(), 1.0),
                5,
                1_000_000,
            ),
        );
        let data = SnapshotManager::serialize_world(&world).unwrap();
        let back = SnapshotManager::deserialize_world(&data).unwrap();
        assert_eq!(back.tick(), 10);
        assert_eq!(back.entities.len(), 1);
        let restored = back.entities.get(&eid).unwrap();
        assert_eq!(restored.pos(), (3.5, 7.2)); // 坐标匹配
        assert_eq!(restored.appearance.glyph, "@"); // 外观匹配
        assert_eq!(restored.appearance.color, "red");
        assert!(restored.is_alive()); // 存活状态匹配
        assert_eq!(restored.code_hash, "code_hash_abc");
    }

    #[test]
    fn should_snapshot_true_when_interval_met() {
        // 读写：间隔 >= 100 tick 返回 true
        let mgr = SnapshotManager::new();
        assert!(mgr.should_snapshot(100, 0)); // 正好 100
        assert!(mgr.should_snapshot(150, 50)); // 间隔 100
        assert!(mgr.should_snapshot(1000, 900)); // 间隔 100
        assert!(mgr.should_snapshot(250, 100)); // 间隔 150
    }

    #[test]
    fn should_snapshot_false_when_interval_not_met() {
        // 读写：间隔 < 100 tick 返回 false
        let mgr = SnapshotManager::new();
        assert!(!mgr.should_snapshot(0, 0));
        assert!(!mgr.should_snapshot(50, 0));
        assert!(!mgr.should_snapshot(99, 0));
        assert!(!mgr.should_snapshot(149, 50)); // 间隔 99
    }

    #[test]
    fn prune_keeps_all_recent_snapshots() {
        // 保留策略：7 天内的快照全保留（均为 100 倍数，%100==0）
        let mgr = SnapshotManager::new();
        let current = 5000u64; // 距所有快照 age < 604800（7 天）
        let snaps = vec![(100u64, 0u64), (200, 0), (300, 0), (500, 0)];
        let to_delete = mgr.prune_snapshots(&snaps, current);
        assert!(to_delete.is_empty(), "7 天内快照应全保留，但建议删除: {to_delete:?}");
    }

    #[test]
    fn prune_medium_keeps_every_1000_tick() {
        // 保留策略：7 天外（medium 段）只保留每 1000 tick 的
        let mgr = SnapshotManager::new();
        // 7 天 = 604800 tick；current = 614800，所有快照 age 落在 medium 段
        let current = 614800u64;
        let snaps = vec![(1000u64, 0u64), (1100, 0), (1500, 0), (2000, 0)];
        let to_delete = mgr.prune_snapshots(&snaps, current);
        // 应删 1100、1500（%1000 != 0），保留 1000、2000
        assert!(to_delete.contains(&1100), "应删除 1100");
        assert!(to_delete.contains(&1500), "应删除 1500");
        assert!(!to_delete.contains(&1000), "应保留 1000");
        assert!(!to_delete.contains(&2000), "应保留 2000");
    }

    #[test]
    fn prune_old_keeps_every_10000_tick() {
        // 保留策略：30 天外（old 段）只保留每 10000 tick 的
        let mgr = SnapshotManager::new();
        // 30 天 = 2592000 tick；current = 2692000，所有快照 age 落在 old 段
        let current = 2692000u64;
        let snaps = vec![(0u64, 0u64), (5000, 0), (10000, 0), (15000, 0), (20000, 0)];
        let to_delete = mgr.prune_snapshots(&snaps, current);
        // 应删 5000、15000（%10000 != 0），保留 0、10000、20000
        assert!(to_delete.contains(&5000), "应删除 5000");
        assert!(to_delete.contains(&15000), "应删除 15000");
        assert!(!to_delete.contains(&0), "应保留 0");
        assert!(!to_delete.contains(&10000), "应保留 10000");
        assert!(!to_delete.contains(&20000), "应保留 20000");
    }
}
