//! 世界状态（WorldState）及世界聚合类型
//!
//! WorldState 是世界内核的中央数据结构，聚合实体/区域/服务/变换/物理覆盖/快照。
//! 本模块同时定义这些聚合所需的数据结构（数据由 core 持有，逻辑由各 engine 实现）。
//! 详见架构文档 2.1 / 2.5 / 2.6 / 2.7 节。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::entity::{Entity, EntityId, PlayerId};
use crate::physics::{DefaultPhysics, PhysicsOverride};

/// 区域唯一标识
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ZoneId(pub Uuid);

impl ZoneId {
    /// 生成一个新的随机区域ID
    pub fn new() -> Self {
        let id = Self(Uuid::new_v4());
        tracing::debug!(zone_id = %id.0, "ZoneId 生成");
        id
    }
}

impl Default for ZoneId {
    fn default() -> Self {
        Self::new()
    }
}

/// 服务唯一标识
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceId(pub Uuid);

impl ServiceId {
    /// 生成一个新的随机服务ID
    pub fn new() -> Self {
        let id = Self(Uuid::new_v4());
        tracing::debug!(service_id = %id.0, "ServiceId 生成");
        id
    }
}

impl Default for ServiceId {
    fn default() -> Self {
        Self::new()
    }
}

/// 空间变换唯一标识
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransformId(pub Uuid);

impl TransformId {
    /// 生成一个新的随机变换ID
    pub fn new() -> Self {
        let id = Self(Uuid::new_v4());
        tracing::debug!(transform_id = %id.0, "TransformId 生成");
        id
    }
}

impl Default for TransformId {
    fn default() -> Self {
        Self::new()
    }
}

/// 区域边界框（轴对齐）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    /// X轴下界
    pub x_min: f64,
    /// Y轴下界
    pub y_min: f64,
    /// X轴上界
    pub x_max: f64,
    /// Y轴上界
    pub y_max: f64,
}

impl BoundingBox {
    /// 创建边界框
    pub fn new(x_min: f64, y_min: f64, x_max: f64, y_max: f64) -> Self {
        let bb = Self { x_min, y_min, x_max, y_max };
        tracing::debug!(?bb.x_min, ?bb.y_min, ?bb.x_max, ?bb.y_max, "BoundingBox 创建");
        bb
    }

    /// 点是否在边界框内（含边界）
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x_min && x <= self.x_max && y >= self.y_min && y <= self.y_max
    }
}

/// 区域（架构文档 2.5 节）
///
/// `transform` 为区域内的坐标变换 WASM 字节码（None 表示无变换）。
/// 区域规则逻辑（on_enter/on_cross）由 space-engine 实现。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    /// 区域ID
    pub id: ZoneId,
    /// 创建者
    pub owner: PlayerId,
    /// 区域边界
    pub bounds: BoundingBox,
    /// 区域内的坐标变换（WASM 字节码，None=无）
    pub transform: Option<Vec<u8>>,
    /// 区域规则代码 hash（on_enter/on_cross 等）
    pub rules_code_hash: String,
    /// 创建 tick
    pub created_tick: u64,
}

impl Zone {
    /// 创建区域（id 自动生成，transform 默认 None）
    pub fn new(
        owner: PlayerId,
        bounds: BoundingBox,
        rules_code_hash: String,
        created_tick: u64,
    ) -> Self {
        let zone = Self {
            id: ZoneId::new(),
            owner,
            bounds,
            transform: None,
            rules_code_hash,
            created_tick,
        };
        tracing::debug!(zone_id = %zone.id.0, created_tick, "区域注册");
        zone
    }
}

/// 服务（架构文档 2.6 节）
///
/// 提供方实体死亡则服务消失（call 返回 Void）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    /// 服务ID
    pub id: ServiceId,
    /// 提供方实体
    pub owner: EntityId,
    /// 服务名（用于 call 查找，可重复）
    pub name: String,
    /// 处理函数代码 hash
    pub handler_code_hash: String,
    /// 创建 tick
    pub created_tick: u64,
}

