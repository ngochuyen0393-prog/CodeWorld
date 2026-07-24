//! 沙箱通道：ChannelManager 实现 probe/attack_at/call
//!
//! 架构文档 3.2 节（沙箱通道接口）+ 4.2（攻击流程）+ 4.3（探测流程）。

use crate::observe::{collect_meta, ProbeResult};
use crate::sandbox::{SandboxHandle, SandboxManager};
use codeworld_core::render::AttackResult;
use codeworld_core::world::WorldState;
use serde::{Deserialize, Serialize};

/// 服务调用状态（架构 3.2）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallStatus {
    /// 调用成功
    Ok,
    /// 服务不存在/owner 死亡（不报错，不泄露信息）
    Void,
    /// 超时（MVP 未实现，Phase 2 加 wasmtime 超时检测）
    Timeout,
}

/// 服务调用结果（架构 3.2）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallResult {
    /// 服务返回的数据
    pub data: Option<Vec<u8>>,
    /// 调用状态
    pub status: CallStatus,
}

/// 沙箱通道管理器（架构 3.2 + 4.2 + 4.3）
///
/// 无状态：probe/attack_at/call 为关联函数，操作 WorldState/SandboxManager。
pub struct ChannelManager;

impl ChannelManager {
    /// 创建管理器
    pub fn new() -> Self {
        tracing::debug!("ChannelManager 创建");
        ChannelManager
    }

    /// 探测目标（架构 4.3）：调用 on_probe + 收集被动观测数据
    ///
    /// 没导出 on_probe 时返回空响应 + meta。
    pub fn probe(
        sandbox_mgr: &SandboxManager,
        sandbox: &mut SandboxHandle,
        request: &[u8],
    ) -> ProbeResult {
        let response = match sandbox_mgr.call_on_probe(sandbox, request) {
            Ok(r) => r,
            Err(e) => {
                // 探测不应崩溃：on_probe 调用失败（fuel 耗尽/trap）记 warn，返回空响应
                tracing::warn!(error = %e, "on_probe 调用失败，返回空响应");
                Vec::new()
            }
        };
        let meta = collect_meta(sandbox.store.data());
        tracing::info!(resp_len = response.len(), gas = meta.gas_consumed, "probe 完成");
        ProbeResult { response, meta }
    }

    /// 检查坐标是否有实体（架构 4.2 的坐标命中判定）
    ///
    /// 注意：这是同步的坐标命中检查，**不是**完整攻击流程。Phase 2 的真实攻击走队列模型
    /// （玩家代码调 `env.attack_at` import 排队 -> 主循环阶段二 `process_attacks` 统一执行
    /// on_attack 拦截 / 路线A替换 / life 判定 / 击杀掠夺，见 bin/codeworld）。本函数仅做
    /// 坐标->实体的命中探测，保留供需要同步判断坐标占用的场景使用，不被攻击主流程调用。
    pub fn attack_at(
        world: &WorldState,
        _sandbox_mgr: &SandboxManager,
        pos: (f64, f64),
        _payload: &[u8],
    ) -> AttackResult {
        // MVP O(n) 线性扫描；后续用空间索引优化（Phase 2）
        let target = world.entities.values().find(|e| e.pos() == pos);
        let Some(_) = target else {
            tracing::info!(?pos, "attack 落空");
            return AttackResult::miss();
        };
        tracing::info!(?pos, "attack 命中（坐标检查；完整流程走 process_attacks）");
        AttackResult::new(true, false, false) // hit, not intercepted, not killed
    }

