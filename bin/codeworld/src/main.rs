//! codeworld 主程序：把 7 个 crate 串成可运行的 tick 循环
//!
//! 借鉴 Screeps 两阶段 tick 模型 + AzerothCore 管理器链 Update 模式 + Bevy 插件式组合。
//! 主程序只编排不实现逻辑：main() 负责 init/循环/信号，tick() 负责单 tick 的串行状态更新。
//! MVP 已接入玩家代码执行（阶段一，单线程逐个实体）；并行留待 Phase 2。
//!
//! 架构文档 4.1 节 MVP 简化版 tick 流程：
//! 1. time-engine: tick++
//! 2. 阶段一：玩家代码执行（单线程，调 on_tick + life，life()=false 标记死亡）
//! 3. 阶段二：状态更新（串行）
//!    a. physics.step(world)    b. 死亡清理    c. renderer.render(world)    d. 每100tick存快照
//! 4. sleep 到下一个 tick

mod config;

use anyhow::{Context, Result};
use codeworld_arena::attack::{PendingAttack, DEFAULT_KILL_WASM};
use codeworld_arena::sandbox::{SandboxHandle, SandboxManager};
use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
use codeworld_core::pending::PendingChange;
use codeworld_core::world::WorldState;
use codeworld_physics::default::{CollisionPair, DefaultPhysicsEngine};
use codeworld_render::{enter_terminal, exit_terminal, Renderer, Viewport};
use codeworld_storage::StorageEngine;
use codeworld_time::clock::TimeEngine;
use config::Config;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing_subscriber::filter::LevelFilter;

/// 实体-沙箱映射表（架构文档 4.1 阶段一）
///
/// 把 EntityId 映射到 SandboxHandle，tick 阶段一据此逐个实体执行玩家代码。
/// SandboxHandle 持有 wasmtime Store 不可 Clone，故用 HashMap 存唯一所有权。
struct EntitySandboxMap {
    manager: SandboxManager,
    sandboxes: HashMap<EntityId, SandboxHandle>,
}

impl EntitySandboxMap {
    /// 创建映射表（初始化 SandboxManager）
    fn new() -> Result<Self> {
        Ok(Self {
            manager: SandboxManager::new()?,
            sandboxes: HashMap::new(),
        })
    }

    /// 为实体创建沙箱
    fn create(&mut self, entity_id: EntityId, wasm_bytes: &[u8], fuel: u64) -> Result<()> {
        let handle = self.manager.create_sandbox(wasm_bytes, fuel, entity_id)?;
        self.sandboxes.insert(entity_id, handle);
        tracing::info!(entity_id = %entity_id.0, "沙箱创建");
        Ok(())
    }

    /// 移除实体沙箱（实体死亡时调用，销毁 WASM 实例释放内存）
    fn remove(&mut self, entity_id: &EntityId) {
        if let Some(handle) = self.sandboxes.remove(entity_id) {
            self.manager.destroy_sandbox(handle);
            tracing::debug!(entity_id = %entity_id.0, "沙箱销毁");
        }
    }

    /// 执行阶段一：逐个实体调 on_tick() + life()（MVP 单线程）
    /// 返回需要标记死亡的 EntityId 列表（world 借用中不可同时改，由 tick 统一 kill）
    fn run_player_code(&mut self, world: &WorldState) -> Vec<EntityId> {
        let mut to_kill = Vec::new();
        // 先收集 owned EntityId 再遍历，避免边遍历 world.entities 边改 self.sandboxes 的借用冲突
        let entity_ids: Vec<EntityId> = world.entities.keys().copied().collect();
        for entity_id in entity_ids {
            // 防御：只处理存活实体（架构 4.1"把所有存活实体放进任务队列"）。
            // 正常流程死亡实体已在上一 tick 阶段二 d 步移除，此为兜底，避免对残留死者跑玩家代码。
            let is_alive = world
                .entities
                .get(&entity_id)
                .map(|e| e.is_alive())
                .unwrap_or(false);
            if !is_alive {
                continue;
            }
            // 无沙箱的实体（如框架测试实体）跳过，不参与玩家代码执行
            let Some(handle) = self.sandboxes.get_mut(&entity_id) else {
                continue;
            };
            // 重置本 tick 指令配额（架构 4.1：每 tick 重置指令配额）。
            // MVP 指令计量靠 wasmtime fuel（consume_fuel），fuel 跨 tick 累计消耗会耗尽，
            // 导致 call_life 返回 Err 误杀实体；架构要求"超额不死亡、下次 tick 继续"，
            // 故每 tick 复位 fuel 到配额上限。
            // 注：Entity.instruction_guard（core 元数据）MVP 未接入 WASM 执行（consume 未被调用），
            //     实际计量靠 fuel；Phase 2 同步两者时再调 instruction_guard.reset()。
            // TODO(Phase2)：由 arena SandboxManager 封装 reset_fuel，避免主程序直连 wasmtime Store。
            let fuel_limit = handle.store.data().fuel_limit;
            if let Err(e) = handle.store.set_fuel(fuel_limit) {
                tracing::warn!(entity_id = %entity_id.0, error = %e, "fuel 重置失败，跳过本 tick");
                continue;
            }
            // 调 on_tick（没导出则 call_on_tick 内部跳过）
            if let Err(e) = self.manager.call_on_tick(handle) {
                tracing::warn!(entity_id = %entity_id.0, error = %e, "on_tick 执行失败");
            }
            // 调 life()：false 或调用失败均标记死亡
            match self.manager.call_life(handle) {
                Ok(true) => {} // 仍存活
                Ok(false) => {
                    tracing::info!(entity_id = %entity_id.0, "life()=false，标记死亡");
                    to_kill.push(entity_id);
                }
                Err(e) => {
                    tracing::warn!(entity_id = %entity_id.0, error = %e, "life() 调用失败，标记死亡");
                    to_kill.push(entity_id);
                }
            }
        }
        to_kill
    }

    /// 收集本 tick 所有沙箱排队的攻击请求（架构 4.2：阶段一 on_tick 调 env.attack_at 排队）
    ///
    /// drain 各沙箱 pending_attacks 后清空，保证不跨 tick 残留。在 tick 阶段二 a2 步调用。
    fn collect_pending_attacks(&mut self) -> Vec<PendingAttack> {
        let mut all = Vec::new();
        for handle in self.sandboxes.values_mut() {
            let data = handle.store.data_mut();
            all.append(&mut data.pending_attacks);
        }
        if !all.is_empty() {
            tracing::info!(count = all.len(), "[collect_pending_attacks] 收集攻击请求");
        }
        all
    }

