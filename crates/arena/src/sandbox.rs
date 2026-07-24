//! WASM 沙箱管理：SandboxManager/SandboxHandle/SandboxData
//!
//! 架构文档六节（WASM 沙箱设计）+ 6.3（安全防线）。
//! 基于 wasmtime，启用 fuel 指令计量。spike/route_a 已验证 API。

use anyhow::Result;
use wasmtime::{Caller, Config, Engine, Instance, Linker, Module, Store};

use crate::attack::PendingAttack;
use codeworld_core::entity::EntityId;

/// 沙箱用户数据（Store data，用于被动观测，架构 4.3）
///
/// `mem_read_count`/`mem_write_count` MVP 不真实统计（wasmtime 不直接暴露，
/// 需 import 函数拦截，留 Phase 2），保留字段供 ProbeMeta 结构完整。
///
/// `entity_id`/`pending_attacks` 为攻击流程服务（架构 4.2）：import 函数 env.attack_at
/// 在 on_tick 执行期间被调，此时 world 被借用不可改，故只把攻击请求推入 pending_attacks，
/// 由主循环阶段二统一收集处理（与 PendingChange 模式一致）。
#[derive(Debug, Default)]
pub struct SandboxData {
    /// 初始 fuel 配额（用于计算 consumed）
    pub fuel_limit: u64,
    /// 已消耗 fuel（call 后由 update_fuel 同步）
    pub fuel_consumed: u64,
    /// 内存读次数（MVP 不真实统计）
    pub mem_read_count: u64,
    /// 内存写次数（MVP 不真实统计）
    pub mem_write_count: u64,
    /// API 调用列表（import 函数调用时记录）
    pub api_calls: Vec<String>,
    /// 本沙箱所属实体ID（import 函数需据此识别攻击者；Default 随机值仅占位，
    /// 真实沙箱由 create_sandbox 显式设置）
    pub entity_id: EntityId,
    /// 本 tick 排队的攻击请求（env.attack_at 推入，阶段二收集后 drain 清空）
    pub pending_attacks: Vec<PendingAttack>,
}

impl SandboxData {
    /// 创建空沙箱数据
    pub fn new() -> Self {
        Self::default()
    }
}

/// 沙箱句柄（持有 Store + Instance）
pub struct SandboxHandle {
    /// WASM Store（含用户数据 + fuel）
    pub store: Store<SandboxData>,
    /// WASM 实例
    pub instance: Instance,
}

/// 沙箱管理器（架构文档六节）
///
/// 持有 wasmtime Engine（启用 fuel 指令计量）和 Linker（注册系统 import 函数）。
pub struct SandboxManager {
    engine: Engine,
    linker: Linker<SandboxData>,
}