    /// 调用服务（架构 3.2）
    ///
    /// 服务不存在/owner 死亡 -> Void（不报错，不泄露信息）；存在 -> Ok（MVP 空数据）。
    pub fn call(world: &WorldState, service_name: &str, _params: &[u8]) -> CallResult {
        let service = world.services.values().find(|s| s.name == service_name);
        let Some(service) = service else {
            tracing::info!(service = service_name, "服务不存在");
            return CallResult {
                data: None,
                status: CallStatus::Void,
            };
        };
        let owner_alive = world
            .entities
            .get(&service.owner)
            .map(|e| e.is_alive())
            .unwrap_or(false);
        if !owner_alive {
            tracing::info!(service = service_name, "服务 owner 死亡");
            return CallResult {
                data: None,
                status: CallStatus::Void,
            };
        }
        tracing::info!(service = service_name, "服务调用成功（MVP 空数据）");
        CallResult {
            data: Some(Vec::new()),
            status: CallStatus::Ok,
        }
    }
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxManager;
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
    use codeworld_core::world::{Service, WorldState};
    use uuid::Uuid;

    const PROBE_WAT: &str = r#"(module
        (func (export "on_probe") (param i32) (result i32)
          local.get 0 i32.const 1 i32.add))"#;

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

    fn wasm(wat_text: &str) -> Vec<u8> {
        wat::parse_bytes(wat_text.as_bytes()).unwrap().into_owned()
    }

    #[test]
    fn call_status_variants_distinct() {
        assert_ne!(CallStatus::Ok, CallStatus::Void);
        assert_ne!(CallStatus::Void, CallStatus::Timeout);
    }

    #[test]
    fn probe_returns_response_and_meta() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(PROBE_WAT), 1000, EntityId(Uuid::from_u128(1))).unwrap();
        let result = ChannelManager::probe(&mgr, &mut handle, &[42]);
        // on_probe(42) -> 43，响应非空
        assert!(!result.response.is_empty());
    }

    #[test]
    fn probe_no_export_returns_empty_response() {
        let mgr = SandboxManager::new().unwrap();
        let wat = r#"(module (func (export "life") (result i32) i32.const 1))"#;
        let mut handle = mgr.create_sandbox(&wasm(wat), 1000, EntityId(Uuid::from_u128(1))).unwrap();
        let result = ChannelManager::probe(&mgr, &mut handle, &[]);
        assert!(result.response.is_empty()); // 没 on_probe -> 空响应
    }

    #[test]
    fn attack_at_miss_when_no_entity() {
        let world = WorldState::new();
        let mgr = SandboxManager::new().unwrap();
        let result = ChannelManager::attack_at(&world, &mgr, (5.0, 5.0), &[]);
        assert!(!result.hit); // miss
    }

    #[test]
    fn attack_at_hit_when_entity_at_pos() {
        let mut world = WorldState::new();
        let e = make_entity(1, 5.0, 5.0);
        world.entities.insert(e.id, e);
        let mgr = SandboxManager::new().unwrap();
        let result = ChannelManager::attack_at(&world, &mgr, (5.0, 5.0), &[]);
        assert!(result.hit); // 命中
    }

    #[test]
    fn call_returns_void_when_service_missing() {
        let world = WorldState::new();
        let result = ChannelManager::call(&world, "nonexistent", &[]);
        assert_eq!(result.status, CallStatus::Void);
    }

    #[test]
    fn call_returns_ok_when_service_exists() {
        let mut world = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let e = make_entity(1, 0.0, 0.0);
        world.entities.insert(e.id, e);
        let svc = Service::new(owner, "heal".to_string(), "hash".to_string(), 0);
        world.services.insert(svc.id, svc);
        let result = ChannelManager::call(&world, "heal", &[]);
        assert_eq!(result.status, CallStatus::Ok);
    }

    #[test]
    fn call_returns_void_when_owner_dead() {
        let mut world = WorldState::new();
        let owner = EntityId(Uuid::from_u128(1));
        let mut e = make_entity(1, 0.0, 0.0);
        e.kill();
        world.entities.insert(e.id, e);
        let svc = Service::new(owner, "heal".to_string(), "hash".to_string(), 0);
        world.services.insert(svc.id, svc);
        let result = ChannelManager::call(&world, "heal", &[]);
        assert_eq!(result.status, CallStatus::Void); // owner 死亡 -> Void
    }
}
