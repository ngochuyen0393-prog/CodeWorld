// 路线B：funcref表间接调用
//
// 原理：所有export函数通过函数引用表（funcref table）间接调用，
// life()内部用 call_indirect 调用表项0。attack时修改表项0，
// 把它指向攻击者提供的函数，从而改变life()行为，且不改函数体、不丢内存。
//
// 验证点：
//   1. wasmtime是否支持运行时修改funcref table元素
//   2. 需要什么样的WASM模块结构（table + call_indirect）
//   3. 能否只替换表中的某一项而不影响其他函数
//   4. Linear Memory状态是否保留
//   5. 跨沙箱注入的可行性（关键限制点）

use anyhow::{anyhow, Result};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;
use wasmtime::{Caller, Engine, Instance, Linker, Module, Ref, Store};

type HostData = u64;

fn make_linker(engine: &Engine) -> Result<Linker<HostData>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap("env", "log", |mut caller: Caller<HostData>, _v: i32| {
        *caller.data_mut() += 1;
    })?;
    Ok(linker)
}

fn compile(engine: &Engine, wat_text: &str) -> Result<Module> {
    let wasm = wat::parse_bytes(wat_text.as_bytes())?;
    Module::new(engine, &wasm)
}

fn call_life(store: &mut Store<HostData>, instance: &Instance) -> Result<i32> {
    let life = instance.get_typed_func::<(), i32>(&mut *store, "life")?;
    life.call(&mut *store, ())
}

fn call_get_state(store: &mut Store<HostData>, instance: &Instance) -> Result<i32> {
    let f = instance.get_typed_func::<(), i32>(&mut *store, "get_state")?;
    f.call(&mut *store, ())
}

fn call_set_state(store: &mut Store<HostData>, instance: &Instance, v: i32) -> Result<()> {
    let f = instance.get_typed_func::<i32, ()>(&mut *store, "set_state")?;
    f.call(&mut *store, v)?;
    Ok(())
}

/// 调用 life_via(idx)：经call_indirect调用vtable[idx]，用于验证"只改一项"
fn call_life_via(store: &mut Store<HostData>, instance: &Instance, idx: i32) -> Result<i32> {
    let f = instance.get_typed_func::<i32, i32>(&mut *store, "life_via")?;
    f.call(&mut *store, idx)
}

// === 受害者模块 ===
// vtable 是 funcref 表，表项0是life()的真正实现。
// life() 经 call_indirect 调用表项0；life_via(i) 调用表项i（用于验证"只改一项"）。
// 初始两项都指向 life_true(返回1)。
const VICTIM_WAT: &str = r#"
(module
  (import "env" "log" (func $log (param i32)))
  (type $life_t (func (result i32)))
  (table (export "vtable") 2 funcref)
  (memory (export "mem") 1)
  (func $life_true (result i32) i32.const 1)
  (elem (i32.const 0) $life_true $life_true)
  (func (export "life") (result i32)
    i32.const 0
    call_indirect (type $life_t))
  (func (export "life_via") (param $i i32) (result i32)
    local.get $i
    call_indirect (type $life_t))
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

// === 攻击者payload模块 ===
// 只导出一个 dead() -> 0。attack时把它的函数引用塞进受害者的vtable[0]。
const ATTACKER_WAT: &str = r#"
(module
  (func (export "dead") (result i32) i32.const 0))
"#;

fn victim_life_value() -> Result<i32> {
    let engine = Engine::default();
    let module = compile(&engine, VICTIM_WAT)?;
    let linker = make_linker(&engine)?;
    let mut store = Store::new(&engine, 0u64);
    let instance = linker.instantiate(&mut store, &module)?;
    call_life(&mut store, &instance)
}

/// 模拟attack：把攻击者的dead函数引用塞进受害者vtable[0]，返回新life()
fn life_after_table_attack() -> Result<i32> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?;
    let victim_mod = compile(&engine, VICTIM_WAT)?;
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;
    let mut store = Store::new(&engine, 0u64);

    // 受害者实例
    let victim = linker.instantiate(&mut store, &victim_mod)?;
    // 攻击者payload实例化在【同一个Store】，才能取其Func塞进受害者的表
    let attacker = linker.instantiate(&mut store, &attacker_mod)?;

    // attack前确认 life()=1
    let _before = call_life(&mut store, &victim)?;

    // 取攻击者的 dead 函数引用（类型 () -> i32，与vtable元素类型一致）
    let dead_func = attacker
        .get_func(&mut store, "dead")
        .ok_or_else(|| anyhow!("attacker未导出dead"))?;
    // 取受害者导出的vtable
    let vtable = victim
        .get_table(&mut store, "vtable")
        .ok_or_else(|| anyhow!("victim未导出vtable"))?;
    // attack核心：运行时改写funcref表项0，指向攻击者的dead
    vtable.set(&mut store, 0, Ref::Func(Some(dead_func)))?;

    // attack后 life() 经call_indirect走vtable[0]=dead -> 返回0(false)
    call_life(&mut store, &victim)
}