    /// 对目标实体执行单次攻击（架构 4.2：on_attack 拦截 -> 路线A替换沙箱 -> 检查 life）
    ///
    /// - on_attack 返回 true -> 拦截，不替换、不击杀。
    /// - 未拦截 -> 用 payload 替换目标整个 WASM 模块（路线A）；payload 为空用 DEFAULT_KILL_WASM。
    /// - 替换后调 life()：false=击杀成功；调用失败视为击杀（破损沙箱等同死亡）。
    /// - 不处理掠夺：掠夺需改 world（survival），由调用方 process_attacks 在击杀后处理。
    fn attack_target(&mut self, target_id: &EntityId, payload: &[u8]) -> Result<AttackOutcome> {
        // 1. on_attack 拦截检查（旧沙箱）
        let intercepted = {
            let Some(handle) = self.sandboxes.get_mut(target_id) else {
                anyhow::bail!("目标 {:?} 无沙箱，无法攻击代码", target_id);
            };
            self.manager.call_on_attack(handle, payload)?
        };
        if intercepted {
            tracing::info!(target = %target_id.0, "[attack_target] 攻击被 on_attack 拦截");
            return Ok(AttackOutcome {
                intercepted: true,
                killed: false,
            });
        }
        // 2. 替换沙箱（路线A）：移出旧 handle -> replace（保留 entity_id）-> 插回
        let old = self
            .sandboxes
            .remove(target_id)
            .context("目标沙箱在 on_attack 后已不存在")?;
        let fuel = old.store.data().fuel_limit; // 沿用原指令配额
        let wasm_bytes: &[u8] = if payload.is_empty() {
            &DEFAULT_KILL_WASM
        } else {
            payload
        };
        let new_handle = self.manager.replace_sandbox(old, wasm_bytes, fuel)?;
        self.sandboxes.insert(*target_id, new_handle);
        tracing::info!(target = %target_id.0, "[attack_target] 目标沙箱已替换（路线A）");
        // 3. 检查 life()：false=击杀成功
        let killed = {
            let handle = self
                .sandboxes
                .get_mut(target_id)
                .context("替换后目标沙箱丢失")?;
            match self.manager.call_life(handle) {
                Ok(false) => true,
                Ok(true) => false,
                Err(e) => {
                    tracing::warn!(
                        target = %target_id.0,
                        error = %e,
                        "[attack_target] 替换后 life() 调用失败，视为击杀"
                    );
                    true
                }
            }
        };
        Ok(AttackOutcome {
            intercepted: false,
            killed,
        })
    }
}

/// 单次攻击结果（attack_target 返回，供 process_attacks 决定是否掠夺）
struct AttackOutcome {
    /// 是否被 on_attack 拦截
    intercepted: bool,
    /// 是否击杀成功（替换后 life()=false）
    killed: bool,
}

fn main() -> Result<()> {
    let config = Config::load();
    init_tracing(&config.log_level);
    tracing::info!(tick_ms = config.tick_interval_ms, db = %config.db_path, "CodeWorld 主程序启动");

    // 打开存储，尝试恢复快照；无快照则新建世界
    let mut storage = StorageEngine::open(&config.db_path)?;
    let mut world = match storage.load_latest_world()? {
        Some(w) => {
            tracing::info!(tick = w.tick(), entities = w.entities.len(), "从快照恢复世界");
            w
        }
        None => {
            tracing::info!("无快照，创建新世界");
            WorldState::new()
        }
    };

    // 初始化各引擎（架构文档 4.1：time/physics/render）
    let mut time_engine = TimeEngine::new();
    let physics = DefaultPhysicsEngine::new();
    let viewport = Viewport::new(
        config.viewport_center,
        config.viewport_half_width,
        config.viewport_half_height,
        config.term_rows,
        config.term_cols,
    );
    let mut renderer = Renderer::new(viewport);

    // 实体-沙箱映射表（架构 4.1 阶段一：玩家代码执行）
    // 注意：从快照恢复的实体只有元数据、无沙箱（WASM 实例不持久化），
    //       这些实体 run_player_code 会跳过；重新挂沙箱属 Phase 2 范畴。
    let mut sandbox_map = EntitySandboxMap::new()?;

    // 新世界或恢复后无实体：放测试实体（含 WASM 沙箱）让 MVP 有东西渲染、能跑玩家代码
    if world.entities.is_empty() {
        spawn_test_entities(&mut world, &mut sandbox_map);
    }

    // 信号处理：后台线程监听 stdin，按 Enter/EOF 置退出标志（零依赖，符合 MVP 最简方案）
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        r.store(false, Ordering::SeqCst);
    });

    // 延迟变更队列（架构文档 4.1 节阶段一产出、阶段二 a 统一应用）
    // 玩家代码执行期间产生的变更放入 pending，本 tick 阶段二统一应用后清空
    let mut pending: Vec<PendingChange> = Vec::new();

    tracing::info!("tick 循环开始（按 Enter 退出）");
    // 进入终端渲染模式：清屏 + 隐藏光标
    // stdout 专供 ANSI 画面，日志已走 stderr，进入后画面由 tick 第 e 步 flush 维护
    if let Err(e) = enter_terminal(&mut std::io::stdout().lock()) {
        tracing::warn!(error = %e, "进入终端渲染模式失败，继续以普通输出运行");
    }
    // panic 时尽力恢复终端（隐藏光标/颜色状态不能残留砖化终端），再交回默认 hook 打印 panic 信息
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = exit_terminal(&mut std::io::stdout().lock());
        default_panic_hook(info);
    }));
    while running.load(Ordering::SeqCst) {
        tick(
            &mut world,
            &mut time_engine,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        std::thread::sleep(config.tick_interval());
    }

    // 关闭：恢复终端 + 强制存快照（架构文档 4.1 收尾）
    tracing::info!("收到退出信号，正在保存世界快照...");
    let save_result = storage.force_save(&world);
    // 无论保存结果如何都恢复终端（避免残留隐藏光标/颜色状态）
    if let Err(e) = exit_terminal(&mut std::io::stdout().lock()) {
        tracing::warn!(error = %e, "恢复终端失败");
    }
    save_result?;
    tracing::info!(tick = world.tick(), entities = world.entities.len(), "主程序已关闭");
    Ok(())
}

/// 初始化 tracing 日志，级别从配置字符串读取（非法时回退 info）
///
/// 日志写 stderr：stdout 专供 ANSI 画面渲染，若日志走默认 stdout 会与画面交错破坏显示。
/// （日志走 stderr 可 `codeworld 2>logs.txt` 分离查看；同一 TTY 裸跑仍可能交错，MVP 限制）
fn init_tracing(log_level: &str) {
    let level = log_level.parse::<LevelFilter>().unwrap_or(LevelFilter::INFO);
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .init();
}

