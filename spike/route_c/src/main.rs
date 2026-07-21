// 路线C：Host Hook拦截
//
// 原理：框架在调用life()时不直接调WASM export，而是在host层加中间层。
// attack时修改host层的"覆写标记"，life()的返回值被host层拦截/篡改为false。
// 不改WASM模块，host层完全可控；每实例独立hook标记。
//
// 验证点：
//   1. wasmtime的Linker能否包装/拦截export函数的调用（诚实结论：Linker只管import，
//      拦截在host调用层实现，本spike用调用层wrapper演示）
//   2. 能否只拦截返回值而不改函数体
//   3. 能否针对单个Instance设置hook（不影响其他Instance）
//   4. Linear Memory状态是否保留
//   5. 所有export都加hook的性能开销

use anyhow::Result;
use std::time::Instant;
use wasmtime::{Caller, Engine, Instance, Linker, Module, Store};

/// 每实例（每Store）的host层数据：包含attack覆写标记
#[derive(Default)]
struct HostData {
    log_calls: u64,
    /// attack标记：true时host层把life()拦截为false
    life_override: bool,
}

fn make_linker(engine: &Engine) -> Result<Linker<HostData>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap("env", "log", |mut caller: Caller<HostData>, _v: i32| {
        caller.data_mut().log_calls += 1;
    })?;
    Ok(linker)
}

fn compile(engine: &Engine, wat_text: &str) -> Result<Module> {
    let wasm = wat::parse_bytes(wat_text.as_bytes())?;
    Ok(Module::new(engine, &wasm)?)
}

/// Host层hook：未attack时调真实WASM life()；attack后(life_override=true)直接返回0
/// 这就是路线C的核心——在host调用层拦截，不改WASM函数体
fn hook_life(store: &mut Store<HostData>, instance: &Instance) -> Result<i32> {
    if store.data().life_override {
        // host层拦截：不调WASM，直接返回false
        return Ok(0);
    }
    let life = instance.get_typed_func::<(), i32>(&mut *store, "life")?;
    Ok(life.call(&mut *store, ())?)
}

fn call_get_state(store: &mut Store<HostData>, instance: &Instance) -> Result<i32> {
    let f = instance.get_typed_func::<(), i32>(&mut *store, "get_state")?;
    Ok(f.call(&mut *store, ())?)
}

fn call_set_state(store: &mut Store<HostData>, instance: &Instance, v: i32) -> Result<()> {
    let f = instance.get_typed_func::<i32, ()>(&mut *store, "set_state")?;
    f.call(&mut *store, v)?;
    Ok(())
}

// === 受害者模块 ===
// life() 是普通直接导出（不需要funcref表），返回1。
// 框架在host层包一层 hook_life()：未attack时透传真实life()，attack后直接返回0。
const VICTIM_WAT: &str = r#"
(module
  (import "env" "log" (func $log (param i32)))
  (memory (export "mem") 1)
  (func (export "life") (result i32) i32.const 1)
  (func (export "set_state") (param $v i32)
    i32.const 0
    local.get $v
    i32.store
    local.get $v
    call $log)
  (func (export "get_state") (result i32)
    i32.const 0
    i32.load))
"#;

fn life_via_hook_normal() -> Result<i32> {
    let engine = Engine::default();
    let module = compile(&engine, VICTIM_WAT)?;
    let linker = make_linker(&engine)?;
    let mut store = Store::new(&engine, HostData::default());
    let instance = linker.instantiate(&mut store, &module)?;
    hook_life(&mut store, &instance)
}

/// attack后hook应返回0（host层拦截，即使WASM life()仍返回1）
fn life_via_hook_after_attack() -> Result<i32> {
    let engine = Engine::default();
    let module = compile(&engine, VICTIM_WAT)?;
    let linker = make_linker(&engine)?;
    let mut store = Store::new(&engine, HostData::default());
    let instance = linker.instantiate(&mut store, &module)?;
    // attack前确认 life=1
    let _before = hook_life(&mut store, &instance)?;
    // attack：设置host层覆写标记
    store.data_mut().life_override = true;
    // attack后 hook返回0（WASM life()仍返回1，但被host层篡改）
    hook_life(&mut store, &instance)
}

/// 单实例隔离：attack实例A，A的hook=0，B的hook=1（不受影响）
fn per_instance_hook_isolation() -> Result<(i32, i32)> {
    let engine = Engine::default();
    let module = compile(&engine, VICTIM_WAT)?;
    let linker = make_linker(&engine)?;
    // 每实例独立Store -> 独立HostData -> 独立hook标记
    let mut store_a = Store::new(&engine, HostData::default());
    let instance_a = linker.instantiate(&mut store_a, &module)?;
    let mut store_b = Store::new(&engine, HostData::default());
    let instance_b = linker.instantiate(&mut store_b, &module)?;
    // attack只打A
    store_a.data_mut().life_override = true;
    let a = hook_life(&mut store_a, &instance_a)?;
    let b = hook_life(&mut store_b, &instance_b)?;
    Ok((a, b))
}

