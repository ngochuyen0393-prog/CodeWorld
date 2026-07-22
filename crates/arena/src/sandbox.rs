//! WASM 沙箱管理：SandboxManager/SandboxHandle/SandboxData
//!
//! 架构文档六节（WASM 沙箱设计）+ 6.3（安全防线）。
//! 基于 wasmtime，启用 fuel 指令计量。spike/route_a 已验证 API。

use anyhow::Result;
use wasmtime::{Caller, Config, Engine, Instance, Linker, Module, Store};

/// 沙箱用户数据（Store data，用于被动观测，架构 4.3）
///
/// `mem_read_count`/`mem_write_count` MVP 不真实统计（wasmtime 不直接暴露，
/// 需 import 函数拦截，留 Phase 2），保留字段供 ProbeMeta 结构完整。
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
        let engine = Engine::new(&config)?;
        let linker = Self::make_linker(&engine)?;
        tracing::debug!("SandboxManager 创建");
        Ok(Self { engine, linker })
    }

    /// 构造 Linker，注册系统 import 函数（env.log，记录 API 调用）
    fn make_linker(engine: &Engine) -> Result<Linker<SandboxData>> {
        let mut linker = Linker::new(engine);
        linker.func_wrap("env", "log", |mut caller: Caller<SandboxData>, _v: i32| {
            caller.data_mut().api_calls.push("log".to_string());
        })?;
        Ok(linker)
    }

    /// 创建沙箱：编译 WASM + 实例化 + 设置 fuel
    pub fn create_sandbox(&self, wasm_bytes: &[u8], fuel: u64) -> Result<SandboxHandle> {
        let module = Module::new(&self.engine, wasm_bytes)?;
        let mut store = Store::new(&self.engine, SandboxData::new());
        store.data_mut().fuel_limit = fuel;
        store.set_fuel(fuel)?;
        let instance = self.linker.instantiate(&mut store, &module)?;
        tracing::debug!(fuel, "沙箱创建");
        Ok(SandboxHandle { store, instance })
    }

    /// 销毁沙箱（drop Store/Instance，线性内存随之释放）
    pub fn destroy_sandbox(&self, handle: SandboxHandle) {
        drop(handle);
        tracing::debug!("沙箱销毁");
    }

    /// 替换沙箱（路线A：销毁旧 + 创建新，旧内存状态丢失）
    pub fn replace_sandbox(
        &self,
        handle: SandboxHandle,
        new_wasm_bytes: &[u8],
        fuel: u64,
    ) -> Result<SandboxHandle> {
        tracing::debug!("沙箱替换（路线A）");
        drop(handle);
        self.create_sandbox(new_wasm_bytes, fuel)
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

    /// 调用 on_probe(request) -> Vec<u8>（WASM 签名 i32->i32，request 取首字节，返回 le 4 字节）
    pub fn call_on_probe(&self, handle: &mut SandboxHandle, request: &[u8]) -> Result<Vec<u8>> {
        let arg = request.first().copied().unwrap_or(0) as i32;
        let func = handle.instance.get_typed_func::<i32, i32>(&mut handle.store, "on_probe")?;
        let result = func.call(&mut handle.store, arg)?;
        self.update_fuel(handle)?;
        tracing::debug!(arg, result, "call on_probe");
        Ok(result.to_le_bytes().to_vec())
    }

    /// 调用 on_attack(payload) -> bool（i32 != 0 = 拦截）
    pub fn call_on_attack(&self, handle: &mut SandboxHandle, payload: &[u8]) -> Result<bool> {
        let arg = payload.first().copied().unwrap_or(0) as i32;
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000).unwrap();
        assert!(mgr.call_life(&mut handle).unwrap());
    }

    #[test]
    fn call_life_returns_false_for_zero() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_FALSE_WAT), 1000).unwrap();
        assert!(!mgr.call_life(&mut handle).unwrap());
    }

    #[test]
    fn has_export_checks_function() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000).unwrap();
        assert!(mgr.has_export(&mut handle, "life").unwrap());
        assert!(!mgr.has_export(&mut handle, "nonexistent").unwrap());
    }

    #[test]
    fn call_on_probe_returns_response() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(PROBE_WAT), 1000).unwrap();
        // on_probe(42) -> 43
        let response = mgr.call_on_probe(&mut handle, &[42]).unwrap();
        let val = i32::from_le_bytes(response.as_slice().try_into().unwrap());
        assert_eq!(val, 43);
    }

    #[test]
    fn call_on_attack_returns_intercept() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(ATTACK_INTERCEPT_WAT), 1000).unwrap();
        assert!(mgr.call_on_attack(&mut handle, &[1]).unwrap()); // 拦截
    }

    #[test]
    fn replace_sandbox_creates_new() {
        let mgr = SandboxManager::new().unwrap();
        let handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000).unwrap();
        let mut handle = mgr.replace_sandbox(handle, &wasm(LIFE_FALSE_WAT), 1000).unwrap();
        assert!(!mgr.call_life(&mut handle).unwrap()); // life()=0
    }

    #[test]
    fn destroy_sandbox_drops_handle() {
        let mgr = SandboxManager::new().unwrap();
        let handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000).unwrap();
        mgr.destroy_sandbox(handle); // 不崩即可
    }

    #[test]
    fn call_on_tick_skips_if_no_export() {
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000).unwrap();
        // LIFE_TRUE_WAT 没有 on_tick，call_on_tick 不应报错
        assert!(mgr.call_on_tick(&mut handle).is_ok());
    }
}