/// attack只改vtable[0]，vtable[1]应不受影响 -> life_via(1) 仍返回1
fn life_via_one_after_attack() -> Result<i32> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?;
    let victim_mod = compile(&engine, VICTIM_WAT)?;
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;
    let mut store = Store::new(&engine, 0u64);
    let victim = linker.instantiate(&mut store, &victim_mod)?;
    let attacker = linker.instantiate(&mut store, &attacker_mod)?;
    // attack只改vtable[0]
    let dead_func = attacker
        .get_func(&mut store, "dead")
        .ok_or_else(|| anyhow!("attacker未导出dead"))?;
    let vtable = victim
        .get_table(&mut store, "vtable")
        .ok_or_else(|| anyhow!("victim未导出vtable"))?;
    vtable.set(&mut store, 0, Ref::Func(Some(dead_func)))?;
    // vtable[1]未被动，仍指向life_true
    call_life_via(&mut store, &victim, 1)
}

/// 内存保留验证：attack前写42，attack后读出应仍为42（同一实例，内存未动）
fn memory_after_table_attack() -> Result<i32> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?;
    let victim_mod = compile(&engine, VICTIM_WAT)?;
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;
    let mut store = Store::new(&engine, 0u64);
    let victim = linker.instantiate(&mut store, &victim_mod)?;
    let attacker = linker.instantiate(&mut store, &attacker_mod)?;
    // attack前写42到Linear Memory
    call_set_state(&mut store, &victim, 42)?;
    // attack改表项（不动内存）
    let dead_func = attacker
        .get_func(&mut store, "dead")
        .ok_or_else(|| anyhow!("attacker未导出dead"))?;
    let vtable = victim
        .get_table(&mut store, "vtable")
        .ok_or_else(|| anyhow!("victim未导出vtable"))?;
    vtable.set(&mut store, 0, Ref::Func(Some(dead_func)))?;
    // attack后读内存 -> 仍42
    call_get_state(&mut store, &victim)
}

/// 跨沙箱注入实验：用另一Store(独立沙箱)的Func去改受害者vtable，
/// 返回实际结果描述（rejected(err)/rejected(panic)/allowed）。
fn cross_store_injection_outcome() -> Result<String> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?;
    let victim_mod = compile(&engine, VICTIM_WAT)?;
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;
    // 两个独立Store = 两个独立实体沙箱
    let mut store1 = Store::new(&engine, 0u64);
    let mut store2 = Store::new(&engine, 0u64);
    let victim = linker.instantiate(&mut store1, &victim_mod)?;
    let attacker = linker.instantiate(&mut store2, &attacker_mod)?;
    // dead来自store2（攻击者沙箱）
    let dead_func = attacker
        .get_func(&mut store2, "dead")
        .ok_or_else(|| anyhow!("attacker未导出dead"))?;
    let vtable = victim
        .get_table(&mut store1, "vtable")
        .ok_or_else(|| anyhow!("victim未导出vtable"))?;
    // 尝试把store2的Func塞进store1的表（用catch_unwind同时捕获Err和panic）
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        vtable.set(&mut store1, 0, Ref::Func(Some(dead_func)))
    }));
    let desc = match outcome {
        Ok(Ok(())) => "allowed: 跨Store注入竟成功（沙箱隔离隐患）".to_string(),
        Ok(Err(e)) => format!("rejected(err): 跨Store funcref注入被拒绝 - {e}"),
        Err(_) => "rejected(panic): wasmtime以panic方式拦截跨Store引用".to_string(),
    };
    Ok(desc)
}

