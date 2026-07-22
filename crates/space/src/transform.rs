//! 变换管线：TransformPipeline 负责变换的注册/移除/应用
//!
//! 架构文档 2.7 节（空间变换）+ 4.1 节 c 步（按 priority 排序后依次应用）。
//! MVP 不执行真实 WASM 变换函数（arena 模块负责），提供 IdentityExecutor 恒等变换。

use codeworld_core::entity::EntityId;
use codeworld_core::world::{TransformEntry, TransformId, TransformScope, WorldState};

/// 变换函数执行器 trait（抽象 WASM 执行）
///
/// MVP 提供 [`IdentityExecutor`]（恒等变换，不执行 WASM）；
/// arena 模块完成后提供 WasmExecutor 实现真实执行。
pub trait TransformFnExecutor {
    /// 执行变换函数：输入坐标 -> 输出坐标
    fn execute(&self, fn_bytecode: &[u8], input: (f64, f64)) -> (f64, f64);
}

/// 恒等变换执行器（MVP 默认）
///
/// 直接返回输入坐标，不执行 WASM 字节码。arena 模块完成后替换为真实执行器。
pub struct IdentityExecutor;

impl TransformFnExecutor for IdentityExecutor {
    fn execute(&self, _fn_bytecode: &[u8], input: (f64, f64)) -> (f64, f64) {
        tracing::debug!(?input, "IdentityExecutor 恒等变换");
        input
    }
}

/// 变换管线（架构文档 2.7 节 + 4.1 节 c 步）
///
/// 无状态：变换存于 `WorldState.transforms`。
pub struct TransformPipeline;

impl TransformPipeline {
    /// 创建管线
    pub fn new() -> Self {
        tracing::debug!("TransformPipeline 创建");
        TransformPipeline
    }

    /// 注册变换到 WorldState（架构 3.2：register_transform）
    pub fn register_transform(
        world: &mut WorldState,
        owner: EntityId,
        scope: TransformScope,
        transform_fn: Vec<u8>,
        priority: i32,
        created_tick: u64,
    ) -> TransformId {
        let entry = TransformEntry::new(owner, scope, transform_fn, priority, created_tick);
        let id = entry.id;
        tracing::info!(?id, priority, "变换注册");
        world.transforms.push(entry);
        id
    }

    /// 移除变换（架构 3.2：remove_transform）
    ///
    /// 移除成功返回 true，不存在返回 false。
    pub fn remove_transform(world: &mut WorldState, transform_id: TransformId) -> bool {
        let before = world.transforms.len();
        world.transforms.retain(|t| t.id != transform_id);
        let removed = world.transforms.len() < before;
        tracing::info!(?transform_id, removed, "变换移除");
        removed
    }

    /// 获取影响某实体的变换（按 priority 降序排序）
    ///
    /// scope 过滤：Entity scope 匹配 entity_id；Zone scope 匹配实体所在 zone
    /// （实体坐标在该 zone bounds 内）。
    pub fn get_transforms_for_entity(
        world: &WorldState,
        entity_id: EntityId,
    ) -> Vec<&TransformEntry> {
        let pos = world.entities.get(&entity_id).map(|e| e.pos());
        let mut result: Vec<&TransformEntry> = world
            .transforms
            .iter()
            .filter(|t| match &t.scope {
                TransformScope::Entity(eid) => *eid == entity_id,
                TransformScope::Zone(zid) => match (pos, world.zones.get(zid)) {
                    (Some((x, y)), Some(zone)) => zone.bounds.contains(x, y),
                    _ => false,
                },
            })
            .collect();
        result.sort_by_key(|t| std::cmp::Reverse(t.priority)); // 降序：高优先级在前
        tracing::debug!(?entity_id, count = result.len(), "查询实体变换");
        result
    }