/// 单 tick 串行状态更新（架构文档 4.1 节 MVP 简化版）
///
/// 拆为独立函数以便单元测试（main() 含无限循环与信号处理不可测）。
fn tick(
    world: &mut WorldState,
    time_engine: &mut TimeEngine,
    physics: &DefaultPhysicsEngine,
    renderer: &mut Renderer,
    storage: &mut StorageEngine,
    pending: &mut Vec<PendingChange>,
    sandbox_map: &mut EntitySandboxMap,
) {
    // ① 时间推进（time-engine 计数 + 世界 tick 同步推进）
    let should_snapshot = time_engine.advance_tick();
    world.advance_tick();
    let current_tick = world.tick();

    // ② 阶段一：玩家代码执行（MVP 单线程，逐个实体）
    //    调每个实体沙箱的 on_tick() + life()；life()=false 或调用失败则标记死亡。
    //    玩家代码执行期间产生的变更放入 pending，阶段二 a 统一应用（架构 4.1）。
    //    run_player_code 只读 world、返回待杀 EntityId 列表；kill 在此统一做
    //    （world 在 run_player_code 中被借用，不可同时修改）。
    let to_kill = sandbox_map.run_player_code(world);
    for entity_id in &to_kill {
        if let Some(entity) = world.entities.get_mut(entity_id) {
            entity.kill();
        }
    }
    if !to_kill.is_empty() {
        tracing::info!(tick = current_tick, killed = to_kill.len(), "阶段一标记死亡");
    }
    //    注意：不在此处清空 pending--apply_pending_changes 的 drain(..) 会消费并清空队列，
    //    保证不跨 tick 残留（架构 4.1 "不立即应用、同 tick 统一应用"）。
    //    TODO(Phase2)：架构文档 4.1 要求生存点数检查在阶段一每个实体线程内、on_tick 之前
    //    执行（归零则不跑 on_tick）。当前 decay_survival 放在阶段二（任务要求），
    //    前移到阶段一需同步调整，否则将死的实体会多跑一个 tick 的 on_tick。

    // ③ 阶段二：状态更新（串行）
    //   a. 应用 PendingChanges（spawn/destroy/set_pos/set_velocity，架构 4.1 a 步）
    apply_pending_changes(world, pending);

    //   a2. 处理攻击（架构 4.2：阶段一 on_tick 调 env.attack_at 排队的请求在此统一执行）
    //      插在 apply_pending_changes 之后、decay_survival 之前：攻击产生的死亡先于每日衰减处理。
    //      import 函数只排队（无 world 访问权），实际 hit/miss/击杀/掠夺在此判断。
    let pending_attacks = sandbox_map.collect_pending_attacks();
    if !pending_attacks.is_empty() {
        process_attacks(world, sandbox_map, &pending_attacks);
    }

    //   b. 生存点数扣减（架构 4.1：每 86400 tick 扣 1 点，归零标记死亡）
    decay_survival(world, current_tick);

    //   c. 物理规则（重力/地面/速度/碰撞，架构 4.1 b 步）
    //      先检测碰撞（接触事件），再执行物理步骤（含碰撞解决），最后处理接触事件。
    //      在 step 之前检测：step 内部的 resolve_collision 会把重叠实体挤到 x+1，
    //      挤完坐标变了再检测就测不到接触，故接触事件必须在 step 之前捕获。
    //      （step 内部也会再 detect 一次用于 resolve，MVP 实体少可接受重复调用。）
    let collision_pairs = physics.detect_collisions(&world.entities);
    let changes = physics.step(world);
    if !changes.is_empty() {
        tracing::debug!(tick = current_tick, change_count = changes.len(), "物理变更");
    }
    //      接触触发沙箱通道（架构 4.1 b 步）：记录碰撞双方信息，MVP 仅日志
    //      TODO(Phase2)：让 step() 返回碰撞对，消除此处与 step 内部的重复 detect_collisions 调用
    let contact_count = handle_collisions(&collision_pairs, &world.entities);
    if contact_count > 0 {
        tracing::info!(tick = current_tick, contact_count, "接触事件处理");
    }

    //   d. 死亡清理：移除死亡实体 + 销毁对应沙箱（释放 WASM 实例内存）
    //      先收集死亡 EntityId 并销毁沙箱，再 retain 移除实体（retain 后无法再遍历死者）
    let dead_ids: Vec<EntityId> = world
        .entities
        .iter()
        .filter(|(_, e)| !e.is_alive())
        .map(|(id, _)| *id)
        .collect();
    for id in &dead_ids {
        sandbox_map.remove(id);
    }
    let before = world.entities.len();
    world.entities.retain(|_, e| e.is_alive());
    let removed = before - world.entities.len();
    if removed > 0 {
        tracing::info!(tick = current_tick, removed, "死亡实体清理");
    }

    //   e. 渲染输出（架构 4.1 e 步；有差异才 flush）
    let diffs = renderer.render(world);
    if !diffs.is_empty() {
        renderer.flush();
        tracing::debug!(tick = current_tick, diff_count = diffs.len(), "渲染输出");
    }

    //   f. 快照存储（每 100 tick，架构 4.1 d 步）
    if should_snapshot {
        match storage.save_world_snapshot(world) {
            Ok(saved_tick) if saved_tick > 0 => {
                tracing::info!(tick = current_tick, "快照已存储");
            }
            Ok(_) => {} // 间隔未到，跳过
            Err(e) => {
                tracing::warn!(tick = current_tick, error = %e, "快照存储失败");
            }
        }
    }

    // 每 10 tick 输出一次状态日志
    if current_tick.is_multiple_of(10) {
        tracing::info!(
            tick = current_tick,
            entities = world.entities.len(),
            "tick 运行中"
        );
    }
}

/// 应用延迟变更队列（架构文档 4.1 节阶段二 a 步）
///
/// 阶段一玩家代码执行期间产生的变更（spawn/destroy/set_pos/set_velocity）
/// 放入 pending 队列，此处统一应用到 WorldState。应用后队列清空。
fn apply_pending_changes(world: &mut WorldState, pending: &mut Vec<PendingChange>) {
    for change in pending.drain(..) {
        match change {
            PendingChange::Spawn { entity } => {
                tracing::info!(entity_id = %entity.id.0, "应用 Spawn");
                world.entities.insert(entity.id, entity);
            }
            PendingChange::Destroy { entity } => {
                tracing::info!(entity_id = %entity.0, "应用 Destroy");
                if let Some(e) = world.entities.get_mut(&entity) {
                    e.kill();
                }
            }
            PendingChange::SetPos { entity, new_pos } => {
                tracing::debug!(entity_id = %entity.0, ?new_pos, "应用 SetPos");
                if let Some(e) = world.entities.get_mut(&entity) {
                    e.set_pos(new_pos);
                }
            }
            PendingChange::SetVelocity {
                entity,
                new_velocity,
            } => {
                tracing::debug!(entity_id = %entity.0, ?new_velocity, "应用 SetVelocity");
                if let Some(e) = world.entities.get_mut(&entity) {
                    e.set_velocity(new_velocity);
                }
            }
        }
    }
}

/// 生存点数扣减（架构文档 4.1 节阶段二 b 步）
///
/// 遍历存活实体调用 survival.decay(current_tick)，归零则标记死亡。
/// 死亡清理步（retain is_alive）会随后移除死亡实体。
fn decay_survival(world: &mut WorldState, current_tick: u64) {
    for entity in world.entities.values_mut() {
        if !entity.is_alive() {
            continue;
        }
        let before = entity.survival.points();
        entity.survival.decay(current_tick);
        let after = entity.survival.points();
        if after < before {
            tracing::info!(
                entity_id = %entity.id.0,
                tick = current_tick,
                before,
                after,
                "生存点数扣减"
            );
        }
        if entity.survival.is_dead() {
            entity.kill();
            tracing::info!(
                entity_id = %entity.id.0,
                tick = current_tick,
                "生存点数归零，实体死亡"
            );
        }
    }
}

