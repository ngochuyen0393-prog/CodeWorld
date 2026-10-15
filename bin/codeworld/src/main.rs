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
mod input;
mod net;
mod players;

use anyhow::{Context, Result};
use codeworld_arena::attack::{PendingAttack, DEFAULT_KILL_WASM};
use codeworld_arena::sandbox::{
    PendingDestroy, PendingSpawn, ProbeBridge, SandboxHandle, SandboxManager,
};
use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
use codeworld_core::pending::PendingChange;
use codeworld_core::world::{BoundingBox, WorldState};
use codeworld_physics::default::DefaultPhysicsEngine;
use codeworld_render::{Renderer, Viewport};
use codeworld_storage::StorageEngine;
use codeworld_time::clock::TimeEngine;
use config::Config;
use players::PlayerDirectory;
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
    /// 全局可见实体 id 快照（三层感知 Phase A：fill_visible_snapshots 填充，
    /// run_player_code 挂桥用；EntityId(Uuid) 升序）
    visible_ids: Vec<EntityId>,
    /// 与 visible_ids 平行的坐标快照
    visible_positions: Vec<(f64, f64)>,
    /// on_probe 独立小 fuel 预算（架构 3.3 probe_target_instruction_limit；
    /// main() 从 config.probe_fuel 设置，默认 10_000）
    probe_fuel: u64,
    /// 世界实体总数上限（门二 spawn 配套；main() 从 config.max_entities 设置，默认 64）
    max_entities: usize,
    /// 玩家目录（门一进场 + 在线锚 + 残影期，players.rs）。
    /// 默认禁用：既有测试跑 tick 零文件系统副作用；main() 按配置装配启用实例
    players: PlayerDirectory,
    /// "no player online" 提示已打过一次（操控层 v0：无玩家按键只提示一次不刷屏）
    warned_no_player: bool,
    /// me 声明无效提示已打过一次（#024：拼错/未进场/已下线 warn 不刷屏）
    warned_invalid_me: bool,
    /// 上一拍操控的玩家名（#024 下线 latch 检测依据）
    last_controlled: Option<String>,
    /// 操控目标下线 latch（#024：下线后本局锁定观战，重新进场不自动恢复，重启重开）
    controlled_offline_latched: bool,
    /// WS 鼠标点击走位持续目标（#027：归属实体 + 世界坐标；跨拍保持，到达/Stop/换角清除）
    ws_move_target: Option<(EntityId, (f64, f64))>,
    /// 操控独占是否生效中（#025：me 在线=true；状态翻转沿打生命周期日志）
    ctrl_exclusive_active: bool,
}

impl EntitySandboxMap {
    /// 创建映射表（初始化 SandboxManager）
    fn new() -> Result<Self> {
        Ok(Self {
            manager: SandboxManager::new()?,
            sandboxes: HashMap::new(),
            visible_ids: Vec::new(),
            visible_positions: Vec::new(),
            probe_fuel: 10_000,
            max_entities: 64,
            players: PlayerDirectory::disabled(),
            warned_no_player: false,
            warned_invalid_me: false,
            last_controlled: None,
            controlled_offline_latched: false,
            ws_move_target: None,
            ctrl_exclusive_active: false,
        })
    }