/// 测量 table.set 改写单个表项的耗时（纳秒）
fn table_set_cost_nanos() -> Result<u64> {
    let engine = Engine::default();
    let linker = make_linker(&engine)?;
    let victim_mod = compile(&engine, VICTIM_WAT)?;
    let attacker_mod = compile(&engine, ATTACKER_WAT)?;
    let mut store = Store::new(&engine, 0u64);
    let victim = linker.instantiate(&mut store, &victim_mod)?;
    let attacker = linker.instantiate(&mut store, &attacker_mod)?;
    let vtable = victim
        .get_table(&mut store, "vtable")
        .ok_or_else(|| anyhow!("victim未导出vtable"))?;
    // 预热
    for _ in 0..100 {
        let f = attacker.get_func(&mut store, "dead").unwrap();
        vtable.set(&mut store, 0, Ref::Func(Some(f)))?;
    }
    let start = Instant::now();
    for _ in 0..1000 {
        let f = attacker.get_func(&mut store, "dead").unwrap();
        vtable.set(&mut store, 0, Ref::Func(Some(f)))?;
    }
    Ok(start.elapsed().as_nanos() as u64 / 1000)
}

fn main() -> Result<()> {
    println!("==== [路线B] funcref表间接调用 ====");
    println!("[1] victim life() 初始 = {} (期望1)", victim_life_value()?);
    println!("[2] attack改vtable[0]后 life() = {} (期望0)", life_after_table_attack()?);
    println!(
        "[3] attack后 life_via(1) = {} (期望1=只改了表项0)",
        life_via_one_after_attack()?
    );
    println!(
        "[4] attack后 内存读出 = {} (期望42=内存保留)",
        memory_after_table_attack()?
    );
    println!("[5] 跨Store注入 = {}", cross_store_injection_outcome()?);
    let cost = table_set_cost_nanos()?;
    println!("[6] table.set单次耗时 = {}ns ({:.3}us)", cost, cost as f64 / 1000.0);
    println!("==== 结论：funcref表机制可行，但跨沙箱注入被拒，需定制编译 ====");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn victim_life_initially_returns_true() {
        // life()经call_indirect调用vtable[0]，初始指向life_true -> 返回1
        assert_eq!(victim_life_value().unwrap(), 1, "victim life() 初始应返回 1(true)");
    }

    #[test]
    fn attack_flips_table_entry_to_false() {
        // attack把vtable[0]改成攻击者的dead函数，life()应返回0(false)
        assert_eq!(
            life_after_table_attack().unwrap(),
            0,
            "attack后life()应返回0(false)"
        );
    }

    #[test]
    fn only_targeted_table_entry_changes() {
        // attack只改vtable[0]，vtable[1]仍指向life_true -> life_via(1)应返回1
        assert_eq!(
            life_via_one_after_attack().unwrap(),
            1,
            "只应改vtable[0]，vtable[1]不受影响 -> life_via(1)仍为1"
        );
    }

    #[test]
    fn memory_preserved_across_attack() {
        // attack前写42到内存，attack只改表项不动内存 -> 读出应仍为42
        assert_eq!(
            memory_after_table_attack().unwrap(),
            42,
            "funcref表attack不应丢失Linear Memory状态，应仍读出42"
        );
    }

    #[test]
    fn cross_store_injection_is_rejected() {
        // 用另一Store(沙箱)的Func去改受害者vtable，wasmtime应拒绝（沙箱隔离）
        let desc = cross_store_injection_outcome().unwrap();
        assert!(
            desc.starts_with("rejected"),
            "跨Store的funcref注入应被拒绝，实际结果 = {desc}"
        );
    }
}