/// 处理阶段一排队的攻击请求（架构 4.2 + 4.1 阶段二 a2 步）
///
/// 对每个 PendingAttack：在坐标找存活实体（attack_at 攻击坐标不是 EntityId，目标移动
/// 可能 miss）-> attack_target（on_attack 拦截 / 路线A替换 / 检查 life）-> 击杀成功则
/// 击杀掠夺（从死者转移 1 生存点数给攻击者，自攻自跳过）并标记死亡（清理步 d 移除实体+销毁沙箱）。
///
/// 攻击在阶段二处理（不实时）：玩家代码调 import 只排队，实际击杀结果下一 tick 才能感知。
fn process_attacks(world: &mut WorldState, sandbox_map: &mut EntitySandboxMap, attacks: &[PendingAttack]) {
    for atk in attacks {
        // 找目标：坐标上的存活实体（find 返回第一个命中；HashMap 顺序随机，测试用唯一坐标避免歧义）
        let target_id = world
            .entities
            .values()
            .find(|e| e.is_alive() && e.pos() == atk.target_pos)
            .map(|e| e.id);
        let Some(target_id) = target_id else {
            tracing::info!(
                attacker = %atk.attacker.0,
                pos = ?atk.target_pos,
                "[process_attacks] 攻击落空（坐标无存活实体）"
            );
            continue;
        };
        // 执行攻击：on_attack 拦截 -> 替换沙箱 -> 检查 life
        let outcome = match sandbox_map.attack_target(&target_id, &atk.payload) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(
                    attacker = %atk.attacker.0,
                    target = %target_id.0,
                    error = %e,
                    "[process_attacks] 攻击执行失败"
                );
                continue;
            }
        };
        if outcome.intercepted {
            tracing::info!(
                attacker = %atk.attacker.0,
                target = %target_id.0,
                "[process_attacks] 攻击被拦截"
            );
            continue;
        }
        if outcome.killed {
            // 击杀掠夺：从死者转移 1 生存点数给攻击者（自攻自跳过转移但仍击杀）
            if atk.attacker != target_id {
                plunder_survival(world, target_id, atk.attacker, 1);
            } else {
                tracing::info!(
                    entity = %target_id.0,
                    "[process_attacks] 自攻自击杀，跳过掠夺"
                );
            }
            // 标记死亡（死亡清理步 d 移除实体 + 销毁沙箱）
            if let Some(e) = world.entities.get_mut(&target_id) {
                e.kill();
            }
            tracing::info!(
                attacker = %atk.attacker.0,
                target = %target_id.0,
                "[process_attacks] 击杀成功"
            );
        } else {
            tracing::info!(
                attacker = %atk.attacker.0,
                target = %target_id.0,
                "[process_attacks] 命中但未击杀（life=true）"
            );
        }
    }
}

/// 击杀掠夺：从 donor（死者）转移 n 生存点数给 recipient（攻击者）
///
/// 借用处理：transfer 需同时 &mut 两个 SurvivalPoints（同 HashMap 两个 key）。
/// 解法--临时移出 donor 到局部变量，get_mut recipient 做转移，再插回 donor
/// （自攻自由调用方跳过，不进本函数，donor_id != recipient_id）。
fn plunder_survival(world: &mut WorldState, donor_id: EntityId, recipient_id: EntityId, n: u64) {
    // 临时移出 donor 避免同时 &mut HashMap 两个 key
    let mut donor = match world.entities.remove(&donor_id) {
        Some(e) => e,
        None => {
            tracing::warn!(donor = %donor_id.0, "[plunder_survival] donor 不存在，跳过");
            return;
        }
    };
    let mut transferred = false;
    if let Some(recipient) = world.entities.get_mut(&recipient_id) {
        // transfer：self=donor（捐赠方/死者），to=recipient（攻击者）；不足返回 false 双方不变
        transferred = donor.survival.transfer(&mut recipient.survival, n);
    } else {
        tracing::warn!(recipient = %recipient_id.0, "[plunder_survival] recipient 不存在");
    }
    world.entities.insert(donor_id, donor);
    if transferred {
        tracing::info!(
            from = %donor_id.0,
            to = %recipient_id.0,
            n,
            "[plunder_survival] 击杀掠夺：生存点数转移"
        );
    } else {
        tracing::info!(
            from = %donor_id.0,
            to = %recipient_id.0,
            n,
            "[plunder_survival] 转移失败（点数不足或 recipient 不存在）"
        );
    }
}

/// 处理碰撞事件（架构文档 4.1 节 b 步：接触触发沙箱通道）
///
/// 架构 4.1：物理碰撞后，框架自动为碰撞双方开启沙箱通道，双方获得对方的
/// EntityId 和坐标；是否 probe/attack 由玩家代码决定，框架只负责开通道。
///
/// MVP：只记录日志（接触双方 EntityId + 重叠坐标），返回接触对数量便于测试断言。
/// 跳过含死亡实体的碰撞对：死亡实体本 tick 会被死亡清理步移除，不应为其开通道/
/// 记接触（阶段一 life()=false 或 decay_survival 归零 kill 的实体此时仍在
/// world.entities 中，detect_collisions 会扫到，需在此按 is_alive 过滤）。
/// Phase 2：通过 sandbox_map 为双方开启 probe/attack 通道，让玩家代码在下一 tick
/// 的 on_tick 中可以 probe/attack 对方（当前 MVP 的 on_tick 是空操作 WAT，无此能力）。
fn handle_collisions(pairs: &[CollisionPair], entities: &HashMap<EntityId, Entity>) -> usize {
    let mut count = 0;
    for pair in pairs {
        // 跳过含死亡实体的碰撞对（架构 4.1 接触触发针对存活实体间交互）
        let a_alive = entities.get(&pair.a).map(|e| e.is_alive()).unwrap_or(false);
        let b_alive = entities.get(&pair.b).map(|e| e.is_alive()).unwrap_or(false);
        if !a_alive || !b_alive {
            tracing::debug!(a = %pair.a.0, b = %pair.b.0, "跳过含死亡实体的碰撞对");
            continue;
        }
        tracing::debug!(
            a = %pair.a.0,
            b = %pair.b.0,
            pos = ?pair.pos,
            "接触事件：框架为碰撞双方开启沙箱通道"
        );
        count += 1;
    }
    count
}

/// 创建测试实体（让 MVP 有东西渲染）
fn create_test_entity(pos: (f64, f64), tick: u64) -> Entity {
    Entity::new(
        EntityId::new(),
        PlayerId::new(),
        "test".to_string(),
        pos,
        Appearance::new("@".to_string(), "red".to_string(), 1.0),
        tick,
        1_000_000, // 指令配额
    )
}

/// 生成测试实体塞入世界（让 MVP 有东西渲染、能跑玩家代码）
///
/// 两个实体分别置于 (0, 10) 与 (5, 20)，都不站地面（y=0），
/// 这样默认重力规则下能看到下落效果，日志比实体贴在地面更有信息量。
/// 每个实体同时创建 WASM 沙箱（PLAYER_WAT：life()=true + on_tick 调 log）。
fn spawn_test_entities(world: &mut WorldState, sandbox_map: &mut EntitySandboxMap) {
    let fuel = 1_000_000; // 100 万条指令配额
    for pos in [(0.0, 10.0), (5.0, 20.0)] {
        let e = create_test_entity(pos, world.tick());
        // 为实体创建沙箱；沙箱创建失败则不插入实体（避免无沙箱的孤儿实体）
        if sandbox_map.create(e.id, &wasm(PLAYER_WAT), fuel).is_ok() {
            tracing::info!(entity_id = %e.id.0, pos = ?e.pos(), "创建测试实体+沙箱");
            world.entities.insert(e.id, e);
        } else {
            tracing::warn!(entity_id = %e.id.0, "沙箱创建失败，跳过该测试实体");
        }
    }
}

/// WAT 文本 -> WASM 字节码（编译玩家代码模块）
fn wasm(wat_text: &str) -> Vec<u8> {
    wat::parse_bytes(wat_text.as_bytes())
        .expect("WAT 编译失败：检查 WAT 文本语法")
        .into_owned()
}