impl Service {
    /// 创建服务（id 自动生成）
    pub fn new(
        owner: EntityId,
        name: String,
        handler_code_hash: String,
        created_tick: u64,
    ) -> Self {
        let svc = Self {
            id: ServiceId::new(),
            owner,
            name,
            handler_code_hash,
            created_tick,
        };
        tracing::debug!(service_id = %svc.id.0, name = %svc.name, "服务注册");
        svc
    }
}

/// 空间变换作用域（架构文档 2.7 节，MVP 不含 Global）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransformScope {
    /// 只影响自己
    Entity(EntityId),
    /// 影响某区域内所有实体
    Zone(ZoneId),
}

/// 空间变换注册条目（架构文档 2.7 节）
///
/// `transform_fn` 为变换函数 WASM 字节码，实际执行由 space-engine 负责。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformEntry {
    /// 变换ID
    pub id: TransformId,
    /// 注册者
    pub owner: EntityId,
    /// 作用域
    pub scope: TransformScope,
    /// 变换函数（WASM 字节码）
    pub transform_fn: Vec<u8>,
    /// 优先级（多个变换叠加时的顺序）
    pub priority: i32,
    /// 创建 tick
    pub created_tick: u64,
}

impl TransformEntry {
    /// 创建变换条目（id 自动生成）
    pub fn new(
        owner: EntityId,
        scope: TransformScope,
        transform_fn: Vec<u8>,
        priority: i32,
        created_tick: u64,
    ) -> Self {
        let te = Self {
            id: TransformId::new(),
            owner,
            scope,
            transform_fn,
            priority,
            created_tick,
        };
        tracing::debug!(transform_id = %te.id.0, priority, "空间变换注册");
        te
    }
}

/// 世界状态快照（单条）
///
/// `data` 为序列化后的世界状态字节，具体序列化由 storage 模块负责。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldSnapshot {
    /// 快照对应的 tick
    pub tick: u64,
    /// 序列化后的世界状态
    pub data: Vec<u8>,
}

impl WorldSnapshot {
    /// 创建快照
    pub fn new(tick: u64, data: Vec<u8>) -> Self {
        tracing::debug!(tick, size = data.len(), "世界快照创建");
        Self { tick, data }
    }
}

/// 历史快照存储（架构文档 2.1 / 七、持久化）
///
/// 数据结构由 core 持有；快照创建/恢复/保留策略由 time-engine 与 storage 实现。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnapshotStore {
    snapshots: HashMap<u64, WorldSnapshot>,
}

impl SnapshotStore {
    /// 创建空快照库
    pub fn new() -> Self {
        Self::default()
    }

    /// 存入（覆盖同 tick）快照
    pub fn store(&mut self, tick: u64, data: Vec<u8>) {
        tracing::debug!(tick, size = data.len(), "快照存入");
        self.snapshots.insert(tick, WorldSnapshot::new(tick, data));
    }

    /// 按 tick 取快照
    pub fn get(&self, tick: u64) -> Option<&WorldSnapshot> {
        self.snapshots.get(&tick)
    }

    /// 最近（最大 tick）的快照
    pub fn latest(&self) -> Option<&WorldSnapshot> {
        self.snapshots.values().max_by_key(|s| s.tick)
    }

    /// 快照数量
    pub fn count(&self) -> usize {
        self.snapshots.len()
    }
}

/// 世界状态（架构文档 2.1 节）
///
/// 世界内核的中央数据结构。`tick` 由框架单调推进，其余集合由各 engine 读写。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldState {
    /// 全局 tick 计数器
    tick: u64,
    /// 所有存活实体
    pub entities: HashMap<EntityId, Entity>,
    /// 注册的区域
    pub zones: HashMap<ZoneId, Zone>,
    /// 注册的服务
    pub services: HashMap<ServiceId, Service>,
    /// 空间变换注册表
    pub transforms: Vec<TransformEntry>,
    /// 物理规则覆盖注册表
    pub physics_overrides: Vec<PhysicsOverride>,
    /// 默认物理参数
    pub default_physics: DefaultPhysics,
    /// 历史快照
    pub snapshots: SnapshotStore,
}

