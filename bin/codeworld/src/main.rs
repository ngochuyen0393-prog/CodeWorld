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
            // 感知快照填充（get_pos/scan 数据通路）：import 回调拿不到 world，
            // 阶段一执行玩家代码【前】把坐标快照写进 SandboxData——玩家代码
            // 读到的是 tick 开始时的状态（架构 4.1 快照语义）。
            if let Some(e) = world.entities.get(&entity_id) {
                let nearest = find_nearest_enemy(world, &entity_id);
                let data = handle.store.data_mut();
                data.pos = e.pos();
                data.nearest_enemy = nearest;
            }
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

    /// 收集本 tick 所有沙箱排队的移动请求（env.move_by 推入，阶段二并入 pending 队列）
    ///
    /// drain 各沙箱 pending_moves 后清空，保证不跨 tick 残留。在 tick 阶段二 a 步
    /// apply_pending_changes 之前调用（收集到的 SetPos 与主 pending 队列一起应用）。
    fn collect_pending_moves(&mut self) -> Vec<PendingChange> {
        let mut all = Vec::new();
        for handle in self.sandboxes.values_mut() {
            let data = handle.store.data_mut();
            all.append(&mut data.pending_moves);
        }
        if !all.is_empty() {
            tracing::debug!(count = all.len(), "[collect_pending_moves] 收集移动请求");
        }
        all
    }

    /// 收集本 tick 所有沙箱的 log marker 值序列（战况字幕数据源）
    ///
    /// drain 各沙箱 log_markers 后清空，保证不跨 tick 残留。在阶段二攻击结算之后
    /// 调用（D 的盾量上报发生在 on_attack 收件期间，早于此步）。
    fn collect_all_log_markers(&mut self) -> Vec<i32> {
        let mut all = Vec::new();
        for handle in self.sandboxes.values_mut() {
            all.append(&mut handle.store.data_mut().log_markers);
        }
        all
    }

    /// 收件：调目标【当前沙箱】的 on_attack 询问是否拦截（架构 4.2 两阶段-收件）
    ///
    /// 只问不改：不换血、不击杀；防守方 fuel 随每次调用自然消耗（围殴=磨算力）。
    /// 返回 true=拦截。目标无沙箱 -> Err（调用方作废该刀）。
    fn defend(&mut self, target_id: &EntityId, payload: &[u8]) -> Result<bool> {
        let Some(handle) = self.sandboxes.get_mut(target_id) else {
            anyhow::bail!("目标 {:?} 无沙箱，无法防守", target_id);
        };
        self.manager.call_on_attack(handle, payload)
    }

    /// 结算：路线A换血 + 检查 life（架构 4.2 两阶段-结算）
    ///
    /// - fuel 取旧沙箱【剩余量】而非配额上限：防"反复被攻击回满配额"刷算力（P1）。
    /// - payload 非法（编译/实例化失败）：旧沙箱原样放回（P0 僵尸实体修复），
    ///   "存活实体必有沙箱"不变量不破，返回 Err 交调用方记攻击失败。
    /// - 返回 killed：替换后 life()=false。life() 无法执行（Err）= 结果未知，
    ///   按架构 4.2 默认行为 life 维持原值，不判死。
    /// - 不处理掠夺：掠夺需改 world（survival），由调用方 settle_attacks 在击杀后处理。
    fn replace_code(&mut self, target_id: &EntityId, payload: &[u8]) -> Result<bool> {
        // fuel 剩余量与原配额在任何 remove 之前读（读失败也不能弄丢旧沙箱）
        let (fuel, old_limit) = {
            let Some(handle) = self.sandboxes.get_mut(target_id) else {
                anyhow::bail!("目标 {:?} 无沙箱，无法换血", target_id);
            };
            (handle.store.get_fuel()?, handle.store.data().fuel_limit)
        };
        let old = self
            .sandboxes
            .remove(target_id)
            .context("目标沙箱刚确认存在却又丢失")?;
        let wasm_bytes: &[u8] = if payload.is_empty() {
            &DEFAULT_KILL_WASM
        } else {
            payload
        };
        let mut new_handle = match self.manager.replace_sandbox(old, wasm_bytes, fuel) {
            Ok(h) => h,
            Err((old_handle, e)) => {
                // P0 僵尸修复：编译/实例化失败，旧沙箱放回映射表
                self.sandboxes.insert(*target_id, old_handle);
                tracing::warn!(
                    target = %target_id.0,
                    error = %e,
                    "[replace_code] payload 编译/实例化失败，旧沙箱已还原"
                );
                anyhow::bail!("换血失败：payload 非法");
            }
        };
        // P1 修复（审查第二轮）：fuel_limit 恢复原配额——fuel 继承只继承【当 tick 剩余量】
        //（防吃刀回满刷配额），上限不缩水（防反复被攻击单调递减磨死，绕过架构 4.1
        // "超额不死亡、下次 tick 继续"保护）
        new_handle.store.data_mut().fuel_limit = old_limit;
        self.sandboxes.insert(*target_id, new_handle);
        tracing::info!(
            target = %target_id.0,
            fuel_inherited = fuel,
            fuel_limit_restored = old_limit,
            "[replace_code] 目标沙箱已替换（路线A，fuel 继承剩余量，配额恢复原值）"
        );
        // 检查 life()：false=击杀成功。
        // P1 修复（审查第一轮）：life() 无法执行（Err）= 攻击结果未知，按架构 4.2 默认行为
        // 处理（life 维持原值，不被覆写）-> 不判死。防止防守 fuel 被磨穿的边界下
        // 良性刀（击晕/绑架）被误收割"击杀+掠夺"（铁律 4：无击杀=无掠夺）。
        let handle = self
            .sandboxes
            .get_mut(target_id)
            .context("替换后目标沙箱丢失")?;
        match self.manager.call_life(handle) {
            Ok(false) => Ok(true),
            Ok(true) => Ok(false),
            Err(e) => {
                tracing::warn!(
                    target = %target_id.0,
                    error = %e,
                    "[replace_code] 替换后 life() 调用失败，life 维持原值不判死"
                );
                Ok(false)
            }
        }
    }
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
        spawn_test_entities(&mut world, &mut sandbox_map, config.d_shield, config.h_enabled);
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
    //      先并入阶段一排队的移动请求（env.move_by），随本步统一应用（与攻击排队同模式）
    pending.extend(sandbox_map.collect_pending_moves());
    apply_pending_changes(world, pending);

    //   a2. 处理攻击（架构 4.2 两阶段模型：阶段一 on_tick 调 env.attack_at 排队的请求在此统一执行）
    //      插在 apply_pending_changes 之后、decay_survival 之前：攻击产生的死亡先于每日衰减处理。
    //      import 函数只排队（无 world 访问权），实际 hit/miss/拦截/结算在此判断：
    //      收件（逐刀问目标当前沙箱 on_attack，不换血不击杀）-> 结算（攻击者 id 升序逐刀执行）。
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

    //   d2. 战况字幕（渲染层展示，随 tick 更新；架构 4.2 铁律 3：字幕不写回 world）
    //      盾量来自 D 玩家代码 env.log marker 自愿上报（收件期 on_attack 的 log(7)+log(余量)），
    //      host 只解析上报流、不读沙箱内部全局（沙箱隔离不破坏）。
    //      距离/存活数取死亡清理后的 world 快照（entities 里只剩存活实体，len 即存活数）；
    //      A 或 D 缺席（被击杀/未登场）则距离显示占位符。
    let markers = sandbox_map.collect_all_log_markers();
    let shield_report = parse_shield_markers(&markers);
    let attacker = world
        .entities
        .values()
        .find(|e| e.code_hash == ATTACKER_CODE);
    let defender = world
        .entities
        .values()
        .find(|e| e.code_hash == DEFENDER_CODE);
    let dist = match (attacker, defender) {
        (Some(a), Some(d)) => {
            let (ax, ay) = a.pos();
            let (dx, dy) = d.pos();
            Some(((ax - dx).powi(2) + (ay - dy).powi(2)).sqrt())
        }
        _ => None,
    };
    renderer.update_caption(
        current_tick,
        dist,
        defender.is_some(),
        if defender.is_some() { shield_report } else { None },
        world.entities.len(),
    );

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
                // 死亡实体不应用移动：同 tick 内 life()=false 实体排队的移动作废
                //（死亡清理步随后移除实体，尸体位移属无人消费的顺序耦合，应用侧直接拦）
                if let Some(e) = world.entities.get_mut(&entity).filter(|e| e.is_alive()) {
                    tracing::debug!(entity_id = %entity.0, ?new_pos, "应用 SetPos");
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

/// 待结算攻击（收件产出 -> 结算消费）
struct PendingStrike {
    /// 被攻击的实体（坐标解析产物，结算时换血/击杀的对象）
    target: EntityId,
    /// 原始攻击请求（attacker/payload）
    attack: PendingAttack,
}

/// 处理阶段一排队的攻击请求（架构 4.2 两阶段模型 + 4.1 阶段二 a2 步）
///
/// 收件阶段 deliver_attacks：过滤已死攻击者 -> 坐标解析目标 -> 逐刀调目标【当前沙箱】的
/// on_attack（不换血不击杀，防守 fuel 自然消耗）-> 未拦截的刀进入待结算列表。
/// 结算阶段 settle_attacks：全部待结算刀【全局按攻击者 EntityId 升序】依次执行
/// （确定序，消灭随机性）；每刀执行前再查攻防双方存活（可能被同批更早的刀击杀）；
/// 击杀则掠夺 1 生存点数并标记死亡（清理步 d 移除实体+销毁沙箱）。
///
/// 攻击在阶段二处理（不实时）：玩家代码调 import 只排队，实际击杀结果下一 tick 才能感知。
/// 收件期间（on_attack 里）新排的攻击留在防守者沙箱里，下一 tick 的 collect 才处理。
fn process_attacks(world: &mut WorldState, sandbox_map: &mut EntitySandboxMap, attacks: &[PendingAttack]) {
    let strikes = deliver_attacks(world, sandbox_map, attacks);
    settle_attacks(world, sandbox_map, &strikes);
}

/// 收件阶段：逐刀问防守、产出待结算列表（架构 4.2 两阶段-收件）
///
/// - 攻击者已死 -> 整刀作废（不触发防守、不结算；防死者点数蒸发，P1）。
/// - 坐标解析命中多个存活实体时 find 取首个（HashMap 顺序随机，测试用唯一坐标避免歧义）。
/// - 防守询问失败（如防守方 fuel 耗尽）-> 该刀作废（架构 4.2：on_attack 超时=默认行为，
///   life 维持原值不被覆写）。
/// - 输出全局按攻击者 EntityId 升序（结算确定序）。
fn deliver_attacks(
    world: &WorldState,
    sandbox_map: &mut EntitySandboxMap,
    attacks: &[PendingAttack],
) -> Vec<PendingStrike> {
    let mut strikes: Vec<PendingStrike> = Vec::new();
    let mut voided = 0usize; // 攻击者已死作废
    let mut missed = 0usize; // 坐标落空
    let mut defense_failed = 0usize; // 防守询问失败作废
    let mut intercepted_count = 0usize; // on_attack 拦截
    for atk in attacks {
        // 攻击者已死 -> 攻击作废
        let attacker_alive = world
            .entities
            .get(&atk.attacker)
            .map(|e| e.is_alive())
            .unwrap_or(false);
        if !attacker_alive {
            voided += 1;
            tracing::info!(
                attacker = %atk.attacker.0,
                "[deliver_attacks] 攻击者已死，攻击作废"
            );
            continue;
        }
        // 坐标解析：找坐标上的存活实体（attack_at 攻击坐标不是 EntityId，目标移动可能 miss）
        let Some(target_id) = world
            .entities
            .values()
            .find(|e| e.is_alive() && e.pos() == atk.target_pos)
            .map(|e| e.id)
        else {
            missed += 1;
            tracing::info!(
                attacker = %atk.attacker.0,
                pos = ?atk.target_pos,
                "[deliver_attacks] 攻击落空（坐标无存活实体）"
            );
            continue;
        };
        // 收件：调目标【当前沙箱】的 on_attack（本阶段不换血不击杀）
        let intercepted = match sandbox_map.defend(&target_id, &atk.payload) {
            Ok(v) => v,
            Err(e) => {
                defense_failed += 1;
                tracing::warn!(
                    attacker = %atk.attacker.0,
                    target = %target_id.0,
                    error = %e,
                    "[deliver_attacks] 防守询问失败，该刀作废"
                );
                continue;
            }
        };
        if intercepted {
            intercepted_count += 1;
        }
        tracing::info!(
            attacker = %atk.attacker.0,
            target = %target_id.0,
            intercepted,
            "[deliver_attacks] 收件：on_attack 结果"
        );
        if !intercepted {
            strikes.push(PendingStrike {
                target: target_id,
                attack: atk.clone(),
            });
        }
    }
    // 确定序：全局按攻击者 EntityId 升序（结算顺序，消灭遍历随机性）
    strikes.sort_by_key(|s| s.attack.attacker.0);
    tracing::info!(
        total = attacks.len(),
        voided,
        missed,
        defense_failed,
        intercepted = intercepted_count,
        pending = strikes.len(),
        "[deliver_attacks] 收件完成：作废/落空/防守失败/拦截/待结算"
    );
    strikes
}

/// 结算阶段：按攻击者 id 升序逐刀执行（换血/击杀/掠夺，架构 4.2 两阶段-结算）
///
/// 每刀执行前再查攻防双方存活：攻击者可能已被同批更早的刀击杀（其刀作废，点数不蒸发），
/// 目标可能已被同批先执行的刀击杀（后到的刀跳过，先执行者的换血生效）。
fn settle_attacks(world: &mut WorldState, sandbox_map: &mut EntitySandboxMap, strikes: &[PendingStrike]) {
    for (idx, strike) in strikes.iter().enumerate() {
        // 执行前再查攻击者存活
        let attacker_alive = world
            .entities
            .get(&strike.attack.attacker)
            .map(|e| e.is_alive())
            .unwrap_or(false);
        if !attacker_alive {
            tracing::info!(
                attacker = %strike.attack.attacker.0,
                "[settle_attacks] 攻击者已死，该刀作废"
            );
            continue;
        }
        // 执行前再查目标存活（可能已被同批先执行的刀击杀）
        let target_alive = world.entities.get(&strike.target).map(|e| e.is_alive()).unwrap_or(false);
        if !target_alive {
            tracing::info!(
                attacker = %strike.attack.attacker.0,
                target = %strike.target.0,
                "[settle_attacks] 目标已死，该刀跳过"
            );
            continue;
        }
        // 换血（P0：失败时旧沙箱已由 replace_code 还原）
        match sandbox_map.replace_code(&strike.target, &strike.attack.payload) {
            Ok(killed) => {
                if killed {
                    // 击杀掠夺：从死者转移 1 生存点数给攻击者（自攻自跳过转移但仍击杀）
                    if strike.attack.attacker != strike.target {
                        plunder_survival(world, strike.target, strike.attack.attacker, 1);
                    } else {
                        tracing::info!(
                            entity = %strike.target.0,
                            "[settle_attacks] 自攻自击杀，跳过掠夺"
                        );
                    }
                    // 标记死亡（死亡清理步 d 移除实体 + 销毁沙箱）
                    if let Some(e) = world.entities.get_mut(&strike.target) {
                        e.kill();
                    }
                    tracing::info!(
                        attacker = %strike.attack.attacker.0,
                        target = %strike.target.0,
                        order = idx,
                        "[settle_attacks] 击杀成功"
                    );
                } else {
                    tracing::info!(
                        attacker = %strike.attack.attacker.0,
                        target = %strike.target.0,
                        order = idx,
                        "[settle_attacks] 命中未杀（life=true，击晕）"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    attacker = %strike.attack.attacker.0,
                    target = %strike.target.0,
                    order = idx,
                    error = %e,
                    "[settle_attacks] 攻击执行失败（旧沙箱已还原）"
                );
            }
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

/// 解析 log marker 流中的盾量上报（战况字幕数据源，纯函数）
///
/// D 的玩家代码上报协议（env.log marker，位置配对不猜值）：
/// - `log(7)`（拦截）后紧跟 `log(余量)` —— 配对消费，7 后的下一个值即盾量
/// - `log(8)`（盾尽放行）—— 盾 0，其后无配对值
/// - 其他 marker（A 的 0/2/3、D 的 9 反制、H 的 4/5/6）不携带盾信息，忽略
///
/// 协议前提（审查 P2 记录）：①当前只有 D 的代码会发 7/8；②7+盾值在同一次
/// on_attack 内原子成对发出。未来多玩家 marker 值域可能撞 7/8，届时应按沙箱
/// 分组解析（各沙箱独立跑本函数再合并），Phase 2 议题。
///
/// 返回最新一次上报的盾量；流中无上报（或截断对）返回 None（字幕显示占位符）。
fn parse_shield_markers(markers: &[i32]) -> Option<i32> {
    let mut shield = None;
    let mut prev_was_intercept = false;
    for &m in markers {
        if prev_was_intercept {
            // 7 后紧跟的值 = 盾余量（含"盾值恰好等于 7"的情况，位置配对天然正确）
            shield = Some(m);
            prev_was_intercept = false;
        } else if m == 8 {
            shield = Some(0);
            prev_was_intercept = false;
        } else {
            prev_was_intercept = m == 7;
        }
    }
    shield
}

/// 感知规则：找 target 最近的其他存活实体坐标（env.scan 的数据源，MVP 上帝视角单敌人）
///
/// - 排除自己（不能锁定自己为敌人）
/// - 排除死亡实体（尸体不是感知目标）
/// - None = 世界没有其他存活实体（scan 返回 0）
/// - 架构 3.2 感知接口完整版为 get_visible_entities（可见实体列表）；MVP 单敌人取最近者
fn find_nearest_enemy(world: &WorldState, target: &EntityId) -> Option<(f64, f64)> {
    let me = world.entities.get(target)?;
    let mut best: Option<(f64, (f64, f64))> = None;
    for e in world.entities.values() {
        if !e.is_alive() || e.id == *target {
            continue;
        }
        let (dx, dy) = (e.pos().0 - me.pos().0, e.pos().1 - me.pos().1);
        let dist2 = dx * dx + dy * dy;
        // 严格小于：同距并列时取遍历首个（HashMap 顺序随机，测试用明确距离避免歧义）
        if best.map(|(d, _)| dist2 < d).unwrap_or(true) {
            best = Some((dist2, e.pos()));
        }
    }
    let (_, pos) = best?;
    Some(pos)
}

/// 生物代码标识（code_hash 字段承载身份，字幕与布阵日志按它找实体）
const ATTACKER_CODE: &str = "attacker";
const DEFENDER_CODE: &str = "defender";
const HUNTER_CODE: &str = "hunter";

/// 生成开局布阵生物（可玩性三连版）
///
/// A 攻击者（红"A"）@(5,0)：直线冲锋逼近，进范围连砍空 payload 击杀刀
/// D 防御者（青"D"）@(15,0)：原地站桩+盾 d_shield 拦截+每 4 tick 反制
/// H 搅局者（黄"H"）@(10,8)：中场上空，落地后观望游走，敌人进 3 格参战（h_enabled 开关）
/// A/D 站地面 y=0：地面实体不受重力，坐标 tick 间稳定，scan/attack_at 的
/// 坐标精确匹配（f64 相等）不被物理漂移打穿。
/// 默认参数（盾 10）双生物剧本 D 的反制先杀死 A；盾 1 则 A 磨穿防守反杀。
/// H 登场后 A 冲锋路线上多一个变数：混战结局由结算顺序与拦截奇偶共同决定。
fn spawn_test_entities(
    world: &mut WorldState,
    sandbox_map: &mut EntitySandboxMap,
    d_shield: i32,
    h_enabled: bool,
) {
    let fuel = 1_000_000; // 100 万条指令配额（fuel trap 会进程 abort，配额必须给足）
    let mut spawn = |code_id: &str, glyph: &str, color: &str, pos: (f64, f64), wat: &[u8]| -> bool {
        let e = Entity::new(
            EntityId::new(),
            PlayerId::new(),
            code_id.to_string(), // code_hash 字段承载生物代码标识（Entity 无 name 字段，MVP 用它区分身份）
            pos,
            Appearance::new(glyph.to_string(), color.to_string(), 1.0),
            world.tick(),
            1_000_000,
        );
        // 沙箱创建失败则不插入实体（避免无沙箱的孤儿实体）
        if sandbox_map.create(e.id, wat, fuel).is_ok() {
            tracing::info!(entity_id = %e.id.0, pos = ?e.pos(), "生物登场：{code_id}");
            world.entities.insert(e.id, e);
            true
        } else {
            tracing::warn!("沙箱创建失败，{code_id} 未登场");
            false
        }
    };
    let a_ok = spawn(ATTACKER_CODE, "A", "red", (5.0, 0.0), &wasm(ATTACKER_WAT));
    let d_ok = spawn(
        DEFENDER_CODE,
        "D",
        "cyan",
        (15.0, 0.0),
        &wasm(&defender_wat(d_shield)),
    );
    // H 搅局者（可选登场）：出生中场上空 (10,8)，受重力 1-2 tick 内落地站稳
    let h_ok = if h_enabled {
        spawn(HUNTER_CODE, "H", "yellow", (10.0, 8.0), &wasm(HUNTER_WAT))
    } else {
        false
    };
    tracing::info!(
        attacker_spawned = a_ok,
        defender_spawned = d_ok,
        hunter_spawned = h_ok,
        distance_a_d = 10.0,
        defender_shield = d_shield,
        "开局布阵：A 攻击者冲锋 / D 防御者站桩（盾每拦一刀-1，每4tick反制）/ H 搅局者={}（投机者：敌人进3格参战，被攻击50%拦截）",
        if h_enabled { "登场" } else { "缺席" }
    );
}

/// WAT 文本 -> WASM 字节码（编译玩家代码模块）
fn wasm(wat_text: &str) -> Vec<u8> {
    wat::parse_bytes(wat_text.as_bytes())
        .expect("WAT 编译失败：检查 WAT 文本语法")
        .into_owned()
}

/// 攻击者 A 的玩家代码 WAT（红色，性格：直线冲锋莽夫）
///
/// 策略（on_tick 每 tick 执行）：
/// 1. scan 找最近敌人（坐标写入自己的 memory[0..8]=x / [8..16]=y），
///    找不到（log 0）就原地待机
/// 2. get_pos 读自己坐标，算朝向向量与欧氏距离
/// 3. 距离 <= 1.5：进入攻击范围（log 2），对敌人坐标排空 payload 击杀刀
///    （attack_at 只排队，实际拦截/换血/击杀由框架两阶段结算）
/// 4. 未进范围（log 3）：归一化方向走 1.0 步（move_by 排队，阶段二应用）
///
/// 无 on_attack 导出 = 不设防：被反制刀命中即被换血击杀。
/// 日志 marker：0=无敌人待机 2=进入攻击范围 3=移动中
const ATTACKER_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "on_tick")
    (local $mx f64) (local $my f64) (local $ex f64) (local $ey f64)
    (local $dx f64) (local $dy f64) (local $dist f64)
    ;; 1. 扫描敌人；没有敌人就待机
    (if (i32.eqz (call $scan (i32.const 0) (i32.const 8)))
      (then
        (call $log (i32.const 0))
        (return)))
    ;; 2. 读敌人坐标（scan 写入 memory）与自己坐标（多值返回先压 x 再压 y）
    (local.set $ex (f64.load (i32.const 0)))
    (local.set $ey (f64.load (i32.const 8)))
    (call $get_pos)
    (local.set $my)
    (local.set $mx)
    ;; 3. 朝向向量与欧氏距离
    (local.set $dx (f64.sub (local.get $ex) (local.get $mx)))
    (local.set $dy (f64.sub (local.get $ey) (local.get $my)))
    (local.set $dist (f64.sqrt
      (f64.add (f64.mul (local.get $dx) (local.get $dx))
               (f64.mul (local.get $dy) (local.get $dy)))))
    ;; 4. 距离 <= 1.5：进入攻击范围，排空 payload 击杀刀（默认击杀 WAT）
    (if (f64.le (local.get $dist) (f64.const 1.5))
      (then
        (call $log (i32.const 2))
        (drop (call $attack_at (local.get $ex) (local.get $ey)
                               (i32.const 0) (i32.const 0)))
        (return)))
    ;; 5. 未进范围：每 tick 归一化方向走 1.0 步
    (call $log (i32.const 3))
    (drop (call $move_by
      (f64.div (local.get $dx) (local.get $dist))
      (f64.div (local.get $dy) (local.get $dist)))))
  (func (export "life") (result i32) i32.const 1))"#;

/// 防御者 D 的玩家代码 WAT（青色，性格：塔防铁壁+定期反击）
///
/// 策略：
/// - on_tick：原地站立（不移动）；每 4 tick 反制一刀——扫描敌人坐标，
///   对该坐标排空 payload 击杀刀（敌人若在移动则坐标过期落空，站定才挨打）
/// - on_attack（有防守）：护盾有余 -> 拦截（log 7 后跟剩余盾量），盾尽 -> 放行（log 8）
///   "配额有限"由玩家代码自己定（盾是规则不是框架能力，架构 4 能力 vs 规则分层）
/// - life()=true；被打穿盾后被换血击杀（默认击杀 WAT life()=false）
///
/// 日志 marker：7=拦截 8=盾尽放行 9=反制
fn defender_wat(shield: i32) -> String {
    format!(
        r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  ;; 护盾余量（拦截配额）：每拦一刀 -1，归零后不再拦截
  (global $shield (mut i32) (i32.const {shield}))
  ;; 已拦截刀数（观测用）
  (global $intercepts (mut i32) (i32.const 0))
  ;; tick 计数：每 4 tick 反制一刀
  (global $ticks (mut i32) (i32.const 0))
  ;; 被攻击：盾有余拦截，盾尽放行
  (func (export "on_attack") (param i32) (result i32)
    global.get $shield
    if (result i32)
      global.get $shield i32.const 1 i32.sub global.set $shield
      global.get $intercepts i32.const 1 i32.add global.set $intercepts
      i32.const 7 call $log
      global.get $shield call $log
      i32.const 1
    else
      i32.const 8 call $log
      i32.const 0
    end)
  ;; 每 tick：计数 +1；非 4 的倍数直接返回；反制刀对扫描到的敌人坐标落刀
  (func (export "on_tick")
    global.get $ticks i32.const 1 i32.add global.set $ticks
    global.get $ticks i32.const 4 i32.rem_u
    if
      return
    end
    (if (i32.eqz (call $scan (i32.const 0) (i32.const 8)))
      (then (return)))
    i32.const 9 call $log
    (drop (call $attack_at (f64.load (i32.const 0)) (f64.load (i32.const 8))
                           (i32.const 0) (i32.const 0))))
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_intercepts") (result i32) global.get $intercepts)
  (func (export "get_shield") (result i32) global.get $shield))"#
    )
}

/// 搅局者 H 的玩家代码 WAT（黄色，性格：投机者——挑软柿子捏，谁近打谁）
///
/// 策略（on_tick 每 tick 执行）：
/// 1. scan 找最近敌人，找不到（log 0）就待机（全场只剩自己时不折腾）
/// 2. 算与最近敌人的距离：
///    - 距离 <= 3.0：参战（log 5），对敌人坐标排空 payload 击杀刀，站定不出游
///    - 距离 > 3.0：观望（log 4），原地小幅游走（x 方向 ±0.5，方向按 tick 奇偶
///      ——WAT 无随机源，用 tick 奇偶当伪随机）
/// 3. on_attack（被攻击时）：50% 拦截（奇数 tick 拦，log 6），否则放行
///    （$ticks 只在 on_tick 自增：拦截答案按 tick 确定性奇偶交替，WAT 无随机源，
///    "50%"指跨 tick 的长期频率，不是不可预测）
///
/// 日志 marker：0=无敌待机 4=观望游走 5=参战攻击 6=拦截成功
const HUNTER_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  ;; tick 计数：on_attack 的 50% 伪随机源（奇=拦截），也是游走方向源
  (global $ticks (mut i32) (i32.const 0))
  ;; 被攻击：奇数 tick 拦截（log 6），偶数放行
  (func (export "on_attack") (param i32) (result i32)
    global.get $ticks i32.const 1 i32.and
    if (result i32)
      i32.const 6 call $log
      i32.const 1
    else
      i32.const 0
    end)
  (func (export "on_tick")
    (local $ex f64) (local $ey f64) (local $mx f64) (local $my f64)
    (local $dx f64) (local $dy f64) (local $dist f64)
    global.get $ticks i32.const 1 i32.add global.set $ticks
    ;; 1. 扫描最近敌人；没有就待机
    (if (i32.eqz (call $scan (i32.const 0) (i32.const 8)))
      (then
        (call $log (i32.const 0))
        (return)))
    (local.set $ex (f64.load (i32.const 0)))
    (local.set $ey (f64.load (i32.const 8)))
    (call $get_pos)
    (local.set $my)
    (local.set $mx)
    (local.set $dx (f64.sub (local.get $ex) (local.get $mx)))
    (local.set $dy (f64.sub (local.get $ey) (local.get $my)))
    (local.set $dist (f64.sqrt
      (f64.add (f64.mul (local.get $dx) (local.get $dx))
               (f64.mul (local.get $dy) (local.get $dy)))))
    ;; 2. 距离 <= 3.0：参战，对敌人坐标排击杀刀
    (if (f64.le (local.get $dist) (f64.const 3.0))
      (then
        (call $log (i32.const 5))
        (drop (call $attack_at (local.get $ex) (local.get $ey)
                               (i32.const 0) (i32.const 0)))
        (return)))
    ;; 3. 观望：原地小幅游走（偶数 tick +0.5，奇数 tick -0.5）
    (call $log (i32.const 4))
    (if (i32.eqz (i32.and (global.get $ticks) (i32.const 1)))
      (then (drop (call $move_by (f64.const 0.5) (f64.const 0.0))))
      (else (drop (call $move_by (f64.const -0.5) (f64.const 0.0))))))
  (func (export "life") (result i32) i32.const 1))"#;

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

    /// 玩家代码 WAT：life()=true + on_tick() 调 log（测试实体默认玩家代码；
    /// 最简可玩版后生产代码不再使用，移入测试模块）
    const PLAYER_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_tick") i32.const 42 call $log))"#;

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
    fn apply_pending_changes_skips_setpos_for_dead_entity() {
        // P2（审查 1 轮）：同 tick 内 life()=false 实体排队的移动不应用（尸体不位移），
        // 死亡清理步随后移除实体，SetPos 目标无人消费属顺序耦合，直接在应用侧作废
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(5));
        let mut e = make_entity(5, (2.0, 3.0));
        e.kill(); // 预先标记死亡（模拟同 tick life()=false）
        world.entities.insert(id, e);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id,
            new_pos: (9.0, 9.0),
        }];
        apply_pending_changes(&mut world, &mut pending);
        assert_eq!(
            world.entities.get(&id).unwrap().pos(),
            (2.0, 3.0),
            "死亡实体的 SetPos 不应应用"
        );
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
        // 经典双生物布阵（H 关闭）：A 攻击者红"A"@(5,0)，D 防御者青"D"@(15,0)。
        // 都站地面 y=0（地面实体不受重力，坐标稳定，scan/attack 坐标精确匹配）。
        let mut world = WorldState::new();
        let mut sandbox_map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut sandbox_map, 10, false);
        assert_eq!(world.entities.len(), 2, "H 关闭时应创建两只生物");
        let attacker = world
            .entities
            .values()
            .find(|e| e.code_hash == "attacker")
            .expect("应有攻击者 A");
        assert_eq!(attacker.pos(), (5.0, 0.0), "A 应在 (5,0)");
        assert_eq!(attacker.appearance.glyph, "A");
        assert_eq!(attacker.appearance.color, "red");
        let defender = world
            .entities
            .values()
            .find(|e| e.code_hash == "defender")
            .expect("应有防御者 D");
        assert_eq!(defender.pos(), (15.0, 0.0), "D 应在 (15,0)");
        assert_eq!(defender.appearance.glyph, "D");
        assert_eq!(defender.appearance.color, "cyan");
        // 两只都挂了沙箱（存活实体必有沙箱）
        assert!(
            sandbox_map.sandboxes.contains_key(&attacker.id)
                && sandbox_map.sandboxes.contains_key(&defender.id),
            "两只生物都应有沙箱"
        );
    }

    #[test]
    fn spawn_test_entities_three_with_h_enabled() {
        // 混战布阵（H 登场）：第三只生物 H 黄"H"@(10,8) 中场上空，落地后受重力站稳
        let mut world = WorldState::new();
        let mut sandbox_map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut sandbox_map, 10, true);
        assert_eq!(world.entities.len(), 3, "H 登场时应创建三只生物");
        let hunter = world
            .entities
            .values()
            .find(|e| e.code_hash == "hunter")
            .expect("应有搅局者 H");
        assert_eq!(hunter.pos(), (10.0, 8.0), "H 应在 (10,8) 中场上空");
        assert_eq!(hunter.appearance.glyph, "H");
        assert_eq!(hunter.appearance.color, "yellow");
        assert!(sandbox_map.sandboxes.contains_key(&hunter.id), "H 应有沙箱");
    }

    #[test]
    fn spawn_test_entities_d_shield_passthrough() {
        // ② 翻盘实验：d_shield 参数透传到 D 沙箱的盾全局（观测导出 get_shield）
        let mut world = WorldState::new();
        let mut sandbox_map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut sandbox_map, 1, false);
        let d = world
            .entities
            .values()
            .find(|e| e.code_hash == "defender")
            .expect("应有防御者 D")
            .id;
        assert_eq!(
            call_i32_export(&mut sandbox_map, &d, "get_shield"),
            1,
            "d_shield=1 应透传到 D 的盾全局（翻盘剧本）"
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

    /// 防守计数模块 WAT：on_attack 每被调一次全局计数 +1，返回参数==魔法值时拦截
    /// （0x01010101：raw 字节 [1,1,1,1] payload 的 LE i32，用于区分"哪一刀"）
    const COUNTER_DEFENSE_WAT: &str = r#"(module
  (global $count (mut i32) (i32.const 0))
  (func (export "on_attack") (param i32) (result i32)
    global.get $count i32.const 1 i32.add global.set $count
    local.get 0 i32.const 16843009 i32.eq)
  (func (export "get_count") (result i32) global.get $count)
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_ver") (result i32) i32.const 111))"#;

    /// 恒拦截计数 WAT：on_attack 计数 + 恒返回 1（验收#5a 两刀全拦）
    const COUNTER_ALWAYS_INTERCEPT_WAT: &str = r#"(module
  (global $count (mut i32) (i32.const 0))
  (func (export "on_attack") (param i32) (result i32)
    global.get $count i32.const 1 i32.add global.set $count
    i32.const 1)
  (func (export "get_count") (result i32) global.get $count)
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_ver") (result i32) i32.const 111))"#;

    /// 带版本号的击晕模块 WAT：life()=true + get_ver()=ver（观测"谁的代码在目标里"）
    fn ver_module(ver: i32) -> String {
        format!(
            r#"(module
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_ver") (result i32) i32.const {ver}))"#
        )
    }

    /// 带版本号的击杀模块 WAT：life()=false + get_ver()=ver
    fn kill_ver_module(ver: i32) -> String {
        format!(
            r#"(module
  (func (export "life") (result i32) i32.const 0)
  (func (export "get_ver") (result i32) i32.const {ver}))"#
        )
    }

    /// 从实体当前沙箱调用无参 i32 导出（测试观测：计数器/版本号）
    fn call_i32_export(map: &mut EntitySandboxMap, id: &EntityId, name: &str) -> i32 {
        let handle = map.sandboxes.get_mut(id).unwrap();
        let func = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, name)
            .unwrap();
        func.call(&mut handle.store, ()).unwrap()
    }

    /// 从实体当前沙箱调用无参 f64 导出（测试观测：scan 写入的坐标）
    fn call_f64_export(map: &mut EntitySandboxMap, id: &EntityId, name: &str) -> f64 {
        let handle = map.sandboxes.get_mut(id).unwrap();
        let func = handle
            .instance
            .get_typed_func::<(), f64>(&mut handle.store, name)
            .unwrap();
        func.call(&mut handle.store, ()).unwrap()
    }

    #[test]
    fn defend_intercepts() {
        // 收件：目标当前沙箱 on_attack 返回 1 -> 拦截
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(6));
        map.create(target_id, &wasm(DEFENDED_TARGET_WAT), 1_000_000).unwrap();
        assert!(
            map.defend(&target_id, &[]).unwrap(),
            "on_attack 返回 1 应拦截"
        );
    }

    #[test]
    fn defend_no_sandbox_errors() {
        // 边界：目标无沙箱 -> Err（不 panic）
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(8));
        assert!(map.defend(&target_id, &[]).is_err(), "无沙箱目标应返回 Err");
    }

    #[test]
    fn replace_code_empty_payload_kills() {
        // 结算：空 payload -> 默认击杀 WAT（life=false）-> 替换后 life()=false -> 击杀成功
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(5));
        map.create(target_id, &wasm(TARGET_WAT), 1_000_000).unwrap();
        let killed = map.replace_code(&target_id, &[]).unwrap();
        assert!(killed, "空 payload 默认击杀，life 应为 false");
    }

    #[test]
    fn replace_code_payload_life_true_not_killed() {
        // 结算：payload 为 life=true 的 WASM -> 替换后 life()=true -> 命中但未击杀（如击晕）
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(7));
        map.create(target_id, &wasm(TARGET_WAT), 1_000_000).unwrap();
        let killed = map.replace_code(&target_id, &wasm(TARGET_WAT)).unwrap();
        assert!(!killed, "life=true payload 不应击杀");
    }

    #[test]
    fn replace_code_no_sandbox_errors() {
        // 边界：目标无沙箱 -> Err（不 panic）
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(9));
        assert!(
            map.replace_code(&target_id, &[]).is_err(),
            "无沙箱目标应返回 Err"
        );
    }

    #[test]
    fn replace_code_garbage_payload_keeps_target_sandbox() {
        // P0 僵尸修复（验收#1）：垃圾字节 payload 编译失败 -> Err，但目标沙箱条目仍在
        // 且可继续跑玩家代码（原 life()=true 模块未被破坏）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(10));
        world.entities.insert(id, make_entity(10, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(id, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        let garbage: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        let result = map.replace_code(&id, garbage);
        assert!(result.is_err(), "垃圾 payload 应换血失败");
        // 不变量：存活实体必有沙箱 —— 条目仍在且原代码可用
        assert!(map.sandboxes.contains_key(&id), "失败后目标沙箱条目应仍在");
        let to_kill = map.run_player_code(&world);
        assert!(
            !to_kill.contains(&id),
            "失败后原沙箱应可继续 on_tick/life（to_kill={:?}）",
            to_kill
        );
    }

    #[test]
    fn replace_code_uses_remaining_fuel_not_limit() {
        // P1（验收#4）+ 审查 P1-2：换血继承旧沙箱【当 tick 剩余量】（防吃刀回满刷配额），
        // 但 fuel_limit 恢复原配额（防反复被攻击单调递减磨死，绕过"超额不死亡"保护）
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(11));
        map.create(target_id, &wasm(TARGET_WAT), 1_000_000).unwrap();
        // 模拟已消耗过半：直接把剩余量压到 500
        {
            let handle = map.sandboxes.get_mut(&target_id).unwrap();
            handle.store.set_fuel(500).unwrap();
        }
        map.replace_code(&target_id, &wasm(TARGET_WAT)).unwrap();
        let handle = map.sandboxes.get(&target_id).unwrap();
        assert_eq!(
            handle.store.data().fuel_limit,
            1_000_000,
            "新沙箱配额上限应恢复原配额，不随换血缩水"
        );
        let remaining = handle.store.get_fuel().unwrap();
        assert!(
            remaining < 500,
            "新沙箱应从剩余量 500 起步（life() 消耗后 <500），实际 {remaining}"
        );
    }

    /// 缺 life 导出的模块（换血后 call_life 无法执行 -> Err，无 wasm trap 的干净构造）
    const NO_LIFE_WAT: &str = r#"(module (func (export "on_tick")))"#;

    /// on_attack 签名不匹配的模块（defend 询问失败 -> Err，无 wasm trap 的干净构造）
    const BAD_SIGNATURE_ON_ATTACK_WAT: &str = r#"(module
  (func (export "on_attack") (param i64) (result i64) i64.const 1)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn replace_code_life_unavailable_not_killed() {
        // 审查 P1-1：换血后 life() 无法执行（Err）-> 按"life 维持原值"处理不判死，
        // 防止良性刀在防守 fuel 被磨穿的边界下误收割"击杀+掠夺"
        // （架构 4.2 默认行为延伸：配额耗尽/结果未知时 life 不被覆写）。
        // 构造用"缺 life 导出"（host 侧 Err）：wasm 内 out-of-fuel trap 在 Windows +
        // wasmtime 25.0.3 下会 nounwind abort 进程，无法安全构造真实 fuel-trap 路径，
        // Err 分支行为二者等价（fuel 磨穿到 instantiate 都烧不起时走 P0 路径同样不判死）。
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(13));
        map.create(target_id, &wasm(TARGET_WAT), 1_000_000).unwrap();
        let killed = map
            .replace_code(&target_id, &wasm(NO_LIFE_WAT))
            .unwrap();
        assert!(!killed, "life() 无法执行时不应判死");
        // 沙箱条目仍在（新模块已挂上，但本刀未达成击杀效果）
        assert!(map.sandboxes.contains_key(&target_id), "换血后沙箱条目应仍在");
    }

    #[test]
    fn process_attacks_defense_exhausted_blade_void() {
        // 需求审查 P1（补覆盖）：防守询问失败 -> 该刀作废，目标不被换血、存活。
        // 构造用"on_attack 签名不匹配"（host 侧 Err）：wasm 内 out-of-fuel trap 在
        // Windows + wasmtime 25.0.3 下会 nounwind abort 进程（见 .cc-bugs #5），
        // 无法在测试中安全构造真实 fuel-trap 路径，Err 处理行为二者等价。
        // "未被换血"用 api_calls 哨兵判定：换血=新 store 会清空哨兵。
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 50, (0.0, 0.0), BAD_SIGNATURE_ON_ATTACK_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 51, (5.0, 0.0), PLAYER_WAT);
        {
            let handle = map.sandboxes.get_mut(&a).unwrap();
            handle.store.data_mut().api_calls.push("sentinel".to_string());
        }
        let attacks = vec![PendingAttack {
            attacker: b,
            target_pos: (0.0, 0.0),
            payload: vec![9],
        }];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(world.entities.get(&a).unwrap().is_alive(), "防守失败 -> 刀作废，A 应存活");
        let handle = map.sandboxes.get(&a).unwrap();
        assert!(
            handle.store.data().api_calls.contains(&"sentinel".to_string()),
            "A 应仍是原沙箱（未被换血，哨兵应保留）"
        );
    }

    #[test]
    fn replace_code_garbage_fuel_unchanged() {
        // P0 附带：换血失败时目标 fuel 不变（旧沙箱原样保留，未被重置）
        let mut map = test_sandbox_map();
        let target_id = EntityId(Uuid::from_u128(12));
        map.create(target_id, &wasm(TARGET_WAT), 1_000_000).unwrap();
        {
            let handle = map.sandboxes.get_mut(&target_id).unwrap();
            handle.store.set_fuel(777).unwrap();
        }
        let garbage: &[u8] = &[0xDE, 0xAD, 0xBE, 0xEF];
        assert!(map.replace_code(&target_id, garbage).is_err());
        let handle = map.sandboxes.get(&target_id).unwrap();
        assert_eq!(handle.store.get_fuel().unwrap(), 777, "失败后 fuel 应原样保留");
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

    // === 两阶段攻击模型（收件：按目标分组逐刀 on_attack；结算：固定顺序执行）===

    #[test]
    fn process_attacks_dead_attacker_blade_void_after_victim_killed_first() {
        // 验收#2：同批 A(id1) 杀 B(id2)、B 也排了刀打 A。
        // 结算按攻击者 id 升序：A 的刀先执行杀 B；轮到 B 的刀时 B 已死 -> 作废不执行。
        // 旧实现不查攻击者存活，B 的刀照跑会把 A 也杀了（或掠夺蒸发）。
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 1, (0.0, 0.0), PLAYER_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.0), TARGET_WAT);
        let attacks = vec![
            PendingAttack {
                attacker: a,
                target_pos: (5.0, 0.0),
                payload: vec![], // A -> B：默认击杀
            },
            PendingAttack {
                attacker: b,
                target_pos: (0.0, 0.0),
                payload: vec![], // B -> A：默认击杀（B 死后应作废）
            },
        ];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(!world.entities.get(&b).unwrap().is_alive(), "B 应被 A 击杀");
        assert!(world.entities.get(&a).unwrap().is_alive(), "B 死后其刀应作废，A 应存活");
        assert_eq!(
            world.entities.get(&a).unwrap().survival.points(),
            4,
            "A 掠夺 B 的 1 点；B 的刀作废 -> 点数不蒸发"
        );
        assert_eq!(world.entities.get(&b).unwrap().survival.points(), 2, "B 只被掠夺一次");
    }

    #[test]
    fn process_attacks_voids_attack_from_predeceased_attacker() {
        // 收件前过滤：攻击者在排队后、处理前已死（如阶段一 life()=false）->
        // 其攻击完全作废，防守方的 on_attack 不被调用（不烧防守 fuel）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 20, (0.0, 0.0), COUNTER_ALWAYS_INTERCEPT_WAT);
        let dead = make_entity_with_sandbox(&mut world, &mut map, 21, (5.0, 0.0), TARGET_WAT);
        // 攻击者在排队后死亡（模拟阶段一 life()=false / Destroy）
        world.entities.get_mut(&dead).unwrap().kill();
        let attacks = vec![PendingAttack {
            attacker: dead,
            target_pos: (0.0, 0.0),
            payload: vec![],
        }];
        process_attacks(&mut world, &mut map, &attacks);
        assert_eq!(call_i32_export(&mut map, &a, "get_count"), 0, "死者攻击不应触发防守方 on_attack");
        assert!(world.entities.get(&a).unwrap().is_alive(), "防守方应完好");
    }

    #[test]
    fn process_attacks_two_attackers_both_intercepted() {
        // 验收#5a：B、C 同 tick 攻击 A，A 的 on_attack 被调用两次（同一原版沙箱），
        // 两刀全拦 -> A 完好、代码未换、点数不变
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 30, (0.0, 0.0), COUNTER_ALWAYS_INTERCEPT_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 31, (5.0, 0.0), PLAYER_WAT);
        let c = make_entity_with_sandbox(&mut world, &mut map, 32, (6.0, 0.0), PLAYER_WAT);
        let attacks = vec![
            PendingAttack { attacker: b, target_pos: (0.0, 0.0), payload: vec![9] },
            PendingAttack { attacker: c, target_pos: (0.0, 0.0), payload: vec![9] },
        ];
        process_attacks(&mut world, &mut map, &attacks);
        assert_eq!(call_i32_export(&mut map, &a, "get_count"), 2, "A 的 on_attack 应被调两次");
        assert_eq!(call_i32_export(&mut map, &a, "get_ver"), 111, "两刀全拦 -> A 仍是原版代码");
        assert!(world.entities.get(&a).unwrap().is_alive(), "A 应完好");
        assert_eq!(world.entities.get(&a).unwrap().survival.points(), 3, "拦截不扣点");
    }

    #[test]
    fn process_attacks_two_attackers_one_intercepted() {
        // 验收#5b：只拦 B（payload 前 4 字节 == 魔法值才拦截）-> 仅 C 的攻击执行（A 换成 C 的代码）
        // B 的 payload 用 raw 字节 [1,1,1,1]（LE i32 == 16843009）：被拦截故无需是合法 WASM
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 30, (0.0, 0.0), COUNTER_DEFENSE_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 31, (5.0, 0.0), PLAYER_WAT);
        let c = make_entity_with_sandbox(&mut world, &mut map, 32, (6.0, 0.0), PLAYER_WAT);
        let c_payload = wasm(&ver_module(3001));
        let attacks = vec![
            PendingAttack { attacker: b, target_pos: (0.0, 0.0), payload: vec![1, 1, 1, 1] },
            PendingAttack { attacker: c, target_pos: (0.0, 0.0), payload: c_payload },
        ];
        process_attacks(&mut world, &mut map, &attacks);
        // 注：换血后 A 的代码已是 C 的 payload（无 get_count 导出），
        // "两刀都问过 on_attack" 由 both_intercepted 测试的计数断言覆盖
        assert_eq!(
            call_i32_export(&mut map, &a, "get_ver"),
            3001,
            "只拦 B -> 仅 C 的换血执行，A 应是 C 的代码"
        );
        assert!(world.entities.get(&a).unwrap().is_alive(), "C 的 payload life=true，A 存活");
    }

    #[test]
    fn process_attacks_two_attackers_none_intercepted_order_by_attacker_id() {
        // 验收#5c：全不拦 -> 按攻击者 EntityId 升序执行（确定序）。
        // 两刀都是击晕模块（life=true）：两刀都会执行，最终代码 = 后执行者。
        // B(id31) 先执行、C(id32) 后执行 -> A 最终是 C 的代码（ver 3001）。
        // 旧实现按 attacks 切片顺序执行：这里故意把 C 的刀排在前面 -> 旧实现最终 ver 会是 2001。
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 30, (0.0, 0.0), TARGET_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 31, (5.0, 0.0), PLAYER_WAT);
        let c = make_entity_with_sandbox(&mut world, &mut map, 32, (6.0, 0.0), PLAYER_WAT);
        let attacks = vec![
            PendingAttack { attacker: c, target_pos: (0.0, 0.0), payload: wasm(&ver_module(3001)) },
            PendingAttack { attacker: b, target_pos: (0.0, 0.0), payload: wasm(&ver_module(2001)) },
        ];
        process_attacks(&mut world, &mut map, &attacks);
        assert_eq!(
            call_i32_export(&mut map, &a, "get_ver"),
            3001,
            "结算按攻击者 id 升序：B(31) 先、C(32) 后 -> 最终是 C 的代码"
        );
        assert!(world.entities.get(&a).unwrap().is_alive(), "击晕不击杀");
        assert_eq!(
            world.entities.get(&a).unwrap().survival.points(),
            3,
            "未击杀无掠夺"
        );
    }

    #[test]
    fn process_attacks_first_killer_blocks_later_blades() {
        // 验收#5c 变体：先执行的刀击杀目标 -> 后到的刀因目标已死跳过（先执行者的换血生效）
        // B(id31) 的刀是击杀模块（life=false），C(id32) 的刀是击晕模块（ver 3001）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 30, (0.0, 0.0), TARGET_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 31, (5.0, 0.0), PLAYER_WAT);
        let c = make_entity_with_sandbox(&mut world, &mut map, 32, (6.0, 0.0), PLAYER_WAT);
        let attacks = vec![
            PendingAttack { attacker: c, target_pos: (0.0, 0.0), payload: wasm(&ver_module(3001)) },
            PendingAttack { attacker: b, target_pos: (0.0, 0.0), payload: wasm(&kill_ver_module(2002)) },
        ];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(!world.entities.get(&a).unwrap().is_alive(), "B 的击杀刀应生效");
        // A 已死，后续观测其代码版本：沙箱此时仍在 map（死亡清理在 tick 步 d），读 ver 应为 2002
        assert_eq!(
            call_i32_export(&mut map, &a, "get_ver"),
            2002,
            "先执行者（B）的换血生效，C 的刀因目标已死未执行"
        );
        assert_eq!(
            world.entities.get(&b).unwrap().survival.points(),
            4,
            "B 击杀掠夺 +1"
        );
        assert_eq!(
            world.entities.get(&c).unwrap().survival.points(),
            3,
            "C 的刀未执行，无掠夺"
        );
    }

    #[test]
    fn process_attacks_defense_fuel_drains_per_blade() {
        // C.3 防守消耗自然成立：每次 on_attack 消耗防守者 fuel（围殴=磨算力），
        // 两刀拦截后防守方 fuel 剩余量应低于满配额
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 40, (0.0, 0.0), COUNTER_ALWAYS_INTERCEPT_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 41, (5.0, 0.0), PLAYER_WAT);
        let attacks = vec![
            PendingAttack { attacker: b, target_pos: (0.0, 0.0), payload: vec![9] },
        ];
        process_attacks(&mut world, &mut map, &attacks);
        assert_eq!(call_i32_export(&mut map, &a, "get_count"), 1);
        let handle = map.sandboxes.get(&a).unwrap();
        let remaining = handle.store.get_fuel().unwrap();
        assert!(
            remaining < 1_000_000,
            "on_attack 消耗防守 fuel，剩余应低于满配额，实际 {remaining}"
        );
    }

    // === 攻击全链路集成（验收#1：玩家代码调 attack_at import）===

    /// 攻击方 WAT：导出 memory + on_tick 调 env.attack_at(5.0, 0.0, 0, 0)（空 payload 默认击杀）
    /// 命名注意：与顶层生物 ATTACKER_WAT 区分，防模块内同名遮蔽（踩坑实录见 .cc-bugs）
    const PIPELINE_ATTACKER_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "on_tick")
    f64.const 5.0 f64.const 0.0 i32.const 0 i32.const 0 call $attack_at drop)
  (func (export "life") (result i32) i32.const 1))"#;

    // === find_nearest_enemy（感知规则：scan 的数据源）===

    #[test]
    fn find_nearest_enemy_picks_closest_alive_other() {
        // 规则：最近的其他存活实体（上帝视角单敌人 MVP 感知）
        let mut world = WorldState::new();
        let me = EntityId(Uuid::from_u128(1));
        let near = EntityId(Uuid::from_u128(2));
        let far = EntityId(Uuid::from_u128(3));
        world.entities.insert(me, make_entity(1, (0.0, 0.0)));
        world.entities.insert(near, make_entity(2, (3.0, 4.0))); // 距离 5
        world.entities.insert(far, make_entity(3, (30.0, 40.0))); // 距离 50
        let found = find_nearest_enemy(&world, &me);
        assert_eq!(found, Some((3.0, 4.0)), "应锁定距离 5 的最近实体");
    }

    #[test]
    fn find_nearest_enemy_excludes_self() {
        // 规则：排除自己（不能把自己当敌人锁定）
        let mut world = WorldState::new();
        let me = EntityId(Uuid::from_u128(1));
        world.entities.insert(me, make_entity(1, (7.0, 8.0)));
        let found = find_nearest_enemy(&world, &me);
        assert_eq!(found, None, "独存实体不应锁定自己");
    }

    #[test]
    fn find_nearest_enemy_excludes_dead() {
        // 规则：排除死亡实体（尸体不是感知目标）
        let mut world = WorldState::new();
        let me = EntityId(Uuid::from_u128(1));
        let corpse = EntityId(Uuid::from_u128(2));
        let mut corpse_e = make_entity(2, (1.0, 0.0));
        corpse_e.kill();
        world.entities.insert(me, make_entity(1, (0.0, 0.0)));
        world.entities.insert(corpse, corpse_e);
        let found = find_nearest_enemy(&world, &me);
        assert_eq!(found, None, "唯一邻居已死，应返回 None");
    }

    // === run_player_code 感知快照填充（get_pos/scan 的数据通路）===

    /// 扫描探针 WAT（同 arena 测试）：on_tick 调 scan(0,8) 存返回值，
    /// 导出读回函数供 host 断言快照填充是否到位
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
    fn run_player_code_fills_perception_snapshot_for_scan() {
        // 数据通路：run_player_code 每 tick 填充 SandboxData.pos/nearest_enemy，
        // 玩家代码 on_tick 里 scan 拿到的是 world 当前（tick 开始时）状态。
        let mut world = WorldState::new();
        let a = EntityId(Uuid::from_u128(1));
        let b = EntityId(Uuid::from_u128(2));
        world.entities.insert(a, make_entity(1, (0.0, 0.0)));
        world.entities.insert(b, make_entity(2, (5.0, 7.0)));
        let mut map = test_sandbox_map();
        map.create(a, &wasm(SCAN_PROBE_WAT), 1_000_000).unwrap();
        map.create(b, &wasm(SCAN_PROBE_WAT), 1_000_000).unwrap();
        map.run_player_code(&world);
        // A 视角：最近敌人是 B (5,7)
        assert_eq!(call_i32_export(&mut map, &a, "get_found"), 1, "A 应扫描到 B");
        let x = call_f64_export(&mut map, &a, "get_enemy_x");
        let y = call_f64_export(&mut map, &a, "get_enemy_y");
        assert_eq!(x, 5.0, "scan 写入的敌人 x 应为 B 的坐标");
        assert_eq!(y, 7.0, "scan 写入的敌人 y 应为 B 的坐标");
        // B 视角：最近敌人是 A (0,0)
        assert_eq!(call_i32_export(&mut map, &b, "get_found"), 1, "B 应扫描到 A");
        let x = call_f64_export(&mut map, &b, "get_enemy_x");
        assert_eq!(x, 0.0, "scan 写入的敌人 x 应为 A 的坐标");
    }

    #[test]
    fn run_player_code_scan_zero_when_alone() {
        // 负面路径：世界只有一个实体 -> scan 返回 0（找不到敌人）
        let mut world = WorldState::new();
        let a = EntityId(Uuid::from_u128(1));
        world.entities.insert(a, make_entity(1, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(a, &wasm(SCAN_PROBE_WAT), 1_000_000).unwrap();
        map.run_player_code(&world);
        assert_eq!(
            call_i32_export(&mut map, &a, "get_found"),
            0,
            "独存实体 scan 应返回 0"
        );
    }

    // === move_by 全链路（import 排队 -> tick 阶段二应用）===

    /// 右移一步 WAT：on_tick 调 move_by(1.0, 0.0)
    const RIGHT_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (func (export "on_tick")
    f64.const 1.0 f64.const 0.0 call $move_by drop)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn tick_applies_move_by_queued_in_phase_one() {
        // 全链路：on_tick 调 move_by 排队 SetPos -> tick 阶段二 a 步统一应用 -> 坐标变更。
        // 实体站地面 (0,0)：物理无干扰（地面实体不受重力），tick 后应恰在 (1.0, 0.0)。
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        sandbox_map.create(id, &wasm(RIGHT_MOVER_WAT), 1_000_000).unwrap();
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        assert_eq!(
            world.entities.get(&id).unwrap().pos(),
            (1.0, 0.0),
            "move_by 排队的 SetPos 应在阶段二应用（右移 1 步）"
        );
    }

    #[test]
    fn collect_pending_moves_drains_sandboxes() {
        // collect_pending_moves：drain 各沙箱 pending_moves 并清空（不跨 tick 残留）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(id, &wasm(RIGHT_MOVER_WAT), 1_000_000).unwrap();
        map.run_player_code(&world);
        let moves = map.collect_pending_moves();
        assert_eq!(moves.len(), 1, "应收集 1 条移动请求");
        assert!(
            map.collect_pending_moves().is_empty(),
            "收集后应清空（不跨调用残留）"
        );
    }

    // === 两只真性格生物（攻击者 A / 防御者 D）：行为与胜负演进 ===

    /// 构造自定义外观实体（生物测试用：A 红 / D 青，glyph 各自设定）
    fn make_creature(id_u: u128, pos: (f64, f64), name: &str, glyph: &str, color: &str) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(99)),
            name.to_string(),
            pos,
            Appearance::new(glyph.to_string(), color.to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    /// 战场脚手架：A(攻击者)@(5,0) 红、D(防御者)@(15,0) 青，各挂玩家代码沙箱，
    /// 返回 (world, time, physics, renderer, storage, pending, sandbox_map, a_id, d_id)
    /// 渲染器用宽屏 50x80（对齐真实配置；字幕整行 47 字符需完整放下供断言）
    #[allow(clippy::type_complexity)]
    fn battle_stage(
        defender_shield: i32,
    ) -> (
        WorldState,
        TimeEngine,
        DefaultPhysicsEngine,
        Renderer,
        StorageEngine,
        Vec<PendingChange>,
        EntitySandboxMap,
        EntityId,
        EntityId,
    ) {
        let mut world = WorldState::new();
        let a = EntityId(Uuid::from_u128(1));
        let d = EntityId(Uuid::from_u128(2));
        world
            .entities
            .insert(a, make_creature(1, (5.0, 0.0), "attacker", "A", "red"));
        world
            .entities
            .insert(d, make_creature(2, (15.0, 0.0), "defender", "D", "cyan"));
        let mut sandbox_map = test_sandbox_map();
        sandbox_map
            .create(a, &wasm(ATTACKER_WAT), 1_000_000)
            .unwrap();
        sandbox_map
            .create(d, &wasm(&defender_wat(defender_shield)), 1_000_000)
            .unwrap();
        (
            world,
            TimeEngine::new(),
            DefaultPhysicsEngine::new(),
            Renderer::new(Viewport::new((0.0, 0.0), 50.0, 25.0, 50, 80)),
            StorageEngine::open_in_memory().unwrap(),
            Vec::new(),
            sandbox_map,
            a,
            d,
        )
    }

    /// 跑 N 个 tick（测试内无 sleep，tick() 纯逻辑）
    #[allow(clippy::too_many_arguments)]
    fn run_ticks(
        world: &mut WorldState,
        time: &mut TimeEngine,
        physics: &DefaultPhysicsEngine,
        renderer: &mut Renderer,
        storage: &mut StorageEngine,
        pending: &mut Vec<PendingChange>,
        sandbox_map: &mut EntitySandboxMap,
        n: u64,
    ) {
        for _ in 0..n {
            tick(world, time, physics, renderer, storage, pending, sandbox_map);
        }
    }

    #[test]
    fn attacker_approaches_defender_step_by_step() {
        // A 行为：开局 scan 锁定 D，逐 tick 朝 D 走 1.0 步（10.0 -> 1.0，轨迹单调递减），
        // 进攻击范围（<=1.5）前不动手（未发生攻击 = D 拦截计数 0）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, a, d) =
            battle_stage(10);
        let mut prev_dist = 10.0f64;
        for step in 1..=9u64 {
            run_ticks(
                &mut world,
                &mut time,
                &physics,
                &mut renderer,
                &mut storage,
                &mut pending,
                &mut map,
                1,
            );
            let pa = world.entities.get(&a).unwrap().pos();
            let pd = world.entities.get(&d).unwrap().pos();
            let dist = ((pa.0 - pd.0).powi(2) + (pa.1 - pd.1).powi(2)).sqrt();
            assert!(
                dist < prev_dist,
                "第 {step} tick A 应逼近 D：{prev_dist} -> {dist}"
            );
            prev_dist = dist;
        }
        let pa = world.entities.get(&a).unwrap().pos();
        assert_eq!(
            pa,
            (14.0, 0.0),
            "9 tick 后 A 应走到 (14,0)（每 tick 恰走 1.0）"
        );
        assert!(prev_dist <= 1.5, "9 tick 后应进入攻击范围，实际距离 {prev_dist}");
        assert_eq!(
            call_i32_export(&mut map, &d, "get_intercepts"),
            0,
            "进范围前 A 未攻击，D 不应有拦截记录"
        );
    }

    #[test]
    fn battle_defender_counters_and_kills_attacker() {
        // 结局一（默认参数，盾=10）：A 进范围连砍 3 刀（tick 10/11/12）全被 D 拦截，
        // D 每 4 tick 反制，tick 12 的反制刀命中静止的 A（A 无 on_attack）
        // -> A 被击杀、D 掠夺 1 点。全程走两阶段结算（拦截在收件、击杀在结算）。
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, a, d) =
            battle_stage(10);
        run_ticks(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            15,
        );
        assert!(!world.entities.contains_key(&a), "A 应被 D 的反制刀击杀");
        assert!(world.entities.get(&d).unwrap().is_alive(), "D 应存活");
        assert_eq!(
            world.entities.get(&d).unwrap().survival.points(),
            4,
            "D 应掠夺 A 的 1 点（3->4）"
        );
        assert_eq!(
            call_i32_export(&mut map, &d, "get_intercepts"),
            3,
            "D 应拦截 A 的 3 刀（tick 10/11/12）"
        );
        assert_eq!(
            call_i32_export(&mut map, &d, "get_shield"),
            7,
            "盾 10 被磨掉 3，应剩 7"
        );
    }

    #[test]
    fn battle_attacker_breaks_thin_shield_and_kills_defender() {
        // 结局二（参数路径，盾=1）：A 磨穿防守——tick 10 耗掉最后一格盾，
        // tick 11 的刀不再被拦截 -> 结算换血击杀 D、A 掠夺 1 点。
        // 盾=1 时 D 的下一轮反制（tick 12）永远等不到，结果确定非随机。
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, a, d) =
            battle_stage(1);
        run_ticks(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            12,
        );
        assert!(!world.entities.contains_key(&d), "D 应被 A 磨穿防守后击杀");
        assert!(world.entities.get(&a).unwrap().is_alive(), "A 应存活");
        assert_eq!(
            world.entities.get(&a).unwrap().survival.points(),
            4,
            "A 应掠夺 D 的 1 点（3->4）"
        );
    }

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
            make_entity_with_sandbox(&mut world, &mut sandbox_map, 1, (0.0, 0.0), PIPELINE_ATTACKER_WAT);
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

    // === 战况字幕：盾量 marker 解析与收集 ===

    /// 连发两个 marker 的 WAT（字幕收集测试用，与 arena 侧 DOUBLE_LOG_WAT 同款；
    /// 本地重定义避免跨 crate 引用 tests 内私有常量）
    const CAPTION_LOG_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (func (export "on_tick")
    i32.const 7 call $log
    i32.const 5 call $log)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn parse_shield_markers_pair_reports_value() {
        // 规则：log(7)（拦截）后紧跟的值 = D 上报的剩余盾量
        assert_eq!(parse_shield_markers(&[7, 9]), Some(9), "拦截对 [7,9] 应上报盾量 9");
    }

    #[test]
    fn parse_shield_markers_latest_pair_wins() {
        // 规则：多次拦截取最后一对的值；反制 marker 9 混在流中不干扰位置配对
        // 序列语义：反制9 | 拦截+盾9 | 反制9 | 拦截+盾8 | 反制9 | 拦截+盾7
        let markers = [9, 7, 9, 9, 7, 8, 9, 7, 7];
        assert_eq!(parse_shield_markers(&markers), Some(7), "最后一对 [7,7] 应上报盾量 7");
    }

    #[test]
    fn parse_shield_markers_exhausted_marker_means_zero() {
        // 规则：log(8)（盾尽放行）= 盾 0；盾尽后不再有配对值
        assert_eq!(parse_shield_markers(&[7, 1, 8]), Some(0), "盾尽 marker 应解析为 0");
    }

    #[test]
    fn parse_shield_markers_non_defender_markers_mean_none() {
        // 负面路径：A 的 marker（0/2/3）不含盾上报 -> None（字幕显示占位符）
        assert_eq!(parse_shield_markers(&[0, 2, 3]), None);
    }

    #[test]
    fn parse_shield_markers_empty_and_truncated() {
        // 边界：空流 -> None；截断对（只有 7 没有盾值）-> None（不臆测）
        assert_eq!(parse_shield_markers(&[]), None);
        assert_eq!(parse_shield_markers(&[7]), None, "截断对不应臆测盾量");
    }

    #[test]
    fn collect_all_log_markers_drains_all_sandboxes_and_clears() {
        // 收集：drain 全部沙箱的 log_markers 并清空（不跨 tick 残留）
        let mut world = WorldState::new();
        let a = EntityId(Uuid::from_u128(71));
        let b = EntityId(Uuid::from_u128(72));
        world.entities.insert(a, make_entity(71, (0.0, 0.0)));
        world.entities.insert(b, make_entity(72, (5.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(a, &wasm(CAPTION_LOG_WAT), 1_000_000).unwrap();
        map.create(b, &wasm(CAPTION_LOG_WAT), 1_000_000).unwrap();
        // 触发两个沙箱各 log 两次（marker 7,5）
        map.run_player_code(&world);
        let markers = map.collect_all_log_markers();
        assert_eq!(markers.len(), 4, "两个沙箱各 2 条 marker 应共 4 条");
        assert!(
            markers.iter().filter(|&&m| m == 7).count() == 2
                && markers.iter().filter(|&&m| m == 5).count() == 2,
            "应含两个沙箱的 [7,5] 序列（沙箱遍历顺序随机，只验集合）"
        );
        assert!(
            map.collect_all_log_markers().is_empty(),
            "收集后应清空（不跨调用残留）"
        );
    }

    // === 第三只生物 H（黄"H" 搅局者）：行为分支与混战链路 ===

    #[test]
    fn hunter_watches_when_enemy_far() {
        // H 行为分支一（观望）：最近敌人距离 > 3 -> marker 4 + 原地小幅游走 ±0.5，不出刀
        let mut world = WorldState::new();
        let h = EntityId(Uuid::from_u128(80));
        let enemy = EntityId(Uuid::from_u128(81));
        world
            .entities
            .insert(h, make_creature(80, (0.0, 0.0), "hunter", "H", "yellow"));
        world
            .entities
            .insert(enemy, make_creature(81, (20.0, 20.0), "far", "F", "gray"));
        let mut map = test_sandbox_map();
        map.create(h, &wasm(HUNTER_WAT), 1_000_000).unwrap();
        map.create(enemy, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        map.run_player_code(&world);
        let markers = map.collect_all_log_markers();
        assert!(
            markers.contains(&4),
            "远处敌人应观望（marker 4），实际 {:?}",
            markers
        );
        let handle = map.sandboxes.get_mut(&h).unwrap();
        let moves = &handle.store.data().pending_moves;
        assert_eq!(moves.len(), 1, "观望应小幅游走 1 步");
        match &moves[0] {
            PendingChange::SetPos { new_pos, .. } => {
                // 游走方向 ±0.5 沿 x 轴（tick 奇偶决定正负，二者皆合法）
                assert!(
                    (new_pos.0.abs() - 0.5).abs() < 1e-9 && new_pos.1 == 0.0,
                    "游走步幅应为 x ±0.5，实际 {:?}",
                    new_pos
                );
            }
            other => panic!("观望应排队 SetPos，实际 {other:?}"),
        }
        assert!(
            handle.store.data().pending_attacks.is_empty(),
            "观望状态不应出刀"
        );
    }

    #[test]
    fn hunter_attacks_when_enemy_close() {
        // H 行为分支二（参战）：最近敌人距离 <= 3 -> marker 5 + 对敌人坐标出刀，不移动
        let mut world = WorldState::new();
        let h = EntityId(Uuid::from_u128(82));
        let enemy = EntityId(Uuid::from_u128(83));
        world
            .entities
            .insert(h, make_creature(82, (0.0, 0.0), "hunter", "H", "yellow"));
        world
            .entities
            .insert(enemy, make_creature(83, (1.0, 0.0), "prey", "P", "gray"));
        let mut map = test_sandbox_map();
        map.create(h, &wasm(HUNTER_WAT), 1_000_000).unwrap();
        map.create(enemy, &wasm(TARGET_WAT), 1_000_000).unwrap();
        map.run_player_code(&world);
        let markers = map.collect_all_log_markers();
        assert!(
            markers.contains(&5),
            "近距离敌人应参战（marker 5），实际 {:?}",
            markers
        );
        let handle = map.sandboxes.get_mut(&h).unwrap();
        let attacks = &handle.store.data().pending_attacks;
        assert_eq!(attacks.len(), 1, "参战应排队 1 刀");
        assert_eq!(
            attacks[0].target_pos,
            (1.0, 0.0),
            "刀应落在 scan 到的敌人坐标"
        );
        assert!(
            handle.store.data().pending_moves.is_empty(),
            "参战状态不应游走"
        );
    }

    #[test]
    fn hunter_intercepts_on_odd_ticks_only() {
        // H 防守：50% 拦截（tick 奇偶伪随机）——奇数 tick 拦截（marker 6），偶数放行
        let mut map = test_sandbox_map();
        let h = EntityId(Uuid::from_u128(84));
        map.create(h, &wasm(HUNTER_WAT), 1_000_000).unwrap();
        // on_tick 自增 $ticks：调 1 次 -> ticks=1（奇）
        map.manager
            .call_on_tick(map.sandboxes.get_mut(&h).unwrap())
            .unwrap();
        assert!(
            map.manager
                .call_on_attack(map.sandboxes.get_mut(&h).unwrap(), &[0])
                .unwrap(),
            "奇数 tick 应拦截"
        );
        // 再调 1 次 -> ticks=2（偶）
        map.manager
            .call_on_tick(map.sandboxes.get_mut(&h).unwrap())
            .unwrap();
        assert!(
            !map
                .manager
                .call_on_attack(map.sandboxes.get_mut(&h).unwrap(), &[0])
                .unwrap(),
            "偶数 tick 应放行"
        );
        let markers = map.collect_all_log_markers();
        assert!(
            markers.contains(&6),
            "拦截成功应上报 marker 6，实际 {:?}",
            markers
        );
    }

    #[test]
    fn hunter_idle_when_no_enemy() {
        // H 负面路径：全场只剩自己（无敌人）-> marker 0 待机，不游走不出刀
        let mut world = WorldState::new();
        let h = EntityId(Uuid::from_u128(85));
        world
            .entities
            .insert(h, make_creature(85, (0.0, 0.0), "hunter", "H", "yellow"));
        let mut map = test_sandbox_map();
        map.create(h, &wasm(HUNTER_WAT), 1_000_000).unwrap();
        map.run_player_code(&world);
        let markers = map.collect_all_log_markers();
        assert!(
            markers.contains(&0),
            "无敌人应待机（marker 0），实际 {:?}",
            markers
        );
        let handle = map.sandboxes.get_mut(&h).unwrap();
        assert!(handle.store.data().pending_moves.is_empty(), "待机不游走");
        assert!(
            handle.store.data().pending_attacks.is_empty(),
            "待机不出刀"
        );
    }

    #[test]
    fn hunter_blade_kills_third_party_via_two_phase_settlement() {
        // 混战链路：H 出刀 -> 既有两阶段结算 -> 第三方被击杀 + H 掠夺 1 点
        // （复用 process_attacks，H 的刀从真实沙箱 pending_attacks 收集，非手工构造）
        let mut world = WorldState::new();
        let h = EntityId(Uuid::from_u128(86));
        let victim = EntityId(Uuid::from_u128(87));
        world
            .entities
            .insert(h, make_creature(86, (0.0, 0.0), "hunter", "H", "yellow"));
        world
            .entities
            .insert(victim, make_creature(87, (1.0, 0.0), "prey", "P", "gray"));
        let mut map = test_sandbox_map();
        map.create(h, &wasm(HUNTER_WAT), 1_000_000).unwrap();
        map.create(victim, &wasm(TARGET_WAT), 1_000_000).unwrap();
        // 阶段一：H 参战排刀
        map.run_player_code(&world);
        let attacks = map.collect_pending_attacks();
        assert_eq!(attacks.len(), 1, "H 应排 1 刀");
        // 阶段二：两阶段结算（victim 无防守 -> 被击杀 + H 掠夺）
        process_attacks(&mut world, &mut map, &attacks);
        assert!(
            !world.entities.get(&victim).unwrap().is_alive(),
            "H 的刀应击杀第三方"
        );
        assert_eq!(
            world.entities.get(&h).unwrap().survival.points(),
            4,
            "H 应掠夺 1 点（3->4）"
        );
    }

    // === tick 字幕接线（渲染层展示随 tick 更新）===

    /// 字幕内容观测点：Renderer 保存的行字符串（tick 内 flush 会 swap buffer，
    /// buffer current 不再持有本帧；buffer 落格行为已由 render crate 测试覆盖）
    fn renderer_row0(renderer: &Renderer) -> String {
        renderer.caption_line().to_string()
    }

    #[test]
    fn tick_updates_caption_alive_and_distance() {
        // 集成：tick 内更新字幕——开局 alive=2 / dist=10.0；A 逼近后 dist 缩小；
        // A 被击杀后 dist=-- / alive=1（存活数变化、缺席占位）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, a, _d) =
            battle_stage(10);
        let run = |world: &mut WorldState,
                       time: &mut TimeEngine,
                       renderer: &mut Renderer,
                       storage: &mut StorageEngine,
                       pending: &mut Vec<PendingChange>,
                       map: &mut EntitySandboxMap| {
            run_ticks(world, time, &physics, renderer, storage, pending, map, 1);
        };
        // 开局字幕（第 1 tick 后：alive=2，dist 尚在 10 附近）
        run(&mut world, &mut time, &mut renderer, &mut storage, &mut pending, &mut map);
        assert!(
            renderer_row0(&renderer).contains("alive=2"),
            "开局字幕应显示存活 2，实际 {:?}",
            renderer_row0(&renderer)
        );
        assert!(
            renderer_row0(&renderer).contains("dist=9.0"),
            "第 1 tick A 逼近后距离应约 9.0，实际 {:?}",
            renderer_row0(&renderer)
        );
        // 15 tick：A 被 D 反制击杀（默认盾 10 时间线）-> dist=-- / alive=1
        for _ in 0..14 {
            run(&mut world, &mut time, &mut renderer, &mut storage, &mut pending, &mut map);
        }
        assert!(
            !world.entities.contains_key(&a),
            "回归前提：15 tick 时 A 应已被击杀"
        );
        assert!(
            renderer_row0(&renderer).contains("dist=--"),
            "A 死后距离应显示占位符，实际 {:?}",
            renderer_row0(&renderer)
        );
        assert!(
            renderer_row0(&renderer).contains("alive=1"),
            "A 死后存活数应为 1，实际 {:?}",
            renderer_row0(&renderer)
        );
    }

    #[test]
    fn tick_caption_shield_counts_down_with_intercepts() {
        // 集成：D 拦截 A 的 3 刀（tick 10/11/12），字幕盾量经 marker 上报逐次下降，
        // A 死后停在 7（盾量=拦截配额余量，来自玩家代码自愿上报，host 不读沙箱内部）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, a, d) =
            battle_stage(10);
        run_ticks(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            15,
        );
        assert!(
            !world.entities.contains_key(&a),
            "回归前提：A 应已被击杀"
        );
        assert!(world.entities.contains_key(&d), "回归前提：D 应存活");
        let line = renderer_row0(&renderer);
        assert!(
            line.contains("shield=7"),
            "3 次拦截后字幕应显示盾余 7，实际 {:?}",
            line
        );
        assert!(
            line.contains("alive=1"),
            "A 死后存活数应为 1，实际 {:?}",
            line
        );
    }
}