/// 玩家代码 WAT：life()=true + on_tick() 调 log（测试实体默认玩家代码）
const PLAYER_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_tick") i32.const 42 call $log))"#;

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{EntityId, PlayerId};
    use uuid::Uuid;

    /// 测试用视口（中心 0,0，半宽半高 50，30x30 终端）
    fn test_renderer() -> Renderer {
        Renderer::new(Viewport::new((0.0, 0.0), 50.0, 50.0, 30, 30))
    }

    /// 构造固定 ID 的实体（确定性，便于断言）
    fn make_entity(id_u: u128, pos: (f64, f64)) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            "test".to_string(),
            pos,
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    // === EntitySandboxMap / 阶段一玩家代码执行 ===

    /// 测试用沙箱映射表
    fn test_sandbox_map() -> EntitySandboxMap {
        EntitySandboxMap::new().unwrap()
    }

    /// 死亡玩家 WAT：life()=false
    const DEAD_PLAYER_WAT: &str = r#"(module
  (func (export "life") (result i32) i32.const 0))"#;

    #[test]
    fn run_player_code_calls_life_returns_false() {
        // 阶段一：life()=false 的实体被标记死亡（run_player_code 返回 to_kill 列表）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(id, &wasm(DEAD_PLAYER_WAT), 1_000_000).unwrap();
        let to_kill = map.run_player_code(&world);
        assert!(
            to_kill.contains(&id),
            "life()=false 的实体应在 to_kill 列表，实际 {:?}",
            to_kill
        );
    }

    #[test]
    fn run_player_code_calls_life_returns_true() {
        // 阶段一：life()=true 的实体不被标记死亡（to_kill 不含该实体）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(2));
        world.entities.insert(id, make_entity(2, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(id, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        let to_kill = map.run_player_code(&world);
        assert!(
            !to_kill.contains(&id),
            "life()=true 的实体不应在 to_kill 列表，实际 {:?}",
            to_kill
        );
    }

    #[test]
    fn sandbox_map_create_and_remove() {
        // create 后沙箱生效（run_player_code 能调到 life）；remove 后沙箱销毁、实体被跳过
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(3));
        world.entities.insert(id, make_entity(3, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(id, &wasm(DEAD_PLAYER_WAT), 1_000_000).unwrap();
        // 创建后：DEAD_PLAYER_WAT 的 life()=false -> 应进 to_kill
        let to_kill_before = map.run_player_code(&world);
        assert!(
            to_kill_before.contains(&id),
            "create 后沙箱应生效，实体应进 to_kill，实际 {:?}",
            to_kill_before
        );
        // 移除后：无沙箱 -> run_player_code 跳过，不进 to_kill
        map.remove(&id);
        let to_kill_after = map.run_player_code(&world);
        assert!(
            !to_kill_after.contains(&id),
            "remove 后实体不应进 to_kill，实际 {:?}",
            to_kill_after
        );
    }

    // === apply_pending_changes（阶段二 a 步）===

    #[test]
    fn apply_pending_changes_spawn_inserts_entity() {
        // 阶段二 a 步：Spawn 变更把实体插入 world
        let mut world = WorldState::new();
        let entity = make_entity(7, (3.0, 4.0));
        let id = entity.id;
        let mut pending: Vec<PendingChange> = vec![PendingChange::Spawn { entity }];
        apply_pending_changes(&mut world, &mut pending);
        assert!(world.entities.contains_key(&id), "Spawn 后实体应在 world 中");
        assert_eq!(world.entities.get(&id).unwrap().pos(), (3.0, 4.0));
    }

    #[test]
    fn apply_pending_changes_destroy_kills_entity() {
        // 阶段二 a 步：Destroy 变更标记实体死亡（死亡清理步才真正移除）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut pending: Vec<PendingChange> = vec![PendingChange::Destroy { entity: id }];
        apply_pending_changes(&mut world, &mut pending);
        assert!(
            !world.entities.get(&id).unwrap().is_alive(),
            "Destroy 应标记死亡"
        );
    }

    #[test]
    fn apply_pending_changes_setpos_moves_entity() {
        // 阶段二 a 步：SetPos 变更更新实体坐标
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(2));
        world.entities.insert(id, make_entity(2, (0.0, 0.0)));
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id,
            new_pos: (8.0, 9.0),
        }];
        apply_pending_changes(&mut world, &mut pending);
        assert_eq!(world.entities.get(&id).unwrap().pos(), (8.0, 9.0));
    }

    #[test]
    fn apply_pending_changes_setvelocity_sets_velocity() {
        // 阶段二 a 步：SetVelocity 变更更新实体速度
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(3));
        world.entities.insert(id, make_entity(3, (0.0, 0.0)));
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetVelocity {
            entity: id,
            new_velocity: (1.0, 2.0),
        }];
        apply_pending_changes(&mut world, &mut pending);
        assert_eq!(world.entities.get(&id).unwrap().velocity(), (1.0, 2.0));
    }

    #[test]
    fn apply_pending_changes_drains_queue() {
        // 阶段二 a 步：apply 后 pending 队列清空
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(4));
        world.entities.insert(id, make_entity(4, (0.0, 0.0)));
        let mut pending: Vec<PendingChange> = vec![
            PendingChange::SetPos {
                entity: id,
                new_pos: (1.0, 1.0),
            },
            PendingChange::Destroy { entity: id },
        ];
        apply_pending_changes(&mut world, &mut pending);
        assert!(pending.is_empty(), "apply 后 pending 应清空");
    }

    #[test]
    fn apply_pending_changes_unknown_entity_no_panic() {
        // 边界：对不存在的 EntityId 做 SetPos/SetVelocity/Destroy 应静默跳过、不 panic
        let mut world = WorldState::new();
        let unknown = EntityId(Uuid::from_u128(999));
        let mut pending: Vec<PendingChange> = vec![
            PendingChange::SetPos {
                entity: unknown,
                new_pos: (1.0, 1.0),
            },
            PendingChange::SetVelocity {
                entity: unknown,
                new_velocity: (2.0, 2.0),
            },
            PendingChange::Destroy { entity: unknown },
        ];
        apply_pending_changes(&mut world, &mut pending);
        assert!(pending.is_empty(), "即使目标不存在，变更也应被消费清空");
        assert!(world.entities.is_empty(), "不应插入任何实体");
    }

    // === decay_survival（阶段二 b 步）===

    #[test]
    fn decay_survival_kills_when_points_zero() {
        // 阶段二 b 步：生存点数归零时实体被标记死亡
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0))); // created_tick=0，初始3天
        // 经过 3 天(86400*3 tick)：点数 3->0
        decay_survival(&mut world, 86400 * 3);
        let e = world.entities.get(&id).unwrap();
        assert!(e.survival.is_dead(), "点数应归零");
        assert!(!e.is_alive(), "归零应标记实体死亡");
    }

    #[test]
    fn decay_survival_no_effect_when_not_enough_ticks() {
        // 阶段二 b 步：不足 86400 tick 不扣减、不死亡
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(2));
        world.entities.insert(id, make_entity(2, (0.0, 0.0)));
        decay_survival(&mut world, 86399);
        let e = world.entities.get(&id).unwrap();
        assert!(e.is_alive(), "不足一天不应死亡");
        assert_eq!(e.survival.points(), 3, "不足一天不应扣减");
    }

    #[test]
    fn decay_survival_skips_already_dead_entity() {
        // 边界：已标记死亡的实体不参与 decay（不 panic、点数不变）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(3));
        let mut e = make_entity(3, (0.0, 0.0));
        e.kill(); // 预先标记死亡
        world.entities.insert(id, e);
        decay_survival(&mut world, 86400 * 3); // 足够归零的 tick
        let e = world.entities.get(&id).unwrap();
        assert!(!e.is_alive(), "应保持死亡");
        assert_eq!(e.survival.points(), 3, "死亡实体不应被 decay，点数保持初始值");
    }

    #[test]
    fn tick_advances_world_tick() {
        // 修改：连续 tick 推进世界与时间引擎计数
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        for _ in 0..5 {
            tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut sandbox_map);
        }
        assert_eq!(world.tick(), 5);
        assert_eq!(time.current_tick(), 5);
    }

    #[test]
    fn tick_applies_physics_gravity() {
        // 集成：空中实体受重力下落（架构 4.1 b 步 physics.step）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 10.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut sandbox_map);
        let e = world.entities.get(&id).unwrap();
        // 重力 -9.8：10.0 -> 0.2
        assert!(e.pos().1 < 10.0, "重力应使实体下落");
        assert!(
            (e.pos().1 - 0.2).abs() < 0.01,
            "下落到约 0.2，实际 {}",
            e.pos().1
        );
    }

    #[test]
    fn tick_applies_pending_then_physics() {
        // 集成：pending 的 SetPos 变更在 physics 之前应用（架构 4.1 阶段二 a 在 b 之前）
        // 实体初始在地面 (0,0)，pending 设 SetPos 到 (0,10)
        // - 若 pending 先于 physics：实体到 (0,10) 后受重力 -> (0,0.2)
        // - 若 pending 后于 physics：实体留在 (0,0)（地面无重力），再被 SetPos 覆盖到 (0,10)
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id,
            new_pos: (0.0, 10.0),
        }];
        let mut sandbox_map = test_sandbox_map();
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        let y = world.entities.get(&id).unwrap().pos().1;
        // SetPos 先应用 -> 重力 10.0 -> 0.2（既非 10.0 也非 0.0）
        assert!(
            (y - 0.2).abs() < 0.01,
            "pending 应在 physics 之前应用：预期 ~0.2，实际 {}",
            y
        );
        assert!(pending.is_empty(), "tick 后 pending 应被 apply 清空");
    }

    #[test]
    fn tick_removes_dead_entities() {
        // 修改：死亡实体在死亡清理步被移除（架构 4.1 b 步）
        let mut world = WorldState::new();
        let alive = EntityId(Uuid::from_u128(1));
        let dead = EntityId(Uuid::from_u128(2));
        let mut dead_e = make_entity(2, (5.0, 5.0));
        dead_e.kill();
        world.entities.insert(alive, make_entity(1, (0.0, 0.0)));
        world.entities.insert(dead, dead_e);
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut sandbox_map);
        assert_eq!(world.entities.len(), 1, "死亡实体应被清理");
        assert!(world.entities.contains_key(&alive));
        assert!(!world.entities.contains_key(&dead));
    }

    #[test]
    fn tick_executes_player_code_and_kills_dead() {
        // 集成：life()=false 的实体在 tick 阶段一被标记死亡、阶段二死亡清理步被移出 world
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        sandbox_map.create(id, &wasm(DEAD_PLAYER_WAT), 1_000_000).unwrap();
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        assert!(
            !world.entities.contains_key(&id),
            "life()=false 的实体应被清理"
        );
        assert!(
            world.entities.is_empty(),
            "world 应无实体，实际 {}",
            world.entities.len()
        );
    }

    #[test]
    fn tick_keeps_alive_entities() {
        // 集成：life()=true 的实体在 tick 后仍存活（不被阶段一标记死亡、不被死亡清理移除）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(2));
        world.entities.insert(id, make_entity(2, (0.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        sandbox_map.create(id, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        assert!(
            world.entities.contains_key(&id),
            "life()=true 的实体应存活"
        );
        assert!(
            world.entities.get(&id).unwrap().is_alive(),
            "实体应仍标记存活"
        );
    }

    #[test]
    fn tick_keeps_alive_entity_across_many_ticks() {
        // 回归：fuel 每 tick 重置，life()=true 实体跨多 tick 仍存活。
        // 若不每 tick 重置 fuel，小配额下数 tick 内 fuel 耗尽 -> call_life 返回 Err
        // -> 实体被标记死亡（架构 4.1 要求"超额不死亡、下次 tick 继续"）。
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        // 小 fuel 配额（1000）：若不每 tick 复位，几百 tick 内耗尽
        sandbox_map.create(id, &wasm(PLAYER_WAT), 1_000).unwrap();
        for _ in 0..1_000 {
            tick(
                &mut world,
                &mut time,
                &physics,
                &mut renderer,
                &mut storage,
                &mut pending,
                &mut sandbox_map,
            );
        }
        assert!(
            world.entities.contains_key(&id),
            "1000 tick 后实体应仍存活（fuel 每 tick 重置），实际 world 含 {} 个实体",
            world.entities.len()
        );
        assert!(world.entities.get(&id).unwrap().is_alive());
    }

    #[test]
    fn run_player_code_skips_dead_entities() {
        // 防御：已死亡实体不参与玩家代码执行（架构 4.1"把所有存活实体放进任务队列"）
        // 即使该实体仍有沙箱（life()=false），也不应进 to_kill（已死亡无需再标记）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        let mut e = make_entity(1, (0.0, 0.0));
        e.kill(); // 预先标记死亡
        world.entities.insert(id, e);
        let mut map = test_sandbox_map();
        map.create(id, &wasm(DEAD_PLAYER_WAT), 1_000_000).unwrap();
        let to_kill = map.run_player_code(&world);
        assert!(
            !to_kill.contains(&id),
            "已死亡实体应被跳过，实际 to_kill={:?}",
            to_kill
        );
    }

    #[test]
    fn run_player_code_skips_entity_without_sandbox() {
        // 无沙箱实体（如从快照恢复、未挂玩家代码）被 run_player_code 跳过，不标记死亡
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        // 故意不为实体创建沙箱
        let to_kill = map.run_player_code(&world);
        assert!(!to_kill.contains(&id), "无沙箱实体应被跳过");
        assert!(to_kill.is_empty(), "to_kill 应为空，实际 {:?}", to_kill);
    }

    #[test]
    fn sandbox_removed_when_entity_dies() {
        // 实体死亡（life()=false）后，死亡清理步应销毁其沙箱
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        sandbox_map.create(id, &wasm(DEAD_PLAYER_WAT), 1_000_000).unwrap();
        // tick：阶段一 life()=false 标记死亡，阶段二清理移除实体 + 应销毁沙箱
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        assert!(!world.entities.contains_key(&id), "死亡实体应被移出 world");
        // 验证沙箱已销毁：同 id 实体放回 world，run_player_code 应跳过（沙箱不存在）。
        // 若沙箱未销毁，会调到 life()=false -> 进 to_kill。
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let to_kill = sandbox_map.run_player_code(&world);
        assert!(
            !to_kill.contains(&id),
            "死亡实体沙箱应已销毁，实际仍能调到 life()（to_kill={:?}）",
            to_kill
        );
    }

    #[test]
    fn tick_snapshots_at_interval_100() {
        // 集成：每 100 tick 自动存快照（架构 4.1 d 步）
        let mut world = WorldState::new();
        let mut time = TimeEngine::new(); // 默认间隔 100
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        for _ in 0..100 {
            tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut sandbox_map);
        }
        assert_eq!(world.tick(), 100);
        assert_eq!(
            storage.snapshot_count().unwrap(),
            1,
            "tick 100 应存一次快照"
        );
    }

    #[test]
    fn spawn_test_entities_inserts_two_at_expected_positions() {
        // 测试实体初始位置：一个 (0,10) 一个 (5,20)，均在空中（不站地面 y=0）
        // 这样默认重力规则下能看到下落效果
        let mut world = WorldState::new();
        let mut sandbox_map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut sandbox_map);
        assert_eq!(world.entities.len(), 2, "应创建两个测试实体");
        let positions: Vec<(f64, f64)> = world.entities.values().map(|e| e.pos()).collect();
        assert!(
            positions.contains(&(0.0, 10.0)),
            "应有实体在 (0,10)，实际 {:?}",
            positions
        );
        assert!(
            positions.contains(&(5.0, 20.0)),
            "应有实体在 (5,20)，实际 {:?}",
            positions
        );
    }

    #[test]
    fn tick_then_force_save_then_restore() {
        // 集成：tick 推进 -> force_save -> load_latest_world 恢复一致
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(7));
        world.entities.insert(id, make_entity(7, (3.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        for _ in 0..5 {
            tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut sandbox_map);
        }
        assert_eq!(world.tick(), 5);
        assert_eq!(world.entities.len(), 1);
        storage.force_save(&world).unwrap();
        let loaded = storage.load_latest_world().unwrap().unwrap();
        assert_eq!(loaded.tick(), 5);
        assert_eq!(loaded.entities.len(), 1);
        assert!(loaded.entities.contains_key(&id));
        assert_eq!(loaded.entities.get(&id).unwrap().pos(), (3.0, 0.0));
    }

    // === handle_collisions（阶段二 c 步：接触触发沙箱通道）===

    /// 构造碰撞对（测试辅助，固定 EntityId 便于断言）
    fn make_collision_pair(a: u128, b: u128, pos: (f64, f64)) -> CollisionPair {
        CollisionPair {
            a: EntityId(Uuid::from_u128(a)),
            b: EntityId(Uuid::from_u128(b)),
            pos,
        }
    }

    /// 构造存活实体表（测试辅助，id 与 make_collision_pair 的 a/b 对齐）
    fn make_entities(ids: &[u128]) -> HashMap<EntityId, Entity> {
        ids.iter()
            .map(|&id| {
                let e = make_entity(id, (5.0, 5.0));
                (e.id, e)
            })
            .collect()
    }

    #[test]
    fn handle_collisions_returns_zero_when_no_collision() {
        // 无碰撞对：返回 0，不 panic（架构 4.1 b 步：无接触则不开通道）
        let pairs: Vec<CollisionPair> = Vec::new();
        let entities = HashMap::new();
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 0, "无碰撞对应返回 0");
    }

    #[test]
    fn handle_collisions_returns_one_for_single_pair() {
        // 单个存活碰撞对：返回 1（两个实体同坐标 -> 1 对接触）
        let entities = make_entities(&[1, 2]);
        let pairs = vec![make_collision_pair(1, 2, (5.0, 5.0))];
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 1, "单个存活碰撞对应返回 1");
    }

    #[test]
    fn handle_collisions_returns_three_for_three_pairs() {
        // 三个存活碰撞对（三个实体两两同坐标，C(3,2)=3）：返回 3
        let entities = make_entities(&[1, 2, 3]);
        let pairs = vec![
            make_collision_pair(1, 2, (5.0, 5.0)),
            make_collision_pair(1, 3, (5.0, 5.0)),
            make_collision_pair(2, 3, (5.0, 5.0)),
        ];
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 3, "三个存活碰撞对应返回 3");
    }

    #[test]
    fn handle_collisions_counts_pairs_from_detect_collisions() {
        // 集成：两实体同坐标 -> detect_collisions 返回 1 对 -> handle_collisions 返回 1
        // 验证 detect -> handle 链路：物理引擎检测到的碰撞对原样传给接触处理
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let e1 = make_entity(1, (5.0, 5.0));
        let e2 = make_entity(2, (5.0, 5.0));
        entities.insert(e1.id, e1);
        entities.insert(e2.id, e2);
        let pairs = engine.detect_collisions(&entities);
        assert_eq!(pairs.len(), 1, "同坐标应检测到 1 对碰撞");
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 1, "handle_collisions 应返回 1");
    }

    #[test]
    fn handle_collisions_skips_pairs_with_dead_entity() {
        // 接触事件只对存活实体对记录：含死亡实体的碰撞对应跳过（不计入返回数）。
        // 架构 4.1 接触触发针对存活实体间交互；死亡实体本 tick 会被死亡清理步移除，
        // 不应为其开通道/记接触。阶段一 life()=false 或 decay_survival 归零 kill 的
        // 实体此时仍在 world.entities 中（死亡清理在 step d 步），detect_collisions
        // 会扫到，故 handle_collisions 需按 is_alive 过滤。
        let mut entities = HashMap::new();
        let alive_e = make_entity(1, (5.0, 5.0));
        let mut dead_e = make_entity(2, (5.0, 5.0));
        dead_e.kill();
        entities.insert(alive_e.id, alive_e);
        entities.insert(dead_e.id, dead_e);
        let pairs = vec![make_collision_pair(1, 2, (5.0, 5.0))];
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 0, "含死亡实体的碰撞对应被跳过，返回 0");
    }

    #[test]
    fn tick_resolves_overlapping_entities_after_detect_reorder() {
        // 回归测试（架构 4.1 b 步）：两实体同坐标 -> tick 在 physics.step 之前检测
        // 接触事件，step 解决碰撞（后创建的挤到 x+1），tick 后两实体坐标不再相同。
        //
        // 本测试只断言"碰撞解决后坐标不同"——这是 physics.step 的既有能力，并非
        // handle_collisions 的新行为。保留它的意义是回归保护：验证 tick 内
        // "先 detect 再 step 再 handle" 的重排没有破坏既有碰撞解决流程。
        // handle_collisions 本身（含死亡实体跳过）由其单元测试覆盖；
        // tick() 返回 ()，无法在此断言接触计数（已知测试缺口，见报告）。
        let mut world = WorldState::new();
        let id1 = EntityId(Uuid::from_u128(1));
        let id2 = EntityId(Uuid::from_u128(2));
        world.entities.insert(id1, make_entity(1, (5.0, 0.0)));
        world.entities.insert(id2, make_entity(2, (5.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        let pos1 = world.entities.get(&id1).unwrap().pos();
        let pos2 = world.entities.get(&id2).unwrap().pos();
        assert_ne!(
            pos1, pos2,
            "碰撞解决后两实体坐标应不同（一方被挤开），实际 {pos1:?} 与 {pos2:?}"
        );
    }

    // === attack_target（架构 4.2：on_attack 拦截 -> 替换沙箱 -> 检查 life）===

    /// 目标 WAT：life()=true，无 on_attack（不拦截）
    const TARGET_WAT: &str = r#"(module (func (export "life") (result i32) i32.const 1))"#;

    /// 防御目标 WAT：life()=true + on_attack 返回 1（拦截攻击）
    const DEFENDED_TARGET_WAT: &str = r#"(module
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_attack") (param i32) (result i32) i32.const 1))"#;

    #[test]
    fn attack_target_empty_payload_kills() {
        // 空 payload -> 默认击杀 WAT（life=false）-> 替换后 life()=false -> 击杀成功
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(5));
        map.create(target_id, &wasm(TARGET_WAT), 1_000_000).unwrap();
        let outcome = map.attack_target(&target_id, &[]).unwrap();
        assert!(!outcome.intercepted, "无 on_attack 不应拦截");
        assert!(outcome.killed, "空 payload 默认击杀，life 应为 false");
    }

    #[test]
    fn attack_target_on_attack_intercepts() {
        // 目标导出 on_attack 返回 1 -> 拦截，不替换沙箱、不击杀
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(6));
        map.create(target_id, &wasm(DEFENDED_TARGET_WAT), 1_000_000).unwrap();
        let outcome = map.attack_target(&target_id, &[]).unwrap();
        assert!(outcome.intercepted, "on_attack 返回 1 应拦截");
        assert!(!outcome.killed, "拦截后不应击杀");
    }

    #[test]
    fn attack_target_payload_life_true_not_killed() {
        // payload 为 life=true 的 WASM -> 替换后 life()=true -> 命中但未击杀（如击晕）
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(7));
        map.create(target_id, &wasm(TARGET_WAT), 1_000_000).unwrap();
        let outcome = map.attack_target(&target_id, &wasm(TARGET_WAT)).unwrap();
        assert!(!outcome.intercepted);
        assert!(!outcome.killed, "life=true payload 不应击杀");
    }

    #[test]
    fn attack_target_no_sandbox_errors() {
        // 边界：目标无沙箱 -> Err（不 panic）
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(8));
        let result = map.attack_target(&target_id, &[]);
        assert!(result.is_err(), "无沙箱目标应返回 Err");
    }

    // === process_attacks（阶段二 a2：找目标 -> attack_target -> 掠夺 -> kill）===

    /// 构造存活实体并挂沙箱（测试辅助）
    fn make_entity_with_sandbox(
        world: &mut WorldState,
        map: &mut EntitySandboxMap,
        id_u: u128,
        pos: (f64, f64),
        wat: &str,
    ) -> EntityId {
        let id = EntityId(Uuid::from_u128(id_u));
        let e = make_entity(id_u, pos);
        map.create(id, &wasm(wat), 1_000_000).unwrap();
        world.entities.insert(id, e);
        id
    }

    #[test]
    fn process_attacks_miss_when_no_entity_at_pos() {
        // 攻击落空（验收#5）：坐标无存活实体 -> 不崩溃、不击杀
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker =
            make_entity_with_sandbox(&mut world, &mut map, 1, (0.0, 0.0), PLAYER_WAT);
        let attacks = vec![PendingAttack {
            attacker,
            target_pos: (99.0, 99.0),
            payload: vec![],
        }];
        process_attacks(&mut world, &mut map, &attacks);
        assert_eq!(world.entities.len(), 1, "落空不应移除实体");
        assert!(world.entities.contains_key(&attacker));
    }

    #[test]
    fn process_attacks_kills_and_plunders() {
        // 击杀+掠夺（验收#4）：空 payload 默认击杀 -> 目标死亡 + 从死者转移 1 生存点数给攻击者
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker =
            make_entity_with_sandbox(&mut world, &mut map, 1, (0.0, 0.0), PLAYER_WAT);
        let target =
            make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.0), TARGET_WAT);
        assert_eq!(world.entities.get(&attacker).unwrap().survival.points(), 3);
        let attacks = vec![PendingAttack {
            attacker,
            target_pos: (5.0, 0.0),
            payload: vec![],
        }];
        process_attacks(&mut world, &mut map, &attacks);
        // 目标被标记死亡（kill，retain 在 tick 死亡清理步才移除）
        assert!(!world.entities.get(&target).unwrap().is_alive(), "目标应被击杀");
        // 攻击者掠夺 +1（3 -> 4）
        assert_eq!(
            world.entities.get(&attacker).unwrap().survival.points(),
            4,
            "击杀掠夺 +1 生存点数"
        );
        // 死者被掠夺 -1（3 -> 2）
        assert_eq!(
            world.entities.get(&target).unwrap().survival.points(),
            2,
            "死者被掠夺 -1"
        );
    }

    #[test]
    fn process_attacks_intercepted_no_plunder() {
        // on_attack 拦截（验收#2）：不击杀、不掠夺（双方生存点数不变）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker =
            make_entity_with_sandbox(&mut world, &mut map, 1, (0.0, 0.0), PLAYER_WAT);
        let target =
            make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.0), DEFENDED_TARGET_WAT);
        let attacks = vec![PendingAttack {
            attacker,
            target_pos: (5.0, 0.0),
            payload: vec![],
        }];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(world.entities.get(&target).unwrap().is_alive(), "拦截后目标应存活");
        assert_eq!(
            world.entities.get(&attacker).unwrap().survival.points(),
            3,
            "拦截不掠夺"
        );
        assert_eq!(
            world.entities.get(&target).unwrap().survival.points(),
            3,
            "拦截不扣减"
        );
    }

    #[test]
    fn process_attacks_self_attack_kills_no_plunder() {
        // 自攻自（技术约束）：允许击杀但跳过掠夺转移（生存点数不变）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker =
            make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.0), TARGET_WAT);
        // attacker 攻击自己所在坐标 -> target_id == attacker
        let attacks = vec![PendingAttack {
            attacker,
            target_pos: (5.0, 0.0),
            payload: vec![],
        }];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(
            !world.entities.get(&attacker).unwrap().is_alive(),
            "自攻自应击杀自己"
        );
        assert_eq!(
            world.entities.get(&attacker).unwrap().survival.points(),
            3,
            "自攻自不掠夺，点数不变"
        );
    }

    // === 攻击全链路集成（验收#1：玩家代码调 attack_at import）===

    /// 攻击方 WAT：导出 memory + on_tick 调 env.attack_at(5.0, 0.0, 0, 0)（空 payload 默认击杀）
    const ATTACKER_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "on_tick")
    f64.const 5.0 f64.const 0.0 i32.const 0 i32.const 0 call $attack_at drop)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn tick_attack_pipeline_kills_and_plunders() {
        // 集成：ATTACKER 的 on_tick 调 env.attack_at -> 排队 -> 阶段二 process_attacks
        // -> 击杀目标 + 掠夺 1 生存点数给攻击者
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        // 攻击方在 (0,0)，on_tick 调 attack_at(5.0, 0.0, 0, 0)（空 payload 默认击杀）
        let attacker =
            make_entity_with_sandbox(&mut world, &mut sandbox_map, 1, (0.0, 0.0), ATTACKER_WAT);
        // 目标在 (5.0, 0.0)，life=true 无 on_attack
        let target =
            make_entity_with_sandbox(&mut world, &mut sandbox_map, 2, (5.0, 0.0), TARGET_WAT);
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        // 目标被击杀并在死亡清理步移除
        assert!(!world.entities.contains_key(&target), "目标应被击杀移除");
        // 攻击者存活且掠夺 +1（3 -> 4）
        assert!(world.entities.contains_key(&attacker), "攻击者应存活");
        assert_eq!(
            world.entities.get(&attacker).unwrap().survival.points(),
            4,
            "击杀掠夺 +1 生存点数"
        );
    }
}
