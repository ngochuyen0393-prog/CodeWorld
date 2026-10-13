//! WASM 沙箱管理：SandboxManager/SandboxHandle/SandboxData
//!
//! 架构文档六节（WASM 沙箱设计）+ 6.3（安全防线）。
//! 基于 wasmtime，启用 fuel 指令计量。spike/route_a 已验证 API。

use anyhow::Result;
use wasmtime::{Caller, Config, Engine, Instance, Linker, Module, Store};

use crate::attack::PendingAttack;
use codeworld_core::entity::EntityId;
use codeworld_core::pending::PendingChange;

/// attack payload 最大字节数（1MB，架构 4.2 技术保护）
///
/// import 层拒绝超限请求：防一次性拷贝大块线性内存（最大可到 64MB）+
/// 超大模块编译长时间阻塞 tick（编译不受 fuel 约束）。
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

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
    /// 本沙箱实体的坐标快照（host 每 tick 阶段一执行玩家代码【前】填充；
    /// import 回调拿不到 world，玩家代码经 env.get_pos 读到的是 tick 开始时的状态，
    /// 与架构 4.1"玩家代码读取的是 tick 开始时的快照状态"一致）
    pub pos: (f64, f64),
    /// 最近敌人的坐标快照（host 每 tick 阶段一【前】填充；None=世界没有其他存活实体，
    /// env.scan 返回 0）。MVP 感知=屏幕可见层上帝视角单敌人（架构 3.2 感知接口的最简版）。
    pub nearest_enemy: Option<(f64, f64)>,
    /// 本 tick 排队的移动请求（env.move_by 推入 PendingChange::SetPos，
    /// 阶段二收集后并入主 pending 队列统一应用，与攻击排队同模式）
    pub pending_moves: Vec<PendingChange>,
}

impl SandboxData {
    /// 创建空沙箱数据
    pub fn new() -> Self {
        Self::default()
    }
}