impl SandboxManager {
    /// 创建管理器：初始化 Engine（启用 fuel）和 Linker（注册 env.log）
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        // 内存限制：每个 WASM 实例最大 64MB（CLAUDE.md 技术约束）
        // static_memory_maximum_size 设静态内存上限；完整动态内存硬上限用 ResourceLimiter（Phase 2）
        config.static_memory_maximum_size(64 * 1024 * 1024);
        let engine = Engine::new(&config)?;
        let linker = Self::make_linker(&engine)?;
        tracing::debug!("SandboxManager 创建");
        Ok(Self { engine, linker })
    }

    /// 构造 Linker，注册系统 import 函数（env.log / env.attack_at）
    ///
    /// env.attack_at(x, y, payload_ptr, payload_len) -> i32（架构 4.2）：
    ///   - 不检查坐标有无实体（import 无 world 访问权），只把攻击请求排队到
    ///     SandboxData.pending_attacks，实际 hit/miss 在阶段二 process_attacks 判断。
    ///   - payload 从 caller 的 memory export 读取 [ptr..ptr+len]；模块未导出 memory
    ///     或 ptr<0/len<=0/越界 -> 空 payload（空 payload 由阶段二用默认击杀 WAT）。
    ///   - 返回 1=已排队（快速返回，不阻塞 on_tick）。
    fn make_linker(engine: &Engine) -> Result<Linker<SandboxData>> {
        let mut linker = Linker::new(engine);
        linker.func_wrap("env", "log", |mut caller: Caller<SandboxData>, _v: i32| {
            caller.data_mut().api_calls.push("log".to_string());
        })?;
        linker.func_wrap(
            "env",
            "attack_at",
            |mut caller: Caller<SandboxData>, x: f64, y: f64, ptr: i32, len: i32| -> i32 {
                let attacker = caller.data().entity_id;
                let payload = read_payload_from_caller(&mut caller, ptr, len);
                caller.data_mut().pending_attacks.push(PendingAttack {
                    attacker,
                    target_pos: (x, y),
                    payload,
                });
                tracing::info!(
                    attacker = %attacker.0,
                    x,
                    y,
                    payload_len = len,
                    "[attack_at] 攻击已排队"
                );
                1 // 已排队
            },
        )?;
        Ok(linker)
    }

    /// 创建沙箱：编译 WASM + 实例化 + 设置 fuel + 绑定 entity_id
    ///
    /// `entity_id` 写入 SandboxData，使 import 函数 env.attack_at 能识别攻击者。
    pub fn create_sandbox(
        &self,
        wasm_bytes: &[u8],
        fuel: u64,
        entity_id: EntityId,
    ) -> Result<SandboxHandle> {
        let module = Module::new(&self.engine, wasm_bytes)?;
        let mut store = Store::new(&self.engine, SandboxData::new());
        store.data_mut().fuel_limit = fuel;
        store.data_mut().entity_id = entity_id;
        store.set_fuel(fuel)?;
        let instance = self.linker.instantiate(&mut store, &module)?;
        tracing::debug!(fuel, entity_id = %entity_id.0, "沙箱创建");
        Ok(SandboxHandle { store, instance })
    }

    /// 销毁沙箱（drop Store/Instance，线性内存随之释放）
    pub fn destroy_sandbox(&self, handle: SandboxHandle) {
        drop(handle);
        tracing::debug!("沙箱销毁");
    }

    /// 替换沙箱（路线A：销毁旧 + 创建新，旧内存状态丢失）
    ///
    /// 保留旧沙箱的 entity_id 与 fuel_limit：替换的是同一实体的代码，身份不变；
    /// fuel_limit 沿用原配额。签名不变（按值消费 handle，返回新 handle）。
    pub fn replace_sandbox(
        &self,
        handle: SandboxHandle,
        new_wasm_bytes: &[u8],
        fuel: u64,
    ) -> Result<SandboxHandle> {
        // drop 前读出 entity_id，传给新沙箱（攻击替换后仍是同一实体）
        let entity_id = handle.store.data().entity_id;
        tracing::debug!(entity_id = %entity_id.0, "沙箱替换（路线A）");
        drop(handle);
        self.create_sandbox(new_wasm_bytes, fuel, entity_id)
    }

    /// 更新 fuel_consumed（fuel_limit - get_fuel）
    fn update_fuel(&self, handle: &mut SandboxHandle) -> Result<()> {
        let remaining = handle.store.get_fuel()?;
        let limit = handle.store.data().fuel_limit;
        handle.store.data_mut().fuel_consumed = limit.saturating_sub(remaining);
        Ok(())
    }

    /// 调用 life() -> bool（i32 != 0 = true）
    pub fn call_life(&self, handle: &mut SandboxHandle) -> Result<bool> {
        let func = handle.instance.get_typed_func::<(), i32>(&mut handle.store, "life")?;
        let result = func.call(&mut handle.store, ())?;
        self.update_fuel(handle)?;
        tracing::debug!(life = result, "call life");
        Ok(result != 0)
    }

    /// 调用 on_tick()（MVP 空实现，无参数；没导出则跳过）
    pub fn call_on_tick(&self, handle: &mut SandboxHandle) -> Result<()> {
        if self.has_export(handle, "on_tick")? {
            let func = handle.instance.get_typed_func::<(), ()>(&mut handle.store, "on_tick")?;
            func.call(&mut handle.store, ())?;
            self.update_fuel(handle)?;
        }
        Ok(())
    }

    /// 调用 on_probe(request) -> Vec<u8>
    ///
    /// WASM 签名 i32->i32，request 前 4 字节（le）转 i32（不足补 0；完整 Vec 需内存传递，Phase 2）。
    /// 没导出 on_probe 时返回空 Vec（防御性，公共 API 自保）。
    pub fn call_on_probe(&self, handle: &mut SandboxHandle, request: &[u8]) -> Result<Vec<u8>> {
        if !self.has_export(handle, "on_probe")? {
            tracing::debug!("on_probe 未导出，返回空响应");
            return Ok(Vec::new());
        }
        let arg = bytes_to_i32(request);
        let func = handle.instance.get_typed_func::<i32, i32>(&mut handle.store, "on_probe")?;
        let result = func.call(&mut handle.store, arg)?;
        self.update_fuel(handle)?;
        tracing::debug!(arg, result, "call on_probe");
        Ok(result.to_le_bytes().to_vec())
    }

    /// 调用 on_attack(payload) -> bool（i32 != 0 = 拦截）
    ///
    /// 没导出 on_attack 时返回 false（不拦截，架构 4.2：直接替换）。
    /// payload 前 4 字节（le）转 i32（同 call_on_probe）。
    pub fn call_on_attack(&self, handle: &mut SandboxHandle, payload: &[u8]) -> Result<bool> {
        if !self.has_export(handle, "on_attack")? {
            tracing::debug!("on_attack 未导出，不拦截");
            return Ok(false);
        }
        let arg = bytes_to_i32(payload);
        let func = handle.instance.get_typed_func::<i32, i32>(&mut handle.store, "on_attack")?;
        let result = func.call(&mut handle.store, arg)?;
        self.update_fuel(handle)?;
        tracing::debug!(arg, result, "call on_attack");
        Ok(result != 0)
    }

    /// 检查实例是否导出某函数
    pub fn has_export(&self, handle: &mut SandboxHandle, name: &str) -> Result<bool> {
        Ok(handle.instance.get_func(&mut handle.store, name).is_some())
    }

    /// 引擎引用
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

