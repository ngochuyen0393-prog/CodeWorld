// 路线A：整模块替换
//
// 原理：attack成功后，框架销毁目标旧WASM实例，
// 用攻击者提供的payload编译新模块并实例化，替换上去。
//
// 本文件是技术spike验证程序，验证四个点：
//   1. 能否在wasmtime中销毁旧Instance并创建新Instance替换
//   2. 旧实例的Linear Memory状态是否丢失
//   3. 新模块能否复用旧模块的import绑定
//   4. 性能开销（销毁+重建的时间）

use anyhow::Result;
use std::time::Instant;
use wasmtime::{Caller, Engine, Instance, Linker, Module, Store};

// === 受害者模块 ===
// 导出 life() -> 1(true)；导出 mem 线性内存；
// 导出 set_state/get_state 读写内存首字节，用于验证状态保留与否；
// import env.log 用于验证import绑定复用。
const VICTIM_WAT: &str = r#"
(module
  (import "env" "log" (func $log (param i32)))
  (memory (export "mem") 1)
  (func (export "set_state") (param $v i32)
    i32.const 0
    local.get $v
    i32.store
    local.get $v
    call $log)
  (func (export "get_state") (result i32)
    i32.const 0
    i32.load)
  (func (export "life") (result i32)
    i32.const 1))
"#;

// === 攻击者payload模块 ===
// 同样 import env.log、导出 mem 与 life，但 life() -> 0(false)。
// 这就是"attack后替换上去的整模块"。
const ATTACKER_WAT: &str = r#"
(module
  (import "env" "log" (func $log (param i32)))
  (memory (export "mem") 1)
  (func (export "get_state") (result i32)
    i32.const 0
    i32.load)
  (func (export "life") (result i32)
    i32.const 0))
"#;

// Store的数据：env.log被调用的次数（验证import实际接线）
type HostData = u64;

/// 构造Linker，提供 env.log 这个import绑定。
/// 同一个Linker可被受害者和攻击者模块复用（验证点3）。
fn make_linker(engine: &Engine) -> Result<Linker<HostData>> {
    let mut linker = Linker::new(engine);
    // env.log：每次调用把Store内计数器+1，用于验证import被实际调用
    linker.func_wrap("env", "log", |mut caller: Caller<HostData>, _v: i32| {
        *caller.data_mut() += 1;
    })?;
    Ok(linker)
}

/// 调用实例的 life() -> i32
fn call_life(store: &mut Store<HostData>, instance: &Instance) -> Result<i32> {
    let life = instance.get_typed_func::<(), i32>(&mut *store, "life")?;
    life.call(&mut *store, ())
}

/// 写入实例Linear Memory首4字节（模拟角色保存状态）
fn call_set_state(store: &mut Store<HostData>, instance: &Instance, v: i32) -> Result<()> {
    let f = instance.get_typed_func::<i32, ()>(&mut *store, "set_state")?;
    f.call(&mut *store, v)?;
    Ok(())
}

/// 读取实例Linear Memory首4字节
fn call_get_state(store: &mut Store<HostData>, instance: &Instance) -> Result<i32> {
    let f = instance.get_typed_func::<(), i32>(&mut *store, "get_state")?;
    f.call(&mut *store, ())
}

/// 把WAT文本编译成wasmtime模块
fn compile(engine: &Engine, wat_text: &str) -> Result<Module> {
    let wasm = wat::parse_bytes(wat_text.as_bytes())?;
    Module::new(engine, &wasm)
}

/// 受害者刚实例化时 life() 的返回值
fn victim_life_value() -> Result<i32> {
    let engine = Engine::default();
    let module = compile(&engine, VICTIM_WAT)?;
    let linker = make_linker(&engine)?;
    let mut store = Store::new(&engine, 0u64);
    let instance = linker.instantiate(&mut store, &module)?;
    call_life(&mut store, &instance)
}

/// 模拟attack：销毁受害者旧Store/实例，用攻击者模块重建，返回新实例的life()
fn life_after_attack() -> Result<i32> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?;

    // 1. 受害者实例化（一个Store代表一个实体沙箱）
    let victim_mod = compile(&engine, VICTIM_WAT)?;
    {
        let mut vstore = Store::new(&engine, 0u64);
        let victim = linker.instantiate(&mut vstore, &victim_mod)?;
        // attack前确认受害者还活着
        let _before = call_life(&mut vstore, &victim)?;
    } // vstore与victim在此drop -> 旧实例及其Linear Memory被销毁

    // 2. 用攻击者模块重建（同一Linker -> 复用旧import绑定，验证点3）
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;
    let mut astore = Store::new(&engine, 0u64);
    let attacker = linker.instantiate(&mut astore, &attacker_mod)?;

    // 3. 新实例 life() 已被覆写为0(false)
    call_life(&mut astore, &attacker)
}

/// 验证内存状态丢失：受害者写入42，attack替换后，新实例读出0（旧状态未迁移）
fn memory_after_attack() -> Result<i32> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?;
    let victim_mod = compile(&engine, VICTIM_WAT)?;

    // 1. 受害者写入状态42到Linear Memory
    {
        let mut vstore = Store::new(&engine, 0u64);
        let victim = linker.instantiate(&mut vstore, &victim_mod)?;
        call_set_state(&mut vstore, &victim, 42)?;
        // 确认写入成功
        let _written = call_get_state(&mut vstore, &victim)?;
    } // drop -> 旧内存（含42）随Store一起销毁

    // 2. 攻击者模块重建（全新Linear Memory）
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;
    let mut astore = Store::new(&engine, 0u64);
    let attacker = linker.instantiate(&mut astore, &attacker_mod)?;

    // 3. 新实例内存是全新的，首4字节为0 -> 旧状态未迁移
    call_get_state(&mut astore, &attacker)
}

