//! storage crate：SQLite 持久化模块
//!
//! 架构文档七节（持久化与恢复）。编排 [`SqliteStorage`] 后端 + [`SnapshotManager`] 快照管理。
//! MVP：全量快照存 SQLite（元数据 + 内存快照 BLOB + 快照索引），无 WAL/Git（Phase 2）。

pub mod snapshot;
pub mod sqlite;

pub use snapshot::{RetentionPolicy, SnapshotManager};
pub use sqlite::SqliteStorage;

use anyhow::Result;
use codeworld_core::world::WorldState;

/// 存储引擎（编排 SQLite 后端 + 快照管理）
pub struct StorageEngine {
    backend: SqliteStorage,
    snapshot_mgr: SnapshotManager,
    last_snapshot_tick: u64,
}

impl StorageEngine {
    /// 打开文件数据库
    pub fn open(path: &str) -> Result<Self> {
        let backend = SqliteStorage::open(path)?;
        Ok(Self {
            backend,
            snapshot_mgr: SnapshotManager::new(),
            last_snapshot_tick: 0,
        })
    }

    /// 内存数据库（测试用）
    pub fn open_in_memory() -> Result<Self> {
        let backend = SqliteStorage::open_in_memory()?;
        Ok(Self {
            backend,
            snapshot_mgr: SnapshotManager::new(),
            last_snapshot_tick: 0,
        })
    }

    /// 完整流程：检查 should_snapshot，不该存则返回 0；否则序列化存储并返回 tick
    pub fn save_world_snapshot(&mut self, world: &WorldState) -> Result<u64> {
        let tick = world.tick();
        if !self
            .snapshot_mgr
            .should_snapshot(tick, self.last_snapshot_tick)
        {
            tracing::debug!(tick, last = self.last_snapshot_tick, "间隔不足，跳过快照");
            return Ok(0);
        }
        let data = SnapshotManager::serialize_world(world)?;
        self.backend.save_snapshot(tick, &data)?;
        self.last_snapshot_tick = tick;
        tracing::info!(tick, size = data.len(), "世界快照已存储");
        Ok(tick)
    }

    /// 加载最新快照并反序列化
    pub fn load_latest_world(&self) -> Result<Option<WorldState>> {
        match self.backend.load_latest_snapshot()? {
            Some((_tick, data)) => {
                let world = SnapshotManager::deserialize_world(&data)?;
                Ok(Some(world))
            }
            None => Ok(None),
        }
    }

    /// 加载指定 tick 的快照并反序列化
    pub fn load_world_at(&self, tick: u64) -> Result<Option<WorldState>> {
        match self.backend.load_snapshot(tick)? {
            Some(data) => Ok(Some(SnapshotManager::deserialize_world(&data)?)),
            None => Ok(None),
        }
    }

    /// 强制存快照（忽略 should_snapshot，用于测试和关闭时保存）
    pub fn force_save(&mut self, world: &WorldState) -> Result<u64> {
        let tick = world.tick();
        let data = SnapshotManager::serialize_world(world)?;
        self.backend.save_snapshot(tick, &data)?;
        self.last_snapshot_tick = tick;
        tracing::info!(tick, size = data.len(), "世界快照强制存储");
        Ok(tick)
    }

    /// 执行保留策略清理，返回删除数量
    pub fn prune(&self, current_tick: u64) -> Result<usize> {
        // 列出所有快照（tick + size，降序），构造 (tick, created_at) 供保留策略计算
        let all = self.backend.list_snapshots(usize::MAX)?;
        let snaps: Vec<(u64, u64)> = all.iter().map(|(t, _)| (*t, 0)).collect();
        let to_delete = self.snapshot_mgr.prune_snapshots(&snaps, current_tick);
        if to_delete.is_empty() {
            return Ok(0);
        }
        self.backend.delete_snapshots(&to_delete)
    }

    /// 快照总数
    pub fn snapshot_count(&self) -> Result<usize> {
        self.backend.count_snapshots()
    }

