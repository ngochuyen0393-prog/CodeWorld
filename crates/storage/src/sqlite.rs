//! SQLite 存储后端：封装世界快照/实体元数据/键值元数据的持久化操作。
//!
//! 架构文档七节（持久化与恢复）。MVP 用单文件 SQLite（rusqlite bundled），
//! 不实现 WAL 增量日志与 Git 代码仓库（Phase 2）。

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, Connection};
use std::time::{SystemTime, UNIX_EPOCH};

/// SQLite 存储后端
pub struct SqliteStorage {
    conn: Connection,
}

/// u64 -> i64 安全转换（超出 i64 范围则饱和到 i64::MAX）
fn i64_from_u64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// usize -> i64 安全转换
fn i64_from_usize(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// i64 -> u64 安全转换（负值归 0）
fn u64_from_i64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or_default()
}

/// 当前 Unix 时间戳（秒），取值失败归 0
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

impl SqliteStorage {
    /// 打开/创建数据库文件，初始化表结构
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("打开数据库失败: {path}"))?;
        let storage = Self { conn };
        storage.init_schema()?;
        Ok(storage)
    }

    /// 内存数据库（测试用）
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("打开内存数据库失败")?;
        let storage = Self { conn };
        storage.init_schema()?;
        Ok(storage)
    }

    /// 初始化三张表结构（幂等）
    fn init_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS snapshots (
                    tick INTEGER PRIMARY KEY,
                    data BLOB NOT NULL,
                    created_at INTEGER NOT NULL,
                    size INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS entities (
                    entity_id TEXT PRIMARY KEY,
                    owner_id TEXT NOT NULL,
                    code_hash TEXT NOT NULL,
                    pos_x REAL NOT NULL,
                    pos_y REAL NOT NULL,
                    alive INTEGER NOT NULL,
                    created_tick INTEGER NOT NULL,
                    snapshot_tick INTEGER NOT NULL,
                    FOREIGN KEY (snapshot_tick) REFERENCES snapshots(tick)
                );
                CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );",
            )
            .context("初始化表结构失败")?;
        Ok(())
    }

    /// 存快照（INSERT OR REPLACE，同 tick 覆盖）
    pub fn save_snapshot(&self, tick: u64, data: &[u8]) -> Result<()> {
        let now = i64_from_u64(unix_now());
        self.conn
            .execute(
                "INSERT OR REPLACE INTO snapshots (tick, data, created_at, size) VALUES (?1, ?2, ?3, ?4)",
                params![
                    i64_from_u64(tick),
                    data,
                    now,
                    i64_from_usize(data.len()),
                ],
            )
            .context("保存快照失败")?;
        tracing::debug!(tick, size = data.len(), "快照已存入 SQLite");
        Ok(())
    }

    /// 加载指定 tick 的快照
    pub fn load_snapshot(&self, tick: u64) -> Result<Option<Vec<u8>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM snapshots WHERE tick = ?1")?;
        let mut rows = stmt.query(params![i64_from_u64(tick)])?;
        match rows.next()? {
            Some(row) => {
                let data: Vec<u8> = row.get(0)?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    /// 加载最新快照（返回 tick + data）
    pub fn load_latest_snapshot(&self) -> Result<Option<(u64, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tick, data FROM snapshots ORDER BY tick DESC LIMIT 1")?;
        let mut rows = stmt.query(params![])?;
        match rows.next()? {
            Some(row) => {
                let tick: i64 = row.get(0)?;
                let data: Vec<u8> = row.get(1)?;
                Ok(Some((u64_from_i64(tick), data)))
            }
            None => Ok(None),
        }
    }

    /// 列出最近的 N 个快照（tick + size），按 tick 降序
    pub fn list_snapshots(&self, limit: usize) -> Result<Vec<(u64, usize)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tick, size FROM snapshots ORDER BY tick DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![i64_from_usize(limit)], |row| {
            let tick: i64 = row.get(0)?;
            let size: i64 = row.get(1)?;
            Ok((u64_from_i64(tick), u64_from_i64(size) as usize))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 删除指定 tick 之前的快照（tick 严格小于给定值），返回删除数量
    pub fn delete_snapshots_before(&self, tick: u64) -> Result<usize> {
        let deleted = self
            .conn
            .execute(
                "DELETE FROM snapshots WHERE tick < ?1",
                params![i64_from_u64(tick)],
            )
            .context("删除旧快照失败")?;
        Ok(deleted)
    }

    /// 批量存实体元数据（先删后插）
    ///
    /// 参数元组：`(entity_id, owner_id, code_hash, pos_x, pos_y, alive, created_tick)`
    pub fn save_entities(
        &self,
        tick: u64,
        entities: &[(String, String, String, f64, f64, bool, u64)],
    ) -> Result<()> {
        let tick_i = i64_from_u64(tick);
        // 先删后插，整体在同一个事务内（避免删后插入失败导致该 tick 实体元数据丢失）
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM entities WHERE snapshot_tick = ?1",
            params![tick_i],
        )
        .context("清理旧实体元数据失败")?;
        for (entity_id, owner_id, code_hash, pos_x, pos_y, alive, created_tick) in entities {
            tx.execute(
                "INSERT INTO entities (entity_id, owner_id, code_hash, pos_x, pos_y, alive, created_tick, snapshot_tick) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entity_id,
                    owner_id,
                    code_hash,
                    pos_x,
                    pos_y,
                    if *alive { 1i64 } else { 0 },
                    i64_from_u64(*created_tick),
                    tick_i,
                ],
            )?;
        }
        tx.commit()?;
        tracing::debug!(tick, count = entities.len(), "实体元数据已存入");
        Ok(())
    }

    /// 快照总数
    pub fn count_snapshots(&self) -> Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM snapshots", params![], |r| r.get(0))
            .context("统计快照数失败")?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// 存元数据（同 key 覆盖）
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .context("保存元数据失败")?;
        Ok(())
    }

    /// 取元数据
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM metadata WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// 删除指定 tick 集合的快照（保留策略清理用），返回删除数量
    pub fn delete_snapshots(&self, ticks: &[u64]) -> Result<usize> {
        if ticks.is_empty() {
            return Ok(0);
        }
        let placeholders = ticks.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM snapshots WHERE tick IN ({placeholders})");
        let params_vec: Vec<i64> = ticks.iter().map(|&t| i64_from_u64(t)).collect();
        let deleted = self
            .conn
            .execute(&sql, params_from_iter(params_vec))
            .context("批量删除快照失败")?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_snapshot_roundtrip() {
        // 读写：存取往返一致
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.save_snapshot(100, b"hello world").unwrap();
        let loaded = storage.load_snapshot(100).unwrap().unwrap();
        assert_eq!(loaded, b"hello world");
    }

    #[test]
    fn load_snapshot_missing_returns_none() {
        // 边界：空数据库 load_snapshot 返回 None
        let storage = SqliteStorage::open_in_memory().unwrap();
        assert!(storage.load_snapshot(100).unwrap().is_none());
    }

    #[test]
    fn load_latest_snapshot_returns_highest_tick() {
        // 读写：load_latest_snapshot 返回最大 tick 的快照
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.save_snapshot(100, b"a").unwrap();
        storage.save_snapshot(300, b"b").unwrap();
        storage.save_snapshot(200, b"c").unwrap();
        let (tick, data) = storage.load_latest_snapshot().unwrap().unwrap();
        assert_eq!(tick, 300);
        assert_eq!(data, b"b");
    }

    #[test]
    fn load_latest_snapshot_empty_returns_none() {
        // 边界：空数据库 load_latest_snapshot 返回 None
        let storage = SqliteStorage::open_in_memory().unwrap();
        assert!(storage.load_latest_snapshot().unwrap().is_none());
    }

    #[test]
    fn list_snapshots_returns_desc_by_tick() {
        // 读写：list_snapshots 返回按 tick 降序的列表
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.save_snapshot(100, b"a").unwrap();
        storage.save_snapshot(300, b"bb").unwrap();
        storage.save_snapshot(200, b"ccc").unwrap();
        let list = storage.list_snapshots(10).unwrap();
        assert_eq!(list, vec![(300, 2), (200, 3), (100, 1)]);
    }

    #[test]
    fn delete_snapshots_before_only_removes_older() {
        // 修改：delete_snapshots_before 只删除指定 tick 之前的
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.save_snapshot(100, b"a").unwrap();
        storage.save_snapshot(200, b"b").unwrap();
        storage.save_snapshot(300, b"c").unwrap();
        let deleted = storage.delete_snapshots_before(200).unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(storage.count_snapshots().unwrap(), 2);
        assert!(storage.load_snapshot(100).unwrap().is_none());
        assert!(storage.load_snapshot(200).unwrap().is_some());
        assert!(storage.load_snapshot(300).unwrap().is_some());
    }

    #[test]
    fn save_entities_persists_rows() {
        // 读写：save_entities 后能查到（直接 SQL 验证）
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.save_snapshot(100, b"snap").unwrap();
        let entities = vec![
            ("e1".to_string(), "o1".to_string(), "hash1".to_string(), 1.0, 2.0, true, 50u64),
            ("e2".to_string(), "o2".to_string(), "hash2".to_string(), 3.0, 4.0, false, 60u64),
        ];
        storage.save_entities(100, &entities).unwrap();
        let count: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE snapshot_tick = ?1",
                params![100i64],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        // 验证字段：e2 alive=false（存为 0）
        let alive_e2: i64 = storage
            .conn
            .query_row(
                "SELECT alive FROM entities WHERE entity_id = ?1",
                params!["e2"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(alive_e2, 0);
        // 验证坐标
        let (px, py): (f64, f64) = storage
            .conn
            .query_row(
                "SELECT pos_x, pos_y FROM entities WHERE entity_id = ?1",
                params!["e1"],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((px, py), (1.0, 2.0));
    }

    #[test]
    fn metadata_set_get_roundtrip() {
        // 读写：metadata 键值存取往返一致
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.set_metadata("version", "1.0").unwrap();
        assert_eq!(storage.get_metadata("version").unwrap(), Some("1.0".to_string()));
        assert!(storage.get_metadata("missing").unwrap().is_none());
        // 覆盖
        storage.set_metadata("version", "2.0").unwrap();
        assert_eq!(storage.get_metadata("version").unwrap(), Some("2.0".to_string()));
    }

    #[test]
    fn save_snapshot_overwrites_same_tick() {
        // 边界：重复 save_snapshot（同 tick）覆盖旧数据
        let storage = SqliteStorage::open_in_memory().unwrap();
        storage.save_snapshot(100, b"old").unwrap();
        storage.save_snapshot(100, b"new").unwrap();
        assert_eq!(storage.count_snapshots().unwrap(), 1);
        assert_eq!(storage.load_snapshot(100).unwrap().unwrap(), b"new");
    }

    #[test]
    fn open_persists_to_file() {
        // 读写：文件数据库 open 可持久化（重开仍能读到）
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        {
            let storage = SqliteStorage::open(path).unwrap();
            storage.save_snapshot(100, b"file-data").unwrap();
        }
        let storage = SqliteStorage::open(path).unwrap();
        assert_eq!(storage.load_snapshot(100).unwrap().unwrap(), b"file-data");
    }
}