impl WorldState {
    /// 创建空世界（tick=0，默认物理参数）
    pub fn new() -> Self {
        let w = Self {
            tick: 0,
            entities: HashMap::new(),
            zones: HashMap::new(),
            services: HashMap::new(),
            transforms: Vec::new(),
            physics_overrides: Vec::new(),
            default_physics: DefaultPhysics::default(),
            snapshots: SnapshotStore::new(),
        };
        tracing::info!("世界状态创建 (tick=0)");
        w
    }

    /// 当前全局 tick
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// 推进一个 tick（time-engine 每 tick 调用）
    pub fn advance_tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        tracing::debug!(tick = self.tick, "tick 推进");
        self.tick
    }
}

impl Default for WorldState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{Entity, EntityId, PlayerId};
    use crate::physics::{PhysicsOverride, PhysicsScope};
    use uuid::Uuid;

    // === ZoneId / ServiceId / TransformId ===

    #[test]
    fn zone_id_new_unique_and_hashable() {
        let a = ZoneId::new();
        let b = ZoneId::new();
        assert_ne!(a, b);
        let mut m = std::collections::HashMap::new();
        m.insert(a, 1);
        assert_eq!(m.get(&a), Some(&1));
    }

    #[test]
    fn service_id_new_unique_and_hashable() {
        let a = ServiceId::new();
        let b = ServiceId::new();
        assert_ne!(a, b);
        let mut m = std::collections::HashMap::new();
        m.insert(a, 1);
        assert_eq!(m.get(&a), Some(&1));
    }

    #[test]
    fn transform_id_new_unique_and_hashable() {
        let a = TransformId::new();
        let b = TransformId::new();
        assert_ne!(a, b);
        let mut m = std::collections::HashMap::new();
        m.insert(a, 1);
        assert_eq!(m.get(&a), Some(&1));
    }

    #[test]
    fn zone_id_serialize_roundtrip() {
        let id = ZoneId(Uuid::from_u128(7));
        let json = serde_json::to_string(&id).unwrap();
        let back: ZoneId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // === BoundingBox ===

    #[test]
    fn bounding_box_new_stores_corners() {
        // 创建：边界框四角正确存取
        let bb = BoundingBox::new(0.0, 0.0, 10.0, 5.0);
        assert_eq!(bb.x_min, 0.0);
        assert_eq!(bb.y_min, 0.0);
        assert_eq!(bb.x_max, 10.0);
        assert_eq!(bb.y_max, 5.0);
    }

    #[test]
    fn bounding_box_contains_point_inside() {
        // 读写：包含点判定
        let bb = BoundingBox::new(0.0, 0.0, 10.0, 10.0);
        assert!(bb.contains(5.0, 5.0));
        assert!(bb.contains(0.0, 0.0)); // 边界点算包含
        assert!(bb.contains(10.0, 10.0));
    }

    #[test]
    fn bounding_box_excludes_point_outside() {
        // 边界：框外点不包含
        let bb = BoundingBox::new(0.0, 0.0, 10.0, 10.0);
        assert!(!bb.contains(-0.1, 5.0));
        assert!(!bb.contains(5.0, 10.1));
    }

    #[test]
    fn bounding_box_serialize_roundtrip() {
        let bb = BoundingBox::new(1.0, 2.0, 3.0, 4.0);
        let json = serde_json::to_string(&bb).unwrap();
        let back: BoundingBox = serde_json::from_str(&json).unwrap();
        assert_eq!(back.x_max, 3.0);
    }

    // === Zone ===

    #[test]
    fn zone_new_stores_fields_and_generates_id() {
        // 创建：区域字段正确，id自动生成，transform默认None
        let owner = PlayerId(Uuid::from_u128(1));
        let bounds = BoundingBox::new(0.0, 0.0, 10.0, 10.0);
        let zone = Zone::new(
            owner,
            bounds.clone(),
            "rules_hash_abc".to_string(),
            500,
        );
        assert_eq!(zone.owner, owner);
        assert_eq!(zone.bounds.x_max, 10.0);
        assert_eq!(zone.rules_code_hash, "rules_hash_abc");
        assert_eq!(zone.created_tick, 500);
        assert!(zone.transform.is_none()); // 默认无变换
        assert_ne!(zone.id, ZoneId::new());
    }

    #[test]
    fn zone_serialize_roundtrip() {
        let owner = PlayerId(Uuid::from_u128(1));
        let zone = Zone::new(
            owner,
            BoundingBox::new(0.0, 0.0, 5.0, 5.0),
            "h".to_string(),
            1,
        );
        let json = serde_json::to_string(&zone).unwrap();
        let back: Zone = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rules_code_hash, "h");
        assert!(back.transform.is_none());
    }

    // === Service ===

    #[test]
    fn service_new_stores_fields_and_generates_id() {
        // 创建：服务字段正确
        let owner = EntityId(Uuid::from_u128(1));
        let svc = Service::new(
            owner,
            "heal_aura".to_string(),
            "handler_hash".to_string(),
            100,
        );
        assert_eq!(svc.owner, owner);
        assert_eq!(svc.name, "heal_aura");
        assert_eq!(svc.handler_code_hash, "handler_hash");
        assert_eq!(svc.created_tick, 100);
        assert_ne!(svc.id, ServiceId::new());
    }

    #[test]
    fn service_serialize_roundtrip() {
        let svc = Service::new(
            EntityId(Uuid::from_u128(1)),
            "s".to_string(),
            "h".to_string(),
            1,
        );
        let json = serde_json::to_string(&svc).unwrap();
        let back: Service = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "s");
    }

    // === TransformScope / TransformEntry ===

    #[test]
    fn transform_scope_variants_construct() {
        let eid = EntityId(Uuid::from_u128(1));
        let zid = ZoneId(Uuid::from_u128(2));
        assert_eq!(TransformScope::Entity(eid), TransformScope::Entity(eid));
        assert_eq!(TransformScope::Zone(zid), TransformScope::Zone(zid));
        assert_ne!(TransformScope::Entity(eid), TransformScope::Zone(zid));
    }

    #[test]
    fn transform_entry_new_stores_fields() {
        // 创建：变换注册条目字段正确
        let owner = EntityId(Uuid::from_u128(1));
        let te = TransformEntry::new(
            owner,
            TransformScope::Entity(owner),
            vec![0xAB],
            5,
            1000,
        );
        assert_eq!(te.owner, owner);
        assert_eq!(te.scope, TransformScope::Entity(owner));
        assert_eq!(te.transform_fn, vec![0xAB]);
        assert_eq!(te.priority, 5);
        assert_eq!(te.created_tick, 1000);
        assert_ne!(te.id, TransformId::new());
    }

    #[test]
    fn transform_entry_serialize_roundtrip() {
        let owner = EntityId(Uuid::from_u128(1));
        let te = TransformEntry::new(owner, TransformScope::Zone(ZoneId(Uuid::from_u128(2))), vec![], 0, 0);
        let json = serde_json::to_string(&te).unwrap();
        let back: TransformEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.priority, 0);
    }

    // === WorldSnapshot / SnapshotStore ===

    #[test]
    fn snapshot_store_new_is_empty() {
        // 创建：新快照库为空
        let s = SnapshotStore::new();
        assert_eq!(s.count(), 0);
        assert!(s.latest().is_none());
    }

    #[test]
    fn snapshot_store_store_and_get() {
        // 读写：存入后可按tick取回
        let mut s = SnapshotStore::new();
        s.store(100, vec![1, 2, 3]);
        assert_eq!(s.count(), 1);
        let snap = s.get(100).unwrap();
        assert_eq!(snap.tick, 100);
        assert_eq!(snap.data, vec![1, 2, 3]);
    }

    #[test]
    fn snapshot_store_latest_returns_highest_tick() {
        // 边界：latest 返回最大 tick 的快照
        let mut s = SnapshotStore::new();
        s.store(100, vec![1]);
        s.store(300, vec![3]);
        s.store(200, vec![2]);
        let latest = s.latest().unwrap();
        assert_eq!(latest.tick, 300);
    }

    #[test]
    fn snapshot_store_store_overwrites_same_tick() {
        // 边界：同 tick 重复存覆盖
        let mut s = SnapshotStore::new();
        s.store(100, vec![1]);
        s.store(100, vec![9]);
        assert_eq!(s.count(), 1);
        assert_eq!(s.get(100).unwrap().data, vec![9]);
    }

    #[test]
    fn snapshot_store_get_missing_returns_none() {
        // 边界：取不存在的 tick 返回 None
        let s = SnapshotStore::new();
        assert!(s.get(999).is_none());
    }

    // === WorldState ===

    fn sample_entity_with_id(id_u: u128) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            format!("hash_{}", id_u),
            (0.0, 0.0),
            crate::entity::Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    #[test]
    fn world_state_new_is_empty_with_default_physics() {
        // 创建：新世界 tick=0，所有集合为空，物理参数为默认值
        let w = WorldState::new();
        assert_eq!(w.tick(), 0);
        assert_eq!(w.entities.len(), 0);
        assert_eq!(w.zones.len(), 0);
        assert_eq!(w.services.len(), 0);
        assert_eq!(w.transforms.len(), 0);
        assert_eq!(w.physics_overrides.len(), 0);
        assert_eq!(w.default_physics.gravity, -9.8);
        assert_eq!(w.snapshots.count(), 0);
    }

    #[test]
    fn world_state_advance_tick_increments() {
        // 修改：tick 递增
        let mut w = WorldState::new();
        assert_eq!(w.advance_tick(), 1);
        assert_eq!(w.advance_tick(), 2);
        assert_eq!(w.tick(), 2);
    }

    #[test]
    fn world_state_entity_read_write() {
        // 读写：插入实体后可读取，计数正确
        let mut w = WorldState::new();
        let e = sample_entity_with_id(1);
        let id = e.id;
        w.entities.insert(id, e);
        assert_eq!(w.entities.len(), 1);
        assert!(w.entities.contains_key(&id));
        assert!(w.entities.get(&id).unwrap().is_alive());
    }

    #[test]
    fn world_state_get_nonexistent_entity_is_none() {
        // 边界：空世界取实体返回 None
        let w = WorldState::new();
        assert!(!w.entities.contains_key(&EntityId(Uuid::nil())));
    }

    #[test]
    fn world_state_holds_physics_override_and_zone() {
        // 读写：可存入物理覆盖与区域
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        w.physics_overrides.push(PhysicsOverride::new(
            owner,
            PhysicsScope::Global,
            vec![],
            0,
            0,
        ));
        w.zones.insert(
            ZoneId(Uuid::from_u128(1)),
            Zone::new(
                PlayerId(Uuid::from_u128(1)),
                BoundingBox::new(0.0, 0.0, 1.0, 1.0),
                "h".to_string(),
                0,
            ),
        );
        assert_eq!(w.physics_overrides.len(), 1);
        assert_eq!(w.zones.len(), 1);
    }

    #[test]
    fn world_state_serialize_roundtrip_preserves_tick_and_physics() {
        // 读写：序列化往返保留 tick 与默认物理参数
        let mut w = WorldState::new();
        w.advance_tick();
        let json = serde_json::to_string(&w).unwrap();
        let back: WorldState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tick(), 1);
        assert_eq!(back.default_physics.gravity, -9.8);
        assert_eq!(back.entities.len(), 0);
    }
}