/// 验证import绑定复用：同一个Linker同时为受害者和攻击者模块提供env.log
fn attacker_reuses_imports() -> Result<bool> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?; // 同一个Linker，构造一次
    let victim_mod = compile(&engine, VICTIM_WAT)?;
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;

    // 受害者用该Linker实例化
    let mut vstore = Store::new(&engine, 0u64);
    let victim = linker.instantiate(&mut vstore, &victim_mod)?;
    // set_state内部会call $log -> 验证env.log这条import确实接线并被调用
    call_set_state(&mut vstore, &victim, 7)?;
    let victim_log_calls = *vstore.data();

    // 攻击者也用同一Linker实例化（env.log绑定直接复用，无需重新定义）
    let mut astore = Store::new(&engine, 0u64);
    let attacker = linker.instantiate(&mut astore, &attacker_mod)?;
    let _ = call_life(&mut astore, &attacker)?;

    // 复用成立：两模块均被同一Linker成功实例化 + victim的import被实际调用
    Ok(victim_log_calls >= 1)
}

/// 测量"销毁旧实例+重建新实例"的耗时（纳秒），模块预编译不计入
fn rebuild_cost_nanos() -> Result<u64> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?;
    let victim_mod = compile(&engine, VICTIM_WAT)?;
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;

    // 预热几次，避免首次实例化/JIT开销污染测量
    for _ in 0..3 {
        let mut s = Store::new(&engine, 0u64);
        let _ = linker.instantiate(&mut s, &victim_mod)?;
    }

    let start = Instant::now();
    {
        // 销毁：创建受害者实例并随Store一起drop
        let mut vstore = Store::new(&engine, 0u64);
        let _victim = linker.instantiate(&mut vstore, &victim_mod)?;
    }
    // 重建：用攻击者模块创建新实例
    let mut astore = Store::new(&engine, 0u64);
    let _attacker = linker.instantiate(&mut astore, &attacker_mod)?;
    Ok(start.elapsed().as_nanos() as u64)
}

fn main() -> Result<()> {
    println!("==== [路线A] 整模块替换 ====");
    println!("[1] victim life() 初始 = {} (期望1=true)", victim_life_value()?);
    println!("[2] attack后 life() = {} (期望0=false)", life_after_attack()?);
    println!("[3] attack后 内存读出 = {} (期望0=旧状态丢失)", memory_after_attack()?);
    println!(
        "[4] 同一Linker复用import = {} (期望true)",
        attacker_reuses_imports()?
    );
    let cost_ns = rebuild_cost_nanos()?;
    println!("[5] 销毁+重建耗时 = {}ns ({:.3}ms)", cost_ns, cost_ns as f64 / 1_000_000.0);
    println!("==== 结论：整模块替换可行（wasmtime原生支持，但旧内存状态丢失）====");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn victim_life_initially_returns_true() {
        // 受害者模块刚实例化时，life()必须返回1(true)
        assert_eq!(victim_life_value().unwrap(), 1, "victim life() 初始应返回 1(true)");
    }

    #[test]
    fn victim_memory_round_trips() {
        // 校验set_state/get_state确实正确读写内存（顺带防止i32.store栈顺序回归）
        let engine = wasmtime::Engine::default();
        let module = compile(&engine, VICTIM_WAT).unwrap();
        let linker = make_linker(&engine).unwrap();
        let mut store = wasmtime::Store::new(&engine, 0u64);
        let instance = linker.instantiate(&mut store, &module).unwrap();
        call_set_state(&mut store, &instance, 42).unwrap();
        assert_eq!(
            call_get_state(&mut store, &instance).unwrap(),
            42,
            "set_state(42)后get_state应读出42"
        );
    }

    #[test]
    fn attack_replaces_life_to_false() {
        // attack后用攻击者模块替换，life()必须变为0(false) -> 击杀成功
        assert_eq!(
            life_after_attack().unwrap(),
            0,
            "attack后life()应返回0(false)"
        );
    }

    #[test]
    fn attack_loses_memory_state() {
        // 受害者写入状态42，attack替换后新实例的内存是全新的，读出0
        // 证明旧Linear Memory状态丢失（路线A的固有特性）
        assert_eq!(
            memory_after_attack().unwrap(),
            0,
            "attack后旧内存状态应丢失，新实例读出0"
        );
    }

    #[test]
    fn attacker_reuses_import_bindings() {
        // 同一Linker既实例化受害者又实例化攻击者，且env.log确实被调用
        assert!(
            attacker_reuses_imports().unwrap(),
            "新模块应能复用旧模块的import绑定"
        );
    }

    #[test]
    fn rebuild_cost_is_nonzero() {
        // 销毁+重建必须有可测量的开销（且合理：小于100ms）
        let cost_ns = rebuild_cost_nanos().unwrap();
        assert!(cost_ns > 0, "重建开销应大于0");
        assert!(cost_ns < 100_000_000, "重建开销应小于100ms，实际={}ns", cost_ns);
    }
}