/// 内存保留：attack前写42，attack后读出应仍为42（实例未动）
fn memory_preserved_with_hook() -> Result<i32> {
    let engine = Engine::default();
    let module = compile(&engine, VICTIM_WAT)?;
    let linker = make_linker(&engine)?;
    let mut store = Store::new(&engine, HostData::default());
    let instance = linker.instantiate(&mut store, &module)?;
    call_set_state(&mut store, &instance, 42)?;
    // attack（只改host标记，不动WASM实例/内存）
    store.data_mut().life_override = true;
    call_get_state(&mut store, &instance)
}

/// 测量走hook调用life()的单次耗时（纳秒），用于评估"所有export加hook"的开销
fn hook_call_cost_nanos() -> Result<u64> {
    let engine = Engine::default();
    let module = compile(&engine, VICTIM_WAT)?;
    let linker = make_linker(&engine)?;
    let mut store = Store::new(&engine, HostData::default());
    let instance = linker.instantiate(&mut store, &module)?;
    // 预热
    for _ in 0..1000 {
        let _ = hook_life(&mut store, &instance)?;
    }
    let n = 100_000u64;
    let start = Instant::now();
    for _ in 0..n {
        let _ = hook_life(&mut store, &instance)?;
    }
    Ok(start.elapsed().as_nanos() as u64 / n)
}

/// 直接调用life()的单次耗时（纳秒），用于对比hook开销
fn direct_call_cost_nanos() -> Result<u64> {
    let engine = Engine::default();
    let module = compile(&engine, VICTIM_WAT)?;
    let linker = make_linker(&engine)?;
    let mut store = Store::new(&engine, HostData::default());
    let instance = linker.instantiate(&mut store, &module)?;
    for _ in 0..1000 {
        let life = instance.get_typed_func::<(), i32>(&mut store, "life")?;
        let _ = life.call(&mut store, ())?;
    }
    let n = 100_000u64;
    let start = Instant::now();
    for _ in 0..n {
        let life = instance.get_typed_func::<(), i32>(&mut store, "life")?;
        let _ = life.call(&mut store, ())?;
    }
    Ok(start.elapsed().as_nanos() as u64 / n)
}

/// hook相对直接调用的额外开销（纳秒/次）= 真正的"加hook"代价
#[allow(dead_code)] // 供tests::hook_overhead_is_small使用
fn hook_overhead_delta_nanos() -> Result<u64> {
    let hook = hook_call_cost_nanos()?;
    let direct = direct_call_cost_nanos()?;
    Ok(hook.saturating_sub(direct))
}

fn main() -> Result<()> {
    println!("==== [路线C] Host Hook拦截 ====");
    println!("[1] 未attack hook life() = {} (期望1)", life_via_hook_normal()?);
    println!(
        "[2] attack后 hook life() = {} (期望0=host层篡改)",
        life_via_hook_after_attack()?
    );
    let (a, b) = per_instance_hook_isolation()?;
    println!("[3] attack A后 (A, B) = ({}, {}) (期望(0,1)=单实例隔离)", a, b);
    println!(
        "[4] attack后 内存读出 = {} (期望42=内存保留)",
        memory_preserved_with_hook()?
    );
    let hook_ns = hook_call_cost_nanos()?;
    let direct_ns = direct_call_cost_nanos()?;
    println!(
        "[5] hook调用 {}ns/次 vs 直接调用 {}ns/次（额外开销约{}ns/次）",
        hook_ns,
        direct_ns,
        hook_ns.saturating_sub(direct_ns)
    );
    println!("==== 结论：Host Hook可行（最简单，host层完全可控，不改WASM）====");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_normal_returns_real_life() {
        // 未attack时，hook透传真实life() -> 返回1
        assert_eq!(life_via_hook_normal().unwrap(), 1, "未attack时hook应返回真实life()=1");
    }

    #[test]
    fn hook_after_attack_returns_false() {
        // attack设置覆写标记后，host层拦截 -> 返回0（WASM life()仍是1，但被篡改）
        assert_eq!(
            life_via_hook_after_attack().unwrap(),
            0,
            "attack后hook应返回0(false)"
        );
    }

    #[test]
    fn hook_is_per_instance() {
        // attack实例A不影响实例B：(A的hook, B的hook) = (0, 1)
        assert_eq!(
            per_instance_hook_isolation().unwrap(),
            (0, 1),
            "hook应按实例隔离，attack A不影响 B"
        );
    }

    #[test]
    fn memory_state_survives_hook_attack() {
        // attack只改host标记，不动WASM实例 -> 内存应仍读出42
        assert_eq!(
            memory_preserved_with_hook().unwrap(),
            42,
            "host hook不应影响Linear Memory，应仍读出42"
        );
    }

    #[test]
    fn hook_overhead_is_small() {
        // hook相对直接调用的额外开销应很小（<1us），证明"所有export加hook"代价可接受
        let delta = hook_overhead_delta_nanos().unwrap();
        assert!(delta < 1_000, "hook额外开销应小于1us，实际={}ns", delta);
    }
}