    /// 存元数据
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.backend.set_metadata(key, value)
    }

    /// 取元数据
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        self.backend.get_metadata(key)
    }

    /// 关闭前强制存一次快照（如果需要）
    pub fn close(&mut self, world: &WorldState) -> Result<()> {
        self.force_save(world)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
    use uuid::Uuid;

    /// 构造推进到指定 tick 的世界
    fn world_at_tick(tick: u64) -> WorldState {
        let mut w = WorldState::new();
        for _ in 0..tick {
            w.advance_tick();
        }
        w
    }

    #[test]
    fn save_and_load_latest_world_roundtrip() {
        // 读写：save_world_snapshot + load_latest_world 往返一致
        let mut engine = StorageEngine::open_in_memory().unwrap();
        let world = world_at_tick(100); // tick=100，距上次(0)间隔 100，应存
        let tick = engine.save_world_snapshot(&world).unwrap();
        assert_eq!(tick, 100);
        let loaded = engine.load_latest_world().unwrap().unwrap();
        assert_eq!(loaded.tick(), 100);
    }

    #[test]
    fn empty_world_save_and_load() {
        // 读写：空世界存取正确
        let mut engine = StorageEngine::open_in_memory().unwrap();
        let world = world_at_tick(100);
        engine.save_world_snapshot(&world).unwrap();
        let loaded = engine.load_latest_world().unwrap().unwrap();
        assert_eq!(loaded.tick(), 100);
        assert_eq!(loaded.entities.len(), 0);
        assert_eq!(loaded.default_physics.gravity, -9.8);
    }

    #[test]
    fn world_with_entities_save_and_load() {
        // 读写：有实体的世界存取，实体坐标/外观/存活状态匹配
        let mut engine = StorageEngine::open_in_memory().unwrap();
        let mut world = world_at_tick(100);
        let eid = EntityId(Uuid::from_u128(1));
        world.entities.insert(
            eid,
            Entity::new(
                eid,
                PlayerId(Uuid::from_u128(9)),
                "hash_xyz".to_string(),
                (3.5, 7.2),
                Appearance::new("@".to_string(), "red".to_string(), 1.0),
                5,
                1_000_000,
            ),
        );
        engine.save_world_snapshot(&world).unwrap();
        let loaded = engine.load_latest_world().unwrap().unwrap();
        assert_eq!(loaded.entities.len(), 1);
        let e = loaded.entities.get(&eid).unwrap();
        assert_eq!(e.pos(), (3.5, 7.2)); // 坐标匹配
        assert_eq!(e.appearance.color, "red"); // 外观匹配
        assert!(e.is_alive()); // 存活状态匹配
    }

    #[test]
    fn save_world_snapshot_skips_when_interval_not_met() {
        // 读写：should_snapshot 间隔不足时不存储（返回 0）
        let mut engine = StorageEngine::open_in_memory().unwrap();
        let world = world_at_tick(50); // 间隔 50 < 100
        let tick = engine.save_world_snapshot(&world).unwrap();
        assert_eq!(tick, 0);
        assert_eq!(engine.snapshot_count().unwrap(), 0);
    }

    #[test]
    fn force_save_ignores_interval() {
        // 读写：force_save 忽略间隔强制存储
        let mut engine = StorageEngine::open_in_memory().unwrap();
        let world = world_at_tick(50); // 间隔不足，但 force_save 强制
        let tick = engine.force_save(&world).unwrap();
        assert_eq!(tick, 50);
        assert_eq!(engine.snapshot_count().unwrap(), 1);
        let loaded = engine.load_latest_world().unwrap().unwrap();
        assert_eq!(loaded.tick(), 50);
    }

    #[test]
    fn multiple_snapshots_latest_is_newest() {
        // 读写：多次存快照后 load_latest_world 返回最新的
        let mut engine = StorageEngine::open_in_memory().unwrap();
        engine.save_world_snapshot(&world_at_tick(100)).unwrap();
        engine.save_world_snapshot(&world_at_tick(200)).unwrap();
        engine.save_world_snapshot(&world_at_tick(300)).unwrap();
        assert_eq!(engine.snapshot_count().unwrap(), 3);
        let loaded = engine.load_latest_world().unwrap().unwrap();
        assert_eq!(loaded.tick(), 300);
    }

    #[test]
    fn prune_reduces_snapshot_count() {
        // 修改：prune 删除旧快照后 snapshot_count 减少
        let mut engine = StorageEngine::open_in_memory().unwrap();
        // tick=0（%10000==0，old 段保留）与 tick=100（%10000!=0，old 段删除）
        engine.force_save(&world_at_tick(0)).unwrap();
        engine.force_save(&world_at_tick(100)).unwrap();
        assert_eq!(engine.snapshot_count().unwrap(), 2);
        // current 使两者进入 old 段（age >= 2592000）
        let deleted = engine.prune(2692000).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(engine.snapshot_count().unwrap(), 1);
    }

    #[test]
    fn metadata_set_get_roundtrip() {
        // 读写：元数据存取
        let engine = StorageEngine::open_in_memory().unwrap();
        engine.set_metadata("version", "1.0").unwrap();
        assert_eq!(engine.get_metadata("version").unwrap(), Some("1.0".to_string()));
        assert!(engine.get_metadata("missing").unwrap().is_none());
    }

    #[test]
    fn load_world_at_specific_tick() {
        // 读写：load_world_at 加载指定 tick 的快照
        let mut engine = StorageEngine::open_in_memory().unwrap();
        engine.force_save(&world_at_tick(100)).unwrap();
        engine.force_save(&world_at_tick(200)).unwrap();
        let w = engine.load_world_at(100).unwrap().unwrap();
        assert_eq!(w.tick(), 100);
        assert!(engine.load_world_at(999).unwrap().is_none());
    }
}