/// &[u8] 前 4 字节（le）转 i32（不足补 0；WASM i32 限制，完整 Vec 需内存传递 Phase 2）
fn bytes_to_i32(bytes: &[u8]) -> i32 {
    let mut arr = [0u8; 4];
    let len = bytes.len().min(4);
    arr[..len].copy_from_slice(&bytes[..len]);
    i32::from_le_bytes(arr)
}

/// 从 caller 的 memory export 读取 payload 字节（架构 4.2：attack_at 的 payload 参数）
///
/// 模块未导出 memory / ptr<0 / len<=0 / 越界 -> 空 Vec。
/// 空 payload 由阶段二 process_attacks 用 DEFAULT_KILL_WASM（默认击杀）。
fn read_payload_from_caller(caller: &mut Caller<SandboxData>, ptr: i32, len: i32) -> Vec<u8> {
    if ptr < 0 || len <= 0 {
        return Vec::new();
    }
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    else {
        tracing::debug!("[read_payload] 模块未导出 memory，payload 为空");
        return Vec::new();
    };
    let data = memory.data(&*caller);
    let start = ptr as usize;
    let end = start + len as usize;
    if end > data.len() {
        tracing::warn!(
            ptr,
            len,
            mem_len = data.len(),
            "[read_payload] payload 读取越界，置空"
        );
        return Vec::new();
    }
    data[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::EntityId;
    use uuid::Uuid;

    /// 测试用固定 EntityId（沙箱需绑定 entity_id 供 import 函数识别攻击者）
    fn id(u: u128) -> EntityId {
        EntityId(Uuid::from_u128(u))
    }

    const LIFE_TRUE_WAT: &str = r#"(module (func (export "life") (result i32) i32.const 1))"#;
    const LIFE_FALSE_WAT: &str = r#"(module (func (export "life") (result i32) i32.const 0))"#;
    const PROBE_WAT: &str = r#"(module
        (func (export "on_probe") (param i32) (result i32)
          local.get 0 i32.const 1 i32.add))"#;
    const ATTACK_INTERCEPT_WAT: &str =
        r#"(module (func (export "on_attack") (param i32) (result i32) i32.const 1))"#;

    fn wasm(wat_text: &str) -> Vec<u8> {
        wat::parse_bytes(wat_text.as_bytes()).unwrap().into_owned()
    }

    #[test]
    fn new_creates_manager() {
        let mgr = SandboxManager::new().unwrap();
        let _ = mgr.engine();
    }

    #[test]
    fn create_sandbox_returns_handle() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        assert!(mgr.call_life(&mut handle).unwrap());
    }

    #[test]
    fn call_life_returns_false_for_zero() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_FALSE_WAT), 1000, id(1)).unwrap();
        assert!(!mgr.call_life(&mut handle).unwrap());
    }

    #[test]
    fn has_export_checks_function() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        assert!(mgr.has_export(&mut handle, "life").unwrap());
        assert!(!mgr.has_export(&mut handle, "nonexistent").unwrap());
    }

    #[test]
    fn call_on_probe_returns_response() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(PROBE_WAT), 1000, id(1)).unwrap();
        // on_probe(42) -> 43
        let response = mgr.call_on_probe(&mut handle, &[42]).unwrap();
        let val = i32::from_le_bytes(response.as_slice().try_into().unwrap());
        assert_eq!(val, 43);
    }

    #[test]
    fn call_on_attack_returns_intercept() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(ATTACK_INTERCEPT_WAT), 1000, id(1)).unwrap();
        assert!(mgr.call_on_attack(&mut handle, &[1]).unwrap()); // 拦截
    }

    #[test]
    fn replace_sandbox_creates_new() {
        let mgr = SandboxManager::new().unwrap();
        let handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        let mut handle = mgr.replace_sandbox(handle, &wasm(LIFE_FALSE_WAT), 1000).unwrap();
        assert!(!mgr.call_life(&mut handle).unwrap()); // life()=0
    }

    #[test]
    fn destroy_sandbox_drops_handle() {
        let mgr = SandboxManager::new().unwrap();
        let handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        mgr.destroy_sandbox(handle); // 不崩即可
    }

    #[test]
    fn call_on_tick_skips_if_no_export() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, id(1)).unwrap();
        // LIFE_TRUE_WAT 没有 on_tick，call_on_tick 不应报错
        assert!(mgr.call_on_tick(&mut handle).is_ok());
    }

    // === env.attack_at import（架构 4.2：攻击排队）===

    /// 攻击方 WAT：导出 memory + on_tick 调 attack_at(5.0, 5.0, 0, 0)（空 payload）
    const ATTACKER_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "on_tick")
    f64.const 5.0 f64.const 5.0 i32.const 0 i32.const 0 call $attack_at drop)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 带 payload 的攻击方 WAT：data 段写 [1,2,3,4] 到 memory[0..4]，on_tick 调
    /// attack_at(7.0, 8.0, 0, 4)（payload_ptr=0, payload_len=4，验证从 memory 读 payload）
    const ATTACKER_WITH_DATA_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "\01\02\03\04")
  (func (export "on_tick")
    f64.const 7.0 f64.const 8.0 i32.const 0 i32.const 4 call $attack_at drop)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn attack_at_import_queues_pending_attack() {
        // env.attack_at import：on_tick 调 attack_at -> 请求排入 pending_attacks
        // （import 不检查坐标，只排队；实际击杀由阶段二 process_attacks 处理）
        let mgr = SandboxManager::new().unwrap();
        let attacker_id = id(42);
        let mut handle = mgr
            .create_sandbox(&wasm(ATTACKER_WAT), 1000, attacker_id)
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let attacks = &handle.store.data().pending_attacks;
        assert_eq!(attacks.len(), 1, "应排队 1 次攻击");
        assert_eq!(attacks[0].attacker, attacker_id, "攻击者应为沙箱绑定的实体");
        assert_eq!(attacks[0].target_pos, (5.0, 5.0));
        assert!(attacks[0].payload.is_empty(), "空 payload（ptr=0,len=0）");
    }

    #[test]
    fn attack_at_import_reads_payload_from_memory() {
        // import 从 caller 的 memory export 读取 payload 字节 [ptr..ptr+len]
        let mgr = SandboxManager::new().unwrap();
        let attacker_id = id(7);
        let mut handle = mgr
            .create_sandbox(&wasm(ATTACKER_WITH_DATA_WAT), 1000, attacker_id)
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let attacks = &handle.store.data().pending_attacks;
        assert_eq!(attacks.len(), 1);
        assert_eq!(attacks[0].target_pos, (7.0, 8.0));
        assert_eq!(
            attacks[0].payload, vec![1, 2, 3, 4],
            "应从 memory 读取 payload 字节"
        );
    }

    #[test]
    fn replace_sandbox_preserves_entity_id() {
        // 路线A替换后新沙箱保留旧 entity_id（攻击替换的是同一实体的代码，身份不变）
        let mgr = SandboxManager::new().unwrap();
        let attacker_id = id(99);
        let handle = mgr
            .create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, attacker_id)
            .unwrap();
        assert_eq!(handle.store.data().entity_id, attacker_id, "替换前 entity_id 应已绑定");
        let new_handle = mgr
            .replace_sandbox(handle, &wasm(LIFE_FALSE_WAT), 1000)
            .unwrap();
        assert_eq!(
            new_handle.store.data().entity_id,
            attacker_id,
            "替换后 entity_id 应保留"
        );
    }
}