/// 沙箱句柄（持有 Store + Instance）
#[derive(Debug)]
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
    ///   - len 超过 MAX_PAYLOAD_LEN（1MB）直接拒绝：不入队、不拷贝内存
    ///     （防大块拷贝 + 超大模块编译阻塞 tick，编译不受 fuel 约束）。
    ///   - 返回 1=已排队，0=拒绝（超限）。
    fn make_linker(engine: &Engine) -> Result<Linker<SandboxData>> {
        let mut linker = Linker::new(engine);
        // env.log(v)：玩家代码日志（v 为玩家自定义 marker，含义见各生物 WAT 注释）。
        // 记 api_calls 元数据 + tracing::info 输出到 stderr（战斗节点可见性）。
        linker.func_wrap("env", "log", |mut caller: Caller<SandboxData>, v: i32| {
            caller.data_mut().api_calls.push("log".to_string());
            tracing::info!(
                entity_id = %caller.data().entity_id.0,
                marker = v,
                "[player_log] 玩家代码日志"
            );
        })?;
        // env.get_pos() -> (f64, f64)：读自己坐标（架构 3.2 空间接口）。
        // 多值返回用元组（wasmtime multi-value）；数据来自 host 填充的坐标快照。
        linker.func_wrap("env", "get_pos", |mut caller: Caller<SandboxData>| -> (f64, f64) {
            caller.data_mut().api_calls.push("get_pos".to_string());
            let (x, y) = caller.data().pos;
            tracing::debug!(
                entity_id = %caller.data().entity_id.0,
                x,
                y,
                "[get_pos] 玩家代码读取自身坐标"
            );
            (x, y)
        })?;
        // env.scan(x_ptr, y_ptr) -> i32：感知接口 MVP（架构 3.2 感知接口最简版，
        // 文档完整版为 get_visible_entities 返回可见实体列表；MVP 单敌人=最近者）。
        // 有敌人：把最近敌人坐标写进 caller memory（x 8字节@x_ptr、y 8字节@y_ptr），
        // 返回 1；无敌人或写入指针非法：不写 memory，返回 0（玩家代码自判）。
        linker.func_wrap(
            "env",
            "scan",
            |mut caller: Caller<SandboxData>, x_ptr: i32, y_ptr: i32| -> i32 {
                caller.data_mut().api_calls.push("scan".to_string());
                let entity = caller.data().entity_id;
                let Some(enemy) = caller.data().nearest_enemy else {
                    tracing::debug!(entity_id = %entity.0, "[scan] 无敌人（世界无其他存活实体）");
                    return 0;
                };
                // 原子语义预检：两个指针先全量校验（只查不写），全部合法才落笔，
                // 避免其一非法时另一半坐标已写入 memory 的半截副作用
                let ptrs_ok = can_write_f64_to_caller(&mut caller, x_ptr)
                    && can_write_f64_to_caller(&mut caller, y_ptr);
                if !ptrs_ok {
                    tracing::warn!(
                        entity_id = %entity.0,
                        x_ptr,
                        y_ptr,
                        "[scan] 写入指针非法，扫描作废"
                    );
                    return 0;
                }
                write_f64_to_caller(&mut caller, x_ptr, enemy.0);
                write_f64_to_caller(&mut caller, y_ptr, enemy.1);
                tracing::info!(
                    entity_id = %entity.0,
                    enemy_x = enemy.0,
                    enemy_y = enemy.1,
                    "[scan] 锁定最近敌人"
                );
                1
            },
        )?;
        // env.move_by(dx, dy) -> i32：相对移动（任务书指定；架构 3.2 文档为绝对 set_pos，
        // MVP 取相对语义——目标点由 host 侧用坐标快照计算，玩家代码无需自己维护位置）。
        // 合法 -> 排队 PendingChange::SetPos 进 pending_moves（阶段二统一应用，与攻击排队
        // 同模式，不即时改坐标）；拒绝 -> 返回 0 不入队。拒绝条件：
        //   - dx/dy 非有限（NaN/无穷，WASM 浮点除零可产生）：防污染世界状态
        //   - 目标点非有限（有限+有限可上溢为无穷）或 y < 0（钻地，默认物理地面 y=0）
        // 注：同 tick 多次 move_by 各自从同一快照算绝对目标，阶段二按序应用后
        // 仅最后一次生效（非增量累加）；正常用法每 tick 只 move 一次。
        linker.func_wrap(
            "env",
            "move_by",
            |mut caller: Caller<SandboxData>, dx: f64, dy: f64| -> i32 {
                caller.data_mut().api_calls.push("move_by".to_string());
                let entity = caller.data().entity_id;
                let (x, y) = caller.data().pos;
                if !dx.is_finite() || !dy.is_finite() {
                    tracing::warn!(
                        entity_id = %entity.0,
                        dx,
                        dy,
                        "[move_by] 偏移量非有限，拒绝入队"
                    );
                    return 0;
                }
                let target = (x + dx, y + dy);
                // 目标点有限性守卫：dx/dy 各自有限不代表和有限——有限+有限按 IEEE 754
                // 可上溢为无穷（如 x=1e308 再 +1e308），非有限坐标入队会污染世界状态
                //（感知算出 NaN、attack_at 坐标相等判定永失配）
                if !target.0.is_finite() || !target.1.is_finite() {
                    tracing::warn!(
                        entity_id = %entity.0,
                        target_x = target.0,
                        target_y = target.1,
                        "[move_by] 目标点非有限，拒绝入队"
                    );
                    return 0;
                }
                if target.1 < 0.0 {
                    tracing::warn!(
                        entity_id = %entity.0,
                        target_y = target.1,
                        "[move_by] 目标钻地（y<0），拒绝入队"
                    );
                    return 0;
                }
                caller
                    .data_mut()
                    .pending_moves
                    .push(PendingChange::SetPos {
                        entity,
                        new_pos: target,
                    });
                tracing::info!(
                    entity_id = %entity.0,
                    from = ?(x, y),
                    to = ?target,
                    "[move_by] 移动已排队（阶段二应用）"
                );
                1
            },
        )?;
        linker.func_wrap(
            "env",
            "attack_at",
            |mut caller: Caller<SandboxData>, x: f64, y: f64, ptr: i32, len: i32| -> i32 {
                let attacker = caller.data().entity_id;
                // 超限拒绝：在读 memory 之前拦下（避免无谓拷贝），返回 0 告知玩家代码
                if len > 0 && len as usize > MAX_PAYLOAD_LEN {
                    tracing::warn!(
                        attacker = %attacker.0,
                        actual_len = len,
                        max = MAX_PAYLOAD_LEN,
                        "[attack_at] payload 超限，拒绝入队"
                    );
                    return 0;
                }
                let payload = read_payload_from_caller(&mut caller, ptr, len);
                let payload_len = payload.len(); // 实际读取到的字节数（非调用方声称的 len）
                caller.data_mut().pending_attacks.push(PendingAttack {
                    attacker,
                    target_pos: (x, y),
                    payload,
                });
                tracing::info!(
                    attacker = %attacker.0,
                    x,
                    y,
                    payload_len,
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

    /// 替换沙箱（路线A：先建新、成功才拆旧；失败时旧沙箱原样带回）
    ///
    /// P0 僵尸实体修复：旧实现先 drop(handle) 再 Module::new，payload 非法时编译失败
    /// -> 旧沙箱已销毁、新沙箱未建，实体失去沙箱变成僵尸（永不可被攻击、代码永不运行）。
    /// 改为先建后拆：编译/实例化失败时旧沙箱随 Err 原样返回，调用方放回映射表即可，
    /// "存活实体必有沙箱"不变量不破。
    /// fuel 语义：新沙箱配额由调用方传入（attack 路径传旧沙箱剩余量，防反复被攻击回满刷配额）。
    pub fn replace_sandbox(
        &self,
        handle: SandboxHandle,
        new_wasm_bytes: &[u8],
        fuel: u64,
    ) -> std::result::Result<SandboxHandle, (SandboxHandle, anyhow::Error)> {
        // 建新前读出 entity_id，传给新沙箱（攻击替换后仍是同一实体）
        let entity_id = handle.store.data().entity_id;
        match self.create_sandbox(new_wasm_bytes, fuel, entity_id) {
            Ok(new_handle) => {
                // 成功才拆旧。旧沙箱若还有排队未处理的攻击请求，随替换丢弃（warn 提示）。
                let stale = handle.store.data().pending_attacks.len();
                if stale > 0 {
                    tracing::warn!(
                        entity_id = %entity_id.0,
                        count = stale,
                        "旧沙箱仍有未处理攻击请求，随替换丢弃"
                    );
                }
                drop(handle);
                tracing::debug!(entity_id = %entity_id.0, "沙箱替换（路线A，先建后拆）");
                Ok(new_handle)
            }
            Err(e) => {
                tracing::debug!(
                    entity_id = %entity_id.0,
                    error = %e,
                    "沙箱替换失败，旧沙箱保留"
                );
                Err((handle, e))
            }
        }
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
    // checked_add：32 位平台上 start+len 可能回绕成小值绕过越界检查（防御性修复）
    let Some(end) = start.checked_add(len as usize) else {
        tracing::warn!(
            ptr,
            len,
            "[read_payload] payload 读取地址回绕，置空"
        );
        return Vec::new();
    };
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

/// 检查向 caller 的 memory export 写入一个 f64 是否合法（只查不写）
///
/// env.scan 的双指针原子预检用：两个指针全部合法才落笔，避免半写入副作用。
fn can_write_f64_to_caller(caller: &mut Caller<SandboxData>, ptr: i32) -> bool {
    if ptr < 0 {
        return false;
    }
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    else {
        return false;
    };
    let Some(end) = (ptr as usize).checked_add(8) else {
        return false;
    };
    end <= memory.data(&*caller).len()
}

/// 向 caller 的 memory export 写入一个 f64（8字节小端）（env.scan 的敌人坐标输出）
///
/// 模块未导出 memory / ptr<0 / ptr+8 越界 -> false（写入失败，调用方返回 0 作废扫描）。
fn write_f64_to_caller(caller: &mut Caller<SandboxData>, ptr: i32, value: f64) -> bool {
    if ptr < 0 {
        return false;
    }
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    else {
        tracing::debug!("[write_f64] 模块未导出 memory，写入失败");
        return false;
    };
    let start = ptr as usize;
    // checked_add：防 32 位回绕绕过越界检查（与 read_payload 同款防御）
    let Some(end) = start.checked_add(8) else {
        tracing::warn!(ptr, "[write_f64] 写入地址回绕，写入失败");
        return false;
    };
    if end > memory.data(&*caller).len() {
        tracing::warn!(ptr, "[write_f64] 写入越界，写入失败");
        return false;
    }
    memory.data_mut(&mut *caller)[start..end].copy_from_slice(&value.to_le_bytes());
    true
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

    // === P0 僵尸实体修复：先建后拆 ===

    #[test]
    fn replace_sandbox_invalid_wasm_keeps_old_handle() {
        // P0 僵尸修复：非法 WASM payload 编译失败时，旧沙箱必须原样返回（随 Err 载荷），
        // 调用方放回映射表后"存活实体必有沙箱"不变量不破。
        // 旧实现先 drop(handle) 再 Module::new，失败后旧沙箱已销毁 -> 僵尸实体。
        let mgr = SandboxManager::new().unwrap();
        let owner = id(100);
        let handle = mgr.create_sandbox(&wasm(LIFE_TRUE_WAT), 1000, owner).unwrap();
        // 垃圾字节：非合法 WASM 魔数，Module::new 必失败
        let garbage: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        let Err((mut old_handle, _err)) = mgr.replace_sandbox(handle, garbage, 1000) else {
            panic!("非法 payload 应替换失败并带回旧沙箱");
        };
        // 旧沙箱原样可用：life() 仍是原模块的 true
        assert!(
            mgr.call_life(&mut old_handle).unwrap(),
            "失败后旧沙箱应保留且可继续调用"
        );
        // 旧沙箱 entity_id 身份不变
        assert_eq!(old_handle.store.data().entity_id, owner, "旧沙箱身份应不变");
    }

    // === P1：payload 大小上限（import 层拒绝超限）===

    /// 大内存攻击方 WAT：24 页 memory（约 1.5MB），on_tick 连调两次 attack_at——
    /// 第一次 len=上限（应排队），第二次 len=上限+1（应拒绝）
    const OVERSIZED_ATTACKER_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 24)
  (func (export "on_tick")
    f64.const 1.0 f64.const 2.0 i32.const 0 i32.const 1048576 call $attack_at drop
    f64.const 1.0 f64.const 2.0 i32.const 0 i32.const 1048577 call $attack_at drop)
  (func (export "life") (result i32) i32.const 1))"#;

    // === env.get_pos import（架构 3.2：读自己坐标）===

    /// 探针 WAT：on_tick 调 get_pos（多值返回 x,y），存入全局变量供 host 观测
    const GET_POS_PROBE_WAT: &str = r#"(module
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (global $gx (mut f64) (f64.const 0))
  (global $gy (mut f64) (f64.const 0))
  (func (export "on_tick")
    call $get_pos
    global.set $gy
    global.set $gx)
  (func (export "get_x") (result f64) global.get $gx)
  (func (export "get_y") (result f64) global.get $gy)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 从实体当前沙箱调用无参 f64 导出（测试观测探针全局变量）
    fn call_f64_export(handle: &mut SandboxHandle, name: &str) -> f64 {
        let func = handle
            .instance
            .get_typed_func::<(), f64>(&mut handle.store, name)
            .unwrap();
        func.call(&mut handle.store, ()).unwrap()
    }

    #[test]
    fn get_pos_import_returns_sandbox_pos() {
        // get_pos import：on_tick 调 get_pos 拿到 host 填充的自身坐标快照。
        // host 在阶段一前把实体坐标写进 SandboxData.pos（import 回调拿不到 world，
        // 快照模式与 attack_at 排队一致：玩家代码读 tick 开始时的状态）。
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(GET_POS_PROBE_WAT), 100_000, id(301))
            .unwrap();
        handle.store.data_mut().pos = (3.5, 7.25);
        mgr.call_on_tick(&mut handle).unwrap();
        let x = call_f64_export(&mut handle, "get_x");
        let y = call_f64_export(&mut handle, "get_y");
        assert_eq!(x, 3.5, "get_pos 应返回快照坐标 x");
        assert_eq!(y, 7.25, "get_pos 应返回快照坐标 y");
        assert!(
            handle.store.data().api_calls.contains(&"get_pos".to_string()),
            "get_pos 调用应记入 api_calls 元数据"
        );
    }

    // === env.scan import（架构 3.2 感知接口 MVP：最近敌人坐标写 memory）===

    /// 扫描方 WAT：on_tick 调 scan(0, 8)（敌人 x 写 memory[0..8]、y 写 [8..16]），
    /// 返回值存全局；导出读回函数供 host 观测
    const SCAN_PROBE_WAT: &str = r#"(module
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (global $found (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 0 i32.const 8 call $scan global.set $found)
  (func (export "get_found") (result i32) global.get $found)
  (func (export "get_enemy_x") (result f64) (f64.load (i32.const 0)))
  (func (export "get_enemy_y") (result f64) (f64.load (i32.const 8)))
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn scan_import_writes_enemy_pos_to_memory() {
        // scan：有敌人时把最近敌人坐标写入 caller memory（x@ptr0 8字节、y@ptr1 8字节），返回 1
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(SCAN_PROBE_WAT), 100_000, id(401))
            .unwrap();
        handle.store.data_mut().nearest_enemy = Some((-2.5, 9.75));
        mgr.call_on_tick(&mut handle).unwrap();
        let func_found = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_found")
            .unwrap();
        assert_eq!(func_found.call(&mut handle.store, ()).unwrap(), 1, "找到敌人应返回 1");
        let x = call_f64_export(&mut handle, "get_enemy_x");
        let y = call_f64_export(&mut handle, "get_enemy_y");
        assert_eq!(x, -2.5, "敌人 x 应写入 memory[0..8]");
        assert_eq!(y, 9.75, "敌人 y 应写入 memory[8..16]");
        assert!(
            handle.store.data().api_calls.contains(&"scan".to_string()),
            "scan 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn scan_import_returns_zero_when_no_enemy() {
        // 负面路径：世界没有其他存活实体（nearest_enemy=None）-> 返回 0、不写 memory
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(SCAN_PROBE_WAT), 100_000, id(402))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let func_found = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_found")
            .unwrap();
        assert_eq!(func_found.call(&mut handle.store, ()).unwrap(), 0, "无敌人应返回 0");
        let x = call_f64_export(&mut handle, "get_enemy_x");
        assert_eq!(x, 0.0, "无敌人不应写 memory（保持初始 0）");
    }

    #[test]
    fn scan_import_rejects_out_of_bounds_ptr() {
        // 负面路径：写入指针越界（memory 1 页=64KB，ptr=65536 越界）-> 返回 0 不 panic
        const SCAN_BAD_PTR_WAT: &str = r#"(module
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (global $found (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 65536 i32.const 8 call $scan global.set $found)
  (func (export "get_found") (result i32) global.get $found)
  (func (export "life") (result i32) i32.const 1))"#;
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(SCAN_BAD_PTR_WAT), 100_000, id(403))
            .unwrap();
        handle.store.data_mut().nearest_enemy = Some((1.0, 2.0));
        mgr.call_on_tick(&mut handle).unwrap();
        let func_found = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_found")
            .unwrap();
        assert_eq!(
            func_found.call(&mut handle.store, ()).unwrap(),
            0,
            "指针越界应返回 0（扫描作废）"
        );
    }

    // === env.move_by import（架构 3.2 空间接口 MVP：相对移动经 SetPos 队列）===

    /// 移动方 WAT：on_tick 调 move_by(2.0, 3.0)，返回值存全局
    const MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    f64.const 2.0 f64.const 3.0 call $move_by global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 地下移动 WAT：on_tick 调 move_by(0.0, -100.0)（从 y>=0 出发目标必钻地）
    const UNDERGROUND_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    f64.const 0.0 f64.const -100.0 call $move_by global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// NaN 移动 WAT：on_tick 调 move_by(NaN, 0.0)（除零/溢出可产生 NaN，防御性拒绝）
    const NAN_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    f64.const 0.0 f64.const 0.0 f64.div f64.const 0.0 call $move_by global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn move_by_import_queues_setpos_change() {
        // move_by：合法移动 -> 排队 PendingChange::SetPos（目标点=快照坐标+偏移），返回 1。
        // 不即时改坐标：与攻击排队同模式，阶段二统一应用（架构 4.1 PendingChanges）。
        let mgr = SandboxManager::new().unwrap();
        let mover_id = id(501);
        let mut handle = mgr
            .create_sandbox(&wasm(MOVER_WAT), 100_000, mover_id)
            .unwrap();
        handle.store.data_mut().pos = (10.0, 20.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let func_ret = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_ret")
            .unwrap();
        assert_eq!(func_ret.call(&mut handle.store, ()).unwrap(), 1, "合法移动应返回 1");
        let moves = &handle.store.data().pending_moves;
        assert_eq!(moves.len(), 1, "应排队 1 条移动请求");
        match &moves[0] {
            PendingChange::SetPos { entity, new_pos } => {
                assert_eq!(*entity, mover_id, "SetPos 应指向沙箱绑定的实体");
                assert_eq!(*new_pos, (12.0, 23.0), "目标点应为快照坐标+偏移");
            }
            other => panic!("应为 SetPos 变更，实际 {other:?}"),
        }
        assert!(
            handle.store.data().api_calls.contains(&"move_by".to_string()),
            "move_by 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn move_by_import_rejects_underground_target() {
        // 负面路径（移动越界）：目标 y < 0 钻地 -> 拒绝（返回 0，不入队）。
        // 默认物理地面 y=0，钻地移动是非法请求。
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(UNDERGROUND_MOVER_WAT), 100_000, id(502))
            .unwrap();
        handle.store.data_mut().pos = (5.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let func_ret = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_ret")
            .unwrap();
        assert_eq!(func_ret.call(&mut handle.store, ()).unwrap(), 0, "钻地移动应被拒绝");
        assert!(
            handle.store.data().pending_moves.is_empty(),
            "被拒绝的移动不应入队"
        );
    }

    #[test]
    fn move_by_import_rejects_non_finite_delta() {
        // 负面路径：dx/dy 非有限（NaN/无穷，WASM 除零可产生）-> 拒绝（返回 0，不入队），
        // 防止 NaN 坐标污染世界状态。
        // 构造：0.0/0.0 = NaN（WASM 浮点除零不 trap，产生 NaN）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(NAN_MOVER_WAT), 100_000, id(503))
            .unwrap();
        handle.store.data_mut().pos = (5.0, 5.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let func_ret = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_ret")
            .unwrap();
        assert_eq!(func_ret.call(&mut handle.store, ()).unwrap(), 0, "NaN 移动应被拒绝");
        assert!(
            handle.store.data().pending_moves.is_empty(),
            "被拒绝的移动不应入队"
        );
    }

    /// 大坐标移动 WAT：on_tick 调 move_by(1e308, 0.0)——host 侧 pos.x=1e308 时
    /// x+dx 按 IEEE 754 上溢为 +inf（有限+有限可上溢）
    const BIG_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    f64.const 1e308 f64.const 0.0 call $move_by global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn move_by_import_rejects_overflowed_target() {
        // P1（审查 1 轮）：dx/dy 各自有限，但 x+dx 上溢为 +inf -> 目标点非有限，
        // 必须拒绝（入队 SetPos(inf) 会污染世界状态：感知 NaN/攻击坐标永失配）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(BIG_MOVER_WAT), 100_000, id(504))
            .unwrap();
        handle.store.data_mut().pos = (1e308, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let func_ret = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_ret")
            .unwrap();
        assert_eq!(
            func_ret.call(&mut handle.store, ()).unwrap(),
            0,
            "上溢目标点应被拒绝"
        );
        assert!(
            handle.store.data().pending_moves.is_empty(),
            "上溢移动不应入队"
        );
    }

    #[test]
    fn scan_import_writes_nothing_when_y_ptr_invalid() {
        // P2（审查 1 轮）：y 指针非法时 x 也不应被写入（原子语义：全写或全不写），
        // 与注释"写入指针非法：不写 memory"一致
        const SCAN_BAD_Y_PTR_WAT: &str = r#"(module
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (global $found (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 0 i32.const 65536 call $scan global.set $found)
  (func (export "get_found") (result i32) global.get $found)
  (func (export "get_enemy_x") (result f64) (f64.load (i32.const 0)))
  (func (export "life") (result i32) i32.const 1))"#;
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(SCAN_BAD_Y_PTR_WAT), 100_000, id(404))
            .unwrap();
        handle.store.data_mut().nearest_enemy = Some((9.5, 2.5));
        mgr.call_on_tick(&mut handle).unwrap();
        let func_found = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, "get_found")
            .unwrap();
        assert_eq!(func_found.call(&mut handle.store, ()).unwrap(), 0, "y 指针非法应返回 0");
        let x = call_f64_export(&mut handle, "get_enemy_x");
        assert_eq!(x, 0.0, "y 指针非法时 x 不应被写入（保持初始 0）");
    }

    #[test]
    fn attack_at_import_rejects_oversized_payload() {
        // P1：len 超过 1MB 上限的攻击请求被 import 拒绝（不入队），不拷贝大块内存
        let mgr = SandboxManager::new().unwrap();
        let attacker_id = id(201);
        let mut handle = mgr
            .create_sandbox(&wasm(OVERSIZED_ATTACKER_WAT), 100_000, attacker_id)
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let attacks = &handle.store.data().pending_attacks;
        assert_eq!(attacks.len(), 1, "只有上限内的那一刀应入队");
        assert_eq!(
            attacks[0].payload.len(),
            crate::sandbox::MAX_PAYLOAD_LEN,
            "入队 payload 应恰为上限大小（len == 上限不超限）"
        );
    }
}