    /// 填充可见实体快照（三层感知 Phase A 眼睛层，阶段一执行玩家代码【前】调用）
    ///
    /// - viewport 过滤 + 存活过滤（尸体不是感知目标），全局列表按 EntityId(Uuid)
    ///   升序稳定排序（架构 3.2：可见列表稳定）
    /// - 每个沙箱的 per-entity 快照排除自己（env.visible_count/visible_at 数据源）
    fn fill_visible_snapshots(&mut self, world: &WorldState, viewport: &BoundingBox) {
        let mut visible: Vec<(EntityId, (f64, f64))> = world
            .entities
            .values()
            .filter(|e| e.is_alive())
            .filter(|e| {
                let (x, y) = e.pos();
                viewport.contains(x, y)
            })
            .map(|e| (e.id, e.pos()))
            .collect();
        visible.sort_by_key(|a| a.0 .0);
        self.visible_ids = visible.iter().map(|(id, _)| *id).collect();
        self.visible_positions = visible.iter().map(|(_, p)| *p).collect();
        for (id, handle) in self.sandboxes.iter_mut() {
            handle.store.data_mut().visible = visible
                .iter()
                .filter(|(eid, _)| eid != id)
                .map(|(_, p)| *p)
                .collect();
        }
        tracing::debug!(count = visible.len(), "[visible_snapshot] 可见实体快照已填充");
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

    /// 执行阶段一（无操控版，等价 run_player_code_with_control(None)）。
    /// 生产路径走 run_player_code_with_control；本包装供既有测试基线调用。
    #[cfg(test)]
    fn run_player_code(&mut self, world: &WorldState, viewport: &BoundingBox) -> Vec<EntityId> {
        self.run_player_code_with_control(world, viewport, None)
    }

    /// 带操控接管的阶段一（#022 操控层 v0）：control_override 指定的实体本拍跳过
    /// on_tick（自主移动/攻击都不跑，玩家按键=这拍听我的），life() 照常。
    fn run_player_code_with_control(
        &mut self,
        world: &WorldState,
        viewport: &BoundingBox,
        control_override: Option<EntityId>,
    ) -> Vec<EntityId> {
        let mut to_kill = Vec::new();
        // 先收集 owned EntityId 再遍历，避免边遍历 world.entities 边改 self.sandboxes 的借用冲突
        let entity_ids: Vec<EntityId> = world.entities.keys().copied().collect();
        // probe 桥的共享部分（循环外 cast 成 raw 指针：cast 不延长借用，
        // 真正的可变访问只发生在 import 闭包内的窗口期，见 ProbeBridge SAFETY 契约）
        let sandboxes_ptr: *mut HashMap<EntityId, SandboxHandle> = &mut self.sandboxes;
        let manager_ptr: *const SandboxManager = &self.manager;
        let bridge_ids = self.visible_ids.clone();
        let bridge_positions = self.visible_positions.clone();
        let probe_fuel = self.probe_fuel;
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
            // 残影期实体跳过（在线锚下线合同，world-foundation 三章）：不执行玩家代码
            //（on_tick/life 都不调），沙箱保留（残影仍可被攻击/掠夺，消散时才销毁）
            if self.players.is_ghost(&entity_id) {
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
                let nearest = find_nearest_enemy(world, &entity_id, viewport);
                let data = handle.store.data_mut();
                data.pos = e.pos();
                data.nearest_enemy = nearest;
            }
            // probe 通路桥挂载（三层感知 Phase A）：本实体的 on_tick 执行窗口内
            // env.probe 可经桥同步探测其他实体的沙箱；窗口结束立即置空（SAFETY：
            // bridge 是循环体局部值，指针悬垂窗口为零）。
            let mut bridge = ProbeBridge {
                visible_ids: bridge_ids.clone(),
                visible_positions: bridge_positions.clone(),
                sandboxes: sandboxes_ptr,
                manager: manager_ptr,
                caller_id: entity_id,
                probe_fuel,
            };
            handle.store.data_mut().probe_bridge = &mut bridge;
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
                handle.store.data_mut().probe_bridge = std::ptr::null_mut();
                continue;
            }
            // 调 on_tick（没导出则 call_on_tick 内部跳过）。
            // 操控接管本拍（#022 操控优先于自主）：跳过被操控实体的 on_tick——自主
            // 移动/攻击都不跑。取舍：MVP 完全接管（语义清晰"按键=这拍听我的"、实现
            // 最简），细粒度"仅抑制移动副作用"待挂机AI需要持续输出时再做。
            // life() 照常调用（存在合同不受操控影响——操控停，代码层心跳不停）。
            if control_override != Some(entity_id) {
                if let Err(e) = self.manager.call_on_tick(handle) {
                    tracing::warn!(entity_id = %entity_id.0, error = %e, "on_tick 执行失败");
                }
            } else {
                tracing::debug!(entity_id = %entity_id.0, "[control] 本拍操控接管，跳过自主 on_tick");
            }
            // 窗口关闭：置空桥指针（SAFETY 契约，防后续 life 调用期悬垂）
            handle.store.data_mut().probe_bridge = std::ptr::null_mut();
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

    /// 收集本 tick 所有沙箱排队的造物请求（env.spawn 推入，门二）
    ///
    /// drain 各沙箱 pending_spawns 后清空，保证不跨 tick 残留。在 tick 阶段二
    /// process_spawns 调用。
    fn collect_pending_spawns(&mut self) -> Vec<PendingSpawn> {
        let mut all = Vec::new();
        for handle in self.sandboxes.values_mut() {
            all.append(&mut handle.store.data_mut().pending_spawns);
        }
        if !all.is_empty() {
            tracing::info!(count = all.len(), "[collect_pending_spawns] 收集造物请求");
        }
        all
    }

    /// 收集本 tick 所有沙箱排队的销毁请求（env.destroy 推入，门二）
    ///
    /// drain 各沙箱 pending_destroys 后清空，保证不跨 tick 残留。在 tick 阶段二
    /// process_destroys 调用。
    fn collect_pending_destroys(&mut self) -> Vec<PendingDestroy> {
        let mut all = Vec::new();
        for handle in self.sandboxes.values_mut() {
            all.append(&mut handle.store.data_mut().pending_destroys);
        }
        if !all.is_empty() {
            tracing::info!(count = all.len(), "[collect_pending_destroys] 收集销毁请求");
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

    // === 玩家目录（门一进场，players.rs 状态机 + 本处编排）===

    /// 单个玩家进场：读 .wat -> 编译（失败 warn + 黑名单）-> 建实体 + 沙箱 -> 登记在线。
    /// 返回 false = 进场失败（调用方记黑名单）。
    fn spawn_player(&mut self, world: &mut WorldState, name: &str, current_tick: u64) -> bool {
        let path = self.players.dir().join(format!("{name}.wat"));
        let wat_text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(player = name, error = %e, "[players] 玩家文件读取失败");
                return false;
            }
        };
        // 玩家代码编译（不用 wasm()：它遇非法 WAT 会 panic，玩家文件必须容错跳过）
        let wasm_bytes = match wat::parse_bytes(wat_text.as_bytes()) {
            Ok(b) => b.into_owned(),
            Err(e) => {
                tracing::warn!(
                    player = name,
                    error = %e,
                    "[players] 玩家代码编译失败，跳过（删除文件重放可重试）"
                );
                return false;
            }
        };
        let join_index = self.players.next_join_index();
        let entity_id = EntityId::new();
        let entity = Entity::new(
            entity_id,
            PlayerId::new(),
            name.to_string(), // 玩家名 = code_hash 身份承载（同 A/D/H/S 模式）
            (players::spawn_x_for(join_index), 0.0),
            Appearance::new(
                players::glyph_for(name),
                players::color_for(join_index).to_string(),
                1.0,
            ),
            current_tick,
            1_000_000,
        );
        if let Err(e) = self.create(entity_id, &wasm_bytes, 1_000_000) {
            tracing::warn!(player = name, error = %e, "[players] 玩家沙箱创建失败，未进场");
            return false;
        }
        world.entities.insert(entity_id, entity);
        self.players.register_online(name, entity_id);
        tracing::info!(
            player = name,
            glyph = players::glyph_for(name),
            color = players::color_for(join_index),
            pos = ?(players::spawn_x_for(join_index), 0.0),
            "玩家进场：{name}"
        );
        true
    }

    /// 启动初始化：扫描 players/ 目录加载全部玩家（目录不存在自动创建，空目录 = 无玩家）
    fn load_players_initial(&mut self, world: &mut WorldState) -> Result<()> {
        let stems = self.players.scan_dir().context("players 目录扫描失败")?;
        tracing::info!(count = stems.len(), "players/ 目录初始加载");
        let current_tick = world.tick();
        for name in &stems {
            if !self.spawn_player(world, name, current_tick) {
                self.players.mark_failed(name);
            }
        }
        Ok(())
    }

    /// tick 阶段二 d0 步：目录轮询（新 .wat 进场 / 删除下线进残影）+ 残影到期消散 + 记录打扫。
    /// disabled 时 no-op（零文件系统开销）。
    fn step_online_anchor(&mut self, world: &mut WorldState, current_tick: u64) {
        if !self.players.is_enabled() {
            return;
        }
        // 1. 目录轮询：新 .wat 进场 / 消失的文件名下线进残影 / 黑名单解除
        match self.players.scan_dir() {
            Ok(stems) => {
                for name in &stems {
                    if !self.players.is_online(name)
                        && !self.players.is_failed(name)
                        && !self.spawn_player(world, name, current_tick)
                    {
                        self.players.mark_failed(name);
                    }
                }
                for name in self.players.offline_names(&stems) {
                    if let Some(ev) = self.players.note_offline(&name, current_tick) {
                        let kind = if ev.in_combat { "战斗" } else { "普通" };
                        tracing::info!(
                            player = %name,
                            entity_id = %ev.entity_id.0,
                            ghost_ticks = ev.ghost_ticks,
                            in_combat = ev.in_combat,
                            "玩家下线：{name}（{kind}残影 {} 拍）",
                            ev.ghost_ticks
                        );
                    }
                }
                // 文件消失的失败名解除黑名单（"删了再放"可重试）
                self.players.clear_failed_not_in(&stems);
            }
            Err(e) => {
                tracing::warn!(error = %e, "[players] 目录扫描失败，本拍跳过在线锚检测");
            }
        }
        // 2. 残影到期消散：标记死亡，本拍死亡清理步移除实体+销毁沙箱
        //    （不算击杀：无掠夺对象，只是消散——在线锚合同下线语义）
        for entity_id in self.players.expire_due_ghosts(current_tick) {
            if let Some(entity) = world.entities.get_mut(&entity_id) {
                tracing::info!(entity_id = %entity_id.0, "[players] 残影消散（非击杀，无掠夺）");
                entity.kill();
            }
        }
        // 3. 记录打扫：残影期被击杀等已不在世界的实体，清残影/参战记录（防无界增长）
        self.players.prune(&world.entities);
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
    // probe 预算装配（三层感知 Phase A：config 可调的 on_probe 独立小 fuel）
    sandbox_map.probe_fuel = config.probe_fuel;
    // 世界实体上限装配（门二 spawn 配套，decisions.md #020）
    sandbox_map.max_entities = config.max_entities;

    // 新世界或恢复后无实体：放测试实体（含 WASM 沙箱）让 MVP 有东西渲染、能跑玩家代码
    if world.entities.is_empty() {
        spawn_test_entities(&mut world, &mut sandbox_map, config.d_shield, config.h_enabled, config.s_enabled);
    }

    // 门一进场（world-foundation 二）：装配玩家目录（启动扫描 + 每 tick 在线锚轮询）。
    // 目录不存在自动创建；空目录 = 无玩家进场，原四生物剧本照跑。
    sandbox_map.players = PlayerDirectory::new(&config.players_dir);
    if let Err(e) = sandbox_map.load_players_initial(&mut world) {
        tracing::warn!(error = %e, "players/ 目录初始加载失败，进场门暂不可用（世界继续）");
    }

    // 操控层 v0（#022）：raw mode 裸键输入 + 意图队列；旧 stdin read_line 线程退役
    //（read_line 等 Enter/EOF 的退出语义由 crossterm 事件循环接管：Quit 键/EOF 路等价）
    // #027：同一 IntentSink 克隆给 WS 线程——终端键盘与浏览器鼠标单入口双来源
    let running = Arc::new(AtomicBool::new(true));
    let (intent_sink, intent_queue) = input::intent_channel();
    let input_thread = input::spawn_input_thread(intent_sink.clone(), running.clone());

    // 延迟变更队列（架构文档 4.1 节阶段一产出、阶段二 a 统一应用）
    // 玩家代码执行期间产生的变更放入 pending，本 tick 阶段二统一应用后清空
    let mut pending: Vec<PendingChange> = Vec::new();

    // 进入终端操控模式：raw mode + 备用屏 + 清屏 + 隐光标
    // stdout 专供 ANSI 画面，日志已走 stderr；setup 失败降级普通输出运行（键控不可用但不崩）
    if let Err(e) = input::setup_terminal() {
        tracing::warn!(error = %e, "进入终端操控模式失败（raw mode/备用屏），继续以普通输出运行");
    }
    // panic 时尽力恢复终端（raw mode + 备用屏 + 颜色/光标全恢复，不砖化终端），
    // 再交回默认 hook 打印 panic 信息
    input::install_panic_hook();

    // 期四第一刀：WS 快照推送（与终端 ANSI 渲染并行存在，不替代终端）。
    // 端口 0 = 禁用（返回 None，tick 循环走零开销分支）；WS 线程挂掉不影响主循环。
    // #027：第三刀起 WS 线程同时是上行意图入口（转发进共享 intent_sink）
    let ws_snapshots = net::init(&config, intent_sink);

    tracing::info!("tick 循环开始（WASD/方向键操控，空格攻击，q/Enter/Ctrl+C 退出）");
    while running.load(Ordering::SeqCst) {
        // 操控层 v0：每拍开头 drain 意图队列（输入线程生产 -> tick 主线程消费）
        let intents = intent_queue.drain();
        tick_with_intents(
            &mut world,
            &mut time_engine,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
            &intents,
        );
        // 期四第一刀：每拍快照推 WS（渲染步骤旁组装；禁用时 None 完全跳过——不组装不发；
        // 无浏览器连接时 push_snapshot 内部跳过组装，同样零开销）
        if let Some(snapshots) = &ws_snapshots {
            net::push_snapshot(
                snapshots,
                &world,
                renderer.caption_line(),
                renderer.viewport(),
                &sandbox_map.players,
                renderer.controlled(),
            );
        }
        std::thread::sleep(config.tick_interval());
    }

    // 关闭：恢复终端（raw mode + 备用屏 + 颜色/光标全路径）+ 强制存快照（架构文档 4.1 收尾）
    tracing::info!("收到退出信号，正在保存世界快照...");
    let save_result = storage.force_save(&world);
    // 无论保存结果如何都恢复终端（避免残留裸键模式/备用屏/隐藏光标状态）
    input::restore_terminal();
    // 输入线程随 running 标志退出（≤50ms），收尸防悬线程
    let _ = input_thread.join();
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
/// 无操控意图版 = 空意图的 tick_with_intents；生产路径走 tick_with_intents，
/// 本包装供既有测试基线调用（无人按键语义锚）。
#[cfg(test)]
fn tick(
    world: &mut WorldState,
    time_engine: &mut TimeEngine,
    physics: &DefaultPhysicsEngine,
    renderer: &mut Renderer,
    storage: &mut StorageEngine,
    pending: &mut Vec<PendingChange>,
    sandbox_map: &mut EntitySandboxMap,
) {
    tick_with_intents(
        world,
        time_engine,
        physics,
        renderer,
        storage,
        pending,
        sandbox_map,
        &[],
    );
}

/// 带操控意图的单 tick（#022 操控层 v0 主入口）
///
/// intents：本拍积压的按键意图（main 循环每拍开头 drain 输入队列传入）。
/// 期四 WebSocket 意图流复用本入口，只换来源不换路由（input.rs route_intents）。
#[allow(clippy::too_many_arguments)] // 与既有 tick/run_ticks 同签名家族，8 参为 tick 管线固有形状
fn tick_with_intents(
    world: &mut WorldState,
    time_engine: &mut TimeEngine,
    physics: &DefaultPhysicsEngine,
    renderer: &mut Renderer,
    storage: &mut StorageEngine,
    pending: &mut Vec<PendingChange>,
    sandbox_map: &mut EntitySandboxMap,
    intents: &[input::Intent],
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
    //    眼睛列表快照（三层感知 Phase A）：渲染同款 viewport 过滤存活实体，
    //    填进各沙箱 SandboxData.visible + 全局列表（probe 桥用）——玩家代码
    //    env.visible_count/visible_at 读到的是 tick 开始时的世界。
    // 视野边界（渲染同款 viewport）：眼睛列表与 scan 锁敌共用同一个 BoundingBox
    // 实例（同源口径，别造两套"什么算看得见"——架构 4.2 攻击前置条件三路之一）
    let bounds = {
        let vp = renderer.viewport();
        let (min_x, max_x) = vp.world_x_range();
        let (min_y, max_y) = vp.world_y_range();
        BoundingBox::new(min_x, min_y, max_x, max_y)
    };
    sandbox_map.fill_visible_snapshots(world, &bounds);

    // ②5 操控路由（#022 双层模型：操控优先于自主，代码层回落挂机AI）
    //    - 操控目标 = me 声明（CODEWORLD_ME 优先，其次 players/me.txt）且在线的玩家
    //      （#024 身份绑定，修正 #023 进场序；每拍解析，中途声明/修改即生效）
    //    - 意图在拍首路由（坐标取拍首快照），本拍行动立即定案
    let env_me = std::env::var("CODEWORLD_ME").ok();
    let me = players::read_me_declaration(env_me.as_deref(), sandbox_map.players.dir());
    // 上一拍操控的目标本拍不在线 = 操控目标下线（#024：warn 一次 + latch；
    // me.txt 中途改声明他人不触发——那是显式换人，不是断线）
    if let Some(prev) = sandbox_map.last_controlled.clone() {
        if !sandbox_map.controlled_offline_latched
            && me.as_deref() == Some(prev.as_str())
            && !sandbox_map.players.is_online(&prev)
        {
            tracing::warn!(
                player = %prev,
                "[players] 操控目标下线，转观战（重新进场不自动恢复，重启后生效）"
            );
            sandbox_map.controlled_offline_latched = true;
        }
    }
    let online = if sandbox_map.controlled_offline_latched {
        None
    } else {
        sandbox_map.players.controlled_target(me.as_deref())
    };
    sandbox_map.last_controlled = online.as_ref().map(|(name, _)| name.clone());
    let control_target = online
        .as_ref()
        .map(|(_, id)| *id)
        .filter(|id| world.entities.get(id).map(|e| e.is_alive()).unwrap_or(false));
    // me 声明了但名字无效（拼错/未进场/已下线）：warn 一次 + 观战
    if me.is_some()
        && online.is_none()
        && !sandbox_map.warned_invalid_me
        && !sandbox_map.controlled_offline_latched
    {
        tracing::warn!(
            name = ?me,
            "[players] me 声明的玩家名无效（拼错/未进场/已下线），观战模式"
        );
        sandbox_map.warned_invalid_me = true;
    }
    if !intents.is_empty() && control_target.is_none() && !sandbox_map.warned_no_player {
        tracing::debug!("[input] 观战模式无操控目标，按键仅 Quit 生效（no control target）");
        sandbox_map.warned_no_player = true;
    }
    // #025 操控独占生命周期：me 在线期间自主全程挂起（状态翻转沿打日志，不逐拍刷）
    if control_target.is_some() && !sandbox_map.ctrl_exclusive_active {
        if let Some((name, _)) = &online {
            tracing::info!("[ctrl] 操控独占开始 player={name}（自主挂起）");
        }
        sandbox_map.ctrl_exclusive_active = true;
    } else if control_target.is_none() && sandbox_map.ctrl_exclusive_active {
        tracing::info!("[ctrl] 操控释放（自主恢复）");
        sandbox_map.ctrl_exclusive_active = false;
    }
    let nearest = control_target.and_then(|id| find_nearest_enemy(world, &id, &bounds));
    // 操控目标当前坐标（#027：点击走位步向计算依据）
    let entity_pos = control_target
        .and_then(|id| world.entities.get(&id))
        .map(|e| e.pos());
    // 观战/无操控拍：点击走位目标不滞留（换角不交接由 route_intents 兜底，这里清根）
    if control_target.is_none() {
        sandbox_map.ws_move_target = None;
    }
    let action = input::route_intents(
        intents,
        control_target,
        nearest,
        entity_pos,
        &mut sandbox_map.ws_move_target,
    );
    // 字幕 ctrl 段（#024 三态）：操控中 = ctrl:字形；观战（有人但不是你 / 操控目标
    // 下线 latch）= ctrl:--；无玩家在线且从未 latch（四生物基线）= 不显示 ctrl 段
    let ctrl = if sandbox_map.players.has_online() || sandbox_map.controlled_offline_latched {
        Some(
            control_target
                .and(online.as_ref())
                .map(|(name, _)| players::glyph_for(name))
                .unwrap_or_else(|| "--".to_string()),
        )
    } else {
        None
    };
    if let (Some((name, _)), Some((id, act))) = (&online, &action) {
        match act {
            input::ControlledAction::Move(dx, dy) => {
                // 操控移动 = 排队 SetPos（占位裁决照常，一格一物不豁免）；
                // 自主 on_tick 本拍被抑制（run_player_code_with_control），无坐标冲突
                if let Some(e) = world.entities.get(id) {
                    let (x, y) = e.pos();
                    pending.push(PendingChange::SetPos {
                        entity: *id,
                        new_pos: (x + dx, y + dy),
                    });
                }
                tracing::info!("[input] 操控意图 Move({dx},{dy}) player={name}（覆盖自主移动）");
            }
            input::ControlledAction::Attack(tpos) => {
                tracing::info!("[input] 操控意图 Attack target={tpos:?} player={name}（操控刀优先）");
            }
        }
    }
    // #025 操控独占：me 在线（control_target 存在）即全程抑制该实体 on_tick——
    // 不按键角色就站着，代码自主被完全抑制；不存在"无意图拍回落自主"。
    // 有无本拍行动（action）不影响抑制范围；life() 照常（存在合同不受操控影响）。
    let to_kill = sandbox_map.run_player_code_with_control(world, &bounds, control_target);
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
    //      先并入阶段一排队的移动请求（env.move_by），随本步统一应用（与攻击排队同模式）；
    //      再并入产权校验通过的销毁请求（env.destroy，门二）一起应用
    pending.extend(sandbox_map.collect_pending_moves());
    process_destroys(world, sandbox_map, pending);
    // #021：占位制下移动被挡产生的接触对在此收集，阶段二 c 步与物理同格对合并
    let move_blocked_contacts = apply_pending_changes(world, pending, control_target);

    //   a1. 造物处理（门二：env.spawn 排队的请求在此编译/建实体/建沙箱/插入世界）。
    //       放在 destroy 应用之后：本拍自毁的创造者不再造物；上限按应用后世界实况算。
    //       放在攻击处理之前：造物出生即进入世界（本拍攻击队列已定，下拍才会与之交互）
    process_spawns(world, sandbox_map, current_tick);

    //   a2. 处理攻击（架构 4.2 两阶段模型：阶段一 on_tick 调 env.attack_at 排队的请求在此统一执行）
    //      插在 apply_pending_changes 之后、decay_survival 之前：攻击产生的死亡先于每日衰减处理。
    //      import 函数只排队（无 world 访问权），实际 hit/miss/拦截/结算在此判断：
    //      收件（逐刀问目标当前沙箱 on_attack，不换血不击杀）-> 结算（攻击者 id 升序逐刀执行）。
    //      操控攻击（空格）并入同一队列走既有两阶段结算（操控不豁免物理，宪法）；
    //      无可见目标 = 挥空不出刀（但操控仍接管了本拍）。
    let mut pending_attacks = sandbox_map.collect_pending_attacks();
    if let Some((id, input::ControlledAction::Attack(Some(tpos)))) = &action {
        pending_attacks.push(PendingAttack {
            attacker: *id,
            target_pos: *tpos,
            payload: Vec::new(),
        });
    }
    if !pending_attacks.is_empty() {
        process_attacks(world, sandbox_map, &pending_attacks);
    }

    //   b. 生存点数扣减（架构 4.1：每 86400 tick 扣 1 点，归零标记死亡）
    decay_survival(world, current_tick);

    //   c. 物理规则（重力/地面/速度，架构 4.1 b 步；#021 推挤退役——step 只检不推）
    //      接触事件在 step 之前捕获（物理同格检测）：step 内占位重力被挡的实体
    //      坐标不变，检测时序不再敏感（旧推挤"挤完测不到"的时序约束已随退役消失）。
    let collision_pairs = physics.detect_collisions(&world.entities);
    let changes = physics.step(world);
    if !changes.is_empty() {
        tracing::debug!(tick = current_tick, change_count = changes.len(), "物理变更");
    }
    //      接触触发沙箱通道（架构 4.1 b 步）：两类接触合并（#021 B 项）——
    //      ①物理同格检测（same_cell）②移动被挡（move_blocked，阶段二 a 步产生）。
    //      MVP 仅逐对日志；Phase 2：经 sandbox_map 为双方开启 probe/attack 通道。
    let mut contacts: Vec<ContactPair> = collision_pairs
        .into_iter()
        .map(|p| ContactPair {
            a: p.a,
            b: p.b,
            pos: p.pos,
            source: "same_cell",
        })
        .collect();
    contacts.extend(move_blocked_contacts);
    let contact_count = handle_collisions(&contacts, &world.entities);
    if contact_count > 0 {
        tracing::info!(tick = current_tick, contact_count, "接触事件处理");
    }

    //   d0. 玩家目录在线锚（门一进场/下线/残影消散；disabled 时 no-op 零开销）。
    //       攻击结算（a2）之后：本拍参战记录已入册，#013 战斗残影时长计算才完整。
    sandbox_map.step_online_anchor(world, current_tick);

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
        ctrl.as_deref(),
    );

    //   e. 渲染输出（架构 4.1 e 步；有差异才 flush）
    //      操控高亮：被操控实体 glyph 白色渲染（渲染层特判，#022 操控层 v0）
    renderer.set_controlled(control_target);
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

/// 应用延迟变更队列（架构文档 4.1 节阶段二 a 步 + #021 实体占位制）
///
/// 阶段一玩家代码执行期间产生的变更（spawn/destroy/set_pos/set_velocity）
/// 放入 pending 队列，此处统一应用到 WorldState。应用后队列清空。
///
/// #021 占位制：SetPos 应用前过占位检查（一格一物，Screeps 阻挡制）——
/// 目标格被其他存活实体占据 → 移动被拒（留原位）+ 记接触对。本 tick 所有移动
/// 按 EntityId 升序统一裁决（先到先得，确定序，与 physics.step 同源口径）。
///
/// `control_target`：本拍操控目标（期四第四刀）——仅它的 SetPos 应用日志升 INFO
/// （battle.log 可追踪玩家走位），其余实体保持 debug（A/D/S 每拍自主移动，
/// 全升 INFO 会刷爆日志）。
///
/// 返回移动被挡产生的接触对（汇入 handle_collisions 的接触语义，#021 B 项）。
fn apply_pending_changes(
    world: &mut WorldState,
    pending: &mut Vec<PendingChange>,
    control_target: Option<EntityId>,
) -> Vec<ContactPair> {
    let mut contacts: Vec<ContactPair> = Vec::new();
    let mut moves: Vec<(EntityId, (f64, f64))> = Vec::new();
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
                // 移动抽出统一裁决：先收件后按 id 升序应用（与攻击两阶段同模式）
                moves.push((entity, new_pos));
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
    // 移动统一裁决：EntityId 升序（先到先得，确定序）
    moves.sort_by_key(|(id, _)| id.0);
    for (entity, new_pos) in moves {
        // 死亡实体不应用移动：同 tick 内 life()=false 实体排队的移动作废
        //（死亡清理步随后移除实体，尸体位移属无人消费的顺序耦合，应用侧直接拦）
        let alive = world
            .entities
            .get(&entity)
            .map(|e| e.is_alive())
            .unwrap_or(false);
        if !alive {
            continue;
        }
        match codeworld_core::occupancy::cell_occupied_by(&world.entities, new_pos, entity) {
            Some(occupant) => {
                // 目标格被占：移动被拒，实体留在原坐标 + 记接触对（双方 EntityId）
                tracing::info!(
                    entity_id = %entity.0,
                    occupant = %occupant.0,
                    target = ?new_pos,
                    "[move] 移动被挡（目标格被占），实体留在原位"
                );
                contacts.push(ContactPair {
                    a: entity,
                    b: occupant,
                    pos: new_pos,
                    source: "move_blocked",
                });
            }
            None => {
                // 期四第四刀：操控实体的移动应用升 INFO 且带 from→to（土豆实测曾因
                // debug 级在 battle.log 看不到而误判"P 没动"）；其余实体保持 debug
                if control_target == Some(entity) {
                    let from = world.entities.get(&entity).map(|e| e.pos());
                    tracing::info!(
                        entity_id = %entity.0,
                        from = ?from,
                        to = ?new_pos,
                        "[ctrl] 操控移动应用 SetPos（from→to）"
                    );
                } else {
                    tracing::debug!(entity_id = %entity.0, ?new_pos, "应用 SetPos");
                }
                if let Some(e) = world.entities.get_mut(&entity) {
                    e.set_pos(new_pos);
                }
            }
        }
    }
    contacts
}

/// 门二销毁处理（架构 4.1 阶段二 + world-foundation 门二 + decisions.md #015）
///
/// 产权校验（在 apply_pending_changes 之前）：只能销毁 ①自家造物——MVP 只实现①，
/// world 里查目标 owner 是否等于发起者 owner，不匹配拒绝 + warn。校验通过的请求
/// 转 PendingChange::Destroy 进 pending 队列随本步统一应用（标记死亡走死亡清理，
/// **不触发掠夺**——destroy 不是击杀，铁律"掠夺只属于击杀"）。
/// 击杀掠夺式销毁走攻击通路（attack_at），与 destroy 职责分离。
/// 同拍校验基准：发起者与目标都须存活（本拍 life()=false 的发起者请求作废，
/// 与 deliver_attacks 对死攻击者作废同语义）。
fn process_destroys(
    world: &WorldState,
    sandbox_map: &mut EntitySandboxMap,
    pending: &mut Vec<PendingChange>,
) {
    let requests = sandbox_map.collect_pending_destroys();
    for req in requests {
        // 发起者存活（死者请求作废，防"临死带人"）
        let destroyer_ok = world
            .entities
            .get(&req.destroyer)
            .map(|e| e.is_alive())
            .unwrap_or(false);
        // 目标存在且存活（尸体不可再销毁）
        let target_ok = world
            .entities
            .get(&req.target)
            .map(|e| e.is_alive())
            .unwrap_or(false);
        // 产权链校验：发起者 owner == 目标 owner（只能毁自家造物，#015）
        let owner_match = match (
            world.entities.get(&req.destroyer),
            world.entities.get(&req.target),
        ) {
            (Some(d), Some(t)) => d.owner == t.owner,
            _ => false,
        };
        if !destroyer_ok || !target_ok || !owner_match {
            tracing::warn!(
                destroyer = %req.destroyer.0,
                target = %req.target.0,
                destroyer_alive = destroyer_ok,
                target_alive = target_ok,
                owner_match,
                "[process_destroys] 销毁请求被拒绝（产权不匹配或发起者/目标已死）"
            );
            continue;
        }
        tracing::info!(
            destroyer = %req.destroyer.0,
            target = %req.target.0,
            "[process_destroys] 产权匹配，销毁已排队（无掠夺）"
        );
        pending.push(PendingChange::Destroy {
            entity: req.target,
        });
    }
}

/// 门二造物处理（架构 4.1 阶段二 + world-foundation 门二 + decisions.md #012）
///
/// 阶段一 env.spawn 排队的请求在此统一处理（import 层已做零 world 校验：
/// 限流/有限性/钻地/±10 格/外观格式）。本函数按序判：
///   1. 创造者存活（本拍 life()=false 的创造者请求作废）
///   2. 世界实体上限（max_entities，默认 64）——达上限拒绝 + warn
///   3. WAT 编译（wat::parse_str）——失败该次 spawn 作废 + warn（不崩世界，
///      参照玩家进场容错）
///   4. 构造造物：新 EntityId、owner=创造者 owner（产权链 #015）、
///      code_hash="creation"、Appearance(glyph,color)、出生点、created_tick、
///      fuel 配额继承创造者
///   5. 创建沙箱（成功才插入世界——"存活实体必有沙箱"不变量）
///
/// 造物从下一 tick 开始跑自己的代码——**造物是活实体，不是装饰**（自治锚合同：
/// 存续独立于创造者在线状态）。
fn process_spawns(world: &mut WorldState, sandbox_map: &mut EntitySandboxMap, current_tick: u64) {
    let requests = sandbox_map.collect_pending_spawns();
    for req in requests {
        // 创造者必须存活（死者不造物）
        let Some(creator) = world.entities.get(&req.creator).filter(|e| e.is_alive()) else {
            tracing::warn!(
                creator = %req.creator.0,
                "[process_spawns] 创造者已死，造物请求作废"
            );
            continue;
        };
        let owner = creator.owner;
        // 世界实体总数上限
        if world.entities.len() >= sandbox_map.max_entities {
            tracing::warn!(
                creator = %req.creator.0,
                entities = world.entities.len(),
                cap = sandbox_map.max_entities,
                "[process_spawns] 达世界实体上限，造物请求作废"
            );
            continue;
        }
        // 造物 WAT 编译（失败作废不崩世界）
        let Ok(wasm_bytes) = wat::parse_str(&req.code) else {
            tracing::warn!(
                creator = %req.creator.0,
                code_len = req.code.len(),
                "[process_spawns] 造物 WAT 编译失败，该次 spawn 作废"
            );
            continue;
        };
        let entity_id = EntityId::new();
        // #021 出生预检（Factorio 放置检查）：出生格被占 → 该次 spawn 失败。
        // 放在编译之后、建沙箱之前（防沙箱泄漏）；不诞生、不占世界名额，下拍可重试。
        if let Some(occupant) =
            codeworld_core::occupancy::cell_occupied_by(&world.entities, req.pos, entity_id)
        {
            tracing::warn!(
                creator = %req.creator.0,
                occupant = %occupant.0,
                pos = ?req.pos,
                "[process_spawns] 出生格被占，该次 spawn 失败（下拍可重试）"
            );
            continue;
        }
        let entity = Entity::new(
            entity_id,
            owner,
            "creation".to_string(),
            req.pos,
            Appearance::new(req.glyph.clone(), req.color.clone(), 1.0),
            current_tick,
            req.fuel_limit,
        );
        // 沙箱创建成功才插入世界（失败则实体不落世界，不留无沙箱活体）
        if let Err(e) = sandbox_map.create(entity_id, &wasm_bytes, req.fuel_limit) {
            tracing::warn!(
                creator = %req.creator.0,
                error = %e,
                "[process_spawns] 造物沙箱创建失败，该次 spawn 作废"
            );
            continue;
        }
        world.entities.insert(entity_id, entity);
        tracing::info!(
            entity_id = %entity_id.0,
            creator = %req.creator.0,
            owner = %owner.0,
            pos = ?req.pos,
            glyph = %req.glyph,
            "造物诞生：玩家造物进场（门二，产权链归属创造者 owner）"
        );
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
        // 参战记录-攻击者（#013 战斗残影期）：出刀即战斗——落空/拦截/命中都算，
        // 堵"打空再拔线"绕过战斗残影的漏洞。world.tick() 即当前 tick（tick 开头已 advance）。
        sandbox_map.players.record_combat_id(atk.attacker, world.tick());
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
        // 参战记录-目标（#013）：被攻击即参战（含被拦截的收件）
        sandbox_map.players.record_combat_id(target_id, world.tick());
        // 收件：调目标【当前沙箱】的 on_attack（本阶段不换血不击杀）。
        // 残影实体不执行任何玩家代码（在线锚下线合同）：跳过防守询问直接结算——
        // 残影躯壳无人操控，不能继续享受原代码的拦截防守
        let intercepted = if sandbox_map.players.is_ghost(&target_id) {
            tracing::info!(
                attacker = %atk.attacker.0,
                target = %target_id.0,
                "[deliver_attacks] 目标残影期，跳过防守询问直接结算"
            );
            false
        } else {
            match sandbox_map.defend(&target_id, &atk.payload) {
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

/// 接触对（#021 B 项：接触语义 = ①物理同格检测 ②移动被挡，两类都算"接触"，
/// 为将来"接触开通道"铺路；本阶段只记日志不开通道）
#[derive(Debug, Clone, PartialEq)]
struct ContactPair {
    /// 接触方（移动被挡时 = 被挡的移动者）
    a: EntityId,
    /// 被接触方（移动被挡时 = 占据者）
    b: EntityId,
    /// 接触坐标（同格检测 = 重叠坐标；移动被挡 = 被挡的目标格坐标）
    pos: (f64, f64),
    /// 来源标记："same_cell"（物理同格检测）/ "move_blocked"（移动被挡）
    source: &'static str,
}

/// 处理接触事件（架构文档 4.1 节 b 步：接触触发沙箱通道）
///
/// 架构 4.1：物理接触后，框架自动为接触双方开启沙箱通道，双方获得对方的
/// EntityId 和坐标；是否 probe/attack 由玩家代码决定，框架只负责开通道。
///
/// MVP：逐对记录日志（双方 EntityId + 坐标 + 来源，info 级），返回接触对数量
/// 便于测试断言。#021 起接触来源两类：物理同格检测（same_cell）+ 移动被挡
/// （move_blocked），由 tick 合并后汇入本函数。
/// 跳过含死亡实体的接触对：死亡实体本 tick 会被死亡清理步移除，不应为其开通道/
/// 记接触（阶段一 life()=false 或 decay_survival 归零 kill 的实体此时仍在
/// world.entities 中，需在此按 is_alive 过滤）。
/// Phase 2：通过 sandbox_map 为双方开启 probe/attack 通道，让玩家代码在下一 tick
/// 的 on_tick 中可以 probe/attack 对方。
fn handle_collisions(pairs: &[ContactPair], entities: &HashMap<EntityId, Entity>) -> usize {
    let mut count = 0;
    for pair in pairs {
        // 跳过含死亡实体的接触对（架构 4.1 接触触发针对存活实体间交互）
        let a_alive = entities.get(&pair.a).map(|e| e.is_alive()).unwrap_or(false);
        let b_alive = entities.get(&pair.b).map(|e| e.is_alive()).unwrap_or(false);
        if !a_alive || !b_alive {
            tracing::debug!(a = %pair.a.0, b = %pair.b.0, "跳过含死亡实体的接触对");
            continue;
        }
        tracing::info!(
            a = %pair.a.0,
            b = %pair.b.0,
            pos = ?pair.pos,
            source = pair.source,
            "[contact] 接触事件：框架为接触双方开启沙箱通道"
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

/// 感知规则：找 target 最近的**视野内**其他存活实体坐标（env.scan 的数据源）
///
/// - 排除自己（不能锁定自己为敌人）
/// - 排除死亡实体（尸体不是感知目标）
/// - 排除视口外实体（架构 4.2 攻击前置条件"视距内"：眼睛诚实，屏幕外不可锁定，
///   超视距手段由玩家自行构建——decisions.md #018 框架零供给）
/// - None = 视野内没有其他存活实体（scan 返回 0）
/// - 架构 3.2 感知接口完整版为 get_visible_entities（可见实体列表）；MVP 单敌人取最近者
fn find_nearest_enemy(
    world: &WorldState,
    target: &EntityId,
    viewport: &BoundingBox,
) -> Option<(f64, f64)> {
    let me = world.entities.get(target)?;
    let mut best: Option<(f64, (f64, f64))> = None;
    for e in world.entities.values() {
        if !e.is_alive() || e.id == *target {
            continue;
        }
        // 视口过滤（架构 4.2"视距内"前置）：屏幕外不可锁定，与 fill_visible_snapshots
        // 同一个 BoundingBox（tick 同源传入），超视距由玩家自行构建（#018）
        let (ex, ey) = e.pos();
        if !viewport.contains(ex, ey) {
            continue;
        }
        let (dx, dy) = (ex - me.pos().0, ey - me.pos().1);
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
const SPY_CODE: &str = "spy";

/// 生成开局布阵生物（三层感知 Phase A 版）
///
/// A 攻击者（红"A"）@(5,0)：直线冲锋逼近，进范围连砍空 payload 击杀刀
/// D 防御者（青"D"）@(15,0)：原地站桩+盾 d_shield 拦截+每 4 tick 反制
/// H 搅局者（黄"H"）@(10,8)：中场上空，落地后观望游走，敌人进 3 格参战（h_enabled 开关）
/// S 伪装者（紫"S"）@(25,12)：右侧高空悬停（每拍 move_by 抵消重力，永不落地），
///   每 4 tick 眼睛列表扫描+probe 最近者，on_probe 撒谎"我是友军 life=9"；
///   无防守无攻击（间谍脆皮，s_enabled 开关）
/// A/D 站地面 y=0：地面实体不受重力，坐标 tick 间稳定，scan/attack_at 的
/// 坐标精确匹配（f64 相等）不被物理漂移打穿。
/// S 出生位 (25,12) 的取舍（任务书建议 (20,5) 微调）：D 的 scan 锁"最近敌人"，
/// S@20,5 距 D 仅 7.07 会抢在 A(10)/H(9.4) 之前被反制刀点名，tick4 即死、
/// 破坏剧本 1-3 回归；(25,12) 距 D 15.6 排最远也不抢锁；且每拍悬浮位移
/// 使 S 的结算坐标恒差快照 9.8，反制刀坐标过期落空（打不到悬停者）。
/// 默认参数（盾 10）双生物剧本 D 的反制先杀死 A；盾 1 则 A 磨穿防守反杀。
fn spawn_test_entities(
    world: &mut WorldState,
    sandbox_map: &mut EntitySandboxMap,
    d_shield: i32,
    h_enabled: bool,
    s_enabled: bool,
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
    // S 伪装者（可选登场）：右侧高空悬停 (25,12)（玩家代码每拍抵消重力），驻守侦察不移动
    let s_ok = if s_enabled {
        spawn(SPY_CODE, "S", "purple", (25.0, 12.0), &wasm(SPY_WAT))
    } else {
        false
    };
    tracing::info!(
        attacker_spawned = a_ok,
        defender_spawned = d_ok,
        hunter_spawned = h_ok,
        spy_spawned = s_ok,
        distance_a_d = 10.0,
        defender_shield = d_shield,
        "开局布阵：A 攻击者冲锋 / D 防御者站桩（盾每拦一刀-1，每4tick反制）/ H 搅局者={}（投机者：敌人进3格参战，被攻击50%拦截）/ S 伪装者={}（间谍：悬停高空+每4tick扫描+probe，on_probe撒谎友军，无防守）",
        if h_enabled { "登场" } else { "缺席" },
        if s_enabled { "登场" } else { "缺席" }
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
///   先 probe 该坐标看一眼（三层感知 Phase A：只读侦察，结果不用于决策、
///   打谁何时打不变），再对该坐标排空 payload 击杀刀（敌人若在移动则
///   坐标过期落空，站定才挨打）
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
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
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
    ;; 反制前 probe 目标看一眼（只读：探测结果不改变打谁何时打）
    (drop (call $probe (f64.load (i32.const 0)) (f64.load (i32.const 8))
                       (i32.const 16) (i32.const 0) (i32.const 32) (i32.const 64)))
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

/// 伪装者 S 的玩家代码 WAT（紫色，性格：间谍——悬浮高空+高调侦察+撒谎身份）
///
/// 策略（on_tick 每 tick 执行）：
/// 0. 每拍先悬浮：move_by(0,+9.8) 抵消当拍重力（DefaultPhysicsEngine 默认
///    gravity=-9.8；阶段二 a 先应用悬浮位移、物理步再扣重力，净高度不变，
///    S 稳定悬停 y≈12 永不落地）。悬浮在侦察节奏判断之前，每拍都执行，
///    且位移让阶段一快照坐标与阶段二 a2 攻击结算坐标恒差 9.8——反制刀
///    坐标过期落空，"够不着+打不中"就是间谍唯一的自卫
/// 1. tick 计数非 4 倍数直接返回；侦察轮先 log 10（侦察 marker）
/// 2. visible_count 数敌人，空则收工（全场只剩自己时不折腾）
/// 3. visible_at 循环扫一圈（眼睛列表），记与自己的平方距最近者
/// 4. 对最近者 probe(req=[1,2,3,4] -> resp@32 cap 64)，返回值存全局（观测导出）
///
/// on_probe（被探测时）：固定回 "LIE9" 假串（i32 0x3945494C，LE 字节
/// [0x4C,0x49,0x45,0x39]——谎称"友军、life=9"。撒谎=玩家代码自由，框架只透传；
/// 但被动体徵（api_calls 计数/fuel 剩余，host 采集）藏不住高调侦察的代价。
///
/// 无 on_attack 导出 = 不设防：被反制刀命中即被换血击杀（间谍脆皮）。
/// 日志 marker：10=侦察轮开始（D 的反制已占用 9，S 用 10；probe 往返见 host 侧 [probe] 日志）
const SPY_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (import "env" "visible_count" (func $visible_count (result i32)))
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  ;; tick 计数（4 倍数侦察）；probe 返回值（观测导出）
  (global $ticks (mut i32) (i32.const 0))
  (global $probe_ret (mut i32) (i32.const -999))
  ;; 被探测：固定回撒谎假串 "LIE9"
  (func (export "on_probe") (param i32) (result i32) i32.const 0x3945494C)
  (func (export "on_tick")
    (local $n i32) (local $i i32) (local $best f64) (local $bx f64) (local $by f64)
    (local $x f64) (local $y f64) (local $mx f64) (local $my f64)
    (local $dx f64) (local $dy f64) (local $d f64)
    global.get $ticks i32.const 1 i32.add global.set $ticks
    ;; 0. 每拍悬浮：先 +9.8 抵消物理步的 -9.8，净悬停（不受侦察节奏影响）
    (drop (call $move_by (f64.const 0.0) (f64.const 9.8)))
    ;; 1. 每 4 tick 侦察一轮
    (if (i32.rem_u (global.get $ticks) (i32.const 4))
      (then (return)))
    (call $log (i32.const 10))
    ;; 2. 数敌人；空则收工
    (local.set $n (call $visible_count))
    (if (i32.eqz (local.get $n))
      (then (return)))
    ;; 3. 读自己坐标（循环外一次，tick 内不变）
    (call $get_pos)
    (local.set $my)
    (local.set $mx)
    ;; 循环扫一圈找平方距最近者（WAT 无 break，用 block/loop/br_if）
    (f64.const 1e308) local.set $best
    (local.set $i (i32.const 0))
    (block $done
      (loop $scan
        (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
        (drop (call $visible_at (local.get $i) (i32.const 0) (i32.const 8)))
        (local.set $x (f64.load (i32.const 0)))
        (local.set $y (f64.load (i32.const 8)))
        (local.set $dx (f64.sub (local.get $x) (local.get $mx)))
        (local.set $dy (f64.sub (local.get $y) (local.get $my)))
        (local.set $d (f64.add
          (f64.mul (local.get $dx) (local.get $dx))
          (f64.mul (local.get $dy) (local.get $dy))))
        (if (f64.lt (local.get $d) (local.get $best))
          (then
            (local.set $best (local.get $d))
            (local.set $bx (local.get $x))
            (local.set $by (local.get $y))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $scan)))
    ;; 4. 对最近者 probe（request=[1,2,3,4] -> resp@32 cap 64）
    (global.set $probe_ret
      (call $probe (local.get $bx) (local.get $by)
                    (i32.const 16) (i32.const 4) (i32.const 32) (i32.const 64))))
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_probe_ret") (result i32) global.get $probe_ret))"#;

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{EntityId, PlayerId};
    use std::path::PathBuf;
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

    /// 测试用无界视口：覆盖全地图，等价旧"上帝视角"语义——既有测试不关心视口过滤，
    /// 用它保持原行为；视口过滤本身的测试单独构造有界视口
    fn unbounded_bb() -> BoundingBox {
        BoundingBox::new(-1_000_000.0, -1_000_000.0, 1_000_000.0, 1_000_000.0)
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
        let to_kill = map.run_player_code(&world, &unbounded_bb());
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
        let to_kill = map.run_player_code(&world, &unbounded_bb());
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
        let to_kill_before = map.run_player_code(&world, &unbounded_bb());
        assert!(
            to_kill_before.contains(&id),
            "create 后沙箱应生效，实体应进 to_kill，实际 {:?}",
            to_kill_before
        );
        // 移除后：无沙箱 -> run_player_code 跳过，不进 to_kill
        map.remove(&id);
        let to_kill_after = map.run_player_code(&world, &unbounded_bb());
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
        apply_pending_changes(&mut world, &mut pending, None);
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
        apply_pending_changes(&mut world, &mut pending, None);
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
        apply_pending_changes(&mut world, &mut pending, None);
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
        apply_pending_changes(&mut world, &mut pending, None);
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
        apply_pending_changes(&mut world, &mut pending, None);
        assert!(pending.is_empty(), "apply 后 pending 应清空");
    }

    // === #021 实体占位制：移动占位裁决 ===

    #[test]
    fn setpos_blocked_stays_at_origin() {
        // 占位移动：目标格被其他存活实体占据 → 移动被拒，实体留在原坐标
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.0));
        let e2 = make_entity(2, (6.0, 0.0));
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.0, 0.0),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.0), "目标格被占应留原位");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (6.0, 0.0), "占据者不受影响");
    }

    #[test]
    fn setpos_two_movers_same_target_lower_id_wins() {
        // 占位裁决确定序：两实体同抢一格，EntityId 小者先到先得（与排队顺序无关）
        // E2 的请求先入队、E1 的后入队：按队序应用则 E2 赢；按 id 升序裁决则 E1 赢
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.0));
        let e2 = make_entity(2, (7.0, 0.0));
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let mut pending: Vec<PendingChange> = vec![
            PendingChange::SetPos { entity: id2, new_pos: (6.0, 0.0) }, // 先入队
            PendingChange::SetPos { entity: id1, new_pos: (6.0, 0.0) }, // 后入队
        ];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 0.0), "id 小者先到先得");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (7.0, 0.0), "id 大者被挡留原位");
    }

    #[test]
    fn setpos_dead_occupant_does_not_block() {
        // 反向护栏：占据者已死（待清理）→ 不挡移动
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.0));
        let mut e2 = make_entity(2, (6.0, 0.0));
        e2.kill();
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.0, 0.0),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 0.0), "死者不占格，移动放行");
    }

    #[test]
    fn setpos_sequential_vacated_cell_blocked_for_lower_id() {
        // 先到先得语义边界：低 id 先裁决——E1 想进 E2 的格、E2 同拍搬走：
        // E1 裁决时 E2 还在原格 → E1 被挡（Screeps 顺序移动语义，下拍 E1 可进）
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.0));
        let e2 = make_entity(2, (6.0, 0.0));
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let mut pending: Vec<PendingChange> = vec![
            PendingChange::SetPos { entity: id1, new_pos: (6.0, 0.0) },
            PendingChange::SetPos { entity: id2, new_pos: (9.0, 0.0) },
        ];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.0), "E1 裁决时目标格仍被 E2 占 → 被挡");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (9.0, 0.0), "E2 照常搬走");
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
        apply_pending_changes(&mut world, &mut pending, None);
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
        apply_pending_changes(&mut world, &mut pending, None);
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
        let to_kill = map.run_player_code(&world, &unbounded_bb());
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
        let to_kill = map.run_player_code(&world, &unbounded_bb());
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
        let to_kill = sandbox_map.run_player_code(&world, &unbounded_bb());
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
        spawn_test_entities(&mut world, &mut sandbox_map, 10, false, false);
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
        spawn_test_entities(&mut world, &mut sandbox_map, 10, true, false);
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
        spawn_test_entities(&mut world, &mut sandbox_map, 1, false, false);
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

    // === handle_collisions（阶段二 c 步：接触触发沙箱通道，#021 接触记双方）===

    /// 构造接触对（测试辅助，固定 EntityId 便于断言；默认来源 same_cell）
    fn make_contact_pair(a: u128, b: u128, pos: (f64, f64)) -> ContactPair {
        ContactPair {
            a: EntityId(Uuid::from_u128(a)),
            b: EntityId(Uuid::from_u128(b)),
            pos,
            source: "same_cell",
        }
    }

    /// 构造存活实体表（测试辅助，id 与 make_contact_pair 的 a/b 对齐）
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
        // 无接触对：返回 0，不 panic（架构 4.1 b 步：无接触则不开通道）
        let pairs: Vec<ContactPair> = Vec::new();
        let entities = HashMap::new();
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 0, "无接触对应返回 0");
    }

    #[test]
    fn handle_collisions_returns_one_for_single_pair() {
        // 单个存活接触对：返回 1（两个实体同坐标 -> 1 对接触）
        let entities = make_entities(&[1, 2]);
        let pairs = vec![make_contact_pair(1, 2, (5.0, 5.0))];
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 1, "单个存活接触对应返回 1");
    }

    #[test]
    fn handle_collisions_returns_three_for_three_pairs() {
        // 三个存活接触对（三个实体两两同坐标，C(3,2)=3）：返回 3
        let entities = make_entities(&[1, 2, 3]);
        let pairs = vec![
            make_contact_pair(1, 2, (5.0, 5.0)),
            make_contact_pair(1, 3, (5.0, 5.0)),
            make_contact_pair(2, 3, (5.0, 5.0)),
        ];
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 3, "三个存活接触对应返回 3");
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
        // #021：物理同格对转 ContactPair（same_cell 来源）后汇入 handle_collisions
        let contacts: Vec<ContactPair> = pairs
            .into_iter()
            .map(|p| ContactPair {
                a: p.a,
                b: p.b,
                pos: p.pos,
                source: "same_cell",
            })
            .collect();
        let count = handle_collisions(&contacts, &entities);
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
        let pairs = vec![make_contact_pair(1, 2, (5.0, 5.0))];
        let count = handle_collisions(&pairs, &entities);
        assert_eq!(count, 0, "含死亡实体的接触对应被跳过，返回 0");
    }

    #[test]
    fn tick_retires_shove_no_displacement_on_same_cell() {
        // #021 推挤退役回归：两实体同坐标（异常路径）→ tick 后坐标原样保留，
        // 不再出现旧挤压的 x+1 连环位移（P/T 连体环游世界 bug 的退役验证）。
        // 旧断言"坐标应不同（一方被挤开）"随推挤机制一并退役，本测试取而代之。
        // （占位制下正常路径不会再产生同格：移动被挡/spawn 预检/占位重力三闸，
        //   本测试直接构造异常同格，验证即使发生也无推挤。）
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
        assert_eq!(pos1, (5.0, 0.0), "退役后不推挤：实体1坐标不变");
        assert_eq!(pos2, (5.0, 0.0), "退役后不推挤：实体2坐标不变");
    }

    #[test]
    fn tick_move_blocked_records_contact_and_stays() {
        // tick 级集成（#021 A+B）：移动者试图走进占据格 → 留原位；
        // 移动被挡接触对与物理同格对一样汇入 handle_collisions
        // （返回值为 usize 无法从 tick 断言，这里验证坐标行为；接触计数由
        // apply_pending_changes/handle_collisions 单元测试覆盖）
        let mut world = WorldState::new();
        let id1 = EntityId(Uuid::from_u128(1));
        let id2 = EntityId(Uuid::from_u128(2));
        world.entities.insert(id1, make_entity(1, (5.0, 0.0)));
        world.entities.insert(id2, make_entity(2, (6.0, 0.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.0, 0.0),
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
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.0), "目标格被占应留原位");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (6.0, 0.0), "占据者不受影响");
    }

    #[test]
    fn apply_blocked_move_returns_contact_pair() {
        // #021 B 项：apply_pending_changes 返回移动被挡接触对（双方 EntityId + 目标格 + 来源）
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.0));
        let e2 = make_entity(2, (6.0, 0.0));
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.5, 0.2),
        }];
        let contacts = apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(contacts.len(), 1, "移动被挡应产生 1 对接触");
        assert_eq!(contacts[0].a, id1, "a = 被挡的移动者");
        assert_eq!(contacts[0].b, id2, "b = 占据者");
        assert_eq!(contacts[0].pos, (6.5, 0.2), "接触坐标 = 被挡的目标格坐标");
        assert_eq!(contacts[0].source, "move_blocked", "来源 = 移动被挡");
    }

    #[test]
    fn apply_free_move_returns_no_contacts() {
        // 反向护栏：正常移动不产生接触对
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.0));
        let id1 = e1.id;
        world.entities.insert(id1, e1);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.0, 0.0),
        }];
        let contacts = apply_pending_changes(&mut world, &mut pending, None);
        assert!(contacts.is_empty(), "空格移动不产生接触");
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 0.0));
    }

    // === 操控层 v0：tick 级意图路由（#022 操控优先于自主）===

    /// 自主东行者 WAT：每拍 move_by(+1,0)（模拟 pioneer 的挂机AI自主移动）
    const AUTO_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (memory (export "memory") 1)
  (func (export "on_tick") (drop (call $move_by (f64.const 1.0) (f64.const 0.0))))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 操控测试舞台：玩家 p1@(30,0) 挂指定 WAT 沙箱并登记为 me 声明的操控目标（#024），
    /// 返回 (world, time, physics, renderer, storage, pending, sandbox_map, pid, me 目录)。
    /// me 目录 = 临时 players/ 目录（p1.wat 在场 + me.txt="p1"），测试可改文件模拟声明变化。
    #[allow(clippy::type_complexity)]
    fn control_stage(
        player_wat: &str,
    ) -> (
        WorldState,
        TimeEngine,
        DefaultPhysicsEngine,
        Renderer,
        StorageEngine,
        Vec<PendingChange>,
        EntitySandboxMap,
        EntityId,
        PathBuf,
    ) {
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let pid = make_entity_with_sandbox(&mut world, &mut map, 1, (30.0, 0.0), player_wat);
        let dir = temp_players_dir("control");
        std::fs::write(dir.join("p1.wat"), player_wat).unwrap();
        std::fs::write(dir.join("me.txt"), "p1").unwrap();
        map.players = PlayerDirectory::new(&dir);
        map.players.register_online("p1", pid);
        (
            world,
            TimeEngine::new(),
            DefaultPhysicsEngine::new(),
            test_renderer(),
            StorageEngine::open_in_memory().unwrap(),
            Vec::new(),
            map,
            pid,
            dir,
        )
    }

    #[test]
    fn control_move_overrides_autonomous_move() {
        // #022 操控优先于自主：注入 Move(-1,0) -> 本拍自主 move_by(+1,0) 被抑制
        //（on_tick 跳过），实体按操控方向西移 30 -> 29（自主则是 31）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![input::Intent::Move { dx: -1.0, dy: 0.0 }];
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (29.0, 0.0),
            "操控移动覆盖自主移动"
        );
    }

    #[test]
    fn me_online_no_intent_stands_still_ctrl_exclusive() {
        // #025 操控独占：me 在线期间不按键角色就站着（on_tick 全程抑制），
        // 不存在"无意图拍回落自主"——旧语义（#022 回落）被 #025 取代
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (30.0, 0.0),
            "me 在线无按键：自主 move_by(+1,0) 被抑制，站住"
        );
        assert!(
            map.ctrl_exclusive_active,
            "操控独占生效中（状态翻转沿供生命周期日志）"
        );
    }

    #[test]
    fn me_declaration_removed_resumes_autonomy() {
        // #025 下线交接：me.txt 删除 -> 无操控目标 -> 自主恢复（P 自己走起来）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, me_dir) =
            control_stage(AUTO_MOVER_WAT);
        std::fs::remove_file(me_dir.join("me.txt")).unwrap();
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.0),
            "me 删除后自主恢复东行"
        );
        assert!(
            !map.ctrl_exclusive_active,
            "操控释放（自主恢复），独占状态关闭"
        );
    }

    #[test]
    fn ctrl_exclusive_lifecycle_flip_on_gain_and_loss() {
        // #025 生命周期：操控建立拍翻转 true（打"独占开始"日志），失去拍翻转 false（"释放"）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, _pid, me_dir) =
            control_stage(AUTO_MOVER_WAT);
        assert!(!map.ctrl_exclusive_active, "起步未独占");
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(map.ctrl_exclusive_active, "me 在线 -> 独占开始");
        std::fs::write(me_dir.join("me.txt"), "nobody").unwrap(); // 声明改无效名 = 失去操控
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(!map.ctrl_exclusive_active, "声明失效 -> 操控释放（自主恢复）");
    }

    #[test]
    fn control_move_blocked_by_occupancy_stays_in_place() {
        // 操控不豁免物理（宪法）：目标格被占 -> 操控移动被占位裁决挡下，实体留原位
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        // 占据者：无沙箱普通实体（非玩家）放在西邻格
        world.entities.insert(
            EntityId(Uuid::from_u128(9)),
            make_entity(9, (29.0, 0.0)),
        );
        let intents = vec![input::Intent::Move { dx: -1.0, dy: 0.0 }];
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (30.0, 0.0),
            "操控移动被占位裁决挡下（塔前停）"
        );
        assert_eq!(
            world.entities.get(&EntityId(Uuid::from_u128(9))).unwrap().pos(),
            (29.0, 0.0),
            "占据者不受影响"
        );
    }

    #[test]
    fn control_attack_kills_nearest_visible_enemy() {
        // 空格 = 对最近可见敌人出刀（复用 scan 语义与 find_nearest_enemy 同源）；
        // 操控刀走既有两阶段结算 -> 击杀 + 掠夺 1 生存点数（操控不豁免物理）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let victim = make_entity_with_sandbox(&mut world, &mut map, 2, (32.0, 0.0), TARGET_WAT);
        let intents = vec![input::Intent::Attack];
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert!(
            !world.entities.contains_key(&victim),
            "操控刀击杀最近可见敌人（TARGET_WAT 无防守）"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().survival.points(),
            4,
            "击杀掠夺：攻击者 3 -> 4"
        );
    }

    #[test]
    fn keys_without_player_online_are_safe() {
        // 无玩家在线按键：不崩、不操控任何实体（实体自主照常）、记一次 no player online
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        // 实体带自主移动沙箱但从未登记玩家（无操控目标）
        let lone = make_entity_with_sandbox(&mut world, &mut map, 1, (30.0, 0.0), AUTO_MOVER_WAT);
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let intents = vec![
            input::Intent::Move { dx: -1.0, dy: 0.0 },
            input::Intent::Attack,
        ];
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&lone).unwrap().pos(),
            (31.0, 0.0),
            "无玩家在线时按键不操控实体（自主照常）"
        );
        assert!(map.warned_no_player, "应记一次 no player online 提示");
        assert!(
            !renderer.caption_line().contains("ctrl:"),
            "无玩家在线无 ctrl 段（区分“无人”与“有人但不是你”），实际 {}",
            renderer.caption_line()
        );
    }

    #[test]
    fn me_declaration_controls_declared_player_not_first_joined() {
        // #024 核心语义（修正 #023）：操控目标 = me 声明的名字，与进场顺序无关——
        // p1 先登记（进程序第一）、me.txt 声明 p2：西移键操控 p2，p1 走代码自主
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let pid2 = make_entity_with_sandbox(&mut world, &mut map, 2, (35.0, 0.0), AUTO_MOVER_WAT);
        std::fs::write(me_dir.join("p2.wat"), AUTO_MOVER_WAT).unwrap();
        std::fs::write(me_dir.join("me.txt"), "p2").unwrap();
        map.players.register_online("p2", pid2);
        let intents = vec![input::Intent::Move { dx: -1.0, dy: 0.0 }];
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&pid2).unwrap().pos(),
            (34.0, 0.0),
            "声明的 p2 被操控西移（on_tick 被抑制，仅操控位移）"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.0),
            "未声明的 p1 不受操控，代码自主东移"
        );
    }

    #[test]
    fn no_me_declaration_spectator_keys_safe_and_ctrl_dashes() {
        // #024 观战：玩家在线但无 me 声明 -> 无操控目标（按键安全，自主照常），
        // 字幕 ctrl:--（有人但不是你），观战按键提示一次
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, me_dir) =
            control_stage(AUTO_MOVER_WAT);
        std::fs::remove_file(me_dir.join("me.txt")).unwrap();
        let intents = vec![
            input::Intent::Move { dx: -1.0, dy: 0.0 },
            input::Intent::Attack,
        ];
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.0),
            "观战模式按键不操控实体（代码自主照常）"
        );
        assert!(map.warned_no_player, "观战模式按键应提示一次");
        assert!(
            renderer.caption_line().contains("ctrl:--"),
            "有人但不是你 -> ctrl:--，实际 {}",
            renderer.caption_line()
        );
    }

    #[test]
    fn invalid_me_declaration_spectates_and_warns_once() {
        // #024 声明无效（拼错/未进场）：warn 一次 + 回落观战（ctrl:--，按键安全）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, me_dir) =
            control_stage(AUTO_MOVER_WAT);
        std::fs::write(me_dir.join("me.txt"), "ghost").unwrap();
        let intents = vec![input::Intent::Move { dx: -1.0, dy: 0.0 }];
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.0),
            "声明无效 -> 观战，代码自主照常"
        );
        assert!(map.warned_invalid_me, "无效声明应 warn 一次");
        assert!(
            renderer.caption_line().contains("ctrl:--"),
            "声明无效 -> ctrl:--，实际 {}",
            renderer.caption_line()
        );
    }

    #[test]
    fn ws_move_walks_tick_by_tick_until_arrival() {
        // #027 点击走位全链路：拍1 收 WsMove 立即走一步，拍2 无意图续走，拍3 到达停住
        //（30 -> 31 -> 32 到达；之后不再抖动，持续目标清除）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let walk = vec![input::Intent::WsMove { x: 32.0, y: 0.0 }];
        let none: Vec<input::Intent> = vec![];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &walk);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.0), "拍1 向点击目标走一步");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (32.0, 0.0), "拍2 续走到达");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (32.0, 0.0),
            "拍3 到达停住（目标清除，格距<=0.5 判定）"
        );
        assert!(map.ws_move_target.is_none(), "到达后持续目标清除");
    }

    #[test]
    fn ws_stop_cancels_walk_and_stands() {
        // #027 stop 全链路：走位中收到 Stop -> 清目标站住，后续空拍也不复活走位
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let walk = vec![input::Intent::WsMove { x: 35.0, y: 0.0 }];
        let stop = vec![input::Intent::Stop];
        let none: Vec<input::Intent> = vec![];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &walk);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.0), "走位中");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &stop);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.0), "Stop 拍站住");
        assert!(map.ws_move_target.is_none(), "Stop 清除持续目标");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.0), "目标已清，不复活走位");
    }

    #[test]
    fn ws_attack_strikes_clicked_coords_same_pipeline() {
        // #027 点击出刀全链路：对点击坐标出刀（服务器以坐标找人），与键盘空格同一
        // 两阶段结算通路——击杀 + 掠夺，操控不豁免物理
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let victim = make_entity_with_sandbox(&mut world, &mut map, 2, (32.0, 0.0), TARGET_WAT);
        let intents = vec![input::Intent::WsAttack { x: 32.0, y: 0.0 }];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &intents);
        assert!(
            !world.entities.contains_key(&victim),
            "点击坐标处的敌人被操控刀击杀"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().survival.points(),
            4,
            "击杀掠夺：攻击者 3 -> 4（与键盘攻击同通路）"
        );
    }

    #[test]
    fn controlled_offline_latches_spectator_no_auto_restore() {
        // #024 下线回落：操控中的目标下线（删玩家文件）-> warn 一次 + latch 观战；
        // 重新进场（文件回来，新实体）不自动恢复操控——重启重开（MVP 从简）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![input::Intent::Move { dx: -1.0, dy: 0.0 }];
        let no_intents: Vec<input::Intent> = vec![];
        // tick1：正常操控西移 30 -> 29
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (29.0, 0.0), "操控中");
        // 删玩家文件 = 下线信号（在线锚本拍 d0 检测）；tick2 下线进残影
        std::fs::remove_file(me_dir.join("p1.wat")).unwrap();
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &no_intents,
        );
        // tick3：声明仍指向 p1 但已不在线 -> latch 观战，西移键无效（残影 30 不动）
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert!(
            map.controlled_offline_latched,
            "操控目标下线应 latch 观战（本局不自动恢复）"
        );
        assert!(
            !map.warned_invalid_me,
            "latch 路径只打“下线”warn，不重复打“声明无效”warn"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (29.0, 0.0),
            "latch 后操控无效（#025 语义更新：下线检测拍 me 仍在线=独占抑制，实体冻在 29 不再自主多走一步）"
        );
        assert!(
            renderer.caption_line().contains("ctrl:--"),
            "操控目标下线后回落 ctrl:--，实际 {}",
            renderer.caption_line()
        );
        // 玩家文件回来：新实体重新进场（新 EntityId），本局不恢复操控——新实体走代码自主
        std::fs::write(me_dir.join("p1.wat"), AUTO_MOVER_WAT).unwrap();
        // tick4：spawn 拍（新实体进场，本拍不跑玩家代码，出生在 (30,0)）
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &no_intents,
        );
        let new_id = world
            .entities
            .values()
            .find(|e| e.code_hash == "p1")
            .expect("重新进场的 p1 实体应存在")
            .id;
        assert_ne!(new_id, pid, "重新进场是新实体（新 EntityId）");
        // tick5：西移键无效（latch 观战），新实体走代码自主东移 30 -> 31
        tick_with_intents(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&new_id).unwrap().pos(),
            (31.0, 0.0),
            "不自动恢复操控：新实体走代码自主东移（操控西移无效）"
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
        let to_kill = map.run_player_code(&world, &unbounded_bb());
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
        let found = find_nearest_enemy(&world, &me, &unbounded_bb());
        assert_eq!(found, Some((3.0, 4.0)), "应锁定距离 5 的最近实体");
    }

    #[test]
    fn find_nearest_enemy_excludes_self() {
        // 规则：排除自己（不能把自己当敌人锁定）
        let mut world = WorldState::new();
        let me = EntityId(Uuid::from_u128(1));
        world.entities.insert(me, make_entity(1, (7.0, 8.0)));
        let found = find_nearest_enemy(&world, &me, &unbounded_bb());
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
        let found = find_nearest_enemy(&world, &me, &unbounded_bb());
        assert_eq!(found, None, "唯一邻居已死，应返回 None");
    }

    #[test]
    fn find_nearest_enemy_skips_enemy_outside_viewport() {
        // 视口过滤：视野外有更近实体时不选中，锁定视野内的较远者
        //（架构 4.2 攻击前置条件"视距内"；眼睛诚实，decisions.md #018）
        let mut world = WorldState::new();
        let me = EntityId(Uuid::from_u128(10));
        let near_outside = EntityId(Uuid::from_u128(11)); // (2,0) 距离 2，但视野外
        let far_inside = EntityId(Uuid::from_u128(12)); // (10,0) 距离 10，视野内
        world.entities.insert(me, make_entity(10, (0.0, 0.0)));
        world
            .entities
            .insert(near_outside, make_entity(11, (2.0, 0.0)));
        world
            .entities
            .insert(far_inside, make_entity(12, (10.0, 0.0)));
        // 视口 [5,-5]..[25,5]：含 far_inside(10,0)，不含 near_outside(2,0)
        let viewport = BoundingBox::new(5.0, -5.0, 25.0, 5.0);
        let found = find_nearest_enemy(&world, &me, &viewport);
        assert_eq!(
            found,
            Some((10.0, 0.0)),
            "视野外更近实体不得锁定，应选视野内的较远者"
        );
    }

    #[test]
    fn find_nearest_enemy_none_when_all_enemies_outside_viewport() {
        // 视口过滤：只有视野外实体 -> None（scan 返回 0，视野内无目标语义）
        let mut world = WorldState::new();
        let me = EntityId(Uuid::from_u128(13));
        let outside = EntityId(Uuid::from_u128(14)); // (2,0) 视野外
        world.entities.insert(me, make_entity(13, (0.0, 0.0)));
        world.entities.insert(outside, make_entity(14, (2.0, 0.0)));
        let viewport = BoundingBox::new(5.0, -5.0, 25.0, 5.0);
        let found = find_nearest_enemy(&world, &me, &viewport);
        assert_eq!(found, None, "视野内没有存活实体，应返回 None");
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
        map.run_player_code(&world, &unbounded_bb());
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
        map.run_player_code(&world, &unbounded_bb());
        assert_eq!(
            call_i32_export(&mut map, &a, "get_found"),
            0,
            "独存实体 scan 应返回 0"
        );
    }

    #[test]
    fn scan_returns_zero_when_enemy_outside_viewport() {
        // 视口过滤端到端：敌人在视野外时 scan 返回 0（不写坐标）——
        // run_player_code 用 tick 同款视口填 nearest_enemy，视野外不可锁定
        let mut world = WorldState::new();
        let me = EntityId(Uuid::from_u128(15));
        let enemy = EntityId(Uuid::from_u128(16));
        world.entities.insert(me, make_entity(15, (10.0, 0.0))); // 视野内
        world.entities.insert(enemy, make_entity(16, (2.0, 0.0))); // 视野外
        let mut map = test_sandbox_map();
        map.create(me, &wasm(SCAN_PROBE_WAT), 1_000_000).unwrap();
        map.create(enemy, &wasm(SCAN_PROBE_WAT), 1_000_000).unwrap();
        // 视口 [5,-5]..[25,5]：含 me(10,0)，不含 enemy(2,0)
        let viewport = BoundingBox::new(5.0, -5.0, 25.0, 5.0);
        map.run_player_code(&world, &viewport);
        assert_eq!(
            call_i32_export(&mut map, &me, "get_found"),
            0,
            "敌人在视野外时 scan 应返回 0（视野内无目标）"
        );
        let x = call_f64_export(&mut map, &me, "get_enemy_x");
        let y = call_f64_export(&mut map, &me, "get_enemy_y");
        assert_eq!(
            (x, y),
            (0.0, 0.0),
            "视野外目标坐标不得写入 memory（scan 未写指针区）"
        );
    }

    // === 三层感知 Phase A：眼睛列表快照填充（fill_visible_snapshots）===

    /// 测试用视口世界范围（同 test_renderer 的 50x50：世界 (-50,-25)..(50,25)）
    fn test_viewport_bb() -> BoundingBox {
        BoundingBox::new(-50.0, -25.0, 50.0, 25.0)
    }

    #[test]
    fn fill_visible_snapshots_excludes_self_and_outside() {
        // 眼睛列表：viewport 过滤 + 排除自己 + EntityId（Uuid）升序稳定排序
        // 三个实体：a(0,0) 在视口、b(30,20) 在视口、c(90,90) 在视口外
        let mut world = WorldState::new();
        let a = EntityId(Uuid::from_u128(1));
        let b = EntityId(Uuid::from_u128(2));
        let c = EntityId(Uuid::from_u128(3));
        world.entities.insert(a, make_entity(1, (0.0, 0.0)));
        world.entities.insert(b, make_entity(2, (30.0, 20.0)));
        world.entities.insert(c, make_entity(3, (90.0, 90.0)));
        let mut map = test_sandbox_map();
        map.create(a, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        map.create(b, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        map.create(c, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        map.fill_visible_snapshots(&world, &test_viewport_bb());
        // A 视角：只见 B（C 在视口外、自己被排除）
        assert_eq!(
            map.sandboxes.get(&a).unwrap().store.data().visible,
            vec![(30.0, 20.0)],
            "A 的可见列表应只有 B 的坐标（不含自己、不含视口外的 C）"
        );
        // B 视角：只见 A
        assert_eq!(
            map.sandboxes.get(&b).unwrap().store.data().visible,
            vec![(0.0, 0.0)],
            "B 的可见列表应只有 A 的坐标"
        );
        // 全局列表（bridge 用）：viewport 内 2 个，按 Uuid 升序
        assert_eq!(map.visible_ids.len(), 2, "全局可见列表应有 2 个（排除视口外）");
        let id0 = map.visible_ids[0].0;
        let id1 = map.visible_ids[1].0;
        assert!(
            id0 < id1,
            "全局可见列表应按 EntityId(Uuid) 升序稳定排序"
        );
    }

    #[test]
    fn fill_visible_snapshots_excludes_dead() {
        // 规则：尸体不是感知目标（与 scan 的 nearest_enemy 排死者一致）
        let mut world = WorldState::new();
        let a = EntityId(Uuid::from_u128(1));
        let corpse = EntityId(Uuid::from_u128(2));
        let mut corpse_e = make_entity(2, (1.0, 0.0));
        corpse_e.kill();
        world.entities.insert(a, make_entity(1, (0.0, 0.0)));
        world.entities.insert(corpse, corpse_e);
        let mut map = test_sandbox_map();
        map.create(a, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        map.fill_visible_snapshots(&world, &test_viewport_bb());
        assert!(
            map.sandboxes.get(&a).unwrap().store.data().visible.is_empty(),
            "A 的可见列表应为空（唯一邻居已死）"
        );
    }

    // === 三层感知 Phase A：probe 全链路（bridge 挂载 -> import 同步探测）===

    /// 探测者 WAT（main 侧）：on_tick 调 probe(5.0, 7.0, 0, 4, 64, 64)，
    /// 返回值存全局；memory[0..4] 放 request 字节 [1,2,3,4]、[64..] 收响应
    const MAIN_PROBER_WAT: &str = r#"(module
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -999))
  (memory (export "memory") 1)
  (data (i32.const 0) "\01\02\03\04")
  (func (export "on_tick")
    f64.const 5.0 f64.const 7.0 i32.const 0 i32.const 4 i32.const 64 i32.const 64
    call $probe global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "get_resp0") (result i32) (i32.load (i32.const 64)))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 被探测撒谎者 WAT（main 侧）：on_probe 回 "LIE9" 假串（同 arena 测试协议）
    const MAIN_LIAR_WAT: &str = r#"(module
  (func (export "on_probe") (param i32) (result i32) i32.const 0x3945494C)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn run_player_code_probe_reaches_target_and_gets_reply() {
        // 全链路：fill_visible_snapshots -> run_player_code 挂桥 -> 玩家 on_tick 调
        // env.probe -> host 同步调目标 on_probe -> 假串+体徵写回探测者 memory。
        let mut world = WorldState::new();
        let prober_id = EntityId(Uuid::from_u128(1));
        let liar_id = EntityId(Uuid::from_u128(2));
        world.entities.insert(prober_id, make_entity(1, (0.0, 0.0)));
        world.entities.insert(liar_id, make_entity(2, (5.0, 7.0)));
        let mut map = test_sandbox_map();
        map.create(prober_id, &wasm(MAIN_PROBER_WAT), 1_000_000).unwrap();
        map.create(liar_id, &wasm(MAIN_LIAR_WAT), 1_000_000).unwrap();
        map.fill_visible_snapshots(&world, &test_viewport_bb());
        map.run_player_code(&world, &unbounded_bb());
        // 响应 = 假串 4 字节 + 体徵 24 字节 = 28，resp_cap=64 不截断
        assert_eq!(
            call_i32_export(&mut map, &prober_id, "get_ret"),
            28,
            "probe 命中应返回 28（4 字节假串 + 24 字节体徵）"
        );
        // 假串原样到达（撒谎透传：框架只搬运，不审查内容）
        assert_eq!(
            call_i32_export(&mut map, &prober_id, "get_resp0"),
            0x3945494C,
            "探测者应收到目标 on_probe 的假串 LIE9"
        );
        // 探测者的 api_calls 记录了 probe
        assert!(
            map.sandboxes
                .get(&prober_id)
                .unwrap()
                .store
                .data()
                .api_calls
                .contains(&"probe".to_string()),
            "probe 调用应记入探测者 api_calls"
        );
        // 被探测者的 api_calls（含 fill_visible_snapshots 不记、只有目标自身行为）
        // 作为体徵源存在——S 间谍频繁侦察 -> api_calls 高 -> 体徵藏不住
        let liar_data = map.sandboxes.get(&liar_id).unwrap().store.data();
        assert!(
            !liar_data.log_markers.is_empty() || liar_data.fuel_consumed < 1_000_000,
            "目标沙箱应仍可用（探测不破坏目标状态）"
        );
    }

    // === 三层感知 Phase A：伪装者 S（间谍型第四生物）===

    #[test]
    fn spawn_test_entities_spawns_spy_when_enabled() {
        // S 登场：CODEWORLD_S_ENABLED 默认开 -> 四方布阵含紫"S"（code_hash=spy）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut map, 10, true, true);
        let spy = world.entities.values().find(|e| e.code_hash == SPY_CODE);
        assert!(spy.is_some(), "s_enabled=true 应登场伪装者 S");
        let s = spy.unwrap();
        assert_eq!(s.pos(), (25.0, 12.0), "S 应出生在 (25,12) 右侧高空悬停位");
        assert_eq!(s.appearance.glyph, "S", "S 的字形应为紫 S");
        assert_eq!(world.entities.len(), 4, "S 登场时四方布阵应有 4 个实体");
    }

    #[test]
    fn spawn_test_entities_skips_spy_when_disabled() {
        // S 缺席：s_enabled=false -> 三方布阵（剧本 1-3 回归形态）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut map, 10, true, false);
        assert!(
            !world.entities.values().any(|e| e.code_hash == SPY_CODE),
            "s_enabled=false 不应有 S"
        );
        assert_eq!(world.entities.len(), 3, "S 缺席时应有 3 个实体");
    }

    // === 悬浮间谍（S 每拍抵消重力，悬停高空）===

    #[test]
    fn spy_hovers_above_ground_every_tick() {
        // 悬浮间谍：S 出生 (25,12) 空中，每拍 move_by(0,+9.8) 抵消重力
        // （DefaultPhysicsEngine::new() 默认 gravity=-9.8）——跑 8 拍
        // （含侦察 4 倍数拍与 1/2/3 非侦察拍），S 的 y 全程 > 5、永不落地。
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(7));
        world.entities.insert(id, make_entity(7, (25.0, 12.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        sandbox_map.create(id, &wasm(SPY_WAT), 1_000_000).unwrap();
        for tick_n in 1..=8 {
            tick(
                &mut world,
                &mut time,
                &physics,
                &mut renderer,
                &mut storage,
                &mut pending,
                &mut sandbox_map,
            );
            let y = world
                .entities
                .get(&id)
                .unwrap_or_else(|| panic!("第 {tick_n} 拍 S 应存活"))
                .pos()
                .1;
            assert!(
                y > 5.0,
                "第 {tick_n} 拍 S 应悬空（悬浮不受侦察节奏影响），实际 y={y}"
            );
        }
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
        map.run_player_code(&world, &unbounded_bb());
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

    /// 冒烟辅助：世界里是否还有指定字形的实体
    fn glyph_alive(world: &WorldState, glyph: &str) -> bool {
        world.entities.values().any(|e| e.appearance.glyph == glyph)
    }

    #[test]
    fn smoke_default_viewport_four_bio_timeline_unchanged() {
        // 冒烟（scan 视野化回归验收点）：默认视口 50x25（对齐真实 Config），四生物
        // + E 类玩家 @(35,5)（PLAYER_WAT 同 players/_template.wat.txt：只 log 不动不 scan）。
        // 全员都在 [-50,50]x[-25,25] 内，视口过滤是 no-op——剧本时间线必须不变：
        // tick4 D 反制杀 H、tick12 杀 A；E 距离最远无人锁定，全程存活。
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut map, 10, true, true);
        let e = EntityId(Uuid::from_u128(200));
        world
            .entities
            .insert(e, make_creature(200, (35.0, 5.0), "example", "E", "green"));
        map.create(e, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = Renderer::new(Viewport::new((0.0, 0.0), 50.0, 25.0, 50, 80));
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        run_ticks(
            &mut world, &mut time, &physics, &mut renderer, &mut storage,
            &mut pending, &mut map, 3,
        );
        assert!(glyph_alive(&world, "H"), "tick3 H 应还活着");
        run_ticks(
            &mut world, &mut time, &physics, &mut renderer, &mut storage,
            &mut pending, &mut map, 1,
        );
        assert!(!glyph_alive(&world, "H"), "tick4 D 反制应杀 H（时间线回归点一）");
        assert!(glyph_alive(&world, "A"), "tick4 A 应还活着");
        run_ticks(
            &mut world, &mut time, &physics, &mut renderer, &mut storage,
            &mut pending, &mut map, 7,
        );
        assert!(glyph_alive(&world, "A"), "tick11 A 应还活着");
        run_ticks(
            &mut world, &mut time, &physics, &mut renderer, &mut storage,
            &mut pending, &mut map, 1,
        );
        assert!(!glyph_alive(&world, "A"), "tick12 D 反制应杀 A（时间线回归点二）");
        assert!(glyph_alive(&world, "D"), "D 应存活");
        assert!(glyph_alive(&world, "S"), "S 应存活（悬浮躲刀）");
        assert!(glyph_alive(&world, "E"), "E 无人锁定应全程存活");
    }

    #[test]
    fn smoke_shrunken_viewport_out_of_screen_unreachable() {
        // 冒烟（边界效应）：视口缩到 [-10,10]x[-10,10]，经典双生物 A@5/D@15
        // + E 类玩家 @(35,0)。感知诚实三路前置的屏幕语义：
        // - A 看不到屏外 D（@15 在 x>10）-> scan 返 0 站桩不动、永不出刀（D 盾不掉）
        // - D 能锁屏内 A@5 -> tick4 反制杀 A（屏内=看得见=合法目标，D 自己在屏外不妨碍）
        // - E@35 屏外 -> 无人能锁，全程存活（屏外才打不到，本次清算的目标语义）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut map, 10, false, false);
        let e = EntityId(Uuid::from_u128(201));
        // E 出生在地面 y=0：空中出生会被重力拉到地面，坐标断言要求出生位稳定
        world
            .entities
            .insert(e, make_creature(201, (35.0, 0.0), "example", "E", "green"));
        map.create(e, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = Renderer::new(Viewport::new((0.0, 0.0), 10.0, 10.0, 50, 80));
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        run_ticks(
            &mut world, &mut time, &physics, &mut renderer, &mut storage,
            &mut pending, &mut map, 15,
        );
        assert!(!glyph_alive(&world, "A"), "A 应被 D 的 tick4 反制刀击杀（屏内目标照常锁定）");
        assert!(glyph_alive(&world, "D"), "D 应存活（A 锁不到屏外的 D）");
        assert!(glyph_alive(&world, "E"), "E@35 屏外应全程存活（屏外不可锁定）");
        let d_id = world
            .entities
            .values()
            .find(|en| en.appearance.glyph == "D")
            .map(|en| en.id)
            .unwrap();
        assert_eq!(
            call_i32_export(&mut map, &d_id, "get_shield"),
            10,
            "D 盾应原封不动（A 看不到屏外 D，一刀未出）"
        );
        assert_eq!(
            world.entities.get(&e).unwrap().pos(),
            (35.0, 0.0),
            "E 应原地未动（无人锁定无人接触）"
        );
    }

    #[test]
    fn smoke_pioneer_tower_plus5_no_move_on_tower_tick_then_walk_blocked() {
        // 冒烟（#021 F 验收）：真实 players/pioneer.wat 进场（镜像 main() 启动流程：
        // 四生物布阵 + load_players_initial），验证三件事：
        // ① 立塔拍：塔立在 +5 格 (35,0)，P 本拍不移动（留 (30,0)）
        // ② 之后每拍东移一步（31→32→33→34）
        // ③ T 全程留在原地站岗；P 走到塔前 (34,0) 被占位制挡下（移动被挡，不再连体推挤）
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../players/pioneer.wat");
        let wat_text = std::fs::read_to_string(src).expect("应能读到 players/pioneer.wat");
        let dir = temp_players_dir("pioneer-smoke");
        std::fs::write(dir.join("pioneer.wat"), wat_text).unwrap();
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut map, 10, true, true);
        map.players = PlayerDirectory::new(&dir);
        map.load_players_initial(&mut world).unwrap();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = Renderer::new(Viewport::new((0.0, 0.0), 50.0, 25.0, 50, 80));
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let p_id = find_player_id(&world, "pioneer");
        fn tower(w: &WorldState) -> Option<&Entity> {
            w.entities
                .values()
                .find(|e| e.code_hash == "creation" && e.appearance.glyph == "T")
        }
        // tick 1：立塔拍——塔在 +5，P 不动
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert_eq!(world.entities.get(&p_id).unwrap().pos(), (30.0, 0.0), "立塔拍 P 不应移动");
        let t = tower(&world).expect("立塔拍应有塔诞生");
        assert_eq!(t.pos(), (35.0, 0.0), "塔应立在 P 的 +5 格 (35,0)");
        assert!(map.sandboxes.contains_key(&t.id), "塔应有自己的沙箱（活造物）");
        // tick 2-5：每拍东移一步
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 4);
        assert_eq!(world.entities.get(&p_id).unwrap().pos(), (34.0, 0.0), "4 拍后 P 应走到塔前 (34,0)");
        assert_eq!(tower(&world).unwrap().pos(), (35.0, 0.0), "塔应原地站岗");
        // tick 6：P 试图进塔的格 → 占位制挡下（留原位，不推挤不穿塔）
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert_eq!(world.entities.get(&p_id).unwrap().pos(), (34.0, 0.0), "P 走进塔格应被挡在 (34,0)");
        assert_eq!(tower(&world).unwrap().pos(), (35.0, 0.0), "被挡后塔不位移（推挤已退役）");
        // 再跑 2 拍：P 持续被挡（每拍记移动被挡接触），塔纹丝不动
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 2);
        assert_eq!(world.entities.get(&p_id).unwrap().pos(), (34.0, 0.0), "持续被挡");
        assert_eq!(tower(&world).unwrap().pos(), (35.0, 0.0), "塔持续站岗");
        // 四生物不受 P/T 影响：D 站桩存活（H 在 tick4 已被 D 反制清理，属既有时间线）
        assert!(glyph_alive(&world, "D"), "D 应存活");
        assert!(!glyph_alive(&world, "H"), "H 应按既有时间线在 tick4 被 D 反制清理");
        let _ = std::fs::remove_dir_all(&dir);
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
        map.run_player_code(&world, &unbounded_bb());
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
        map.run_player_code(&world, &unbounded_bb());
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
        map.run_player_code(&world, &unbounded_bb());
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
        map.run_player_code(&world, &unbounded_bb());
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
        map.run_player_code(&world, &unbounded_bb());
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

    // === 玩家目录（门一进场 + 在线锚 + 残影期）===

    /// 示例玩家 WAT（与 players/_template.wat.txt、api.md 示例一致：life()=true + on_tick 调 log(1)）
    pub(crate) const EXAMPLE_PLAYER_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_tick") i32.const 1 call $log))"#;

    /// 移动+日志玩家 WAT（残影惰性测试：每拍 log(1) + 右移 1.0）
    const GHOST_MOVER_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_tick")
    i32.const 1 call $log
    f64.const 1.0 f64.const 0.0 call $move_by drop))"#;

    /// 测试用临时玩家目录（进程号 + 随机后缀防并行冲突）
    fn temp_players_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cw-players-it-{}-{}-{}",
            tag,
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 按玩家名找角色实体 ID
    fn find_player_id(world: &WorldState, name: &str) -> EntityId {
        world
            .entities
            .values()
            .find(|e| e.code_hash == name)
            .expect("玩家实体应存在")
            .id
    }

    #[test]
    fn online_anchor_disabled_by_default() {
        // 规则：默认禁用（既有测试跑 tick 零文件系统副作用）——
        // 仓库 players/ 目录存在示例文件也不进场
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        map.step_online_anchor(&mut world, 1);
        assert!(world.entities.is_empty(), "禁用状态不应进场任何玩家");
    }

    #[test]
    fn load_players_initial_creates_dir_and_spawns_players() {
        // 启动加载：目录不存在自动创建；两个 .wat 按文件名序分配颜色/出生点
        let dir = temp_players_dir("initial");
        std::fs::write(dir.join("beta.wat"), EXAMPLE_PLAYER_WAT).unwrap();
        std::fs::write(dir.join("alpha.wat"), EXAMPLE_PLAYER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        map.load_players_initial(&mut world).unwrap();
        assert_eq!(world.entities.len(), 2, "两个玩家文件应都进场");
        let alpha = world
            .entities
            .values()
            .find(|e| e.code_hash == "alpha")
            .expect("alpha 应进场");
        assert_eq!(alpha.pos(), (30.0, 0.0), "第 1 名玩家出生点 (30,0)");
        assert_eq!(alpha.appearance.glyph, "A", "alpha 字形取首字符大写");
        assert_eq!(alpha.appearance.color, "green", "第 1 名玩家从 green 起");
        let beta = world
            .entities
            .values()
            .find(|e| e.code_hash == "beta")
            .expect("beta 应进场");
        assert_eq!(beta.pos(), (35.0, 0.0), "第 2 名玩家出生点 (35,0)");
        assert_eq!(beta.appearance.color, "magenta", "第 2 名玩家 magenta");
        assert!(map.sandboxes.contains_key(&alpha.id), "进场玩家必有沙箱");
        // 空目录分支：全新目录自动创建、无玩家进场
        let empty_dir = temp_players_dir("initial-empty");
        let mut world2 = WorldState::new();
        let mut map2 = test_sandbox_map();
        map2.players = PlayerDirectory::new(&empty_dir);
        map2.load_players_initial(&mut world2).unwrap();
        assert!(empty_dir.is_dir(), "目录应自动创建");
        assert!(world2.entities.is_empty(), "空目录 = 无玩家进场");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty_dir);
    }

    #[test]
    fn player_joins_from_directory_file() {
        // 全链路（验收#1）：players/ 放入 .wat -> tick 检测进场 ->
        // 名字/字形/颜色/出生点正确 + 沙箱建立 + 下一拍 on_tick 真实执行。
        // on_tick 执行用移动行为证明（tick 内 d2 步已 drain log markers，
        // tick 后收集恒为空；log(1) 的可见性由冒烟 stderr [player_log] 验证）
        let dir = temp_players_dir("join");
        std::fs::write(dir.join("example.wat"), GHOST_MOVER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        // tick 1：d0 步扫描目录，example 进场
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let p = world
            .entities
            .values()
            .find(|e| e.code_hash == "example")
            .expect("玩家 example 应进场");
        assert_eq!(p.pos(), (30.0, 0.0), "第 1 名玩家出生点 (30,0)");
        assert_eq!(p.appearance.glyph, "E");
        assert_eq!(p.appearance.color, "green");
        assert!(map.sandboxes.contains_key(&p.id), "进场玩家必有沙箱");
        let pid = p.id;
        // tick 2：玩家代码第一次执行（on_tick 右移 1.0）
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.0),
            "玩家 on_tick 应真实执行（每拍右移 1.0）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn player_offline_ghost_dissipates_after_five_ticks() {
        // 残影状态机（验收#2）：tick1 进场 -> 删文件 -> tick2 检测下线进残影
        // （到期拍 = 2+5 = 7）-> 拍 3..6 实体仍在 -> 拍 7 消散，无残留
        let dir = temp_players_dir("offline");
        std::fs::write(dir.join("example.wat"), EXAMPLE_PLAYER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let pid = find_player_id(&world, "example");
        std::fs::remove_file(dir.join("example.wat")).unwrap();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(world.entities.contains_key(&pid), "下线当拍实体仍在（残影开始）");
        assert!(map.players.is_ghost(&pid), "下线实体应登记残影");
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 4);
        assert!(
            world.entities.contains_key(&pid),
            "残影第 4 拍末（拍 6）实体仍应在世界"
        );
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(
            !world.entities.contains_key(&pid),
            "拍 7（下线+5）残影应消散"
        );
        assert!(
            !map.sandboxes.contains_key(&pid),
            "消散后沙箱应随死亡清理通路销毁"
        );
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(!map.players.is_ghost(&pid), "消散后残影记录应被打扫");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ghost_does_not_execute_player_code() {
        // 残影惰性：不执行玩家代码（位置冻结、无新 log marker），沙箱保留不销毁
        let dir = temp_players_dir("ghost-inert");
        std::fs::write(dir.join("mover.wat"), GHOST_MOVER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let pid = find_player_id(&world, "mover");
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.0),
            "存活期玩家代码应每拍右移 1.0"
        );
        std::fs::remove_file(dir.join("mover.wat")).unwrap();
        // tick 3：阶段一仍执行玩家代码（残影下拍才生效）-> (32,0)，随后 d0 检测下线
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(map.players.is_ghost(&pid), "tick 3 应检测下线进残影");
        let frozen = world.entities.get(&pid).unwrap().pos();
        let _ = map.collect_all_log_markers(); // 清空存活期 marker
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 2);
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            frozen,
            "残影期位置应冻结（不执行玩家代码）"
        );
        assert!(
            map.collect_all_log_markers().is_empty(),
            "残影期不应有新 log marker"
        );
        assert!(
            map.sandboxes.contains_key(&pid),
            "残影期沙箱应保留（消散时才销毁）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // === 战斗残影期（#013：最近 30 拍参战者下线，残影期延长，防拔线逃课）===

    /// 恒拦截防守玩家 WAT（残影防守跳过测试：若防守被问会拦截，换血不发生）
    const GHOST_DEFENDER_WAT: &str = r#"(module
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_attack") (param i32) (result i32) i32.const 1)
  (func (export "get_ver") (result i32) i32.const 111))"#;

    /// 带版本号的击晕模块（观测"谁的代码在目标里"，同既有 ver_module 模式）
    fn ghost_ver_module(ver: i32) -> String {
        format!(
            r#"(module
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_ver") (result i32) i32.const {ver}))"#
        )
    }

    #[test]
    fn combat_participation_extends_offline_ghost_to_exact_ticks() {
        // #013 全链路（验收#3）：玩家 tick1 参战（被击晕刀命中，收件记参战）->
        // tick2 下线 -> 残影期 = 30-(2-1) = 29 拍（>5），到期拍 = 2+29 = 31：
        // 拍 30 仍在，拍 31 消散——具体拍数断言
        let dir = temp_players_dir("combat-ghost");
        std::fs::write(dir.join("example.wat"), EXAMPLE_PLAYER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let pid = find_player_id(&world, "example");
        // 敌人排击晕刀打玩家（world.tick()==1）：收件记参战，玩家存活
        let enemy = make_entity_with_sandbox(&mut world, &mut map, 200, (29.0, 0.0), TARGET_WAT);
        let stun = wasm(&ghost_ver_module(777));
        process_attacks(
            &mut world,
            &mut map,
            &[PendingAttack { attacker: enemy, target_pos: (30.0, 0.0), payload: stun }],
        );
        assert!(world.entities.get(&pid).unwrap().is_alive(), "击晕刀不应击杀");
        std::fs::remove_file(dir.join("example.wat")).unwrap();
        // tick 2：检测下线，战斗残影 29 拍
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(map.players.is_ghost(&pid), "下线应进残影");
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 28);
        assert!(
            world.entities.contains_key(&pid),
            "拍 30（参战后 29 拍内）残影应仍在——普通残影 5 拍早该消散，此即战斗延长"
        );
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(
            !world.entities.contains_key(&pid),
            "拍 31（下线+29）战斗残影应消散"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ghost_killed_during_ghost_plunders_normally() {
        // 验收#3：残影可被正常攻击/击杀，击杀掠夺合法（残影身份不影响掠夺）
        let dir = temp_players_dir("ghost-kill");
        std::fs::write(dir.join("example.wat"), EXAMPLE_PLAYER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let pid = find_player_id(&world, "example");
        std::fs::remove_file(dir.join("example.wat")).unwrap();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(map.players.is_ghost(&pid), "前置：残影已建立");
        // 击杀刀打残影（空 payload = 默认击杀）
        let enemy = make_entity_with_sandbox(&mut world, &mut map, 201, (29.0, 0.0), TARGET_WAT);
        process_attacks(
            &mut world,
            &mut map,
            &[PendingAttack { attacker: enemy, target_pos: (30.0, 0.0), payload: vec![] }],
        );
        assert!(
            !world.entities.get(&pid).unwrap().is_alive(),
            "残影应被击杀"
        );
        assert_eq!(
            world.entities.get(&enemy).unwrap().survival.points(),
            4,
            "击杀掠夺正常发生（3->4），残影身份不影响掠夺合法性"
        );
        // 下一拍：死亡清理移除实体+销毁沙箱；再下拍打扫清残影记录
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 2);
        assert!(!world.entities.contains_key(&pid), "被杀残影应被清理");
        assert!(!map.sandboxes.contains_key(&pid), "被杀残影沙箱应销毁");
        assert!(!map.players.is_ghost(&pid), "被杀残影记录应被打扫");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blade_miss_still_counts_as_combat_for_ghost() {
        // #013 防拔线逃课：出刀即战斗（含落空刀）。玩家对空坐标出刀（落空）后拔线
        // -> 仍按战斗残影计算（30 拍窗内），不能靠"打空再删文件"缩到普通 5 拍
        let dir = temp_players_dir("miss-combat");
        std::fs::write(dir.join("example.wat"), EXAMPLE_PLAYER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let pid = find_player_id(&world, "example");
        // 玩家对空坐标 (99,99) 出刀（落空，无目标）；world.tick()==1
        process_attacks(
            &mut world,
            &mut map,
            &[PendingAttack { attacker: pid, target_pos: (99.0, 99.0), payload: vec![] }],
        );
        assert!(world.entities.get(&pid).unwrap().is_alive(), "落空刀不伤人");
        std::fs::remove_file(dir.join("example.wat")).unwrap();
        // tick 2：下线检测——参战记录应在（落空刀也算战斗），残影 29 拍
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(map.players.is_ghost(&pid), "下线应进残影");
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 28);
        assert!(
            world.entities.contains_key(&pid),
            "落空刀参战应触发战斗残影延长（拍 30 仍在，普通残影 5 拍早消散）"
        );
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(!world.entities.contains_key(&pid), "拍 31 战斗残影到期消散");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ghost_skips_defense_inquiry() {
        // 残影不执行任何玩家代码（含 on_attack 防守）：带恒拦截防守的残影被击晕刀命中
        // -> 跳过防守询问直接结算，换血发生（ver 111 -> 888）
        let dir = temp_players_dir("ghost-defense");
        std::fs::write(dir.join("tanky.wat"), GHOST_DEFENDER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let pid = find_player_id(&world, "tanky");
        std::fs::remove_file(dir.join("tanky.wat")).unwrap();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(map.players.is_ghost(&pid), "前置：残影已建立");
        let enemy = make_entity_with_sandbox(&mut world, &mut map, 202, (29.0, 0.0), TARGET_WAT);
        let stun = wasm(&ghost_ver_module(888));
        process_attacks(
            &mut world,
            &mut map,
            &[PendingAttack { attacker: enemy, target_pos: (30.0, 0.0), payload: stun }],
        );
        assert_eq!(
            call_i32_export(&mut map, &pid, "get_ver"),
            888,
            "残影应跳过防守询问：击晕刀换血生效"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compile_failure_player_skipped_and_blacklisted() {
        // 验收#4：编译失败的 .wat -> warn 跳过，世界不崩；黑名单不逐拍重试；
        // 同名重写内容不热更新（黑名单维持）；删了再放 -> 黑名单解除可进场
        let dir = temp_players_dir("broken");
        std::fs::write(dir.join("broken.wat"), "this is not wat ((").unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        // tick 1：编译失败 -> 无实体、世界正常
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(
            !world.entities.values().any(|e| e.code_hash == "broken"),
            "编译失败玩家不应进场"
        );
        assert_eq!(world.entities.len(), 0, "世界不崩");
        // 同名重写合法内容（不删文件）-> 黑名单维持，不进场（MVP 无热更新）
        std::fs::write(dir.join("broken.wat"), EXAMPLE_PLAYER_WAT).unwrap();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(
            !world.entities.values().any(|e| e.code_hash == "broken"),
            "黑名单内的名字不应逐拍重试"
        );
        // 删了再放 -> 黑名单解除，进场
        std::fs::remove_file(dir.join("broken.wat")).unwrap();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        std::fs::write(dir.join("broken.wat"), EXAMPLE_PLAYER_WAT).unwrap();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(
            world.entities.values().any(|e| e.code_hash == "broken"),
            "删了重放合法文件后应可进场"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // === 门二：spawn/destroy 阶段二处理（world-foundation 门二 + #012/#015）===

    /// 造物可观测代码：on_tick 每拍 log(9)（心跳 marker，验证造物沙箱真的在跑）
    const CREATION_LOG_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (func (export "on_tick") i32.const 9 call $log)
  (func (export "life") (result i32) i32.const 1))"#;

    /// WAT 字符串字面量转义（按字节）：\ 与 " 转义、换行转 \0a（WAT 禁裸换行）、
    /// 非 ASCII 字节逐字节转 \XX（中文注释等 UTF-8 多字节必须显式转义，
    /// 裸高字节 wat crate 会错误解码导致造物代码非 UTF-8）。
    /// 转义后每字节仍解码回 1 字节，len 与原文本字节数一致。
    fn wat_escape(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'\\' => out.push_str("\\\\"),
                b'"' => out.push_str("\\\""),
                b'\n' => out.push_str("\\0a"),
                0x20..=0x7E => out.push(b as char),
                _ => out.push_str(&format!("\\{b:02x}")),
            }
        }
        out
    }

    /// 造物方 WAT 生成器（main 侧）：data 段放造物源码 + 字形/颜色，
    /// on_tick 调 spawn(0, len, x, y, 256, 300)。code_len 由 Rust 计算（手数必错）。
    fn birth_spawner_wat(code: &str, x: &str, y: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const {x} f64.const {y}
    i32.const 8192 i32.const 8256
    call $spawn drop)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
            x = x,
            y = y,
        )
    }

    /// 构造指定 owner 的实体（产权链测试用：不同玩家造不同实体）
    fn make_entity_of_owner(id_u: u128, owner_u: u128, pos: (f64, f64)) -> Entity {
        Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(owner_u)),
            "test".to_string(),
            pos,
            Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        )
    }

    #[test]
    fn process_spawns_births_creation_with_inherited_owner_and_sandbox() {
        // 全链路：创造者 on_tick 调 env.spawn 排队 -> process_spawns 阶段二造物：
        // owner 继承创造者（产权链）、出生坐标、code_hash="creation"、外观生效、
        // 沙箱创建、下一拍造物自己的 on_tick 真的在跑（造物是活实体不是装饰）
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(1));
        world
            .entities
            .insert(creator, make_entity_of_owner(1, 50, (10.0, 0.0)));
        let mut map = test_sandbox_map();
        let spawner = birth_spawner_wat(CREATION_LOG_WAT, "12.0", "0.0");
        map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
        // 阶段一：创造者跑代码，排队 spawn 请求
        map.run_player_code(&world, &unbounded_bb());
        // 阶段二：造物诞生
        process_spawns(&mut world, &mut map, 7);
        assert_eq!(world.entities.len(), 2, "造物应插入世界（1 创造者 + 1 造物）");
        let creation = world
            .entities
            .values()
            .find(|e| e.code_hash == "creation")
            .unwrap_or_else(|| panic!("应有 code_hash=creation 的造物"));
        assert_eq!(
            creation.owner,
            PlayerId(Uuid::from_u128(50)),
            "造物 owner 应继承创造者的 owner（产权链 #015）"
        );
        assert_eq!(creation.pos(), (12.0, 0.0), "造物应出生在请求坐标");
        assert_eq!(creation.appearance.glyph, "T", "造物字形应来自请求");
        assert_eq!(creation.appearance.color, "cyan", "造物颜色应来自请求");
        assert!(creation.is_alive(), "造物出生即存活");
        assert_eq!(creation.created_tick, 7, "created_tick 应为造物诞生拍");
        // 沙箱已创建：造物下一拍自己跑 on_tick（log 心跳 9 可观测）
        let creation_id = creation.id;
        assert!(
            map.sandboxes.contains_key(&creation_id),
            "造物沙箱应已创建"
        );
        map.run_player_code(&world, &unbounded_bb());
        let markers = map.collect_all_log_markers();
        assert!(
            markers.contains(&9),
            "造物的 on_tick 应在下一拍执行（心跳 log(9)），实际 markers {markers:?}"
        );
    }

    #[test]
    fn process_spawns_fails_when_birth_cell_occupied() {
        // #021 C 项（Factorio 放置检查）：出生格被占 → 该次 spawn 失败
        // （不诞生、不崩世界、占据者不受影响；下拍玩家可重试）
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(3));
        world
            .entities
            .insert(creator, make_entity_of_owner(3, 52, (10.0, 0.0)));
        // 出生格 (12,0) 被占据者霸占（别的 owner 的实体）
        let occupant = make_entity_of_owner(4, 53, (12.0, 0.0));
        let occupant_id = occupant.id;
        world.entities.insert(occupant_id, occupant);
        let mut map = test_sandbox_map();
        let spawner = birth_spawner_wat(CREATION_LOG_WAT, "12.0", "0.0");
        map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
        map.run_player_code(&world, &unbounded_bb());
        process_spawns(&mut world, &mut map, 7);
        assert_eq!(world.entities.len(), 2, "出生格被占不应诞生（1 创造者 + 1 占据者）");
        assert!(
            !world.entities.values().any(|e| e.code_hash == "creation"),
            "不应有造物诞生"
        );
        assert!(world.entities.get(&occupant_id).unwrap().is_alive(), "占据者不受影响");
        assert!(world.entities.get(&creator).unwrap().is_alive(), "创造者不受影响");
    }

    #[test]
    fn process_spawns_birth_on_creator_cell_also_blocked() {
        // 一格一物语义钉死：出生格=创造者自己的格 → 也算被占，失败。
        // （移动的 exclude 自己是"原地移动=无操作"；spawn 是造新物，造物若与
        //   创造者同格即产生重叠，正是占位制要防的事，无自豁免。）
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(5));
        world
            .entities
            .insert(creator, make_entity_of_owner(5, 54, (10.0, 0.0)));
        let mut map = test_sandbox_map();
        let spawner = birth_spawner_wat(CREATION_LOG_WAT, "10.0", "0.0");
        map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
        map.run_player_code(&world, &unbounded_bb());
        process_spawns(&mut world, &mut map, 7);
        assert_eq!(world.entities.len(), 1, "出生格=创造者的格算被占，不诞生");
    }

    #[test]
    fn process_spawns_skips_request_when_creator_dead() {
        // 创造者本拍 life()=false 标记死亡 -> 其 spawn 请求作废（死者不造物）
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(2));
        world
            .entities
            .insert(creator, make_entity_of_owner(2, 51, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        let spawner = birth_spawner_wat(CREATION_LOG_WAT, "1.0", "0.0");
        map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
        // 模拟阶段一 life()=false：先跑代码排上请求，再标记死亡
        map.run_player_code(&world, &unbounded_bb());
        world.entities.get_mut(&creator).unwrap().kill();
        process_spawns(&mut world, &mut map, 1);
        assert!(
            !world.entities.values().any(|e| e.code_hash == "creation"),
            "已死创造者的造物请求应作废"
        );
    }

    #[test]
    fn process_spawns_rejects_at_world_entity_cap() {
        // 世界实体总数上限（Config.max_entities，默认 64）：达上限拒绝 + 日志，不崩
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(3));
        world
            .entities
            .insert(creator, make_entity_of_owner(3, 52, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.max_entities = 1; // 上限=1，世界已有 1 个实体
        let spawner = birth_spawner_wat(CREATION_LOG_WAT, "1.0", "0.0");
        map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
        map.run_player_code(&world, &unbounded_bb());
        process_spawns(&mut world, &mut map, 1);
        assert_eq!(
            world.entities.len(),
            1,
            "达实体上限时 spawn 应被拒绝"
        );
    }

    #[test]
    fn process_spawns_drops_uncompilable_code_without_crash() {
        // WAT 编译失败 -> 该次 spawn 作废 + warn，不崩世界（参照玩家进场容错）
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(4));
        world
            .entities
            .insert(creator, make_entity_of_owner(4, 53, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(creator, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        // 直接往创造者沙箱塞一条非法代码的请求（绕过 import 的排队路径，直测阶段二容错）
        map.sandboxes
            .get_mut(&creator)
            .unwrap()
            .store
            .data_mut()
            .pending_spawns
            .push(codeworld_arena::sandbox::PendingSpawn {
                creator,
                pos: (1.0, 0.0),
                code: "this is not wat ((".to_string(),
                glyph: "T".to_string(),
                color: "cyan".to_string(),
                fuel_limit: 1_000_000,
            });
        process_spawns(&mut world, &mut map, 1);
        assert_eq!(world.entities.len(), 1, "编译失败作废，世界不崩");
        assert!(
            !world.entities.values().any(|e| e.code_hash == "creation"),
            "非法代码不应造物"
        );
    }

    #[test]
    fn process_destroys_kills_own_creation_without_plunder() {
        // destroy 自家造物（owner 匹配）-> 排队销毁 + 应用后死亡；无掠夺（不是击杀，
        // 宪法铁律：掠夺只属于击杀）——创造者生存点数不变
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(5));
        let creation = EntityId(Uuid::from_u128(6));
        world
            .entities
            .insert(creator, make_entity_of_owner(5, 60, (0.0, 0.0)));
        world
            .entities
            .insert(creation, make_entity_of_owner(6, 60, (2.0, 0.0))); // 同 owner=60
        let mut map = test_sandbox_map();
        map.create(creator, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        map.sandboxes
            .get_mut(&creator)
            .unwrap()
            .store
            .data_mut()
            .pending_destroys
            .push(codeworld_arena::sandbox::PendingDestroy {
                destroyer: creator,
                target: creation,
            });
        let mut pending: Vec<PendingChange> = Vec::new();
        process_destroys(&world, &mut map, &mut pending);
        assert_eq!(
            pending.len(),
            1,
            "产权匹配的销毁应排队 PendingChange::Destroy"
        );
        apply_pending_changes(&mut world, &mut pending, None);
        assert!(
            !world.entities.get(&creation).unwrap().is_alive(),
            "自家造物应被标记死亡"
        );
        assert_eq!(
            world.entities.get(&creator).unwrap().survival.points(),
            3,
            "destroy 无掠夺：创造者生存点数不变"
        );
    }

    #[test]
    fn process_destroys_rejects_foreign_creation() {
        // 产权校验：他人造物（owner 不匹配）-> 拒绝 + 不入队，目标安然无恙
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(7));
        let foreign = EntityId(Uuid::from_u128(8));
        world
            .entities
            .insert(creator, make_entity_of_owner(7, 70, (0.0, 0.0)));
        world
            .entities
            .insert(foreign, make_entity_of_owner(8, 99, (2.0, 0.0))); // owner 99 != 70
        let mut map = test_sandbox_map();
        map.create(creator, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        map.sandboxes
            .get_mut(&creator)
            .unwrap()
            .store
            .data_mut()
            .pending_destroys
            .push(codeworld_arena::sandbox::PendingDestroy {
                destroyer: creator,
                target: foreign,
            });
        let mut pending: Vec<PendingChange> = Vec::new();
        process_destroys(&world, &mut map, &mut pending);
        assert!(pending.is_empty(), "产权不匹配的销毁应被拒绝（不入队）");
        assert!(
            world.entities.get(&foreign).unwrap().is_alive(),
            "他人造物不应被销毁"
        );
    }

    // === 门二冒烟：哨塔造物（examples/sentinel.wat 真文件，验收#3）===

    /// 哨塔造物源码：examples/ 目录真文件（不放 players/ 免被当玩家自动进场）。
    /// 文档示例与测试同源：这文件语法坏了/逻辑改了，冒烟立刻红。
    const SENTINEL_WAT: &str = include_str!("../../../examples/sentinel.wat");

    /// 只 spawn 一次的玩家进场代码（$done 全局节流，README 示例同款模式）：
    /// 第一拍把哨塔造到身边 (x,y)，之后不再造（防逐拍刷造物撞上限）
    fn once_spawner_wat(code: &str, x: &str, y: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32) (result i32)))
  (global $done (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    (if (i32.eqz (global.get $done))
      (then
        (global.set $done (i32.const 1))
        (i32.const 0)
        (i32.const {len})
        f64.const {x}
        f64.const {y}
        (i32.const 8192)
        (i32.const 8256)
        (call $spawn)
        (drop))))
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
            x = x,
            y = y,
        )
    }

    #[test]
    fn smoke_spawner_births_sentinel_that_reports_sighting() {
        // 验收#3 冒烟（进程内 tick，等价可重复）：玩家代码第一拍 spawn 哨塔造物
        // -> 阶段二"造物诞生"（owner 继承/坐标正确）-> 下一拍哨塔自己的 on_tick
        // 跑起来：视野内有人过境 -> 观察信号 log(2)（#018 哨兵网络正解落地）
        let mut world = WorldState::new();
        let player = EntityId(Uuid::from_u128(1));
        let passerby = EntityId(Uuid::from_u128(2));
        world
            .entities
            .insert(player, make_entity_of_owner(1, 80, (10.0, 0.0)));
        world
            .entities
            .insert(passerby, make_entity_of_owner(2, 81, (30.0, 0.0)));
        let mut map = test_sandbox_map();
        let spawner = once_spawner_wat(SENTINEL_WAT, "13.0", "0.0");
        map.create(player, &wasm(&spawner), 1_000_000).unwrap();
        map.create(passerby, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        // 第 1 拍：玩家排队 spawn -> 阶段二造物诞生
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert_eq!(world.entities.len(), 3, "哨塔造物应诞生（2 起始 + 1 造物）");
        let sentinel = world
            .entities
            .values()
            .find(|e| e.code_hash == "creation")
            .unwrap_or_else(|| panic!("应有哨塔造物诞生"));
        assert_eq!(sentinel.pos(), (13.0, 0.0), "哨塔应出生在玩家请求的坐标");
        assert_eq!(
            sentinel.owner,
            PlayerId(Uuid::from_u128(80)),
            "哨塔 owner 应继承玩家 owner（产权链）"
        );
        let sentinel_id = sentinel.id; // 拷贝 id（world 后续可变借用）
        // 第 2 拍：哨塔自己跑代码——视野内有玩家/路人 -> 观察信号 log(2)
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        // 观测口用 api_calls（永不清空）：log_markers 会被 tick 内字幕步（d2）每拍
        // drain（哨塔的 log(2) 与路人的 42 都进了字幕管线），api_calls 才是留存证据
        let sentinel_calls = map
            .sandboxes
            .get(&sentinel_id)
            .unwrap()
            .store
            .data()
            .api_calls
            .clone();
        assert!(
            sentinel_calls.contains(&"log".to_string()),
            "哨塔应在有人过境时调 log 发观察信号，实际 api_calls {sentinel_calls:?}"
        );
        assert!(
            sentinel_calls.contains(&"visible_count".to_string()),
            "哨塔 on_tick 应在查视野（visible_count）"
        );
        // 第 3 拍：once 节流生效——不再刷造物（世界稳定在 3 个实体）
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert_eq!(world.entities.len(), 3, "once 节流下不应再诞生新造物");
    }
}