    /// 应用变换（架构 4.1 c 步：按 priority 降序依次应用）
    ///
    /// `physical_pos = transform_n(...transform_1(logical_pos))`，高 priority 先应用。
    /// 内部按 priority 降序排序，不依赖输入顺序（防御性设计：调用方可传任意顺序，
    /// 即使 [`get_transforms_for_entity`](Self::get_transforms_for_entity) 已排序也无害）。
    pub fn apply_transforms(
        pos: (f64, f64),
        transforms: &[&TransformEntry],
        executor: &dyn TransformFnExecutor,
    ) -> (f64, f64) {
        let mut sorted: Vec<&TransformEntry> = transforms.to_vec();
        sorted.sort_by_key(|t| std::cmp::Reverse(t.priority)); // 降序
        let result = sorted
            .iter()
            .fold(pos, |acc, t| executor.execute(&t.transform_fn, acc));
        tracing::debug!(?pos, ?result, count = sorted.len(), "应用变换");
        result
    }
}

impl Default for TransformPipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
    use codeworld_core::world::{BoundingBox, TransformEntry, TransformScope, WorldState, Zone};
    use uuid::Uuid;

    fn make_entity(id_u: u128, x: f64, y: f64) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "h".to_string(),
            (x, y),
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    #[test]
    fn identity_executor_returns_input() {
        // IdentityExecutor 恒等变换
        let exec = IdentityExecutor;
        assert_eq!(exec.execute(&[0xAB], (3.0, 4.0)), (3.0, 4.0));
    }

    #[test]
    fn register_transform_adds_to_world() {
        // 读写：注册后 WorldState.transforms 有该变换
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let id = TransformPipeline::register_transform(
            &mut w,
            owner,
            TransformScope::Entity(owner),
            vec![0xAB],
            5,
            100,
        );
        assert_eq!(w.transforms.len(), 1);
        assert_eq!(w.transforms[0].id, id);
        assert_eq!(w.transforms[0].priority, 5);
    }

    #[test]
    fn remove_transform_existing_returns_true() {
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let id = TransformPipeline::register_transform(
            &mut w,
            owner,
            TransformScope::Entity(owner),
            vec![],
            1,
            0,
        );
        assert!(TransformPipeline::remove_transform(&mut w, id));
        assert_eq!(w.transforms.len(), 0);
    }

    #[test]
    fn remove_transform_missing_returns_false() {
        let mut w = WorldState::new();
        let id = codeworld_core::world::TransformId::new();
        assert!(!TransformPipeline::remove_transform(&mut w, id));
    }

    #[test]
    fn get_transforms_for_entity_scope_entity_match() {
        // scope 过滤：Entity scope 只匹配对应实体
        let mut w = WorldState::new();
        let e1 = EntityId(Uuid::from_u128(1));
        let e2 = EntityId(Uuid::from_u128(2));
        TransformPipeline::register_transform(&mut w, e1, TransformScope::Entity(e1), vec![], 10, 0);
        TransformPipeline::register_transform(&mut w, e2, TransformScope::Entity(e2), vec![], 5, 0);
        let ts = TransformPipeline::get_transforms_for_entity(&w, e1);
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].priority, 10);
    }

    #[test]
    fn get_transforms_for_entity_scope_zone_match() {
        // scope 过滤：Zone scope 匹配实体所在 zone（实体在 zone 内）
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let e = make_entity(1, 5.0, 5.0);
        w.entities.insert(e.id, e);
        let zone = Zone::new(
            PlayerId(Uuid::from_u128(9)),
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            "rules".to_string(),
            0,
        );
        let zone_id = zone.id;
        w.zones.insert(zone.id, zone);
        TransformPipeline::register_transform(&mut w, owner, TransformScope::Zone(zone_id), vec![], 7, 0);
        let ts = TransformPipeline::get_transforms_for_entity(&w, EntityId(Uuid::from_u128(1)));
        assert_eq!(ts.len(), 1);
    }

    #[test]
    fn get_transforms_for_entity_scope_zone_not_match() {
        // scope 过滤：实体不在 zone 内则不匹配
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let e = make_entity(1, 100.0, 100.0);
        w.entities.insert(e.id, e);
        let zone = Zone::new(
            PlayerId(Uuid::from_u128(9)),
            BoundingBox::new(0.0, 0.0, 10.0, 10.0),
            "rules".to_string(),
            0,
        );
        let zone_id = zone.id;
        w.zones.insert(zone.id, zone);
        TransformPipeline::register_transform(&mut w, owner, TransformScope::Zone(zone_id), vec![], 7, 0);
        let ts = TransformPipeline::get_transforms_for_entity(&w, EntityId(Uuid::from_u128(1)));
        assert_eq!(ts.len(), 0);
    }

    #[test]
    fn get_transforms_for_entity_sorted_desc_by_priority() {
        // 排序：按 priority 降序
        let mut w = WorldState::new();
        let e1 = EntityId(Uuid::from_u128(1));
        TransformPipeline::register_transform(&mut w, e1, TransformScope::Entity(e1), vec![], 1, 0);
        TransformPipeline::register_transform(&mut w, e1, TransformScope::Entity(e1), vec![], 10, 0);
        TransformPipeline::register_transform(&mut w, e1, TransformScope::Entity(e1), vec![], 5, 0);
        let ts = TransformPipeline::get_transforms_for_entity(&w, e1);
        assert_eq!(ts.len(), 3);
        assert_eq!(ts[0].priority, 10);
        assert_eq!(ts[1].priority, 5);
        assert_eq!(ts[2].priority, 1);
    }

    #[test]
    fn apply_transforms_empty_returns_input() {
        let exec = IdentityExecutor;
        let transforms: Vec<&TransformEntry> = vec![];
        assert_eq!(
            TransformPipeline::apply_transforms((1.0, 2.0), &transforms, &exec),
            (1.0, 2.0)
        );
    }

    #[test]
    fn apply_transforms_identity_returns_input() {
        // IdentityExecutor 恒等，多个变换结果仍是输入
        let exec = IdentityExecutor;
        let mut w = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        TransformPipeline::register_transform(&mut w, owner, TransformScope::Entity(owner), vec![0xAB], 5, 0);
        TransformPipeline::register_transform(&mut w, owner, TransformScope::Entity(owner), vec![0xCD], 1, 0);
        let ts: Vec<&TransformEntry> = w.transforms.iter().collect();
        assert_eq!(
            TransformPipeline::apply_transforms((7.0, 8.0), &ts, &exec),
            (7.0, 8.0)
        );
    }

    /// 测试用 executor：把 bytecode 第一个字节加到 x 坐标（验证应用顺序）
    struct BytecodeAddExecutor;
    impl TransformFnExecutor for BytecodeAddExecutor {
        fn execute(&self, bc: &[u8], input: (f64, f64)) -> (f64, f64) {
            let add = bc.first().copied().unwrap_or(0) as f64;
            (input.0 + add, input.1)
        }
    }

    #[test]
    fn apply_transforms_sorted_desc_applies_high_priority_first() {
        // priority 1/10/5，bytecode [1]/[10]/[5]
        // 降序应用顺序：10 + 5 + 1 = 16（验证高 priority 先应用）
        let owner = EntityId(Uuid::from_u128(1));
        let scope = TransformScope::Entity(owner);
        let transforms = vec![
            TransformEntry::new(owner, scope.clone(), vec![1], 1, 0),
            TransformEntry::new(owner, scope.clone(), vec![10], 10, 0),
            TransformEntry::new(owner, scope, vec![5], 5, 0),
        ];
        let refs: Vec<&TransformEntry> = transforms.iter().collect();
        let exec = BytecodeAddExecutor;
        let result = TransformPipeline::apply_transforms((0.0, 0.0), &refs, &exec);
        assert_eq!(result.0, 16.0);
    }
}
