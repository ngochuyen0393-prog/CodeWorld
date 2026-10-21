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
mod dispose;
mod input;
mod net;
mod players;

use anyhow::{Context, Result};
use codeworld_arena::attack::{PendingAttack, DEFAULT_KILL_WASM};
use codeworld_arena::sandbox::{
    PendingDestroy, PendingSpawn, PendingThrust, ProbeBridge, SandboxHandle, SandboxManager,
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

/// 发射痕迹强弹动能阈值（#034 修订三）：动能（速度×质量）> 50 的弹丸算"强弹"，
/// 痕迹寿命 1 拍加到 3 拍封顶。标定参照：H 标准抛射约 7.8（弱弹 1 拍），
/// mass=10 中速直射 200（强弹 3 拍）。
const TRACE_STRONG_KE: f64 = 50.0;

/// 发射痕迹（#034 修订三：发射即暴露——攻击自由，但攻击留下痕迹）。
/// 痕迹不是实体：不进 world.entities、不占碰撞/沙箱/名额，是主程序持有的
/// 独立标记表；探测面 = 眼睛层 visible 列表 + probe 命中坐标返回发射者身份。
#[derive(Debug, Clone)]
struct LaunchTrace {
    /// 痕迹 id（visible/probe 桥里的身份；独立生成，与实体 id 无关）
    trace_id: EntityId,
    /// 发射点（=弹丸出手位置）
    pos: (f64, f64),
    /// 发射者
    shooter: EntityId,
    /// 发射拍号
    birth_tick: u64,
    /// 寿命（拍）：1 拍基础，动能超阈值 +2（封顶 3）
    ttl: u32,
}

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
    /// 操控飞行相位（期四第六刀简化：Fly/Hover 两态；翻转沿打日志，悬停不刷）；
    /// None=地面/非飞行。操控死亡/下线/落地清零（升力停止，重力接管）
    flight_phase: Option<input::FlightPhase>,
    /// 上拍相触实体对集合（#030 接触边沿触发状态：本拍新进入接触才触发
    /// ContactPair，持续贴着不重复，分开再碰再触发；每拍整体重建）
    last_touching: std::collections::HashSet<(EntityId, EntityId)>,
    /// on_contact 独立小 fuel 预算（D-007 ②，架构 3.3 同族
    /// contact_target_instruction_limit；main() 从 config.contact_fuel 设置，默认 10_000）
    contact_fuel: u64,
    /// on_death 遗嘱预算（D-007 ③，架构 3.3 同族 death_instruction_limit；
    /// main() 从 config.death_fuel 设置，默认 200_000——遗嘱有字数上限=预算即上限）
    death_fuel: u64,
    /// 本拍会话级离场登记（登出 take #025 + 残影到期消散 #013）：两类 kill 都在
    /// d 步遗嘱窗口【之前】发生且都会清掉自己的目录记录（remove_online/ghosts.remove），
    /// d 步 is_ghost 判不到"刚消散的残影"——故在 kill 处直接挂号，遗嘱窗口据此
    /// 跳过 on_death（会话级离场非死亡谢幕）并消费清空
    session_takes: std::collections::HashSet<EntityId>,
    /// 在飞弹丸账本（#034 弹道系统：弹丸 -> 发射者+payload）。
    /// Entity 本体只存物理（质量/速度/半径），发射者豁免与结算 payload 归账本；
    /// 弹丸消亡（命中/落地/超时）时一并清理。不持久化（跨重启按哑弹清场）
    projectiles: HashMap<EntityId, ProjectileMeta>,
    /// 发射痕迹表（#034 修订三：发射即暴露）。独立标记非实体；expire_traces
    /// 每拍清算，寿命尽消亡。不持久化（痕迹最长 3 拍，重启窗口即自然过期）
    traces: Vec<LaunchTrace>,
    /// 点击走位停滞检测窗口（期四第七刀）：目标变更即重开窗，测"当前目标"的
    /// 走位进度；2 拍净位移 < 0.01 = 贴障蹭动/全挡，视为到达清目标
    move_stall: Option<input::MoveStallTrack>,
    /// 本拍连接事件（#025 期五第一刀：WS 操控令牌翻转沿，main 循环每拍写入、
    /// tick ②5 消费清空；WS 禁用/无翻转= None）。处置状态机的触发源
    conn_event: Option<net::ConnEvent>,
    /// 操控射击排队（#036：WsShoot 意图产的定装弹请求；route 后入队，阶段二
    /// a1.5 与 sandbox pending_shoots 同一 process_shoots 出口统一发射。
    /// 同拍至多 1 发由 route 单行动折叠保证——操控路径与 shoot import 限流同语义）
    ctrl_shoots: Vec<codeworld_arena::sandbox::PendingShoot>,
    /// 本拍命中事件缓冲（D-017 协议小刀）：settle_attacks 结算成功时写入，
    /// push_snapshot 随快照广播后由 tick 下一拍清空（读走即清，无观众=丢弃）。
    /// 只缓冲一拍（事件是新闻不是档案）
    attack_events: Vec<AttackEventRecord>,
}

impl EntitySandboxMap {
    /// 创建映射表（初始化 SandboxManager，世界极速用缺省常数——测试基线用）
    #[cfg(test)] // 仅测试装配用（生产路径走 new_with_max_speed）；无此标注 dead_code 误报
    fn new() -> Result<Self> {
        Self::new_with_max_speed(codeworld_core::physics::WORLD_MAX_SPEED)
    }

    /// 创建映射表（指定世界极速，#034 修订一：发射钳制与飞行钳制同源装配）。
    /// 阻力取 core DefaultPhysics 默认值（D-007 收尾 P1-3：未显式装配 drag 的
    /// 旧调用点保持默认口径零回归）
    #[cfg(test)] // D-007 收尾 P1-3 后生产装配走 new_with_reach_physics（drag 走
    // bin Config 装配值）；本构造器仅测试基线用（无此标注 dead_code 误报）
    fn new_with_max_speed(max_speed: f64) -> Result<Self> {
        Self::new_with_reach_physics(
            max_speed,
            codeworld_core::physics::DefaultPhysics::default_values().drag_coefficient,
        )
    }

    /// 创建映射表（指定世界极速 + 够不着判定阻力，D-007 收尾 P1-3）：shoot 够
    /// 不着弹道模拟的 drag 与真实弹丸同一装配源——main() 从 bin Config 传
    /// config.drag_coefficient，CODEWORLD_DRAG 覆盖后判定不与真实弹道分叉
    fn new_with_reach_physics(max_speed: f64, drag: f64) -> Result<Self> {
        Ok(Self {
            manager: SandboxManager::new_with_reach_physics(max_speed, drag)?,
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
            flight_phase: None,
            last_touching: std::collections::HashSet::new(),
            contact_fuel: 10_000,
            death_fuel: 200_000,
            session_takes: std::collections::HashSet::new(),
            move_stall: None,
            conn_event: None,
            ctrl_shoots: Vec::new(),
            attack_events: Vec::new(),
            projectiles: HashMap::new(),
            traces: Vec::new(),
        })
    }

    /// 发射痕迹清算（#034 修订三）：寿命尽的痕迹消亡。
    /// 时序语义：诞生拍（阶段二诞生，当拍清算不消）→ 次拍起的眼睛层可见 →
    /// 可见拍数耗尽后的清算拍消亡——弱弹恰好可见一拍
    fn expire_traces(&mut self, current_tick: u64) {
        let before = self.traces.len();
        self.traces
            .retain(|t| current_tick < t.birth_tick + t.ttl as u64);
        if self.traces.len() != before {
            tracing::debug!(
                expired = before - self.traces.len(),
                "[trace] 发射痕迹消散"
            );
        }
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
        // 发射痕迹并入可见列表（#034 修订三：发射即暴露，痕迹排在实体之后——
        // probe 侧同坐标实体优先）。痕迹非实体：只进感知列表，不进渲染/物理
        let trace_entries: Vec<(EntityId, (f64, f64))> = self
            .traces
            .iter()
            .filter(|t| viewport.contains(t.pos.0, t.pos.1))
            .map(|t| (t.trace_id, t.pos))
            .collect();
        visible.extend(trace_entries);
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
        let bridge_traces: Vec<codeworld_arena::sandbox::TraceProbe> = self
            .traces
            .iter()
            .map(|t| codeworld_arena::sandbox::TraceProbe {
                trace_id: t.trace_id,
                shooter: t.shooter,
                birth_tick: t.birth_tick,
            })
            .collect();
        let probe_fuel = self.probe_fuel;
        // D-016 地形指针（与 probe 桥同款 cast：world 是本函数不可变借用，
        // terrain 指针仅阶段一 on_tick 窗口内挂载进 SandboxData，窗口期 world 存活）
        let terrain_ptr: *const codeworld_core::terrain::Terrain = &world.terrain;
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
                traces: bridge_traces.clone(),
            };
            handle.store.data_mut().probe_bridge = &mut bridge;
            // D-016 地形快照挂载：on_tick 窗口内 env.move_by/spawn 的钻地判据
            // 用真实地形（y<ground(x)）；窗口结束置空（与 probe_bridge 同款契约，
            // 生命期=本次循环体，world 不可变借用贯穿窗口）
            handle.store.data_mut().terrain = terrain_ptr;
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
            // #025 处置抑制：处置等待/AI 待接入的实体同样跳过 on_tick（站着等处置/
            // 等 AI——活角色，life()/防守照常）；Auto 处置=自主恢复（不抑制）。
            // life() 照常调用（存在合同不受操控/处置影响——代码层心跳不停）。
            let disposition_suppressed = world
                .entities
                .get(&entity_id)
                .map(|e| self.players.dispose.suppresses(&e.code_hash))
                .unwrap_or(false);
            if control_override != Some(entity_id) && !disposition_suppressed {
                if let Err(e) = self.manager.call_on_tick(handle) {
                    tracing::warn!(entity_id = %entity_id.0, error = %e, "on_tick 执行失败");
                }
            } else if control_override == Some(entity_id) {
                tracing::debug!(entity_id = %entity_id.0, "[control] 本拍操控接管，跳过自主 on_tick");
            } else {
                tracing::debug!(entity_id = %entity_id.0, "[dispose] 处置抑制本拍 on_tick（等待处置/待 AI）");
            }
            // 窗口关闭：置空桥指针（SAFETY 契约，防后续 life 调用期悬垂）
            handle.store.data_mut().probe_bridge = std::ptr::null_mut();
            // D-016 地形快照窗口同关（life 调用期判空回退平地判据，契约同上）
            handle.store.data_mut().terrain = std::ptr::null();
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

    /// 收集本 tick 各沙箱的悬停声明（env.hover 置位）并清空（期四第七刀）
    ///
    /// 收集即清空——声明不跨拍延续（每拍重申语义，#029 无税但需持续出力声明）。
    /// 产物并入本拍物理免重力集合，与操控悬停同集合同机制（#030 升力独立）。
    fn collect_hover_declarations(&mut self) -> Vec<EntityId> {
        let mut ids = Vec::new();
        for (id, handle) in self.sandboxes.iter_mut() {
            if handle.store.data_mut().hover_declared {
                handle.store.data_mut().hover_declared = false;
                ids.push(*id);
            }
        }
        if !ids.is_empty() {
            tracing::debug!(count = ids.len(), "[hover] 收集悬停声明");
        }
        ids
    }

    /// 收集本 tick 各沙箱排队的 thrust 冲量请求并统一扣除 fuel 代价（D-010a）
    ///
    /// drain 各沙箱 pending_thrusts 后清空（不跨拍残留）；同沙箱累计的
    /// thrust_fuel_cost 从当拍剩余 fuel 一次扣除（饱和减 + 保底 1——绝不由记账
    /// 把油箱打到 0，0 fuel 是 out-of-fuel trap 引信，Windows 上 wasm 内 fuel 烧穿
    /// = 进程 abort，.cc-bugs #5 红线；天然烧干是既有环境级暴露非本刀制造）。
    /// 在 tick 阶段二 apply_pending_changes 之后调用：冲量入速度字段，随后本拍
    /// physics.step 的速度积分路径带位移（起跳→弧线→落地）。
    fn collect_thrust_requests(&mut self) -> Vec<PendingThrust> {
        let mut all = Vec::new();
        for handle in self.sandboxes.values_mut() {
            all.append(&mut handle.store.data_mut().pending_thrusts);
            let cost = handle.store.data_mut().thrust_fuel_cost;
            if cost > 0 {
                let remaining = handle.store.get_fuel().unwrap_or(0);
                let new_fuel = remaining.saturating_sub(cost).max(1);
                if let Err(e) = handle.store.set_fuel(new_fuel) {
                    tracing::warn!(error = %e, cost, "[thrust] fuel 扣除失败（本拍代价作废）");
                } else {
                    tracing::debug!(cost, from = remaining, to = new_fuel, "[thrust] fuel 扣除");
                }
                handle.store.data_mut().thrust_fuel_cost = 0;
            }
        }
        if !all.is_empty() {
            tracing::info!(
                count = all.len(),
                "[collect_thrust_requests] 收集冲量请求"
            );
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

    /// 收集本 tick 所有沙箱排队的射击请求（#034 env.shoot 推入，弹道系统）
    ///
    /// drain 各沙箱 pending_shoots 后清空，保证不跨拍残留。在 tick 阶段二
    /// process_shoots 调用。
    fn collect_pending_shoots(&mut self) -> Vec<codeworld_arena::sandbox::PendingShoot> {
        let mut all = Vec::new();
        for handle in self.sandboxes.values_mut() {
            all.append(&mut handle.store.data_mut().pending_shoots);
        }
        // 操控射击（#036）：并入同一发射出口（驾驶档定装参数已定，直接落地）
        all.append(&mut self.ctrl_shoots);
        if !all.is_empty() {
            tracing::info!(count = all.len(), "[collect_pending_shoots] 收集射击请求");
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
        // 1. 目录轮询：新 .wat 进场 / 消失的文件名下线进残影 / 黑名单解除。
        //    #025：会话级离场（登出带走）的名字跳过重生——重生=重连认领解除标记
        match self.players.scan_dir() {
            Ok(stems) => {
                for name in &stems {
                    if !self.players.is_online(name)
                        && !self.players.is_failed(name)
                        && !self.players.is_absent(name)
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
                        // #025 两条下线路径不互相踩：文件锚删除=账号级下线（残影路径
                        // #013 接管），处置状态让位清除
                        if self.players.dispose.clear(&name) {
                            tracing::info!("[dispose] 文件锚删除，处置状态让位残影路径 player={name}");
                        }
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
        //    D-007 ③：消散登记会话级离场（expire_due_ghosts 已删残影记录，
        //    d 步 is_ghost 判不到，遗嘱窗口凭本集合跳过 on_death）
        for entity_id in self.players.expire_due_ghosts(current_tick) {
            if let Some(entity) = world.entities.get_mut(&entity_id) {
                tracing::info!(entity_id = %entity_id.0, "[players] 残影消散（非击杀，无掠夺）");
                entity.kill();
                self.session_takes.insert(entity_id);
            }
        }
        // 3. 记录打扫：残影期被击杀等已不在世界的实体，清残影/参战记录（防无界增长）；
        //    处置账本同理只留在场玩家（防无界增长）
        self.players.prune(&world.entities);
        self.players.prune_dispose();
    }
}

fn main() -> Result<()> {
    let config = Config::load();
    init_tracing(&config.log_level);
    tracing::info!(tick_ms = config.tick_interval_ms, db = %config.db_path, "CodeWorld 主程序启动");

    // 打开存储，尝试恢复快照；无快照则新建世界
    let mut storage = StorageEngine::open(&config.db_path)?;
    // D-016：快照恢复判定（恢复的世界地形随快照走，不按 config 重生成高度图）
    let (mut world, restored_from_snapshot) = match storage.load_latest_world()? {
        Some(w) => {
            tracing::info!(tick = w.tick(), entities = w.entities.len(), "从快照恢复世界");
            (w, true)
        }
        None => {
            tracing::info!("无快照，创建新世界");
            (WorldState::new(), false)
        }
    };
    // #034：弹丸不跨重启——发射账本（发射者/payload）不持久化，恢复快照里的在飞
    // 弹丸没有归属信息，按哑弹清场（寿命 ≤30 拍的消耗品，重启窗口即自然超时）
    let stale_projectiles = world
        .entities
        .values()
        .filter(|e| codeworld_core::collision::is_projectile(e))
        .count();
    if stale_projectiles > 0 {
        world
            .entities
            .retain(|_, e| !codeworld_core::collision::is_projectile(e));
        tracing::info!(cleared = stale_projectiles, "[弹丸] 快照恢复：在飞弹丸按哑弹清场（发射账本不持久化）");
    }

    // 初始化各引擎（架构文档 4.1：time/physics/render）
    let mut time_engine = TimeEngine::new();
    // #034：阻力常量装配（世界基石参数，CODEWORLD_DRAG 可调；默认 0.05 标定射程 5~15 格）
    // #034 修订一：世界极速装配（视觉可读性常数，CODEWORLD_MAX_SPEED 可调；默认 25），
    // 同一参数对象喂给物理步（飞行中钳制）——发射端钳制在下方 sandbox_map 装配同源
    let physics = DefaultPhysicsEngine::new_with_params(codeworld_core::physics::DefaultPhysics {
        drag_coefficient: config.drag_coefficient,
        max_speed: config.max_speed,
        ..codeworld_core::physics::DefaultPhysics::default_values()
    });

    // D-016 地形装配：新世界按 config 预设+种子生成高度图（快照恢复的世界
    // 地形随快照走——高度数组是持久世界状态，不重生成）；平地也调 set_terrain
    //（幂等，Flat 即默认值），日志只在非平地打
    if !restored_from_snapshot {
        let preset = codeworld_core::terrain::TerrainPreset::parse(&config.terrain_preset);
        // 非法值护栏：parse 已回退平地，坏配置仍值得一句 warn（坏值不进世界，也不无声消失）
        if preset == codeworld_core::terrain::TerrainPreset::Flat
            && !matches!(
                config.terrain_preset.trim().to_ascii_lowercase().as_str(),
                "" | "flat" | "平地"
            )
        {
            tracing::warn!(raw = %config.terrain_preset, "[terrain] CODEWORLD_TERRAIN 非法值，回退平地");
        }
        if preset != codeworld_core::terrain::TerrainPreset::Flat {
            tracing::info!(preset = preset.as_str(), seed = config.terrain_seed,
                "[terrain] D-016 地形装配（CODEWORLD_TERRAIN/CODEWORLD_SEED）");
        }
        world.set_terrain(codeworld_core::terrain::Terrain::new(preset, config.terrain_seed));
    }
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
    // D-007 收尾 P1-3：够不着判定 drag 用装配值（与物理步同一 config.drag_coefficient，
    // CODEWORLD_DRAG 覆盖后判定与真实弹道同源不分叉）
    let mut sandbox_map = EntitySandboxMap::new_with_reach_physics(config.max_speed, config.drag_coefficient)?;
    // probe 预算装配（三层感知 Phase A：config 可调的 on_probe 独立小 fuel）
    sandbox_map.probe_fuel = config.probe_fuel;
    // D-007 三兄弟预算装配（接触回调/遗嘱钩子，模式同 probe_fuel）
    sandbox_map.contact_fuel = config.contact_fuel;
    sandbox_map.death_fuel = config.death_fuel;
    // 世界实体上限装配（门二 spawn 配套，decisions.md #020）
    sandbox_map.max_entities = config.max_entities;

    // 新世界或恢复后无实体：放测试实体（含 WASM 沙箱）让 MVP 有东西渲染、能跑玩家代码
    if world.entities.is_empty() {
        spawn_test_entities(&mut world, &mut sandbox_map, config.d_shield, config.h_enabled, config.s_enabled);
    }

    // 门一进场（world-foundation 二）：装配玩家目录（启动扫描 + 每 tick 在线锚轮询）。
    // 目录不存在自动创建；空目录 = 无玩家进场，原四生物剧本照跑。
    sandbox_map.players = PlayerDirectory::new(&config.players_dir);
    // #025 处置等待窗口装配（CODEWORLD_DISPOSE_WAIT；0=断开即默认带走）
    sandbox_map.players.set_dispose_wait(config.dispose_wait_ticks);
    if let Err(e) = sandbox_map.load_players_initial(&mut world) {
        tracing::warn!(error = %e, "players/ 目录初始加载失败，进场门暂不可用（世界继续）");
    }

    // 操控层 v0（#022）：raw mode 裸键输入 + 意图队列；旧 stdin read_line 线程退役
    //（read_line 等 Enter/EOF 的退出语义由 crossterm 事件循环接管：Quit 键/EOF 路等价）
    // #027：同一 IntentSink 克隆给 WS 线程——终端键盘与浏览器鼠标单入口双来源
    // #036 双标志分离：running=世界进程（Q 二次确认才置）；terminal_alive=终端客户端
    //（q/Enter/Ctrl+C 只关终端渲染与键盘，世界后台常驻——Minecraft dedicated server 语义）
    let running = Arc::new(AtomicBool::new(true));
    let terminal_alive = Arc::new(AtomicBool::new(true));
    let (intent_sink, intent_queue) = input::intent_channel();
    let input_thread =
        input::spawn_input_thread(intent_sink.clone(), running.clone(), terminal_alive.clone());

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

    tracing::info!("tick 循环开始（WASD/方向键操控，空格攻击，q=终端客户端退出，Q+Q=关闭世界）");
    // #025：操控令牌上拍值（翻转沿检测；WS 禁用时恒 false，处置永不触发）
    let mut conn_prev = false;
    while running.load(Ordering::SeqCst) {
        // #025 下线处置触发源：操控令牌翻转沿检测（重连认领=Claimed / 意外断开=Released；
        // 主动登出沿由 tick 侧 logout_handled 抑制——登出意图与释放沿同拍到达）
        sandbox_map.conn_event = ws_snapshots.as_ref().and_then(|h| {
            let now = h.ctrl_taken.load(Ordering::SeqCst);
            let ev = match (conn_prev, now) {
                (false, true) => Some(net::ConnEvent::Claimed),
                (true, false) => Some(net::ConnEvent::Released),
                _ => None,
            };
            conn_prev = now;
            ev
        });
        // #036 世界常驻：终端客户端退出（q）后渲染静默——世界照常 tick/推快照/存档，
        // 只是 ANSI 帧不再写 stdout（终端已还原用户 shell，外泄会污染显示）
        renderer.set_output_enabled(terminal_alive.load(Ordering::SeqCst));
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
        if let Some(handle) = &ws_snapshots {
            // D-017 协议小刀：本拍攻击命中事件（攻方/目标ID/目标位置）take 出来
            // 随快照一起广播——不管有没有观众都取走（无观众=丢弃，事件不跨拍滞留；
            // tick 下一拍的 ①0 清空是双保险）
            let attack_events = std::mem::take(&mut sandbox_map.attack_events);
            net::push_snapshot(
                &handle.snapshots,
                &world,
                renderer.caption_line(),
                renderer.viewport(),
                &sandbox_map.players,
                renderer.controlled(),
                &attack_events,
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

    // ①0 命中事件缓冲清空（D-017 协议小刀）：上一拍的攻击命中事件已随上一拍
    // 快照广播（或无观众被丢弃），事件是新闻不是档案——本拍缓冲从头开始收集
    sandbox_map.attack_events.clear();

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

    // ── #025 下线处置（期五第一刀）：会话级登出/断开的三选一状态机 ──
    // 顺序：登出落地 > 连接事件（认领/断开）> 等待超时清算 > 控制解析。
    // 意图分流：Logout 不走走位/攻击路由（不是操控动作），先于控制解析处理
    let mut logout_handled = false;
    let mut ctrl_intents: Vec<input::Intent> = Vec::with_capacity(intents.len());
    for it in intents {
        if let input::Intent::Logout { choice } = *it {
            logout_handled = true;
            match me.as_deref() {
                Some(name) if sandbox_map.players.is_online(name) => {
                    tracing::info!(
                        player = name,
                        choice = choice.wire_name(),
                        "[dispose] 收到登出消息"
                    );
                    dispose_apply_logout(sandbox_map, world, name, choice);
                }
                _ => {
                    tracing::warn!("[dispose] 登出消息无法落地（无 me 声明或声明玩家不在线），忽略");
                }
            }
        } else {
            ctrl_intents.push(*it);
        }
    }
    // 连接事件（WS 操控令牌翻转沿；WS 禁用/无翻转=None，take 后清空防跨拍残留）
    let conn_event = sandbox_map.conn_event.take();
    match conn_event {
        Some(net::ConnEvent::Claimed) => {
            // 重连认领：me 声明玩家的处置/离场标记一律清除（操控回归/重新进场，
            // #025 令牌语义：主动登出释放令牌，重连的第一个连接重新认领）
            if let Some(name) = me.as_deref() {
                if let Some(prev) = sandbox_map.players.dispose.clear_all(name) {
                    match prev {
                        dispose::DisposeState::Auto => {
                            tracing::info!(
                                player = name,
                                "[dispose] 接管者被顶掉（我的角色我随时拿回，操控独占恢复）"
                            );
                        }
                        _ => {
                            tracing::info!("[dispose] 重连取消处置（操控恢复）player={name}");
                        }
                    }
                }
                if sandbox_map.players.clear_absent(name) {
                    tracing::info!("[dispose] 重新进场 player={name}（离场标记解除，目录扫描重生角色）");
                }
            }
        }
        Some(net::ConnEvent::Released) if !logout_handled => {
            // 意外断开（浏览器直接关掉，非主动登出）：进入处置等待窗口；
            // 主动登出的令牌释放沿被 logout_handled 抑制（登出已有明确处置）
            if let Some(name) = me.as_deref() {
                if sandbox_map.players.is_online(name) {
                    match sandbox_map.players.dispose.begin_wait(name, current_tick) {
                        dispose::WaitOutcome::Waiting => tracing::info!(
                            player = name,
                            wait_ticks = sandbox_map.players.dispose.wait_ticks(),
                            "[dispose] 处置等待开始（超时默认带走）"
                        ),
                        dispose::WaitOutcome::TakeNow => {
                            tracing::info!("[dispose] 处置等待=0 拍，立即默认 take player={name}");
                            dispose_take(sandbox_map, world, name, "等待窗口为 0 拍");
                        }
                        dispose::WaitOutcome::Skipped => {}
                    }
                }
            }
        }
        _ => {}
    }
    // 等待超时：默认 take（宪法 #025：世界不等人，超时=玩家没选=带走）
    for name in sandbox_map.players.dispose.expire_due(current_tick) {
        tracing::info!("[dispose] 等待超时，默认 take player={name}");
        dispose_take(sandbox_map, world, &name, "等待超时");
    }

    // 上一拍操控的目标本拍不在线 = 操控目标下线（#024：warn 一次 + latch；
    // me.txt 中途改声明他人不触发——那是显式换人，不是断线）。
    // #025 豁免：会话级离场（登出带走）不算"下线消失"——不触发观战 latch
    //（重新进场解除标记后操控直接恢复，重启 latch 语义专指文件锚下线）
    if let Some(prev) = sandbox_map.last_controlled.clone() {
        if !sandbox_map.controlled_offline_latched
            && me.as_deref() == Some(prev.as_str())
            && !sandbox_map.players.is_online(&prev)
            && !sandbox_map.players.is_absent(&prev)
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
        // #025：处置中的玩家不被 me 声明重认领（Auto=接管者拥有/AiAwait=站桩等 AI/
        // Wait=等处置）——抢回操控必须走重连认领（conn Claimed 沿清账）
        sandbox_map
            .players
            .controlled_target(me.as_deref())
            .filter(|(name, _)| !sandbox_map.players.dispose.has(name))
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
    // 操控目标当前坐标与身体半径（#027 步向计算 + #030 空中判定线=半径线）
    let entity_pos = control_target
        .and_then(|id| world.entities.get(&id))
        .map(|e| e.pos());
    let entity_radius = control_target
        .and_then(|id| world.entities.get(&id))
        .map(|e| e.radius())
        .unwrap_or(0.5);
    // 观战/无操控拍：点击走位目标不滞留（换角不交接由 route_intents 兜底，这里清根）；
    // 飞行相位一并清零（操控死亡/下线 = 喷气停止，重力接管）
    if control_target.is_none() {
        sandbox_map.ws_move_target = None;
        sandbox_map.flight_phase = None;
    }
    // 停滞窗口随目标变更重开（期四第七刀）：新目标以拍首坐标开窗——窗口量的是
    // "当前目标"的逐拍走位进度，旧目标的蹭动历史不带入
    let prev_ws_target = sandbox_map.ws_move_target;
    let action = input::route_intents(
        &ctrl_intents,
        control_target,
        nearest,
        entity_pos,
        entity_radius,
        &mut sandbox_map.ws_move_target,
    );
    if sandbox_map.ws_move_target != prev_ws_target {
        sandbox_map.move_stall = sandbox_map
            .ws_move_target
            .and_then(|_| entity_pos.map(input::MoveStallTrack::start));
    }
    // 字幕 ctrl 段（#024 三态 + #025 处置扩展）：操控中 = ctrl:字形；
    // 处置接管 = ctrl:auto（stay_auto 自主活动）/ ctrl:ai-await（站桩等 AI）；
    // 观战（有人但不是你 / 操控目标下线 latch / 处置等待）= ctrl:--；
    // 无玩家在线且从未 latch（四生物基线）= 不显示 ctrl 段
    let ctrl = if sandbox_map.players.has_online() || sandbox_map.controlled_offline_latched {
        let base = control_target
            .and(online.as_ref())
            .map(|(name, _)| players::glyph_for(name))
            .unwrap_or_else(|| "--".to_string());
        // 处置状态签优先于观战占位（登出后仍在场：世界知道角色处于什么接管状态）
        let disposed = me
            .as_deref()
            .and_then(|name| sandbox_map.players.dispose.get(name));
        Some(match disposed {
            Some(dispose::DisposeState::Auto) => "auto".to_string(),
            Some(dispose::DisposeState::AiAwait) => "ai-await".to_string(),
            _ => base,
        })
    } else {
        None
    };
    // 本拍悬停集合（#030 升力独立）：aerial 意图实体进物理免重力集合——
    // 移动被挡/滑动/出刀时升力照出，被挡≠坠落（期四第五刀 bug 根治）。
    // 升力是意图层持续作用：悬停维持拍由持续目标每拍重发零位移 aerial 步
    let mut aloft: std::collections::HashSet<EntityId> = std::collections::HashSet::new();
    if let (Some((name, _)), Some((id, act))) = (&online, &action) {
        match act {
            input::ControlledAction::Move { dx, dy, aerial } => {
                // 操控移动 = 排队 SetPos（半径碰撞裁决照常，不豁免物理）；
                // 自主 on_tick 本拍被抑制（run_player_code_with_control），无坐标冲突
                if let Some(e) = world.entities.get(id) {
                    let (x, y) = e.pos();
                    pending.push(PendingChange::SetPos {
                        entity: *id,
                        new_pos: (x + dx, y + dy),
                    });
                }
                if *aerial {
                    aloft.insert(*id);
                    // 相位沿日志：零位移=悬停，矢量位移=飞行（状态切换才打，维持不刷）
                    let phase = if *dx == 0.0 && *dy == 0.0 {
                        input::FlightPhase::Hover
                    } else {
                        input::FlightPhase::Fly
                    };
                    if sandbox_map.flight_phase != Some(phase) {
                        tracing::info!("[ctrl] 操控飞行（{}）player={name}", phase.name());
                        sandbox_map.flight_phase = Some(phase);
                    }
                }
                tracing::info!("[input] 操控意图 Move({dx},{dy},aerial={aerial}) player={name}（覆盖自主移动）");
            }
            input::ControlledAction::Attack(tpos) => {
                // 空中出刀不掉（升力独立）：悬空实体攻击拍照常免重力——
                // 出刀不再排喷气补偿 SetPos（旧 +9.8 位移 hack 退役）
                if let Some(e) = world.entities.get(id) {
                    if e.pos().1 > e.radius() {
                        aloft.insert(*id);
                    }
                }
                tracing::info!("[input] 操控意图 Attack target={tpos:?} player={name}（操控刀优先）");
            }
            input::ControlledAction::Shoot { tx, ty } => {
                // 操控射击（#036 + 期五第六刀炮塔解算器）：升力独立同款——悬空射击
                // 拍照常免重力；走位目标未清（边走边射）。弹道参数按目标实时解算
                //（驾驶档基准 12+阻力补偿，够不着爬坡封顶 18）；无解不发射（INFO
                // 明示，不打必落空的哑弹）
                if let Some(e) = world.entities.get(id) {
                    if e.pos().1 > e.radius() {
                        aloft.insert(*id);
                    }
                    match solve_launch_vector(e.pos().0, e.pos().1, *tx, *ty, CTRL_SHOOT_SPEED) {
                        Some((vx, vy)) => {
                            sandbox_map
                                .ctrl_shoots
                                .push(codeworld_arena::sandbox::PendingShoot {
                                    shooter: *id,
                                    vx,
                                    vy,
                                    mass: CTRL_SHOOT_MASS,
                                    binding: 0,
                                    payload: Vec::new(),
                                });
                        }
                        None => {
                            tracing::info!(
                                player = %name,
                                target_x = *tx,
                                target_y = *ty,
                                "[ctrl] 射击解算无解（目标超出定装射程），不发射"
                            );
                        }
                    }
                }
                tracing::info!("[input] 操控意图 Shoot target=({tx},{ty}) player={name}（炮塔解算：基准{CTRL_SHOOT_SPEED}×阻力补偿，上限{CTRL_SHOOT_MAX_EFF_SPEED}）");
            }
        }
    }
    // 离开飞行（落地到达/地面意图/出刀后无悬停锚）：相位清零打结束沿——重力接管
    let still_flying = match &action {
        Some((_, input::ControlledAction::Move { aerial: true, .. })) => true,
        Some((id, input::ControlledAction::Attack(_))) => world
            .entities
            .get(id)
            .map(|e| e.pos().1 > e.radius())
            .unwrap_or(false),
        // 空中射击不掉（#036 升力独立）：射击拍悬空实体照常免重力
        Some((id, input::ControlledAction::Shoot { .. })) => world
            .entities
            .get(id)
            .map(|e| e.pos().1 > e.radius())
            .unwrap_or(false),
        _ => false,
    };
    if sandbox_map.flight_phase.is_some() && !still_flying {
        tracing::info!(
            "[ctrl] 操控飞行结束（重力接管）player={}",
            online.as_ref().map(|(n, _)| n.as_str()).unwrap_or("?")
        );
        sandbox_map.flight_phase = None;
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
    // 玩家代码悬停声明并入本拍免重力集合（期四第七刀，#029/#030）：env.hover 置位
    // 的实体与操控悬停同集合同机制；收集即清空——每拍重申，上一拍声明不延续
    let hover_declarations = sandbox_map.collect_hover_declarations();
    aloft.extend(hover_declarations);
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
    // #030：移动裁决改半径碰撞（分轴滑动），移动被挡不再产生接触对——
    // 接触语义统一为"身体相触"，由阶段二 c 步全量扫描+边沿触发裁决
    apply_pending_changes(world, pending, control_target);

    //   a0. thrust 冲量应用（D-010a 冲量小刀）：插在 apply_pending_changes 之后、
    //       physics.step 之前——冲量入速度字段（v += impulse，世界极速钳制），随后
    //       本拍 physics.step 的速度积分路径带位移（起跳→弧线→落地）。同拍被攻击
    //       杀死的实体在此跳过（已死不出力）；fuel 代价已在 collect 里统一扣除
    let thrust_requests = sandbox_map.collect_thrust_requests();
    if !thrust_requests.is_empty() {
        apply_thrust_requests(world, thrust_requests, physics.params().max_speed);
    }

    // 贴障停滞检测（期四第七刀，b8.log ±0.08 蹭动实锤）：点击走位 2 拍窗口
    // 净位移 < 0.01 = 全挡原地/滑出滑回蹭动 -> 视为到达清目标（防死循环蹭动）。
    // 悬停维持拍位置恒定是主动悬停（实体或目标在空中），不参与地面停滞判定。
    // #036 修正：射击/出刀拍实体不动是"本拍没尝试走路"（行动被攻击/射击占用），
    // 不是"被挡"——跳过停滞判定并冻结窗口（否则点地后立刻开火会误清走位目标，
    // "边走边射/边走边砍"的并存承诺被停滞检测破坏）
    let walk_attempted = !matches!(
        &action,
        Some((_, input::ControlledAction::Attack(_))) | Some((_, input::ControlledAction::Shoot { .. }))
    );
    if let Some((tid, tpos)) = sandbox_map.ws_move_target {
        if walk_attempted && Some(tid) == control_target {
            if let Some(e) = world.entities.get(&tid) {
                if e.is_alive() {
                    let (px, py) = e.pos();
                    let r = e.radius();
                    let track = sandbox_map
                        .move_stall
                        .get_or_insert_with(|| input::MoveStallTrack::start((px, py)));
                    if py <= r && tpos.1 <= r && track.check((px, py)) {
                        tracing::info!("[ctrl] 操控目标停滞，清除目标（贴障）");
                        sandbox_map.ws_move_target = None;
                        sandbox_map.move_stall = None;
                    }
                }
            }
        }
    } else {
        sandbox_map.move_stall = None;
    }

    //   a1. 造物处理（门二：env.spawn 排队的请求在此编译/建实体/建沙箱/插入世界）。
    //       放在 destroy 应用之后：本拍自毁的创造者不再造物；上限按应用后世界实况算。
    //       放在攻击处理之前：造物出生即进入世界（本拍攻击队列已定，下拍才会与之交互）
    process_spawns(world, sandbox_map, current_tick);

    //   a1.5 弹道步（#034 弹道系统）：
    //       ① 发射落地：env.shoot 排队请求 -> 弹丸实体（离体即世界物体，出手前归代码）
    //       ② 整场积分：弹丸吃重力/阻力/位移（速度积分制，弹丸不吃 aloft——它就是要落地的），
    //          撞身体/落地/超时产出事件（飞行中归世界）
    process_shoots(world, sandbox_map, current_tick);
    // 发射痕迹清算（#034 修订三）：寿命尽的痕迹消亡（排在 process_shoots 之后——
    // 本拍刚诞生的痕迹当拍不清，次拍起的眼睛层可见一至三拍）
    sandbox_map.expire_traces(current_tick);
    let projectile_events = {
        let shooters: HashMap<EntityId, EntityId> = sandbox_map
            .projectiles
            .iter()
            .map(|(p, meta)| (*p, meta.shooter))
            .collect();
        physics.step_projectiles(world, current_tick, &shooters)
    };

    //   a2. 处理攻击（架构 4.2 两阶段模型：阶段一 on_tick 调 env.attack_at 排队的请求在此统一执行）
    //      插在 apply_pending_changes 之后、decay_survival 之前：攻击产生的死亡先于每日衰减处理。
    //      import 函数只排队（无 world 访问权），实际 hit/miss/拦截/结算在此判断：
    //      收件（逐刀问目标当前沙箱 on_attack，不换血不击杀）-> 结算（攻击者 id 升序逐刀执行）。
    //      操控攻击（空格）并入同一队列走既有两阶段结算（操控不豁免物理，宪法）；
    //      弹丸命中/落地结算并入同一批次（#034：无第二结算路径，命后归结算）；
    //      无可见目标 = 挥空不出刀（但操控仍接管了本拍）。
    let mut pending_attacks = sandbox_map.collect_pending_attacks();
    settle_projectile_events(sandbox_map, &projectile_events, &mut pending_attacks);
    if let Some((id, input::ControlledAction::Attack(Some(tpos)))) = &action {
        pending_attacks.push(PendingAttack::strike(*id, *tpos, Vec::new()));
    }
    if !pending_attacks.is_empty() {
        process_attacks(world, sandbox_map, &pending_attacks);
    }

    //   b. 生存点数扣减（架构 4.1：每 86400 tick 扣 1 点，归零标记死亡）
    decay_survival(world, current_tick);

    //   b1. 绑定契约托管清算（#034 修订四：fade_on_death 随主消亡，主人死那一拍
    //       弹即无/塔即无——排在攻击与衰减之后，本拍所有死亡来源都已落地）
    enforce_binding_deaths(world, sandbox_map);

    //   c. 物理规则（#030 半径物理：重力落点解析 + 悬停免重力，架构 4.1 b 步）
    let changes = physics.step(world, &aloft);
    if !changes.is_empty() {
        tracing::debug!(tick = current_tick, change_count = changes.len(), "物理变更");
    }
    //      接触触发沙箱通道（架构 4.1 b 步，#030 边沿触发）：step 后全量扫描相触对
    //      （≤：圆-圆/圆-AABB/塔-塔），与上拍集合差分——新进入接触才触发 ContactPair，
    //      持续贴着不重复，分开后再碰再触发（battle.log 进接触一行不刷屏）
    let touching_now: std::collections::HashSet<(EntityId, EntityId)> =
        if physics.params().collision_enabled {
            physics
                .detect_touching(&world.entities)
                .into_iter()
                .map(|p| (p.a, p.b)) // detect_touching 已保证 a.id < b.id（确定序）
                .collect()
        } else {
            std::collections::HashSet::new()
        };
    let mut contacts: Vec<ContactPair> = Vec::new();
    for (a, b) in &touching_now {
        if !sandbox_map.last_touching.contains(&(*a, *b)) {
            let pos = world
                .entities
                .get(a)
                .map(|e| e.pos())
                .unwrap_or((0.0, 0.0));
            contacts.push(ContactPair {
                a: *a,
                b: *b,
                pos,
                source: "touch",
            });
        }
    }
    sandbox_map.last_touching = touching_now; // 每拍整体重建（上拍状态 = 本拍全量）
    let contact_count = handle_collisions(&contacts, &world.entities);
    if contact_count > 0 {
        tracing::info!(tick = current_tick, contact_count, "接触事件处理（边沿触发）");
    }
    //      接触回调通知（D-007 ②）：新进入接触的对，双方造物代码各收对方坐标
    //      （on_contact 独立预算包裹；世界零过滤，过滤归玩家代码）
    notify_contacts(world, sandbox_map, &contacts);

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
    //      d-will 遗嘱窗口（D-007 ③，#043 死亡谢幕）：必须在 remove 之前（沙箱还在
    //      才能执行遗嘱）、retain 之前（遗嘱产物发射/出生要读死者坐标与产权）。
    //      遗嘱排队的 spawn/shoot 当拍喂回既有管线（先后顺序与 a1/a1.5 同：先造物
    //      后弹道），attack 丢弃（死人不出刀）。残影消散/登出 take 不触发（函数内过滤）。
    let (will_shoots, will_spawns) = run_death_wills(sandbox_map, &dead_ids);
    if !will_spawns.is_empty() {
        // 遗嘱执行人资格：创造者本拍刚死的 spawn 放行（产权链照常继承）
        let creators: Vec<EntityId> = will_spawns.iter().map(|r| r.creator).collect();
        process_spawn_requests(world, sandbox_map, will_spawns, current_tick, &creators);
    }
    if !will_shoots.is_empty() {
        // 死者最后一发照常出膛（发射位置读死者实况坐标——retain 前仍在 world）
        let shooters: Vec<EntityId> = will_shoots.iter().map(|s| s.shooter).collect();
        process_shoot_requests(world, sandbox_map, will_shoots, current_tick, &shooters);
    }
    for id in &dead_ids {
        sandbox_map.remove(id);
        // #025：处置中的实体死亡（等待期/站桩期被杀）——处置状态一并清
        //（掠夺照常：停留的风险，玩家自己选的）
        if let Some(e) = world.entities.get(id) {
            if sandbox_map.players.dispose.clear(&e.code_hash) {
                tracing::info!("[dispose] 实体死亡，处置状态清理 player={}", e.code_hash);
            }
        }
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
    // S 替身台词：拦截 marker 计数 > 0 -> battle.log 打一行伪装台词
    // （间谍中弹不承认的人设味道；marker 本身是 i32，台词由 host 侧翻译呈现）
    let spy_tells = spy_disguise_intercept_count(&markers);
    if spy_tells > 0 {
        tracing::info!(intercepts = spy_tells, "S: 那只是我的替身");
    }
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
        attacker.is_some(),
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

/// #025 登出三选一落地（期五第一刀）
///
/// - take：会话级带走——在场登记摘除（不走残影）+ 实体 kill（本拍死亡清理链移除）
///   + 离场标记（scan 不再自动重生，重连认领=重新进场）；文件锚不动
/// - stay_auto：登记 Auto 处置（on_tick 恢复，操控独占随控制解析释放）
/// - stay_ai：登记 AiAwait 处置（站桩，on_tick 抑制，#014 外部 AI 端点协议留后）
fn dispose_apply_logout(
    sandbox_map: &mut EntitySandboxMap,
    world: &mut WorldState,
    name: &str,
    choice: dispose::LogoutChoice,
) {
    match choice {
        dispose::LogoutChoice::Take => dispose_take(sandbox_map, world, name, "主动登出带走"),
        dispose::LogoutChoice::StayAuto => {
            sandbox_map
                .players
                .dispose
                .apply(name, dispose::DisposeState::Auto);
            tracing::info!("[dispose] 处置完成：stay_auto player={name}");
            tracing::info!("[ctrl] 下线处置：停留+自主接管（on_tick 恢复）player={name}");
        }
        dispose::LogoutChoice::StayAi => {
            sandbox_map
                .players
                .dispose
                .apply(name, dispose::DisposeState::AiAwait);
            tracing::info!(
                "[dispose] 处置完成：stay_ai player={name}（站桩待外部 AI 接入，#014 端点协议留后）"
            );
        }
    }
}

/// #025 take 落地：角色带走（会话级离场，与残影路径并存的下线方式）
///
/// 在场登记摘除（remove_online，非 note_offline——不走残影）+ 实体 kill
///（本拍死亡清理步移除实体+销毁沙箱）+ 离场标记（scan 跳过）+ 处置清账。
/// 死亡清理若再触发同名的处置清除是无害幂等（clear 返回 false 不刷日志）。
fn dispose_take(
    sandbox_map: &mut EntitySandboxMap,
    world: &mut WorldState,
    name: &str,
    reason: &str,
) {
    if let Some(id) = sandbox_map.players.remove_online(name) {
        if let Some(e) = world.entities.get_mut(&id) {
            e.kill();
        }
        // D-007 ③：take 的死亡不触发遗嘱（会话级离场非死亡）——遗嘱窗口据此跳过
        sandbox_map.session_takes.insert(id);
    }
    sandbox_map.players.mark_absent(name);
    sandbox_map.players.dispose.clear(name);
    tracing::info!("[dispose] 处置完成：take player={name}（{reason}；角色已移除，文件锚保留）");
}

/// 应用 thrust 冲量（D-010a 冲量小刀）：v_new = v_old + impulse，世界极速钳制后写回
///
/// 【#021 切割声明】一次性冲量非持续推挤——已退役的"推挤"（持续重叠互相挤开→
/// 连体 bug）是每拍重复施加的位置推力；thrust 是一次性动量注入（撞完就分），
/// 本函数只在阶段二执行一次，不与任何重叠检测耦合。
/// 钳制口径：累加后速度模长 > max_speed（世界极速 25）归一化压回（方向不变），
/// 与弹丸/dynamic 造物同款极速口径——冲量买不到超极速位移。
/// static 钉死造物拒绝（速度不写）：钉死就是钉死，import 侧已扣的 fuel 不退
/// （诚实的代价——同 D-009 "岸上空烧 fuel 踩水"哲学，api.md 载明）。
fn apply_thrust_requests(
    world: &mut WorldState,
    thrusts: Vec<PendingThrust>,
    max_speed: f64,
) {
    for t in thrusts {
        let Some(entity) = world.entities.get_mut(&t.entity) else {
            tracing::debug!(entity = %t.entity.0, "[thrust] 实体不存在，冲量作废");
            continue;
        };
        if !entity.is_alive() {
            tracing::debug!(entity = %t.entity.0, "[thrust] 实体已死，冲量作废");
            continue;
        }
        if entity.code_hash == codeworld_core::collision::CREATION_CODE
            && entity.motion() == codeworld_core::entity::MOTION_STATIC
        {
            tracing::warn!(
                entity = %t.entity.0,
                impulse = ?t.impulse,
                "[thrust] static 钉死造物出力无效（钉死就是钉死，fuel 照扣）"
            );
            continue;
        }
        let old = entity.velocity();
        let summed = (old.0 + t.impulse.0, old.1 + t.impulse.1);
        // 世界极速钳制：归一化压回（方向不变）
        let speed2 = summed.0 * summed.0 + summed.1 * summed.1;
        let new_v = if speed2 > max_speed * max_speed {
            let scale = max_speed / speed2.sqrt();
            (summed.0 * scale, summed.1 * scale)
        } else {
            summed
        };
        entity.set_velocity(new_v);
        tracing::info!(
            entity = %t.entity.0,
            from = ?old,
            impulse = ?t.impulse,
            to = ?new_v,
            "[thrust] 冲量应用（速度累加+极速钳制）"
        );
    }
}

/// 应用延迟变更队列（架构文档 4.1 节阶段二 a 步 + #030 半径物理移动裁决）
///
/// 阶段一玩家代码执行期间产生的变更（spawn/destroy/set_pos/set_velocity）
/// 放入 pending 队列，此处统一应用到 WorldState。应用后队列清空。
///
/// #030 移动裁决（取代 #021 格子占位）：SetPos 目标与任何身体重叠 → **分轴滑动**
/// ——先试整步，撞则试 (new_x, old_y)，再试 (old_x, new_y)，都撞则原地不动
/// （贴墙滑而非整步作废，不推挤）。落地钳制：y < radius → y = radius（脚贴地，
/// 圆心即位置）。本 tick 所有移动按 EntityId 升序统一裁决（先到先得，确定序）。
/// 移动被挡不再产生接触对：接触语义统一为"身体相触"，由阶段二 c 步全量扫描
/// +边沿触发裁决（被挡后贴着的实体必然被扫描捕获）。
///
/// `control_target`：本拍操控目标（期四第四刀）——仅它的 SetPos 应用日志升 INFO
/// （battle.log 可追踪玩家走位），其余实体保持 debug（A/D/S 每拍自主移动，
/// 全升 INFO 会刷爆日志）。
fn apply_pending_changes(world: &mut WorldState, pending: &mut Vec<PendingChange>, control_target: Option<EntityId>) {
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
        let Some(me) = world.entities.get(&entity) else {
            continue;
        };
        let radius = me.radius();
        let old = me.pos();
        // 落地钳制（#030）：y < radius → y = radius（脚贴地）
        let mut target = new_pos;
        if target.1 < radius {
            target.1 = radius;
        }
        // 悬停维持的零位移步跳过（不过裁决不刷日志；悬停升力由 aloft 集合承载，
        // 不依赖本 SetPos 应用）
        if target == old {
            continue;
        }
        // 分轴滑动裁决（#030）：整步 → 滑 x → 滑 y → 原地（贴墙滑非整步作废）
        let (final_pos, slide) = if first_overlap(world, entity, target, radius).is_none() {
            (target, "")
        } else {
            let slide_x = (target.0, old.1);
            let slide_y = (old.0, target.1);
            if slide_x != old && first_overlap(world, entity, slide_x, radius).is_none() {
                (slide_x, "滑x")
            } else if slide_y != old && first_overlap(world, entity, slide_y, radius).is_none() {
                (slide_y, "滑y")
            } else {
                (old, "全挡")
            }
        };
        // 被挡日志 INFO 带滑动信息（battle.log 可见贴墙行为）；操控实体应用日志
        // 升 INFO 带 from→to（第四刀先例），其余实体 debug
        if !slide.is_empty() {
            let blocker = first_overlap(world, entity, target, radius);
            tracing::info!(
                entity_id = %entity.0,
                target = ?target,
                result = if slide == "全挡" { "原地" } else { slide },
                blocker = ?blocker.map(|b| b.0.to_string()),
                "[move] 移动被挡（身体重叠），分轴滑动裁决"
            );
        }
        if control_target == Some(entity) {
            tracing::info!(
                entity_id = %entity.0,
                from = ?old,
                to = ?final_pos,
                slide = if slide.is_empty() { "-" } else { slide },
                "[ctrl] 操控移动应用 SetPos（from→to）"
            );
        } else if slide.is_empty() {
            tracing::debug!(entity_id = %entity.0, ?final_pos, "应用 SetPos");
        }
        if final_pos != old {
            if let Some(e) = world.entities.get_mut(&entity) {
                e.set_pos(final_pos);
            }
        }
    }
}

/// 找与 (pos, radius) 圆身重叠的第一个其他存活身体（EntityId 升序取最小者，
/// 确定序可重现；None = 不重叠）。移动裁决/出生预检共用。
fn first_overlap(
    world: &WorldState,
    mover: EntityId,
    pos: (f64, f64),
    radius: f64,
) -> Option<EntityId> {
    use codeworld_core::collision::{body_of, body_overlap, Body};
    let me = Body::Circle { center: pos, radius };
    let mut best: Option<EntityId> = None;
    for (id, e) in &world.entities {
        if *id == mover || !e.is_alive() {
            continue;
        }
        if body_overlap(&me, &body_of(e)) {
            let better = match best {
                None => true,
                Some(cur) => id.0 < cur.0,
            };
            if better {
                best = Some(*id);
            }
        }
    }
    best
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
    process_spawn_requests(world, sandbox_map, requests, current_tick, &[]);
}

/// 造物请求落地（process_spawns 主体；will_creators = 遗嘱放行名单，D-007 ③）
///
/// 创造者已死分支的例外：#043 遗嘱钩子在死亡清理窗口排队、当拍消费的 spawn
/// 放行（创造者本拍刚死——遗嘱执行人资格，产权链照常继承，死者实体仍在 world
/// 可读 owner）。常规路径传空名单，行为与拆分前完全一致。
fn process_spawn_requests(
    world: &mut WorldState,
    sandbox_map: &mut EntitySandboxMap,
    requests: Vec<PendingSpawn>,
    current_tick: u64,
    will_creators: &[EntityId],
) {
    for req in requests {
        // 创造者必须存活（死者不造物；遗嘱例外：死亡谢幕窗口排队的 spawn 放行）
        let will_pass = will_creators.contains(&req.creator);
        let Some(creator) = world
            .entities
            .get(&req.creator)
            .filter(|e| e.is_alive() || will_pass)
        else {
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
        // #030 出生预检（Factorio 放置检查，半径版）：造物身体与任何存活身体重叠
        // → 该次 spawn 失败。放在编译之后、建沙箱之前（防沙箱泄漏）；不诞生、
        // 不占世界名额，下拍可重试。D-003：dynamic 造物身体=声明 radius 的圆，
        // static 造物身体=出生格对齐 1×1 AABB（现状不变）。
        {
            use codeworld_core::collision::{body_of, body_overlap, creation_aabb, Body};
            let body = if codeworld_core::collision::is_dynamic_creation_candidate(req.motion) {
                Body::Circle {
                    center: req.pos,
                    radius: req.radius,
                }
            } else {
                let (bmin, bmax) = creation_aabb(req.pos);
                Body::Aabb { min: bmin, max: bmax }
            };
            let blocker = world
                .entities
                .iter()
                .filter(|(oid, e)| **oid != entity_id && e.is_alive())
                .find(|(_, e)| body_overlap(&body, &body_of(e)))
                .map(|(oid, _)| *oid);
            if let Some(occupant) = blocker {
                tracing::warn!(
                    creator = %req.creator.0,
                    occupant = %occupant.0,
                    pos = ?req.pos,
                    "[process_spawns] 出生点与现有身体重叠，该次 spawn 失败（下拍可重试）"
                );
                continue;
            }
        }
        let mut entity = Entity::new(
            entity_id,
            owner,
            "creation".to_string(),
            req.pos,
            Appearance::new(req.glyph.clone(), req.color.clone(), 1.0),
            current_tick,
            req.fuel_limit,
        );
        // #030 spawn 第 7 参 radius：import 层已校验钳制（0.05..=25.0），
        // 此处再过一遍 set_radius 兜底（幂等；creation 身体=AABB 不用它，快照携带）
        entity.set_radius(req.radius);
        // #034 spawn 第 8 参 mass：import 层已钳制（0.05..=100.0），此处兜底（幂等）
        entity.set_mass(req.mass);
        // #034 修订四 spawn 第 9 参 binding：契约写进造物数据（出生声明，世界每拍
        // 托管执行）；绑定对象=创造者（fade_on_death 的"主人"）
        entity.set_binding(req.binding);
        entity.set_bound_to(Some(req.creator));
        // D-003 spawn 第 10 参 motion：运动身份写进造物数据（import 层已钳制，
        // 此处直存）——dynamic 造物即刻受完整物理（身体=声明 radius 的圆）
        entity.set_motion(req.motion);
        // 出生初速（spawn 第 11/12 参，D-018 一体式发射提前落地）：import 层已校验
        // （有限+模长 ≤ 世界极速），此处写进造物 velocity 字段。位置积分（造物被
        // 初速带着飞）不做——那是 D-018「通用有速度实体物理」的活；velocity 字段
        // 所有实体都有，static 造物钉死不吃速度物理（初速仅对 dynamic 造物有
        // 物理意义，api.md 载明）
        entity.set_velocity((req.vx, req.vy));
        // 沙箱创建成功才插入世界（失败则实体不落世界，不留无沙箱活体）
        if let Err(e) = sandbox_map.create(entity_id, &wasm_bytes, req.fuel_limit) {
            tracing::warn!(
                creator = %req.creator.0,
                error = %e,
                "[process_spawns] 造物沙箱创建失败，该次 spawn 作废"
            );
            continue;
        }
        // 出生数据包交付（spawn 第 13/14 参读取侧）：写进造物沙箱 SandboxData，
        // 造物代码经 env.birth_blob 随时读回（世界不解释内容，通道归世界内容归
        // 玩家）；随沙箱存活不持久化，跨重启不保留（api.md 载明）
        let blob_len = req.blob.len();
        if let Some(handle) = sandbox_map.sandboxes.get_mut(&entity_id) {
            handle.store.data_mut().birth_blob = req.blob;
        }
        world.entities.insert(entity_id, entity);
        tracing::info!(
            entity_id = %entity_id.0,
            creator = %req.creator.0,
            owner = %owner.0,
            pos = ?req.pos,
            glyph = %req.glyph,
            motion = req.motion,
            vx = req.vx,
            vy = req.vy,
            blob_len,
            "造物诞生：玩家造物进场（门二，产权链归属创造者 owner；D-003 motion={}）",
            if req.motion == codeworld_core::entity::MOTION_DYNAMIC { "dynamic 全活" } else { "static 钉死" }
        );
    }
}

/// 操控定装弹基准炮口速度（格/拍，#036 驾驶档）。12 远低于世界极速 25——发射
/// 钳制必过。操控层是"驾驶"：基准档不由玩家调，自定义弹道走玩家代码 env.shoot。
/// 期五第六刀起实际初速由解算器在速度梯（见 [`CTRL_SHOOT_SPEED_LADDER`]）中
/// 搜索确定，此值×补偿系数是梯首档（最常用的观感档）。
const CTRL_SHOOT_SPEED: f64 = 12.0;
/// 阻力补偿系数：世界每拍速度衰 5%（drag 0.05/mass 1.0），补偿档=基准×1.15。
const CTRL_SHOOT_DRAG_COMP: f64 = 1.15;
/// 自适应火力上限（格/拍）= 速度梯末档。该值决定"够不着"的有效门槛：
/// 离散拍物理（半隐式欧拉，位移=重力后末速度）下弹道顶点峰值
/// ≈ sy + Σ(vy_n)，v=22 高弧顶点 ~12.8 格（12 格高的 S 需 vy≈21.5）、
/// 15 格地面目标需 ~17；13.8 档射程仅 ~10 格（vy>9.8 才能飞过首拍）。
/// 地面无解门槛 ~25 格（22 档射程），低档优先所以常规目标不受影响。
pub const CTRL_SHOOT_MAX_EFF_SPEED: f64 = 22.0;
/// 解算收敛容差（格）：弹道全程与目标的最小距离 ≤ 此值视为可命中。
/// 0.65 = CCD 撞体窗口满值（r弹 0.15 + r身 0.5）——与服务器结算判据完全一致，
/// 不是放宽（弹道扫过窗口即真实命中）。
const CTRL_SOLVE_TOLERANCE: f64 = 0.65;
/// 近距阈值：低于此距离一律低弧（近距高弧解近乎垂直吊射自己脑门，反直觉坏解）
const CTRL_SOLVE_CLOSE_RANGE: f64 = 2.0;
/// 速度搜索梯（格/拍，从低到高）：离散拍物理下速度决定可飞角度域与射程
///（见 [`CTRL_SHOOT_MAX_EFF_SPEED`] 注），解析公式严重失真，故速度也由搜索
/// 确定——逐档尝试，首个可行解即返回（低速度观感档位优先）。末档=火力上限。
const CTRL_SHOOT_SPEED_LADDER: [f64; 7] = [
    CTRL_SHOOT_SPEED * CTRL_SHOOT_DRAG_COMP, // 13.8 补偿档
    15.0,
    16.0,
    17.0,
    18.0,
    20.0,
    CTRL_SHOOT_MAX_EFF_SPEED,
];
/// 操控定装弹质量（#034：mass 只参与物理弹道，永不参与攻击结算）
const CTRL_SHOOT_MASS: f64 = 1.0;

/// 操控弹炮塔解算器（期五第六刀重写：不再恒定 60° 仰角——那是高空目标
/// 四发哑弹的根因，顶点 ~9.6 够不到 12 格高的 S）。给定发射点/目标点/基准
/// 炮口速度，解出能命中目标的初速矢量。
///
/// 【#044 子步宪法锚】本解算器是"弹道积分"子步的预测侧形态：内部
/// simulate_path 逐拍重演世界物理（N=1 整步积分），搜索空间（速度梯×角度）
/// 是固定常数表——判据只读发射点/目标点/世界物理参数（纯仿真输入），
/// 不读任何机器数据，同一输入必然同一解（解算确定性锚测试
/// solve_launch_vector_deterministic_same_input_same_solution）。宪法要求的
/// "N=世界状态的纯函数"在此已然成立：解算结果是 (sx,sy,tx,ty) 与世界常数的
/// 纯函数。docs/decisions.md #044 / dev-plan D-015。
///
/// 选型说明（闭式解 vs 模拟搜索）：教科书闭式解
/// tanθ = (v² ± √(v⁴ - g(g·d² + 2·Δy·v²))) / (g·d) 假设连续时间物理，本世界
/// 是离散拍物理（重力先作用于整拍再位移）：vy < 9.8 的弹**首拍即砸地**，
/// 可飞角度域/射程随速度非线性跳变（13.8 档射程 ~10 格 vs 连续公式 ~19 格），
/// 连续解系统性失真（30° 定角方案当年也因此报废）。故本解算器用模拟搜索：
/// 速度梯逐档 × 角度域粗扫+两轮细化，每个候选按真实物理步序（重力→阻力→
/// 极速钳→位移，与 step_projectiles 同序）模拟弹道全程，取全程最小距离。
/// 判据（0.65=CCD 撞体窗口）与服务器结算完全一致——解算"说能打中"当且仅当
/// 世界物理"真的会命中"，不哑弹。每次解算 ≤5 档 ×~28 角 ×30 拍模拟，微秒级。
///
/// 弧线分支（决定搜索区间）：近距（<[`CTRL_SOLVE_CLOSE_RANGE`]）一律低弧
/// [1°,45°]；否则目标高于发射点（Δy>0）取高弧 [46°,89°]（高抛观感，任务书
/// 业界对标；双根语义：+ 号 tanθ 大 = 角陡 = 高弧），否则低弧。
///
/// 前端弧线预览镜像同算法（#028 镜像铁律，契约锚 net-protocol.md）——
/// 改一处必须同步另一处。
///
/// 返回 None（无解）的路径：零距离目标（贴脸走 attack 分流不解算）/
/// 全速度梯×全角度无一进入命中窗口（目标够不着或物理上打不准）。
pub fn solve_launch_vector(
    sx: f64,
    sy: f64,
    tx: f64,
    ty: f64,
    _base_speed: f64,
) -> Option<(f64, f64)> {
    let dx = tx - sx;
    let dy = ty - sy;
    let d = (dx * dx + dy * dy).sqrt();
    if d < 1e-6 {
        return None; // 零距离目标：贴身刀走 attack 通道，射击解算拒绝
    }
    let phys = codeworld_core::physics::DefaultPhysics::default();
    let high_arc = d >= CTRL_SOLVE_CLOSE_RANGE && dy > 0.0;
    // 模拟弹道全程：真实物理步序（与 crates/physics step_projectiles 同序镜像），
    // 返回逐拍末坐标（含起点），落地 y<半径截断。
    // 【#044 子步宪法锚·弹道积分】逐拍 N=1 整步积分，步数只由弹丸状态决定
    // （落地 y<半径 或 寿命 30 拍截断）——纯状态驱动，无任何机器数据参与。
    let simulate_path = |mut vx: f64, mut vy: f64| -> Vec<(f64, f64)> {
        let r = codeworld_core::collision::PROJECTILE_RADIUS;
        let mut pts = vec![(sx, sy)];
        let (mut x, mut y) = (sx, sy);
        for _ in 0..codeworld_physics::default::PROJECTILE_MAX_AGE_TICKS {
            vy += phys.gravity;
            let f = 1.0 - phys.drag_coefficient / CTRL_SHOOT_MASS;
            vx *= f;
            vy *= f;
            let sp = (vx * vx + vy * vy).sqrt();
            if sp > phys.max_speed {
                let k = phys.max_speed / sp;
                vx *= k;
                vy *= k;
            }
            x += vx;
            y += vy;
            pts.push((x, y));
            if y < r {
                break;
            }
        }
        pts
    };
    // 点到线段最小距离（弹道拍间是直线段，仅拍末采样会漏最近点——
    // 高弧命中窗口在弹道中段下落穿过目标处，必须全程扫描，与服务器 CCD 撞体同判据）
    let seg_min_dist = |a: (f64, f64), b: (f64, f64), p: (f64, f64)| -> f64 {
        let (abx, aby) = (b.0 - a.0, b.1 - a.1);
        let len2 = abx * abx + aby * aby;
        let t = if len2 < 1e-12 {
            0.0
        } else {
            (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0)
        };
        ((p.0 - a.0 - abx * t).powi(2) + (p.1 - a.1 - aby * t).powi(2)).sqrt()
    };
    // 给定速度的角度域搜索：粗扫 12 段 + 两轮细化（每轮步长 /8）。
    // 离散拍物理下 miss(θ) 在近距区不保证单峰（首拍砸地断崖），粗扫保底全局、
    // 细化收精度；最优 miss ≤ 容差才有解
    let scan_angle = |v: f64| -> Option<(f64, f64)> {
        let ux = dx / d; // 水平方位单位矢量（解算发生在发射点→目标的垂直平面内）
        let miss_of = |theta: f64| -> f64 {
            let (vx, vy) = (v * theta.cos() * ux, v * theta.sin());
            simulate_path(vx, vy)
                .windows(2)
                .map(|w| seg_min_dist(w[0], w[1], (tx, ty)))
                .fold(f64::MAX, f64::min)
        };
        let deg = std::f64::consts::PI / 180.0;
        let (lo, hi) = if high_arc {
            (46.0 * deg, 89.0 * deg)
        } else {
            (1.0 * deg, 45.0 * deg)
        };
        let steps = 12;
        let step0 = (hi - lo) / steps as f64;
        let mut best_theta = lo;
        let mut best_miss = miss_of(lo);
        for i in 1..=steps {
            let th = lo + step0 * i as f64;
            let m = miss_of(th);
            if m < best_miss {
                best_miss = m;
                best_theta = th;
            }
        }
        let mut step = step0;
        for _ in 0..2 {
            let (lo2, hi2) = ((best_theta - step).max(lo), (best_theta + step).min(hi));
            for i in 1..=8 {
                let th = lo2 + (hi2 - lo2) * i as f64 / 8.0;
                let m = miss_of(th);
                if m < best_miss {
                    best_miss = m;
                    best_theta = th;
                }
            }
            step /= 8.0;
        }
        if best_miss <= CTRL_SOLVE_TOLERANCE {
            Some((v * best_theta.cos() * ux, v * best_theta.sin()))
        } else {
            None // 该速度档全角度够不着/打不准，升下一档
        }
    };
    for v in CTRL_SHOOT_SPEED_LADDER {
        if let Some(sol) = scan_angle(v) {
            return Some(sol);
        }
    }
    None // 全速度梯×全角度无一进入命中窗口 = 够不着，不发射哑弹
}

/// 发射处理（#034 弹道系统：env.shoot 排队请求 -> 弹丸实体落地成世界物体）
///
/// 离体即世界物体（#034 立法第 1 条）：弹丸是逻辑+物理+渲染三层同体的世界实体，
/// 出手前归代码（排队中），本步之后归世界（物理步接管），命中后归结算（deliver 管线）。
/// - 发射者必须存活（死者不出手，防御性复查）
/// - 世界实体上限同 spawn 口径（弹丸也是世界实体，占名额）
/// - 弹丸外观：glyph="."（小圆点）、颜色=发射者 glyph 色（前端弹道可溯源）
/// - 弹丸半径 PROJECTILE_RADIUS（0.15）、质量/初速 import 层已钳制
///
/// 返回无（弹丸下一段在 tick 的弹道步积分；命中/落地事件归弹道步产出）。
fn process_shoots(world: &mut WorldState, sandbox_map: &mut EntitySandboxMap, current_tick: u64) {
    let requests = sandbox_map.collect_pending_shoots();
    process_shoot_requests(world, sandbox_map, requests, current_tick, &[]);
}

/// 射击请求落地（process_shoots 主体；will_shooters = 遗嘱放行名单，D-007 ③）
///
/// 发射者已死分支的例外：遗嘱钩子当拍排队的 shoot 放行（死者最后一发照常出膛
/// ——烟花/遗物弹，发射位置读死者实况坐标）。常规路径传空名单，行为与拆分前
/// 完全一致。
fn process_shoot_requests(
    world: &mut WorldState,
    sandbox_map: &mut EntitySandboxMap,
    requests: Vec<codeworld_arena::sandbox::PendingShoot>,
    current_tick: u64,
    will_shooters: &[EntityId],
) {
    for req in requests {
        // 发射者必须存活（死者不出手，防御性复查；遗嘱例外：死亡谢幕窗口放行）
        let will_pass = will_shooters.contains(&req.shooter);
        let Some(shooter) = world
            .entities
            .get(&req.shooter)
            .filter(|e| e.is_alive() || will_pass)
        else {
            tracing::warn!(
                shooter = %req.shooter.0,
                "[process_shoots] 发射者已死，射击请求作废"
            );
            continue;
        };
        if world.entities.len() >= sandbox_map.max_entities {
            tracing::warn!(
                shooter = %req.shooter.0,
                entities = world.entities.len(),
                cap = sandbox_map.max_entities,
                "[process_shoots] 达世界实体上限，射击请求作废"
            );
            continue;
        }
        let projectile_id = EntityId::new();
        // 发射者数据先取值（借 world 不可变引用，插入前释放借用）
        let (birth_pos, owner, color) = (shooter.pos(), shooter.owner, shooter.appearance.color.clone());
        let mut projectile = Entity::new(
            projectile_id,
            owner, // 产权链：弹丸归属发射者的 owner（与造物同哲学）
            codeworld_core::collision::PROJECTILE_CODE.to_string(),
            birth_pos, // 从发射者当前位置出手（阶段二实况坐标，含本拍移动）
            Appearance::new(".".to_string(), color, 1.0),
            current_tick,
            1_000_000,
        );
        projectile.set_radius(codeworld_core::collision::PROJECTILE_RADIUS);
        projectile.set_mass(req.mass);
        projectile.set_velocity((req.vx, req.vy));
        // #034 修订四：绑定契约写进弹丸数据（出生声明，世界每拍托管执行——
        // 遗嘱模式：契约不需要链路）+ 账本留档（结算侧 grace 判定数据源）
        projectile.set_binding(req.binding);
        projectile.set_bound_to(Some(req.shooter));
        sandbox_map.projectiles.insert(
            projectile_id,
            ProjectileMeta {
                shooter: req.shooter,
                payload: req.payload,
                binding: req.binding,
            },
        );
        // 发射痕迹（#034 修订三：发射即暴露——攻击自由，但攻击留下痕迹）。
        // 痕迹寿命 1 拍基础，动能（速度×质量）超阈值加到 3 拍封顶：强弹痕迹更长，
        // 反制手段（哨塔网/痕迹追踪反击）玩家自己长，世界只保证"打了就有记录"
        let speed = (req.vx * req.vx + req.vy * req.vy).sqrt();
        let kinetic_energy = speed * req.mass;
        let ttl: u32 = if kinetic_energy > TRACE_STRONG_KE { 3 } else { 1 };
        sandbox_map.traces.push(LaunchTrace {
            trace_id: EntityId::new(),
            pos: birth_pos,
            shooter: req.shooter,
            birth_tick: current_tick,
            ttl,
        });
        world.entities.insert(projectile_id, projectile);
        tracing::info!(
            projectile = %projectile_id.0,
            shooter = %req.shooter.0,
            pos = ?birth_pos,
            vx = req.vx,
            vy = req.vy,
            mass = req.mass,
            kinetic_energy,
            trace_ttl = ttl,
            "[shoot] 弹丸发射+痕迹留下（离体即世界物体：出手前归代码，飞行中归世界）"
        );
    }
}

/// 弹道事件结算（#034：物理步产出的命中/落地事件 -> 既有两阶段攻击管线）
///
/// - HitBody：命中已知的身体 -> PendingAttack（attacker=发射者、target=目标坐标、
///   payload=弹丸携带字节），走 deliver_attacks 语义（on_attack 防御询问->拦截/
///   换血/击杀掠夺）。弹丸消亡（物理步已标记死亡）。
/// - Landed：落地 -> 落点做点命中（#031 判据复用，find_hit_target 在 deliver 里跑），
///   打不中任何身体 = 落地哑弹。
/// - Expired：超时哑弹，无结算。
///
/// 无第二结算路径：本函数只"翻译"成 PendingAttack，换血/掠夺全在 settle_attacks。
fn settle_projectile_events(
    sandbox_map: &mut EntitySandboxMap,
    events: &[codeworld_physics::default::ProjectileEvent],
    pending_attacks: &mut Vec<PendingAttack>,
) {
    use codeworld_physics::default::ProjectileEvent;
    for ev in events {
        match ev {
            ProjectileEvent::HitBody { projectile, target, point } => {
                let Some(meta) = sandbox_map.projectiles.get(projectile) else {
                    tracing::warn!(projectile = %projectile.0, "[弹丸命中] 无账本（跨重启残留？），跳过结算");
                    continue;
                };
                let meta = meta.clone();
                sandbox_map.projectiles.remove(projectile);
                tracing::info!(
                    projectile = %projectile.0,
                    target = %target.0,
                    point = ?point,
                    "[弹丸命中] 结算并入两阶段管线（弹丸消亡）"
                );
                pending_attacks.push(PendingAttack::projectile_strike(
                    meta.shooter,
                    *point,
                    meta.payload,
                    meta.binding == codeworld_core::entity::BINDING_GRACE_OWNER,
                ));
            }
            ProjectileEvent::Landed { projectile, point } => {
                let Some(meta) = sandbox_map.projectiles.get(projectile) else {
                    tracing::warn!(projectile = %projectile.0, "[弹丸落地] 无账本，跳过结算");
                    continue;
                };
                let meta = meta.clone();
                sandbox_map.projectiles.remove(projectile);
                tracing::info!(
                    projectile = %projectile.0,
                    point = ?point,
                    "[弹丸落地] 落点点命中结算（打不中任何身体=哑弹消亡）"
                );
                pending_attacks.push(PendingAttack::projectile_strike(
                    meta.shooter,
                    *point,
                    meta.payload,
                    meta.binding == codeworld_core::entity::BINDING_GRACE_OWNER,
                ));
            }
            ProjectileEvent::Expired { projectile } => {
                sandbox_map.projectiles.remove(projectile);
                tracing::info!(projectile = %projectile.0, "[弹丸哑弹] 超时消亡（寿命上限防永飞）");
            }
        }
    }
}

/// 生存点数扣减（架构文档 4.1 节阶段二 b 步）
///
/// 遍历存活实体调用 survival.decay(current_tick)，归零则标记死亡。
/// 死亡清理步（retain is_alive）会随后移除死亡实体。
fn decay_survival(world: &mut WorldState, current_tick: u64) {    for entity in world.entities.values_mut() {
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

/// 命中事件记录（D-017 协议小刀：随本拍快照广播的"打中了"三件套）。
///
/// 只记结算成功的刀（击杀或击晕都算命中——动画槽位选择只关心砍没砍中）；
/// 落空/拦截/作废不进广播（落空提示前端已有本地预告；拦截是防守方隐私，
/// 广播拦截=替攻方做侦察）。字段对齐 dev-plan D-017 定案原文："攻方/目标ID/
/// 目标位置——本来在广播命中，多带字段，账本不变"。目标位置=**结算时刻
/// 实况坐标**（非攻击点击坐标），是前端动画包方向变体（attack_high/mid/low）
/// 的选槽依据。
pub(crate) struct AttackEventRecord {
    pub(crate) attacker: EntityId,
    pub(crate) target: EntityId,
    pub(crate) target_pos: (f64, f64),
}

/// 绑定契约托管清算（#034 修订四：世界每拍执行出生声明——遗嘱模式，
/// 契约不需要链路）。排在攻击结算/生存衰减之后：本拍所有死亡来源
/// （击杀/衰减/life()=false/登出）都已落地，同生契约当拍生效。
/// - fade_on_death（弹丸与造物同款）：绑定对象已死或不在 → 本体即刻消亡
/// - grace_owner：不在此清算（豁免在 deliver_attacks 结算路径执行）
/// - unbound：跳过（离体即世界之物，同生不共死=现状默认）
fn enforce_binding_deaths(world: &mut WorldState, sandbox_map: &mut EntitySandboxMap) {
    use codeworld_core::entity::BINDING_FADE_ON_DEATH;
    // 弹丸先清（账本同步）：主人死的同生弹即刻消亡（哑弹，INFO 日志）
    let dead_shells: Vec<(EntityId, EntityId)> = sandbox_map
        .projectiles
        .iter()
        .filter(|(_, meta)| meta.binding == BINDING_FADE_ON_DEATH)
        .filter(|(_, meta)| {
            !world
                .entities
                .get(&meta.shooter)
                .map(|e| e.is_alive())
                .unwrap_or(false)
        })
        .map(|(pid, meta)| (*pid, meta.shooter))
        .collect();
    for (pid, shooter) in dead_shells {
        if let Some(e) = world.entities.get_mut(&pid) {
            e.kill();
        }
        sandbox_map.projectiles.remove(&pid);
        tracing::info!(
            projectile = %pid.0,
            shooter = %shooter.0,
            "[binding] 同生契约生效，随主消亡（弹丸哑弹）"
        );
    }
    // 造物与一切声明体：binding=fade 且绑定对象已死/不在 → 消亡
    //（塔随创造者消亡；bound_to=None 的 fade 声明视为主人已失，即刻消亡）
    let doomed: Vec<EntityId> = world
        .entities
        .iter()
        .filter(|(_, e)| e.is_alive() && e.binding() == BINDING_FADE_ON_DEATH)
        .filter(|(_, e)| {
            !e.bound_to()
                .map(|owner| {
                    world
                        .entities
                        .get(&owner)
                        .map(|o| o.is_alive())
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        })
        .map(|(id, _)| *id)
        .collect();
    for id in doomed {
        let bound_to = world.entities.get(&id).and_then(|e| e.bound_to());
        if let Some(e) = world.entities.get_mut(&id) {
            e.kill();
        }
        tracing::info!(
            entity_id = %id.0,
            bound_to = ?bound_to.map(|o| o.0),
            "[binding] 同生契约生效，随主消亡（造物）"
        );
    }
}

/// 在飞弹丸账本（#034 弹道系统）
///
/// Entity 本体只存物理（质量/速度/半径/坐标）；发射者与携带 payload 归本账本——
/// 撞身体/落地结算时据此组装 PendingAttack（attacker=发射者，走既有两阶段管线：
/// 攻杀仍是代码对代码，mass 永不参与结算铁律在管线侧天然成立）。消亡即清理。
#[derive(Debug, Clone)]
struct ProjectileMeta {
    /// 发射者（物理步撞身体豁免它；结算掠夺归属它）
    shooter: EntityId,
    /// 弹丸携带的攻击字节（空=默认击杀模块）
    payload: Vec<u8>,
    /// 绑定契约（#034 修订四：unbound 照飞 / fade_on_death 随主消亡 /
    /// grace_owner 落地不砸发射者）
    binding: i32,
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

/// 攻击点命中（#031）：找"点在体内"的存活实体取最近者（圆=dist<r 严格；
/// 造物 AABB=闭区间包含），距离并列按 EntityId 升序定序（消灭随机性——
/// 延续两阶段结算立法精神）。None = 点不在任何身体内（落空）。
/// 弹丸不可被锁定（#034：弹丸是物体不是生物，attack_at/落点结算永不指向弹丸）。
fn find_hit_target(world: &WorldState, point: (f64, f64)) -> Option<EntityId> {
    use codeworld_core::collision::{body_of, point_in_body};
    let mut best: Option<(f64, EntityId)> = None;
    for (id, e) in &world.entities {
        if !e.is_alive() || codeworld_core::collision::is_projectile(e) {
            continue;
        }
        let body = body_of(e);
        if !point_in_body(point, &body) {
            continue;
        }
        let (ex, ey) = e.pos();
        let dx = point.0 - ex;
        let dy = point.1 - ey;
        let d2 = dx * dx + dy * dy;
        let better = match best {
            None => true,
            Some((bd, bid)) => d2 < bd || (d2 == bd && id.0 < bid.0),
        };
        if better {
            best = Some((d2, *id));
        }
    }
    best.map(|(_, id)| id)
}

/// 收件阶段：逐刀问防守、产出待结算列表（架构 4.2 两阶段-收件）
///
/// - 攻击者已死 -> 整刀作废（不触发防守、不结算；防死者点数蒸发，P1）。
/// - 坐标解析（#031 点命中制）：点在体内（圆 dist<r / AABB 内）的存活实体取最近者
///   （同距按 EntityId 升序）——站桩目标必中，移动目标打不中（猫鼠博弈仍在）。
/// - 接触门（#034 贴身刀）：出手者与目标身体相触（dist < r攻+r目+0.5 胳膊冗余，
///   严格 <）才砍得着；够不着 = 落空（原因"距离不足"，与"点不在任何身体内"区分——
///   远程打击归 shoot 弹丸，attack_at 是黑神话棍法语义的贴身近战）。
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
    let mut out_of_reach = 0usize; // #034 接触门落空（距离不足）
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
        // 参战记录-攻击者（#013 战斗残影期）：出刀即战斗——落空（含距离不足）/拦截/命中都算，
        // 堵"打空再拔线"绕过战斗残影的漏洞。world.tick() 即当前 tick（tick 开头已 advance）。
        sandbox_map.players.record_combat_id(atk.attacker, world.tick());
        // 坐标解析（#031 点命中制）：点在体内取最近者（attack_at 攻击坐标不是
        // EntityId，目标移动出点则落空——猫鼠博弈）
        let Some(target_id) = find_hit_target(world, atk.target_pos) else {
            missed += 1;
            tracing::info!(
                attacker = %atk.attacker.0,
                pos = ?atk.target_pos,
                "[deliver_attacks] 攻击落空（点不在任何身体内）"
            );
            continue;
        };
        // 接触门（#034 贴身刀语义）：点找得到目标，但够不着 = 落空。
        // 严格 <（恰好等于判距=落空，collision.rs melee_in_reach 测试锚定）。
        // 弹丸结算不走本门（contact_exempt=true）：弹丸撞身体本身就是接触事实——
        // 离体之物是攻击者手臂的延伸，再判发射者贴身会把弹道打回瞬发瞬达。
        // #034 修订四攻击豁免条款（声明式）：grace_owner 弹命中发射者本人（现实
        // 场景=落点在自己脚下的落地弹；飞行撞体在物理步已全程豁免发射者）→ 哑弹。
        // unbound 不豁免——落地砸自己合法（世界不豁免，豁免是声明）
        if atk.grace_owner && target_id == atk.attacker {
            tracing::info!(
                attacker = %atk.attacker.0,
                pos = ?atk.target_pos,
                "[binding] grace_owner 豁免生效：弹丸落地不结算发射者（哑弹消亡）"
            );
            continue;
        }
        let attacker = &world.entities[&atk.attacker];
        let target = &world.entities[&target_id];
        let in_reach = codeworld_core::collision::melee_in_reach(
            attacker.pos(),
            attacker.radius(),
            target.pos(),
            target.radius(),
        );
        if !in_reach && !atk.contact_exempt {
            out_of_reach += 1;
            let (ax, ay) = attacker.pos();
            let (tx, ty) = target.pos();
            let dist = ((ax - tx).powi(2) + (ay - ty).powi(2)).sqrt();
            tracing::info!(
                attacker = %atk.attacker.0,
                target = %target_id.0,
                dist,
                reach = attacker.radius() + target.radius() + codeworld_core::collision::MELEE_REACH,
                "[deliver_attacks] 攻击落空（距离不足：非贴身够不着，远程打击请用 shoot 弹丸）"
            );
            continue;
        }
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
        out_of_reach,
        defense_failed,
        intercepted = intercepted_count,
        pending = strikes.len(),
        "[deliver_attacks] 收件完成：作废/落空（含距离不足）/防守失败/拦截/待结算"
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
                // 命中事件（D-017 协议小刀）：结算成功的刀记攻方/目标/目标位置，
                // 随本拍快照广播（击杀或击晕都算命中——动画选槽只关心砍没砍中）。
                // 位置取结算时刻实况坐标（本刀尚未移动过目标，圆心即被打中的地方）
                let hit_pos = world
                    .entities
                    .get(&strike.target)
                    .map(|e| e.pos())
                    .unwrap_or(strike.attack.target_pos);
                sandbox_map.attack_events.push(AttackEventRecord {
                    attacker: strike.attack.attacker,
                    target: strike.target,
                    target_pos: hit_pos,
                });
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

/// 接触回调通知（D-007 ②：on_contact——与 on_attack/on_probe 同族第三兄弟）
///
/// 对每个新进入接触的 ContactPair 通知双方：a 收 (b.x, b.y)、b 收 (a.x, a.y)。
/// 坐标取接触拍实况（c 步 world 实况坐标）。防刷屏双闸：边沿触发（contacts
/// 已是本拍新进入对）+ 过滤归代码（世界零过滤，双方都收通知，白名单/可疑判断
/// 玩家自己写）。燃料：host 侧独立小预算包裹（call_on_contact 内部，probe 同款）。
/// 跳过语义（锚定不 Err）：含死亡方整对跳过（与 handle_collisions 同口径）；
/// 残影侧不唤醒（残影不执行玩家代码既有不变量）；无沙箱侧跳过（框架实体）。
fn notify_contacts(world: &mut WorldState, sandbox_map: &mut EntitySandboxMap, contacts: &[ContactPair]) {
    let budget = sandbox_map.contact_fuel;
    for pair in contacts {
        // 含死亡方整对跳过（死亡实体本拍会被 d 步移除，接触通道针对存活交互）
        let a_alive = world.entities.get(&pair.a).map(|e| e.is_alive()).unwrap_or(false);
        let b_alive = world.entities.get(&pair.b).map(|e| e.is_alive()).unwrap_or(false);
        if !a_alive || !b_alive {
            tracing::debug!(a = %pair.a.0, b = %pair.b.0, "[contact] 跳过含死亡实体的接触通知");
            continue;
        }
        // 双方实况坐标（接触拍实况，非建对时快照）
        let (Some(a_pos), Some(b_pos)) = (
            world.entities.get(&pair.a).map(|e| e.pos()),
            world.entities.get(&pair.b).map(|e| e.pos()),
        ) else {
            continue;
        };
        // 双方通知：自己收对方的坐标
        for (self_id, other_pos) in [(pair.a, b_pos), (pair.b, a_pos)] {
            // 残影休眠不唤醒（残影不执行玩家代码，沙箱保留但世界不驱动它）
            if sandbox_map.players.is_ghost(&self_id) {
                tracing::debug!(entity_id = %self_id.0, "[contact] 残影侧跳过（不唤醒）");
                continue;
            }
            let Some(handle) = sandbox_map.sandboxes.get_mut(&self_id) else {
                tracing::debug!(entity_id = %self_id.0, "[contact] 无沙箱侧跳过");
                continue;
            };
            match sandbox_map
                .manager
                .call_on_contact(handle, other_pos.0, other_pos.1, budget)
            {
                Ok(Some(_used)) => {} // 已通知（预算内消耗由 call_on_contact 记账）
                Ok(None) => {}        // 未导出 on_contact：no-op（不设防同款防御）
                Err(e) => tracing::warn!(
                    entity_id = %self_id.0,
                    error = %e,
                    "[contact] on_contact 执行失败（不影响对方侧通知）"
                ),
            }
        }
    }
}

/// 遗嘱窗口（D-007 ③：on_death 遗嘱钩子，#043 死亡谢幕——世界托管执行）
///
/// 对本拍每个死亡实体（沙箱还在）调用 on_death（host 侧独立预算包裹，预算即
/// 遗嘱字数上限：烧尽即写不完，世界不补偿不重试），并抢救其遗嘱产物：
/// - pending_shoots / pending_spawns 收集返回，由调用方当拍喂回既有管线
///   （遗嘱模式=#035 推广：生前签好条款世界照办，本人无需在场——创造者已死
///   分支对遗嘱队列开例外放行）
/// - pending_attacks 丢弃（死人不出刀，与"死者攻击作废"既有语义对齐）
///
/// 时机契约：必须在 sandbox_map.remove 之前（沙箱还在才能执行遗嘱）、必须在
/// retain 之前（死者实体还在 world，遗嘱产物的发射坐标/产权继承要读它）。
/// 触发排除：残影消散（会话离场合同，消散≠死亡谢幕）、登出 take（会话级离场
/// 非死亡）；decay/被杀/life()=false/处置期死亡全触发。无沙箱（弹丸/框架实体）
/// 自然跳过。
fn run_death_wills(
    sandbox_map: &mut EntitySandboxMap,
    dead_ids: &[EntityId],
) -> (Vec<codeworld_arena::sandbox::PendingShoot>, Vec<PendingSpawn>) {
    let mut will_shoots = Vec::new();
    let mut will_spawns = Vec::new();
    for id in dead_ids {
        // 会话级离场（登出 take / 残影到期消散）：非死亡谢幕，不触发遗嘱
        //（挂号消费掉，防跨拍残留）
        if sandbox_map.session_takes.remove(id) {
            tracing::debug!(entity_id = %id.0, "[will] 会话级离场（take/消散），不触发遗嘱");
            continue;
        }
        // 残影消散：非死亡谢幕（下线合同），不触发遗嘱
        if sandbox_map.players.is_ghost(id) {
            tracing::debug!(entity_id = %id.0, "[will] 残影消散，不触发遗嘱");
            continue;
        }
        let Some(handle) = sandbox_map.sandboxes.get_mut(id) else {
            // 无沙箱（弹丸/框架实体）：无遗嘱可言（debug 级，弹丸消亡是常态不刷屏）
            tracing::debug!(entity_id = %id.0, "[will] 无沙箱，无遗嘱");
            continue;
        };
        let budget = sandbox_map.death_fuel;
        tracing::info!(entity_id = %id.0, budget, "[will] 死亡谢幕：执行遗嘱（世界托管，限步数）");
        match sandbox_map.manager.call_on_death(handle, budget) {
            Ok(Some(used)) => {
                tracing::info!(entity_id = %id.0, fuel_used = used, "[will] 遗嘱执行完毕");
            }
            Ok(None) => {
                tracing::debug!(entity_id = %id.0, "[will] 无 on_death 导出，无遗嘱（零回归）");
            }
            Err(e) => {
                // 遗嘱执行失败不影响死亡清理（世界照办的部分已排队，失败的作废）
                tracing::warn!(entity_id = %id.0, error = %e, "[will] 遗嘱执行失败（不影响死亡清理）");
            }
        }
        // 遗嘱产物抢救（沙箱销毁前）：shoot/spawn 当拍放行，attack 丢弃
        let data = handle.store.data_mut();
        will_shoots.append(&mut data.pending_shoots);
        will_spawns.append(&mut data.pending_spawns);
        let discarded = data.pending_attacks.len();
        data.pending_attacks.clear();
        if discarded > 0 {
            tracing::info!(
                entity_id = %id.0,
                count = discarded,
                "[will] 遗嘱排队的攻击丢弃（死人不出刀）"
            );
        }
    }
    // P2-1 收尾：消费循环结束后清场——两类合法登记（登出 take / 残影到期消散）
    // 都在 d 步【之前】kill 且实体仍在世界，同拍必进 dead_ids 被上面的 remove
    // 消费掉；活到此处的全是死条目（典型：战斗死亡实体已 remove 后登出 take，
    // remove_online 返回旧 id 的登记永不进 dead_ids，跨拍残留无界增长）。
    // 语义："遗嘱窗口已过，未消费的登记全是死条目"。
    if !sandbox_map.session_takes.is_empty() {
        let stale = sandbox_map.session_takes.len();
        sandbox_map.session_takes.clear();
        tracing::debug!(stale, "[will] session_takes 死条目清场（遗嘱窗口已过）");
    }
    (will_shoots, will_spawns)
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

/// 统计 marker 流中 S 替身拦截次数（d2 步伪装台词触发源，纯函数便于测试）
fn spy_disguise_intercept_count(markers: &[i32]) -> usize {
    markers
        .iter()
        .filter(|&&m| m == SPY_INTERCEPT_MARKER)
        .count()
}

/// 感知规则：找 target 最近的**视野内**其他存活实体坐标（env.scan 的数据源）
///
/// - 排除自己（不能锁定自己为敌人）
/// - 排除死亡实体（尸体不是感知目标）
/// - 排除弹丸（#034：弹丸是飞行物体不是敌人，scan/锁定永不指向弹丸——
///   防御者不为拦弹浪费刀；将来拦截射弹玩法另立）
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
        if !e.is_alive() || e.id == *target || codeworld_core::collision::is_projectile(e) {
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

/// S 替身盾拦截 marker：S 的 on_attack 拦截成功时 log(11)，host d2 步据此在
/// battle.log 打伪装台词"S: 那只是我的替身"。不能用 D 的 7/8——那两个值被
/// 盾量字幕协议占用（parse_shield_markers 会把 S 的上报误当 D 的盾量显示）
const SPY_INTERCEPT_MARKER: i32 = 11;

/// 生成开局布阵生物（三层感知 Phase A 版）
///
/// A 攻击者（红"A"）@(5,0)：直线冲锋逼近，进范围连砍空 payload 击杀刀
/// D 防御者（青"D"）@(15,0)：原地站桩+盾 d_shield 拦截+每 4 tick 反制
/// H 搅局者（黄"H"）@(10,8)：中场上空，落地后观望游走，敌人进 3 格参战（h_enabled 开关）
/// S 伪装者（紫"S"）@(25,12)：右侧高空悬停（hover 每拍声明免重力），
///   每 4 tick 眼睛列表扫描+probe 最近者，on_probe 撒谎"我是友军 life=9"；
///   替身盾 3（on_attack 拦截前 3 刀，marker 11 触发 host 伪装台词，盾尽回脆皮，
///   s_enabled 开关）
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
        "开局布阵：A 攻击者冲锋 / D 防御者站桩（盾每拦一刀-1，每4tick反制）/ H 搅局者={}（投机者：敌人进3格参战，被攻击50%拦截）/ S 伪装者={}（间谍：悬停高空+每4tick扫描+probe，on_probe撒谎友军，替身盾3拦截+伪装台词）",
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

/// 防御者 D 的玩家代码 WAT（青色，性格：塔防铁壁+贴身反击+主动接近）
///
/// 策略（#034 attack_at 接触化后的生物适配：贴身刀，够不着就挪过去）：
/// - on_tick：扫描最近敌人；悬停者不追（敌在头顶 2 格开外=空中够不着，
///   塔防不下场追鸟）；贴身（dist < 1.5 = r+r+0.5 胳膊判距）且反制拍
///   （每 4 tick）时先 probe 该坐标看一眼（三层感知 Phase A：只读侦察，
///   结果不用于决策），再排空 payload 击杀刀；未贴身则每拍向敌人挪一步
///   （#034 接近逻辑：attack_at 够不着全落空，不挪=废柴）
/// - on_attack（有防守）：护盾有余 -> 拦截（log 7 后跟剩余盾量），盾尽 -> 放行（log 8）
///   "配额有限"由玩家代码自己定（盾是规则不是框架能力，架构 4 能力 vs 规则分层）
/// - life()=true；被打穿盾后被换血击杀（默认击杀 WAT life()=false）
///
/// 日志 marker：7=拦截 8=盾尽放行 9=反制（贴身反制拍才打，原版远距反制作废）
fn defender_wat(shield: i32) -> String {
    format!(
        r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
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
  ;; 每 tick：扫描 -> 悬停者不追 -> 贴身反制 / 未贴身接近
  (func (export "on_tick")
    (local $ex f64) (local $ey f64) (local $mx f64) (local $my f64)
    (local $dx f64) (local $dy f64) (local $dist f64)
    global.get $ticks i32.const 1 i32.add global.set $ticks
    (if (i32.eqz (call $scan (i32.const 0) (i32.const 8)))
      (then (return)))
    (local.set $ex (f64.load (i32.const 0)))
    (local.set $ey (f64.load (i32.const 8)))
    (call $get_pos)
    (local.set $my)
    (local.set $mx)
    ;; 悬停者不追：敌在头顶 2 格开外（空中够不着；高度安全=物理诚实）
    (if (f64.gt (local.get $ey) (f64.add (local.get $my) (f64.const 2.0)))
      (then (return)))
    (local.set $dx (f64.sub (local.get $ex) (local.get $mx)))
    (local.set $dy (f64.sub (local.get $ey) (local.get $my)))
    (local.set $dist (f64.sqrt
      (f64.add (f64.mul (local.get $dx) (local.get $dx))
               (f64.mul (local.get $dy) (local.get $dy)))))
    ;; 贴身（dist < 1.5 = r+r+0.5）：反制拍 probe+出刀；非反制拍站住
    (if (f64.lt (local.get $dist) (f64.const 1.5))
      (then
        (if (i32.eqz (i32.rem_u (global.get $ticks) (i32.const 4)))
          (then
            i32.const 9 call $log
            ;; 反制前 probe 目标看一眼（只读：探测结果不改变打谁何时打）
            (drop (call $probe (f64.load (i32.const 0)) (f64.load (i32.const 8))
                               (i32.const 16) (i32.const 0) (i32.const 32) (i32.const 64)))
            (drop (call $attack_at (f64.load (i32.const 0)) (f64.load (i32.const 8))
                                   (i32.const 0) (i32.const 0)))))
        (return)))
    ;; 未贴身：每拍向敌人挪一步（#034 接近逻辑，归一化方向走 1.0）
    (drop (call $move_by
      (f64.div (local.get $dx) (local.get $dist))
      (f64.div (local.get $dy) (local.get $dist)))))
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_intercepts") (result i32) global.get $intercepts)
  (func (export "get_shield") (result i32) global.get $shield))"#
    )
}

/// 搅局者 H 的玩家代码 WAT（黄色，性格：投机者——挑软柿子捏，远程搅局为主）
///
/// 策略（on_tick 每 tick 执行；#034 弹道系统生物适配：主战手段换 shoot 抛射，
/// 近战只留贴身机会刀——原 3 格参战刀在接触化后全落空，投机者不追击肉搏）：
/// 1. scan 找最近敌人，找不到（log 0）就待机（全场只剩自己时不折腾）
/// 2. 每 8 拍向最近敌人抛射一发（log 12）：vx = dx/1.71（mass 0.5 阻力下两拍
///    回到出手高度的弹道系数）、vy = +14.7（顶一拍半重力的上抛补偿）、
///    mass 0.5、空 payload 击杀弹。打不打得中看敌人站没站在弹道上——
///    参数是生物自己拼的，打不中=参数没拼对（弹道涌现，框架不管）
/// 3. 距离 <= 1.0（贴身判距内）：机会刀（log 5）对敌人坐标排空 payload
/// 4. 其余：观望游走（log 4，x 方向 ±0.5，方向按 tick 奇偶——WAT 无随机源）
/// 5. on_attack（被攻击时）：50% 拦截（奇数 tick 拦，log 6），否则放行
///
/// 日志 marker：0=无敌待机 4=观望游走 5=贴身机会刀 6=拦截成功 12=抛射
const HUNTER_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "scan" (func $scan (param i32 i32) (result i32)))
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (import "env" "attack_at" (func $attack_at (param f64 f64 i32 i32) (result i32)))
  (import "env" "shoot" (func $shoot (param f64 f64 f64 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  ;; tick 计数：on_attack 的 50% 伪随机源（奇=拦截），也是游走方向源与射击节拍源
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
    ;; 2. 每 8 拍抛射一发（vx=dx/1.71 两拍回弧、vy=+14.7 上抛补偿、mass 0.5、
    ;;    binding 0=unbound——弹道照旧回归锚）。装填拍到此为止（不游走不出刀）
    ;;    ——弹丸从实况位置出手，本拍移动会带偏弹道
    (if (i32.eqz (i32.rem_u (global.get $ticks) (i32.const 8)))
      (then
        (call $log (i32.const 12))
        (drop (call $shoot
          (f64.div (local.get $dx) (f64.const 1.71))
          (f64.const 14.7)
          (f64.const 0.5)
          (i32.const 0) (i32.const 0) (i32.const 0)))
        (return)))
    ;; 3. 贴身机会刀（dist <= 1.0 在贴身判距内；投机者不追击肉搏）
    (if (f64.le (local.get $dist) (f64.const 1.0))
      (then
        (call $log (i32.const 5))
        (drop (call $attack_at (local.get $ex) (local.get $ey)
                               (i32.const 0) (i32.const 0)))
        (return)))
    ;; 4. 观望：原地小幅游走（偶数 tick +0.5，奇数 tick -0.5）
    (call $log (i32.const 4))
    (if (i32.eqz (i32.and (global.get $ticks) (i32.const 1)))
      (then (drop (call $move_by (f64.const 0.5) (f64.const 0.0))))
      (else (drop (call $move_by (f64.const -0.5) (f64.const 0.0))))))
  (func (export "life") (result i32) i32.const 1))"#;

/// 伪装者 S 的玩家代码 WAT（紫色，性格：间谍——悬停高空+高调侦察+撒谎身份+轻装替身）
///
/// 策略（on_tick 每 tick 执行）：
/// 0. 每拍先 hover()：声明"本拍我要悬停"，世界把 S 并入当拍免重力集合
///    （与操控悬停同集合同机制，decisions.md #030 升力独立）。声明不跨拍延续，
///    每拍都要重申——"持续对抗世界规则"的代码体现（#029 无税）。
///    旧 move_by(0,+9.8) 喷气 hack 退役（拍内 12↔21.8 弹跳、反制刀坐标错位
///    两大副作用一并消失）：S 坐标全程精确稳定 (25,12)，被反制刀点名
///    "点在身体内"必中（#031 点命中制）——坐标稳定换来的 Exposure 就是
///    间谍的真实代价，喷气期的"打不中"是坐标错位 bug 不是自卫能力
/// 1. tick 计数非 4 倍数直接返回；侦察轮先 log 10（侦察 marker）
/// 2. visible_count 数敌人，空则收工（全场只剩自己时不折腾）
/// 3. visible_at 循环扫一圈（眼睛列表），记与自己的平方距最近者
/// 4. 对最近者 probe(req=[1,2,3,4] -> resp@32 cap 64)，返回值存全局（观测导出）
///
/// on_probe（被探测时）：固定回 "LIE9" 假串（i32 0x3945494C，LE 字节
/// [0x4C,0x49,0x45,0x39]——谎称"友军、life=9"。撒谎=玩家代码自由，框架只透传；
/// 但被动体徵（api_calls 计数/fuel 剩余，host 采集）藏不住高调侦察的代价。
///
/// on_attack（被攻击时，第八刀新增，拦截模式照抄 defender 的盾）：
/// 护盾有余 -> 拦截（log 11 替身 marker，host 据此在 battle.log 打伪装台词
/// "S: 那只是我的替身"），盾尽 -> 放行（后续攻击照常结算，S 回到脆皮）。
/// 盾不回复（一次性替身）。间谍是轻装：盾 3（远低于 D 的 10，调盾量改下方
/// $shield 初始值）。marker 用 11 不用 D 的 7/8——那两个值被盾量字幕协议
/// 占用（parse_shield_markers 会把 S 的上报误当 D 的盾量显示）。
///
/// 挪窝加分项（每 16 拍伪随机挪窝）跳过：S 一动，(25,12) 测试锚与 D 距离序
/// 点名的确定性时间线全崩（D 按快照坐标出刀，移动目标打不中/打中看拍奇偶），
/// 回归面远超人设收益——间谍的活感由 hover+侦察+撒谎承担。
///
/// 日志 marker：10=侦察轮开始 11=替身拦截（D 的反制已占用 9；probe 往返见 host 侧 [probe] 日志）
const SPY_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (import "env" "get_pos" (func $get_pos (result f64 f64)))
  (import "env" "hover" (func $hover))
  (import "env" "visible_count" (func $visible_count (result i32)))
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  ;; tick 计数（4 倍数侦察）；probe 返回值（观测导出）
  (global $ticks (mut i32) (i32.const 0))
  (global $probe_ret (mut i32) (i32.const -999))
  ;; ===== 替身盾（调盾量改这里：间谍轻装，盾 3）=====
  ;; 护盾余量：每拦一刀 -1，归零后不再拦截（与 D 的盾同模式，盾不回复）
  (global $shield (mut i32) (i32.const 3))
  ;; 已拦截刀数（观测导出用）
  (global $intercepts (mut i32) (i32.const 0))
  ;; 被探测：固定回撒谎假串 "LIE9"
  (func (export "on_probe") (param i32) (result i32) i32.const 0x3945494C)
  ;; 被攻击：盾有余拦截（log 11 替身 marker，host 打伪装台词），盾尽放行
  (func (export "on_attack") (param i32) (result i32)
    global.get $shield
    if (result i32)
      global.get $shield i32.const 1 i32.sub global.set $shield
      global.get $intercepts i32.const 1 i32.add global.set $intercepts
      i32.const 11 call $log
      i32.const 1
    else
      i32.const 0
    end)
  (func (export "on_tick")
    (local $n i32) (local $i i32) (local $best f64) (local $bx f64) (local $by f64)
    (local $x f64) (local $y f64) (local $mx f64) (local $my f64)
    (local $dx f64) (local $dy f64) (local $d f64)
    global.get $ticks i32.const 1 i32.add global.set $ticks
    ;; 0. 每拍悬停声明：本拍免重力（每拍重申，不受侦察节奏影响）
    call $hover
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
  (func (export "get_probe_ret") (result i32) global.get $probe_ret)
  (func (export "get_shield") (result i32) global.get $shield)
  (func (export "get_intercepts") (result i32) global.get $intercepts))"#;

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

    // === #030 半径物理：移动碰撞裁决（取代 #021 格子占位）===

    #[test]
    fn setpos_blocked_stays_at_origin() {
        // 碰撞移动（#030）：目标位与其他存活身体重叠 → 滑动裁决（滑x=目标/滑y=原地
        // 都试不了）→ 原地不动；实体留在原坐标（贴墙站，不推挤）
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.5));
        let e2 = make_entity(2, (6.0, 0.5));
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.0, 0.5),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.5), "目标位被占应留原位");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (6.0, 0.5), "占据者不受影响");
    }

    #[test]
    fn setpos_two_movers_same_target_lower_id_wins() {
        // 裁决确定序：两实体同抢一点，EntityId 小者先到先得（与排队顺序无关）
        // E2 的请求先入队、E1 的后入队：按队序应用则 E2 赢；按 id 升序裁决则 E1 赢
        // （E1 走到与 E2 恰好相切处——距离=半径和不算重叠，先到者贴着占位）
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.5));
        let e2 = make_entity(2, (7.0, 0.5));
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let mut pending: Vec<PendingChange> = vec![
            PendingChange::SetPos { entity: id2, new_pos: (6.0, 0.5) }, // 先入队
            PendingChange::SetPos { entity: id1, new_pos: (6.0, 0.5) }, // 后入队
        ];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 0.5), "id 小者先到先得");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (7.0, 0.5), "id 大者被挡留原位");
    }

    #[test]
    fn setpos_dead_occupant_does_not_block() {
        // 反向护栏：占据者已死（待清理）→ 不挡移动
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.5));
        let mut e2 = make_entity(2, (6.0, 0.5));
        e2.kill();
        let id1 = e1.id;
        world.entities.insert(id1, e1);
        world.entities.insert(e2.id, e2);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.0, 0.5),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 0.5), "死者不挡路，移动放行");
    }

    #[test]
    fn setpos_sequential_vacated_cell_blocked_for_lower_id() {
        // 先到先得语义边界：低 id 先裁决——E1 想进 E2 身上位、E2 同拍搬走：
        // E1 裁决时 E2 还在原位 → E1 被挡（顺序裁决语义，下拍 E1 可进）
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 0.5));
        let e2 = make_entity(2, (6.0, 0.5));
        let id1 = e1.id;
        let id2 = e2.id;
        world.entities.insert(id1, e1);
        world.entities.insert(id2, e2);
        let mut pending: Vec<PendingChange> = vec![
            PendingChange::SetPos { entity: id1, new_pos: (6.0, 0.5) },
            PendingChange::SetPos { entity: id2, new_pos: (9.0, 0.5) },
        ];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.5), "E1 裁决时目标位仍被 E2 占 → 被挡");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (9.0, 0.5), "E2 照常搬走");
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
        // 集成：空中实体受重力下落（架构 4.1 b 步 physics.step；#030 落点解析：
        // 单拍 -9.8 从 10 落到 0.2，落地钳制收进半径线 0.5——脚贴地）
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
        assert!(e.pos().1 < 10.0, "重力应使实体下落");
        assert_eq!(e.pos(), (0.0, 0.5), "落到半径线（0.2 会穿地，被钳到 0.5）");
    }

    #[test]
    fn tick_applies_pending_then_physics() {
        // 集成：pending 的 SetPos 变更在 physics 之前应用（架构 4.1 阶段二 a 在 b 之前）
        // 实体初始在地面，pending 设 SetPos 到 (0,10)
        // - 若 pending 先于 physics：实体到 (0,10) 后受重力落点解析 -> (0,0.5)
        // - 若 pending 后于 physics：实体留在原地，再被 SetPos 覆盖到 (0,10)
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.5)));
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
        // SetPos 先应用 -> 重力落点解析到半径线 0.5（既非 10.0 也非 0.0）
        assert_eq!(y, 0.5, "pending 应在 physics 之前应用：预期 0.5，实际 {}", y);
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

    // === D-016 地形四件套收尾：装配链组合锚 ===

    #[test]
    fn d016_parse_set_terrain_step_entity_never_clips_underground() {
        // D-016 组合链（config 装配口径）：TerrainPreset::parse -> set_terrain ->
        // 一步 physics.step——mountain 世界实体落点不穿地（y >= ground(x) - 1e-9）。
        // 峰/谷/平段多点采样覆盖，谷底（负高度）也不陷入地形面
        let preset = codeworld_core::terrain::TerrainPreset::parse("mountain");
        assert_eq!(preset, codeworld_core::terrain::TerrainPreset::Mountain);
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(preset, 42));
        let engine = DefaultPhysicsEngine::new();
        let mut ids = Vec::new();
        for x in [-40.0f64, -10.0, 0.0, 10.0, 33.7] {
            let ground = world.terrain.height(x);
            let e = make_entity(x as u128, (x, ground + 0.5));
            ids.push(e.id);
            world.entities.insert(e.id, e);
        }
        engine.step(&mut world, &std::collections::HashSet::new());
        for id in ids {
            let e = world.entities.get(&id).unwrap();
            let (x, y) = e.pos();
            let ground = world.terrain.height(x);
            assert!(
                y >= ground - 1e-9,
                "实体落点 ({x},{y}) 不得穿地（ground={ground}）"
            );
        }
    }

    #[test]
    fn d016_mountain_world_one_tick_entity_not_underground() {
        // D-016 集成：mountain 世界一拍完整 tick（阶段一跳过无沙箱实体 ->
        // 阶段二物理步）——峰顶上空实体落点不穿地，贴合峰顶地面
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Mountain,
            42,
        ));
        let (peak_i, &peak_h) = world
            .terrain
            .samples()
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_x = codeworld_core::terrain::TERRAIN_X_MIN
            + peak_i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
        let e = make_entity(7, (peak_x, peak_h + 0.5));
        let id = e.id;
        world.entities.insert(id, e);
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
        let (x, y) = world.entities.get(&id).unwrap().pos();
        assert!(
            y >= world.terrain.height(x) - 1e-9,
            "tick 后落点 ({x},{y}) 不穿地（ground={}）",
            world.terrain.height(x)
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
        assert_eq!(loaded.entities.get(&id).unwrap().pos(), (3.0, 0.5), "实体站半径线（物理落位+序列化往返含 radius）");
    }

    // === handle_collisions（阶段二 c 步：接触触发沙箱通道，#021 接触记双方）===

    /// 构造接触对（测试辅助，固定 EntityId 便于断言；来源 touch=#030 边沿触发）
    fn make_contact_pair(a: u128, b: u128, pos: (f64, f64)) -> ContactPair {
        ContactPair {
            a: EntityId(Uuid::from_u128(a)),
            b: EntityId(Uuid::from_u128(b)),
            pos,
            source: "touch",
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
    fn handle_collisions_counts_pairs_from_detect_touching() {
        // 集成：两身体相触（#030 半径版）-> detect_touching 返回 1 对 -> handle_collisions 返回 1
        // 验证 detect -> handle 链路：物理引擎检测到的接触对原样传给接触处理
        let engine = DefaultPhysicsEngine::new();
        let mut entities = HashMap::new();
        let e1 = make_entity(1, (5.0, 5.0));
        let e2 = make_entity(2, (5.0, 5.0));
        entities.insert(e1.id, e1);
        entities.insert(e2.id, e2);
        let pairs = engine.detect_touching(&entities);
        assert_eq!(pairs.len(), 1, "重叠（同点）应检测到 1 对接触");
        let contacts: Vec<ContactPair> = pairs
            .into_iter()
            .map(|p| ContactPair {
                a: p.a,
                b: p.b,
                pos: p.pos,
                source: "touch",
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
        // #021 推挤退役回归（#030 半径版延续）：两实体同坐标（异常路径）→ tick 后
        // 坐标原样保留（仅落地钳制到半径线），无挤压位移。
        // （半径物理下正常路径不会再产生重叠：移动被挡/spawn 预检双闸，
        //   本测试直接构造异常重叠，验证即使发生也无推挤。）
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
        assert_eq!(pos1, (5.0, 0.5), "不推挤：实体1只被地面钳到半径线");
        assert_eq!(pos2, (5.0, 0.5), "不推挤：实体2只被地面钳到半径线");
    }

    #[test]
    fn tick_move_blocked_records_contact_and_stays() {
        // tick 级集成（#030）：移动者试图走进占位者身体 → 分轴滑动后留在原位侧；
        // 贴着占位者 = 身体相触，由阶段二 c 步接触扫描+边沿触发接管（#021 的
        // move_blocked 接触对来源退役）。
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
        // 目标 (6,0) 落地钳到 (6,0.5) 与占位者 (6,0.5) 重叠 → 滑 x 同挡 → 滑 y 到
        // (5,0.5) 不重叠 → 滑 y 落地站稳；下拍向东的尝试才会全挡贴墙
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (5.0, 0.5), "滑动裁决：滑 y 落地");
        assert_eq!(world.entities.get(&id2).unwrap().pos(), (6.0, 0.5), "占位者不受影响");
    }

    #[test]
    fn apply_blocked_move_full_block_stays() {
        // #030 全挡：目标与滑动替代位全被身体重叠 → 原地不动（不推挤不穿越）
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 5.0));
        let e2 = make_entity(2, (6.0, 5.0));
        let id1 = e1.id;
        world.entities.insert(id1, e1);
        world.entities.insert(e2.id, e2);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.0, 5.0),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(
            world.entities.get(&id1).unwrap().pos(),
            (5.0, 5.0),
            "整步/滑x/滑y 全被挡 → 原地"
        );
    }

    #[test]
    fn apply_blocked_move_slides_x() {
        // #030 滑 x：正北有身体、目标斜向东北被挡 → 沿 x 滑开（贴墙滑）
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 5.0));
        let e2 = make_entity(2, (5.0, 6.0));
        let id1 = e1.id;
        world.entities.insert(id1, e1);
        world.entities.insert(e2.id, e2);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (5.2, 5.9),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(
            world.entities.get(&id1).unwrap().pos(),
            (5.2, 5.0),
            "整步被挡 → 滑 x (5.2, 5.0) 不重叠"
        );
    }

    #[test]
    fn apply_blocked_move_slides_y() {
        // #030 滑 y：正东有身体、目标斜入其身 → 沿 y 滑开
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 5.0));
        let e2 = make_entity(2, (6.0, 5.0));
        let id1 = e1.id;
        world.entities.insert(id1, e1);
        world.entities.insert(e2.id, e2);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (5.9, 5.3),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(
            world.entities.get(&id1).unwrap().pos(),
            (5.0, 5.3),
            "整步被挡 → 滑 x (5.9,5.0) 距 0.5 仍挡 → 滑 y (5.0,5.3) 不重叠"
        );
    }

    #[test]
    fn apply_move_clamps_to_radius_line() {
        // #030 落地钳制：目标 y < radius → y = radius（脚贴地，圆心即位置）
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 5.0));
        let id1 = e1.id;
        world.entities.insert(id1, e1);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (6.0, 0.0),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (6.0, 0.5), "目标 (6,0) 钳到 (6,0.5)");
    }

    #[test]
    fn apply_free_move_no_block() {
        // 反向护栏：不重叠的目标照常应用
        let mut world = WorldState::new();
        let e1 = make_entity(1, (5.0, 5.0));
        let id1 = e1.id;
        world.entities.insert(id1, e1);
        let mut pending: Vec<PendingChange> = vec![PendingChange::SetPos {
            entity: id1,
            new_pos: (7.0, 5.0),
        }];
        apply_pending_changes(&mut world, &mut pending, None);
        assert_eq!(world.entities.get(&id1).unwrap().pos(), (7.0, 5.0));
    }

    // === 操控层 v0：tick 级意图路由（#022 操控优先于自主）===

    /// 自主东行者 WAT：每拍 move_by(+1,0)（模拟 pioneer 的挂机AI自主移动）
    const AUTO_MOVER_WAT: &str = r#"(module
  (import "env" "move_by" (func $move_by (param f64 f64) (result i32)))
  (memory (export "memory") 1)
  (func (export "on_tick") (drop (call $move_by (f64.const 1.0) (f64.const 0.0))))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 操控测试舞台：玩家 p1@(30,0.5)（半径线站位，#030 圆心即位置）挂指定 WAT 沙箱
    /// 并登记为 me 声明的操控目标（#024），
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
        let pid = make_entity_with_sandbox(&mut world, &mut map, 1, (30.0, 0.5), player_wat);
        // #025：代码身份对齐玩家名（处置账本按 code_hash 查表抑制 on_tick，
        // 生产里玩家实体 code_hash=玩家名，测试实体默认 "test" 需对齐）
        world.entities.get_mut(&pid).unwrap().code_hash = "p1".to_string();
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
            (29.0, 0.5),
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
            (30.0, 0.5),
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
            (31.0, 0.5),
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

    // === 下线处置权（#025 期五第一刀）：登出三选一 + 处置等待 + 重连认领 ===

    use net::ConnEvent;

    /// 给 map 注入本拍连接事件（WS 令牌翻转沿；生产由 main 循环每拍写入）
    fn set_conn(map: &mut EntitySandboxMap, ev: Option<ConnEvent>) {
        map.conn_event = ev;
    }

    #[test]
    fn logout_take_removes_entity_and_blocks_auto_respawn() {
        // A1 take：角色立即移除（kill 走本拍死亡清理链）+ 在场登记摘除 + 会话离场标记
        // ——文件锚仍在但 scan 不再自动重生（重新进场=重连认领解除标记）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![input::Intent::Logout {
            choice: dispose::LogoutChoice::Take,
        }];
        tick_with_intents(
            &mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map,
            &intents,
        );
        assert!(!world.entities.contains_key(&pid), "角色已移除（本拍死亡清理）");
        assert!(!map.players.is_online("p1"), "在场登记已摘除（目录锚同步）");
        assert!(map.players.is_absent("p1"), "会话离场标记在册");
        assert!(!map.players.dispose.has("p1"), "take 无残留处置状态");
        // 下一拍：文件锚仍在，但离场标记阻止自动重生（重生=重新进场，不是 scan 顺手捞回）
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(!map.players.is_online("p1"), "离场标记阻止 scan 自动重生");
    }

    #[test]
    fn logout_take_then_reclaim_respawns_player() {
        // 重新进场闭环：take 离场 -> 重连认领解除标记 -> 目录扫描重生角色 -> 恢复可操控
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, _pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let logout = vec![input::Intent::Logout {
            choice: dispose::LogoutChoice::Take,
        }];
        tick_with_intents(
            &mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map,
            &logout,
        );
        set_conn(&mut map, Some(ConnEvent::Claimed));
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(!map.players.is_absent("p1"), "重连认领解除离场标记（重新进场）");
        assert!(map.players.is_online("p1"), "目录扫描重生角色（新实体进场）");
        // 时序：认领拍先重生（d0 在控制解析之后），控制解析下一拍才拿到新实体
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(map.ctrl_exclusive_active, "me 声明恢复，操控独占重建");
    }

    #[test]
    fn logout_stay_auto_resumes_autonomy_same_tick() {
        // A1 stay_auto：处置落地拍 on_tick 即恢复（pioneer 东行一步）——
        // headless 冒烟等价物：move_by 队列重现=battle.log 自主恢复证据；
        // 字幕 ctrl 段从字形切到 auto
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![input::Intent::Logout {
            choice: dispose::LogoutChoice::StayAuto,
        }];
        tick_with_intents(
            &mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.5),
            "stay_auto 落地拍自主 on_tick 恢复（东行一步，30->31）"
        );
        assert_eq!(
            map.players.dispose.get("p1"),
            Some(dispose::DisposeState::Auto),
            "处置状态登记为 auto"
        );
        assert!(!map.ctrl_exclusive_active, "操控独占释放");
        assert!(
            renderer.caption_line().contains("ctrl:auto"),
            "字幕 ctrl 段切自主接管：{}",
            renderer.caption_line()
        );
    }

    #[test]
    fn logout_stay_ai_stands_still_registered() {
        // A1 stay_ai：角色留存+登记 AI 待接入（本刀只立状态：站桩，on_tick 抑制，
        // 外部 AI 端点协议是 #014 未来端点不实现）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![input::Intent::Logout {
            choice: dispose::LogoutChoice::StayAi,
        }];
        tick_with_intents(
            &mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map,
            &intents,
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (30.0, 0.5),
            "stay_ai 站桩（on_tick 抑制，等 AI 接入）"
        );
        assert_eq!(
            map.players.dispose.get("p1"),
            Some(dispose::DisposeState::AiAwait),
            "处置状态登记为 ai-await"
        );
        assert!(
            renderer.caption_line().contains("ctrl:ai-await"),
            "字幕 ctrl 段显示 AI 待接入：{}",
            renderer.caption_line()
        );
    }

    #[test]
    fn disconnect_starts_wait_reconnect_cancels_and_restores_control() {
        // A2 处置窗口：意外断开（非主动登出）-> 等待 30 拍（on_tick 抑制=站着等，
        // 照常在场）；重连=取消处置+操控独占恢复（A3 重连抢回）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        set_conn(&mut map, Some(ConnEvent::Released));
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
            map.players.dispose.get("p1"),
            Some(dispose::DisposeState::Wait { deadline_tick: 31 }),
            "断开拍进入处置等待（当前拍1 + 30拍窗口）"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (30.0, 0.5),
            "等待期 on_tick 抑制（站着不动等处置，非残影）"
        );
        assert!(world.entities.get(&pid).unwrap().is_alive(), "等待期角色活着在场");
        // 重连：取消处置，操控恢复
        set_conn(&mut map, Some(ConnEvent::Claimed));
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(!map.players.dispose.has("p1"), "重连取消处置");
        assert!(map.ctrl_exclusive_active, "操控独占恢复");
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (30.0, 0.5));
    }

    #[test]
    fn wait_timeout_defaults_to_take() {
        // A2 超时：30 拍未选择 = 默认 take（带走）+ 离场标记
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        set_conn(&mut map, Some(ConnEvent::Released));
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        ); // 拍1 进入等待，deadline=31
        for _ in 0..29 {
            tick(
                &mut world,
                &mut time,
                &physics,
                &mut renderer,
                &mut storage,
                &mut pending,
                &mut map,
            );
        }
        assert!(
            world.entities.contains_key(&pid),
            "拍 30 仍未到期（deadline 31），角色还在"
        );
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        ); // 拍 31 到期清算
        assert!(!world.entities.contains_key(&pid), "等待超时默认 take（角色移除）");
        assert!(map.players.is_absent("p1"), "超时带走=会话离场");
        assert!(!map.players.dispose.has("p1"), "清算后无残留");
    }

    #[test]
    fn wait_entity_can_be_attacked_and_killed_disposition_cleared() {
        // A2/A3：等待期角色照常可被攻击可被杀（世界不等人，存在即暴露）；
        // 被杀≠会话离场（无离场标记），死亡清理把处置状态一并清
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        set_conn(&mut map, Some(ConnEvent::Released));
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(map.players.dispose.has("p1"), "前置：处于处置等待");
        // 攻击者对 P 出空 payload 刀（默认击杀，#031 点命中制：打 P 坐标；#034 贴身 1.0）
        let attacker = make_entity_with_sandbox(&mut world, &mut map, 7, (31.0, 0.5), PLAYER_WAT);
        let attacks = vec![PendingAttack::strike(attacker, (30.0, 0.5), vec![])];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(
            !world.entities.get(&pid).unwrap().is_alive(),
            "等待期角色可被击杀（不无敌）"
        );
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(!world.entities.contains_key(&pid), "死亡清理移除实体");
        assert!(!map.players.dispose.has("p1"), "死亡清理把处置状态一并清");
        assert!(!map.players.is_absent("p1"), "被杀≠会话离场（离场标记只在登出 take/超时）");
    }

    #[test]
    fn logout_suppresses_disconnect_wait_same_tick() {
        // 边界：主动登出与令牌释放沿同拍到达（logout 入队即释放令牌）——
        // 登出处置落地，断开沿被抑制（不把处置等待叠加在登出之上）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        set_conn(&mut map, Some(ConnEvent::Released));
        let intents = vec![input::Intent::Logout {
            choice: dispose::LogoutChoice::StayAuto,
        }];
        tick_with_intents(
            &mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map,
            &intents,
        );
        assert_eq!(
            map.players.dispose.get("p1"),
            Some(dispose::DisposeState::Auto),
            "登出处置落地（stay_auto），未被等待覆盖"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.5),
            "自主恢复照常（无等待叠加）"
        );
    }

    #[test]
    fn stay_auto_reclaim_supersedes_takeover() {
        // A3 接管者被顶掉：stay_auto 后 me 重声明+重连=重新抢回操控
        //（操控权=me 声明+连接在，#024 精神——"我的角色我随时拿回"）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let logout = vec![input::Intent::Logout {
            choice: dispose::LogoutChoice::StayAuto,
        }];
        tick_with_intents(
            &mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map,
            &logout,
        );
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.5), "前置：自主东行");
        // 重连：接管者被顶掉，操控独占恢复，自主再次被抑制
        set_conn(&mut map, Some(ConnEvent::Claimed));
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(!map.players.dispose.has("p1"), "auto 处置被顶掉清账");
        assert!(map.ctrl_exclusive_active, "操控独占恢复（抢回）");
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.5),
            "抢回拍 on_tick 抑制（P 站定听指挥）"
        );
    }

    #[test]
    fn file_delete_during_disposition_yields_to_ghost_path() {
        // 两条下线路径不互相踩：处置等待中文件锚被删（账号级下线）——
        // 处置状态让位残影旧路（#013），不互相覆盖
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, me_dir) =
            control_stage(AUTO_MOVER_WAT);
        set_conn(&mut map, Some(ConnEvent::Released));
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(map.players.dispose.has("p1"), "前置：处置等待中");
        std::fs::remove_file(me_dir.join("p1.wat")).unwrap();
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert!(!map.players.dispose.has("p1"), "处置状态让位（文件锚路径接管）");
        assert!(map.players.is_ghost(&pid), "文件锚删除走残影旧路（#013 语义不变）");
    }

    #[test]
    fn no_logout_no_event_disposition_never_triggers() {
        // 回归锚：无登出无连接事件（纯终端模式/浏览器从未连过）——处置永不触发，
        // 操控独占照旧（#025 在线独占语义不被本刀扰动）
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
        assert!(!map.players.dispose.has("p1"), "无事件无处置");
        assert!(map.ctrl_exclusive_active, "操控独占照旧");
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (30.0, 0.5));
    }

    #[test]
    fn control_move_blocked_by_occupancy_stays_in_place() {
        // 操控不豁免物理（宪法）：目标格被占 -> 操控移动被占位裁决挡下，实体留原位
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        // 占据者：无沙箱普通实体（非玩家）放在西邻位
        world.entities.insert(
            EntityId(Uuid::from_u128(9)),
            make_entity(9, (29.0, 0.5)),
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
            (30.0, 0.5),
            "操控移动被碰撞裁决挡下（贴墙滑 y 落半径线站住）"
        );
        assert_eq!(
            world.entities.get(&EntityId(Uuid::from_u128(9))).unwrap().pos(),
            (29.0, 0.5),
            "占据者不受影响"
        );
    }

    #[test]
    fn control_attack_kills_nearest_visible_enemy() {
        // 空格 = 对最近可见敌人出刀（复用 scan 语义与 find_nearest_enemy 同源）；
        // 操控刀走既有两阶段结算 -> 击杀 + 掠夺 1 生存点数（操控不豁免物理）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let victim = make_entity_with_sandbox(&mut world, &mut map, 2, (31.0, 0.0), TARGET_WAT);
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
            (31.0, 0.5),
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
            (34.0, 0.5),
            "声明的 p2 被操控西移（on_tick 被抑制，仅操控位移）"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.5),
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
            (31.0, 0.5),
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
            (31.0, 0.5),
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
        //（30 -> 31 -> 32 到达；之后不再抖动，持续目标清除。#030：实体站半径线，
        // 点击同高地面坐标 (32,0.5)，矢量步=正东）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let walk = vec![input::Intent::WsMove { x: 32.0, y: 0.5 }];
        let none: Vec<input::Intent> = vec![];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &walk);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.5), "拍1 向点击目标走一步");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (32.0, 0.5), "拍2 续走到达");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (32.0, 0.5),
            "拍3 到达停住（目标清除，格距<=0.5 判定）"
        );
        assert!(map.ws_move_target.is_none(), "到达后持续目标清除");
    }

    #[test]
    fn ws_stop_cancels_walk_and_stands() {
        // #027 stop 全链路：走位中收到 Stop -> 清目标站住，后续空拍也不复活走位
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let walk = vec![input::Intent::WsMove { x: 35.0, y: 0.5 }];
        let stop = vec![input::Intent::Stop];
        let none: Vec<input::Intent> = vec![];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &walk);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.5), "走位中");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &stop);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.5), "Stop 拍站住");
        assert!(map.ws_move_target.is_none(), "Stop 清除持续目标");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.5), "目标已清，不复活走位");
    }

    // === 期四第七刀：贴障停滞修复（2 拍净位移窗口 + 到达幂等）===

    #[test]
    fn ws_move_blocked_by_body_stagnation_clears_target() {
        // 贴障修复主链路：P 点击走位被邻格实体挡死（贴站相切合法，#030）——
        // 分轴滑动全挡后位置纹丝不动，停滞检测清目标，不再逐拍蹭动；
        // 后续空拍不复活走位
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        // 阻挡者 @ (31,0.5)：与 P(30,0.5) 相切（距 1.0 = 2r，严格 < 不重叠，贴站合法）
        world
            .entities
            .insert(EntityId(Uuid::from_u128(9)), make_entity(9, (31.0, 0.5)));
        let walk = vec![input::Intent::WsMove { x: 33.0, y: 0.5 }];
        let none: Vec<input::Intent> = vec![];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &walk);
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (30.0, 0.5),
            "被挡拍：身体重叠分轴全挡，原地"
        );
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert!(
            map.ws_move_target.is_none(),
            "贴障停滞：目标自动清除（不逐拍蹭动）"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (30.0, 0.5),
            "停住不蹭动"
        );
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert!(map.ws_move_target.is_none(), "目标已清不复活");
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (30.0, 0.5));
    }

    #[test]
    fn ws_move_arrival_is_idempotent_no_step_after_clear() {
        // 到达清除幂等：踏上目标拍目标仍在册（快照语义下一拍判到达），
        // 到达判定拍清目标且同拍不再生成新步；后续拍零位移零目标
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let walk = vec![input::Intent::WsMove { x: 31.0, y: 0.5 }];
        let none: Vec<input::Intent> = vec![];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &walk);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.5), "终进近一步踏上目标");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert!(map.ws_move_target.is_none(), "到达判定拍清目标");
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.5), "到达拍不再生成新步");
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (31.0, 0.5), "后续拍零位移");
        assert!(map.ws_move_target.is_none(), "后续拍零目标");
    }

    /// 飞行测试便捷：连跑 n 拍空意图（爬升/悬停/降落的延续拍）
    #[allow(clippy::too_many_arguments)]
    fn fly_ticks(
        world: &mut WorldState,
        time: &mut TimeEngine,
        physics: &DefaultPhysicsEngine,
        renderer: &mut Renderer,
        storage: &mut StorageEngine,
        pending: &mut Vec<PendingChange>,
        map: &mut EntitySandboxMap,
        n: u32,
    ) {
        let none: Vec<input::Intent> = vec![];
        for _ in 0..n {
            tick_with_intents(world, time, physics, renderer, storage, pending, map, &none);
        }
    }

    // === 期四第五刀：操控飞行（#029 无税版）===

    #[test]
    fn controlled_flight_click_aerial_point_climbs_and_hovers() {
        // 第六刀主链路（#030 升力独立）：点空中 (30,3) -> 矢量步每拍 +1 爬升
        //（免重力集合承载升力，不再 +9.8 位移 hack）-> 进到达窗（格距≤0.5）
        // 转 hover 维持 -> 目标保留；悬停中 x 不动（独占抑制自主 on_tick）而 y 稳定
        //（免重力生效）——升力独立于移动的分层证明
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let fly = vec![input::Intent::WsMove { x: 30.0, y: 3.0 }];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &fly);
        let y1 = world.entities.get(&pid).unwrap().pos().1;
        assert!((y1 - 1.5).abs() < 1e-9, "首拍矢量爬升 1 格（0.5->1.5），实际 {y1}");
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 2);
        let y3 = world.entities.get(&pid).unwrap().pos().1;
        assert!((y3 - 2.5).abs() < 1e-9, "三拍进到达窗 y=2.5（距目标 0.5），实际 {y3}");
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert_eq!(
            map.flight_phase,
            Some(input::FlightPhase::Hover),
            "到位转悬停相位（状态沿）"
        );
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 3);
        let (px, py) = world.entities.get(&pid).unwrap().pos();
        assert!((py - 2.5).abs() < 1e-6, "悬停维持：免重力位置稳定（被挡≠坠落的机制本体），实际 {py}");
        assert_eq!(px, 30.0, "独占抑制自主 on_tick（AUTO_MOVER 的 +1 东移没跑）");
        assert!(map.ws_move_target.is_some(), "悬停目标保留（新意图才改变）");
    }

    #[test]
    fn controlled_hover_attack_does_not_drop() {
        // 审查 P1（#030 升力独立版）：悬停中出刀不掉——空中出刀实体仍进免重力
        // 集合（升力独立于行动类型），位置稳定不坠落（旧 +9.8 喷气补偿 hack 退役）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let fly = vec![input::Intent::WsMove { x: 30.0, y: 3.0 }];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &fly);
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 5);
        assert_eq!(map.flight_phase, Some(input::FlightPhase::Hover), "前置：悬停已建立");
        let strike = vec![input::Intent::WsAttack { x: 40.0, y: 0.0 }];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &strike);
        let y = world.entities.get(&pid).unwrap().pos().1;
        assert!((y - 2.5).abs() < 1e-6, "悬停中出刀位置稳定不坠落，实际 {y}");
        assert_eq!(map.flight_phase, Some(input::FlightPhase::Hover), "出刀不断气（相位保持）");
    }

    #[test]
    fn controlled_death_stops_hover_jet_and_clears_flight_state() {
        // 操控死亡：意图侧状态清理（flight_phase/ws_move_target 清零 = 喷气停止），
        // 实体移除；坠落/尸体由物理与渲染既有路径接管（#029 中断即坠落）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let fly = vec![input::Intent::WsMove { x: 30.0, y: 3.0 }];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &fly);
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 4);
        assert_eq!(
            map.flight_phase,
            Some(input::FlightPhase::Hover),
            "前置：悬停已建立"
        );
        world.entities.get_mut(&pid).unwrap().kill();
        let none: Vec<input::Intent> = vec![];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &none);
        assert!(!world.entities.contains_key(&pid), "死亡实体被清理");
        assert_eq!(map.flight_phase, None, "飞行相位清零（喷气停止）");
        assert_eq!(map.ws_move_target, None, "悬停/走位目标清零");
    }

    #[test]
    fn controlled_stop_in_air_hovers_not_falls() {
        // 空中 stop：悬停不坠（stop 语义 = 停止移动不是坠落；要落地玩家点地面）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let fly = vec![input::Intent::WsMove { x: 30.0, y: 3.0 }];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &fly);
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 4);
        let stop = vec![input::Intent::Stop];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &stop);
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 2);
        let (px, py) = world.entities.get(&pid).unwrap().pos();
        assert!((py - 2.5).abs() < 1e-6, "stop 后悬停不坠，实际 {py}");
        assert_eq!(px, 30.0, "停止水平移动");
        assert_eq!(map.flight_phase, Some(input::FlightPhase::Hover), "stop 空中转悬停相位");
        let mt = map.ws_move_target.expect("stop 后悬停锚点在册");
        assert_eq!(mt.0, pid);
        assert!((mt.1 .1 - 2.5).abs() < 1e-6, "悬停锚点 = 脚下坐标");
    }

    #[test]
    fn controlled_click_ground_from_air_lands_and_walks() {
        // 悬停中点地面：矢量步斜向受控降落（#030 L 形废除——直奔落点不垂直优先），
        // 终进近踏上点击点（y 落地钳制收进半径线），到达清除 + 退出飞行相位
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let fly = vec![input::Intent::WsMove { x: 30.0, y: 3.0 }];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &fly);
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 4);
        assert_eq!(map.flight_phase, Some(input::FlightPhase::Hover), "前置：悬停已建立");
        let land = vec![input::Intent::WsMove { x: 34.0, y: 0.0 }];
        tick_with_intents(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, &land);
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 5);
        let (lx, ly) = world.entities.get(&pid).unwrap().pos();
        let d2 = (lx - 34.0) * (lx - 34.0) + ly * ly;
        assert!(d2 <= 0.25 + 1e-9, "矢量降落进到达窗（距点击点 ≤0.5），实际 ({lx},{ly})");
        fly_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 2);
        let (wx, wy) = world.entities.get(&pid).unwrap().pos();
        assert!((wx - 34.0).abs() < 1e-9, "终进近踏上点击点 x，实际 {wx}");
        assert!((wy - 0.5).abs() < 1e-9, "落地钳制站半径线（脚贴地），实际 {wy}");
        assert!(map.ws_move_target.is_none(), "地面目标到达清除");
        assert_eq!(map.flight_phase, None, "落地退出飞行相位（重力接管）");
    }

    #[test]
    fn ws_attack_strikes_clicked_coords_same_pipeline() {
        // #027 点击出刀全链路：对点击坐标出刀（服务器以坐标找人），与键盘空格同一
        // 两阶段结算通路——击杀 + 掠夺，操控不豁免物理
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let victim = make_entity_with_sandbox(&mut world, &mut map, 2, (31.0, 0.0), TARGET_WAT);
        let intents = vec![input::Intent::WsAttack { x: 31.0, y: 0.0 }];
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
        assert_eq!(world.entities.get(&pid).unwrap().pos(), (29.0, 0.5), "操控中");
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
            (29.0, 0.5),
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
            .find(|e| e.code_hash == "p1" && e.id != pid)
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
            (31.0, 0.5),
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
        let attacks = vec![PendingAttack::strike(b, (0.0, 0.0), vec![9])];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(world.entities.get(&a).unwrap().is_alive(), "防守失败 -> 刀作废，A 应存活");
        // D-017：防守失败作废的刀不进命中事件广播
        assert!(map.attack_events.is_empty(), "刀作废不应记命中事件");
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

    // === D-017 协议小刀：攻击命中事件（攻方/目标ID/目标位置广播）===

    #[test]
    fn attack_events_recorded_on_hit_stun_and_cleared_per_tick() {
        // 击晕（命中未杀）也记事件——动画选槽只关心砍没砍中，不关心死没死；
        // 位置取结算时刻实况坐标（目标站在哪就记哪，与攻击点击坐标无关）；
        // 缓冲一拍生命周期：tick ①0 清空，下拍落空后缓冲为空
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker =
            make_entity_with_sandbox(&mut world, &mut map, 1, (4.0, 0.5), PLAYER_WAT);
        let target =
            make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.5), TARGET_WAT);
        // 击晕 payload：life()=true（换血成功但目标不死）
        let stun = wasm(&ver_module(111));
        let attacks = vec![PendingAttack::strike(attacker, (5.0, 0.5), stun)];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(world.entities.get(&target).unwrap().is_alive(), "击晕：目标存活");
        assert_eq!(map.attack_events.len(), 1, "击晕也算命中，应记事件");
        assert_eq!(map.attack_events[0].target_pos, (5.0, 0.5), "目标位置=实况坐标");
        // 攻击点击坐标 ≠ 目标身体中心时，事件仍记实况坐标（D-017：位置是选槽依据）
        map.attack_events.clear(); // 模拟 tick ①0（下一拍缓冲清空）
        let attacks2 = vec![PendingAttack::strike(attacker, (5.2, 0.9), wasm(&kill_ver_module(222)))];
        process_attacks(&mut world, &mut map, &attacks2);
        assert_eq!(map.attack_events.len(), 1, "第二刀（击杀）也记事件");
        assert_eq!(
            map.attack_events[0].target_pos, (5.0, 0.5),
            "事件位置=目标实况圆心，非点击坐标 (5.2,0.9)"
        );
    }

    #[test]
    fn attack_events_not_recorded_for_voided_or_dead_attacker() {
        // 攻击者已死的刀整刀作废（收件阶段 voided）——不记事件；
        // 结算阶段目标先死后续刀跳过——不记事件。广播里只有真砍中的刀
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker =
            make_entity_with_sandbox(&mut world, &mut map, 1, (4.0, 0.0), PLAYER_WAT);
        let target =
            make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.0), TARGET_WAT);
        // 攻击者先死：刀入队后攻击者被标记死亡
        let attacks = vec![PendingAttack::strike(attacker, (5.0, 0.0), vec![])];
        world.entities.get_mut(&attacker).unwrap().kill();
        process_attacks(&mut world, &mut map, &attacks);
        assert!(map.attack_events.is_empty(), "死者之刀作废，不记事件");
        // 目标先死：结算前目标已死（deliver 仍找到目标但 settle 跳过）——构造：
        // 目标已被上一刀击杀标记死亡，第二刀投递时 find_hit_target 过滤死者
        // -> 落空路径，同样不记事件
        world.entities.get_mut(&target).unwrap().kill();
        let attacks2 = vec![PendingAttack::strike(attacker, (5.0, 0.0), vec![])];
        // 攻击者复活回来出第二刀（kill 标记不可逆，用新攻击者）
        let attacker2 =
            make_entity_with_sandbox(&mut world, &mut map, 3, (4.5, 0.0), PLAYER_WAT);
        let attacks3 = vec![PendingAttack::strike(attacker2, (5.0, 0.0), vec![])];
        process_attacks(&mut world, &mut map, &attacks2);
        process_attacks(&mut world, &mut map, &attacks3);
        assert!(map.attack_events.is_empty(), "目标已死，刀落空不记事件");
    }

    #[test]
    fn process_attacks_miss_when_no_entity_at_pos() {
        // 攻击落空（验收#5）：坐标无存活实体 -> 不崩溃、不击杀
        // D-017：落空不进命中事件广播（落空提示是前端本地预告，不是世界事实）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker =
            make_entity_with_sandbox(&mut world, &mut map, 1, (0.0, 0.0), PLAYER_WAT);
        let attacks = vec![PendingAttack::strike(attacker, (99.0, 99.0), vec![])];
        process_attacks(&mut world, &mut map, &attacks);
        assert_eq!(world.entities.len(), 1, "落空不应移除实体");
        assert!(world.entities.contains_key(&attacker));
        assert!(map.attack_events.is_empty(), "落空不应记命中事件");
    }

    #[test]
    fn process_attacks_kills_and_plunders() {
        // 击杀+掠夺（验收#4）：空 payload 默认击杀 -> 目标死亡 + 从死者转移 1 生存点数给攻击者。
        // #034 接触门：攻击者必须贴身（dist 1.0 < r+r+0.5）才砍得着
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker =
            make_entity_with_sandbox(&mut world, &mut map, 1, (4.0, 0.0), PLAYER_WAT);
        let target =
            make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.0), TARGET_WAT);
        assert_eq!(world.entities.get(&attacker).unwrap().survival.points(), 3);
        let attacks = vec![PendingAttack::strike(attacker, (5.0, 0.0), vec![])];
        process_attacks(&mut world, &mut map, &attacks);
        // D-017 协议小刀：结算成功（击杀）写一条命中事件——攻方/目标/结算时刻坐标
        assert_eq!(map.attack_events.len(), 1, "击杀应记一条命中事件");
        let ev = &map.attack_events[0];
        assert_eq!(ev.attacker, attacker, "事件攻方=攻击者");
        assert_eq!(ev.target, target, "事件目标=被击杀者");
        assert_eq!(ev.target_pos, (5.0, 0.0), "目标位置=结算时刻实况坐标");
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
            make_entity_with_sandbox(&mut world, &mut map, 1, (4.0, 0.0), PLAYER_WAT);
        let target =
            make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.0), DEFENDED_TARGET_WAT);
        let attacks = vec![PendingAttack::strike(attacker, (5.0, 0.0), vec![])];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(world.entities.get(&target).unwrap().is_alive(), "拦截后目标应存活");
        // D-017：被拦截的刀不进命中事件广播（拦截=防守成功，世界不替攻方宣布"打中了"）
        assert!(map.attack_events.is_empty(), "拦截不应记命中事件");
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
        let attacks = vec![PendingAttack::strike(attacker, (5.0, 0.0), vec![])];
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
        let a = make_entity_with_sandbox(&mut world, &mut map, 1, (4.0, 0.0), PLAYER_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.0), TARGET_WAT);
        let attacks = vec![
            PendingAttack::strike(a, (5.0, 0.0), vec![]), // A -> B：默认击杀（贴身 1.0）
            PendingAttack::strike(b, (4.0, 0.0), vec![]), // B -> A：默认击杀（B 死后应作废）
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
        let attacks = vec![PendingAttack::strike(dead, (0.0, 0.0), vec![])];
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
        let b = make_entity_with_sandbox(&mut world, &mut map, 31, (1.0, 0.0), PLAYER_WAT);
        let c = make_entity_with_sandbox(&mut world, &mut map, 32, (0.0, 1.0), PLAYER_WAT);
        let attacks = vec![
            PendingAttack::strike(b, (0.0, 0.0), vec![9]),
            PendingAttack::strike(c, (0.0, 0.0), vec![9]),
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
        let b = make_entity_with_sandbox(&mut world, &mut map, 31, (1.0, 0.0), PLAYER_WAT);
        let c = make_entity_with_sandbox(&mut world, &mut map, 32, (0.0, 1.0), PLAYER_WAT);
        let c_payload = wasm(&ver_module(3001));
        let attacks = vec![
            PendingAttack::strike(b, (0.0, 0.0), vec![1, 1, 1, 1]),
            PendingAttack::strike(c, (0.0, 0.0), c_payload),
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
        let b = make_entity_with_sandbox(&mut world, &mut map, 31, (1.0, 0.0), PLAYER_WAT);
        let c = make_entity_with_sandbox(&mut world, &mut map, 32, (0.0, 1.0), PLAYER_WAT);
        let attacks = vec![
            PendingAttack::strike(c, (0.0, 0.0), wasm(&ver_module(3001))),
            PendingAttack::strike(b, (0.0, 0.0), wasm(&ver_module(2001))),
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
        let b = make_entity_with_sandbox(&mut world, &mut map, 31, (1.0, 0.0), PLAYER_WAT);
        let c = make_entity_with_sandbox(&mut world, &mut map, 32, (0.0, 1.0), PLAYER_WAT);
        let attacks = vec![
            PendingAttack::strike(c, (0.0, 0.0), wasm(&ver_module(3001))),
            PendingAttack::strike(b, (0.0, 0.0), wasm(&kill_ver_module(2002))),
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
        let b = make_entity_with_sandbox(&mut world, &mut map, 41, (1.0, 0.0), PLAYER_WAT);
        let attacks = vec![
            PendingAttack::strike(b, (0.0, 0.0), vec![9]),
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
    fn spy_hovers_stable_at_spawn_every_tick() {
        // 悬浮间谍（期四第七刀迁移）：S 每拍 hover() 声明悬停（免重力集合），
        // move_by(0,+9.8) 喷气 hack 退役——坐标全程精确稳定 (25,12)，
        // 拍内不再有 +9.8/-9.8 弹跳（阶段一快照与阶段二结算同坐标，
        // D 按快照坐标出刀点在 S 身体内必中，#031 点命中制）。
        // 跑 8 拍（含侦察 4 倍数拍与 1/2/3 非侦察拍）；api_calls 有 hover 无 move_by。
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
            let s = world
                .entities
                .get(&id)
                .unwrap_or_else(|| panic!("第 {tick_n} 拍 S 应存活"));
            assert_eq!(
                s.pos(),
                (25.0, 12.0),
                "第 {tick_n} 拍 S 应稳定悬停 (25,12)（hover 免重力，不弹跳）"
            );
        }
        let handle = sandbox_map.sandboxes.get(&id).unwrap();
        assert!(
            handle.store.data().api_calls.contains(&"hover".to_string()),
            "S 应调用 hover 声明悬停"
        );
        assert!(
            !handle.store.data().api_calls.contains(&"move_by".to_string()),
            "S 不应再调用 move_by（喷气 hack 退役）"
        );
    }

    #[test]
    fn d_blade_at_stable_spy_point_intercepted_by_shield() {
        // 第七刀锚的第八刀续篇：S 坐标稳定 (25,12) 后（hover 免重力，拍内无弹跳），
        // D 按阶段一快照坐标出刀——打点 (25,12) 距 S 圆心 0 点在身体内
        // （#031 点命中制）必中；第八刀起 S 有替身盾，首刀被 on_attack 拦截
        // （S 存活 + 盾 3→2），"命中"语义不变、"击杀"由盾耗尽后的刀完成
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let spy = make_entity_with_sandbox(&mut world, &mut map, 7, (25.0, 12.0), SPY_WAT);
        // #034 贴身刀：出刀者须与 S 相触（dist 0.9 < r+r+0.5）——远距刀已物理落空
        let d = make_entity_with_sandbox(&mut world, &mut map, 2, (25.9, 12.0), PLAYER_WAT);
        let attacks = vec![PendingAttack::strike(d, (25.0, 12.0), vec![])];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(
            world.entities.get(&spy).unwrap().is_alive(),
            "首刀应被替身盾拦截（S 存活，盾 3→2）"
        );
        assert_eq!(
            call_i32_export(&mut map, &spy, "get_shield"),
            2,
            "首刀拦截后替身盾应剩 2"
        );
    }

    #[test]
    fn spy_shield_intercepts_three_then_fourth_kills() {
        // 第八刀主体：S 的替身盾（on_attack 拦截，人设"你砍中的可能不是我"）——
        // 前 3 刀全拦截（S 存活 + 盾 3→0 逐刀递减 + marker 11 替身台词触发源 ×3），
        // 第 4 刀盾尽放行进入正常结算（S 回到脆皮被击杀，与 D 的盾尽放行同语义）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let spy = make_entity_with_sandbox(&mut world, &mut map, 7, (25.0, 12.0), SPY_WAT);
        // #034 贴身刀：出刀者贴身 S（dist 0.9）才砍得着
        let d = make_entity_with_sandbox(&mut world, &mut map, 2, (25.9, 12.0), PLAYER_WAT);
        let blade_at_spy = |attacker: EntityId| {
            vec![PendingAttack::strike(attacker, (25.0, 12.0), vec![])]
        };
        for round in 1..=3 {
            process_attacks(&mut world, &mut map, &blade_at_spy(d));
            assert!(
                world.entities.get(&spy).unwrap().is_alive(),
                "第 {round} 刀应被替身盾拦截，S 存活"
            );
            assert_eq!(
                call_i32_export(&mut map, &spy, "get_shield"),
                3 - round,
                "第 {round} 刀拦截后替身盾应剩 {}",
                3 - round
            );
        }
        assert_eq!(
            call_i32_export(&mut map, &spy, "get_intercepts"),
            3,
            "替身盾应记录 3 次拦截"
        );
        // 替身台词触发源：3 次拦截 = marker 11 ×3（host 据此打"S: 那只是我的替身"）
        let markers = map.collect_all_log_markers();
        assert_eq!(
            markers
                .iter()
                .filter(|&&m| m == SPY_INTERCEPT_MARKER)
                .count(),
            3,
            "3 次拦截应发 3 个替身 marker（battle.log 台词触发源）"
        );
        // 第 4 刀：盾尽放行（on_attack 返回 0），正常结算击杀
        process_attacks(&mut world, &mut map, &blade_at_spy(d));
        assert!(
            !world.entities.get(&spy).unwrap().is_alive(),
            "盾尽后第 4 刀应击杀 S（回到脆皮）"
        );
    }

    // === env.hover 悬停声明全链路（期四第七刀：玩家代码声明 -> 免重力集合）===

    /// 常悬停 WAT：on_tick 每拍 hover()（S 迁移后的行为对照锚）
    const ALWAYS_HOVER_WAT: &str = r#"(module
  (import "env" "hover" (func $hover))
  (func (export "on_tick") call $hover)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 隔拍悬停 WAT：奇数拍 hover、偶数拍不声明（验证声明不跨拍延续）
    const EVEN_TICK_HOVER_WAT: &str = r#"(module
  (import "env" "hover" (func $hover))
  (global $ticks (mut i32) (i32.const 0))
  (func (export "on_tick")
    global.get $ticks i32.const 1 i32.add global.set $ticks
    (if (i32.rem_u (global.get $ticks) (i32.const 2))
      (then call $hover)))
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn hover_declaration_lifts_entity_against_gravity() {
        // 悬停声明接入免重力集合：空中实体每拍 hover() -> 物理免重力，
        // 坐标纹丝不动（无位移无坠落——与操控悬停同集合同机制）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(7));
        world.entities.insert(id, make_entity(7, (5.0, 12.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        sandbox_map
            .create(id, &wasm(ALWAYS_HOVER_WAT), 1_000_000)
            .unwrap();
        for tick_n in 1..=3 {
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
                (5.0, 12.0),
                "第 {tick_n} 拍悬停实体坐标应纹丝不动（免重力）"
            );
        }
    }

    #[test]
    fn hover_declaration_does_not_persist_across_ticks() {
        // 每拍重申语义：偶数拍不声明 -> 该拍重力照扣（12 -> 2.2），声明不延续；
        // 奇数拍重新声明 -> 免重力停在当前高度（低空悬停同样成立）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(7));
        world.entities.insert(id, make_entity(7, (5.0, 12.0)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        sandbox_map
            .create(id, &wasm(EVEN_TICK_HOVER_WAT), 1_000_000)
            .unwrap();
        // 拍1（奇数拍，声明悬停）：纹丝不动
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
            (5.0, 12.0),
            "拍1 已声明悬停：免重力纹丝不动"
        );
        // 拍2（偶数拍，不声明）：重力照扣 12 - 9.8 ≈ 2.2（上拍声明不延续）
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        let y2 = world.entities.get(&id).unwrap().pos().1;
        assert!(
            (y2 - 2.2).abs() < 1e-9,
            "拍2 未声明：重力照扣应落到 2.2，实际 {y2}"
        );
        // 拍3（奇数拍，重新声明）：停在 2.2 低空悬停
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
            (5.0, y2),
            "拍3 重新声明：低空悬停停在当前高度"
        );
    }

    #[test]
    fn collect_hover_declarations_drains_flags() {
        // 收集即清空（不跨拍残留）：collect 后二次收集为空
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.0)));
        let mut map = test_sandbox_map();
        map.create(id, &wasm(ALWAYS_HOVER_WAT), 1_000_000).unwrap();
        map.run_player_code(&world, &unbounded_bb());
        assert_eq!(
            map.collect_hover_declarations(),
            vec![id],
            "应收集到该实体的悬停声明"
        );
        assert!(
            map.collect_hover_declarations().is_empty(),
            "收集后应清空（声明不跨拍残留）"
        );
    }

    // === thrust 冲量全链路（D-010a：import 排队 -> 阶段二应用 -> 物理弹道）===

    /// 一次性跳 WAT：on_tick 首拍 thrust(0, 7.7)，之后不再跳（弹道锚测试用）
    const JUMPER_ONE_SHOT_WAT: &str = r#"(module
  (import "env" "thrust" (func $thrust (param f64 f64) (result i32)))
  (global $done (mut i32) (i32.const 0))
  (func (export "on_tick")
    (if (i32.eqz (global.get $done))
      (then
        (global.set $done (i32.const 1))
        (drop (call $thrust (f64.const 0.0) (f64.const 7.7))))))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 记账用 thrust WAT：on_tick 调 thrust(0, 7.7)（fuel 扣除测试用）
    const THRUSTER_WAT_HOST: &str = r#"(module
  (import "env" "thrust" (func $thrust (param f64 f64) (result i32)))
  (func (export "on_tick")
    (drop (call $thrust (f64.const 0.0) (f64.const 7.7))))
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn apply_thrust_accumulates_velocity_clamps_and_skips_dead_or_static() {
        // 应用规则：v_new = v_old + impulse；超世界极速归一化压回；死实体跳过；
        // static 钉死造物拒绝（速度不写，fuel 照扣=诚实的代价）
        let mut world = WorldState::new();
        let a = make_entity(1, (0.0, 0.5));
        let id_a = a.id;
        world.entities.insert(id_a, a);
        let mut b = make_entity(2, (0.0, 0.5));
        b.set_velocity((20.0, 0.0));
        let id_b = b.id;
        world.entities.insert(id_b, b);
        let mut dead = make_entity(3, (0.0, 0.5));
        dead.kill();
        let id_dead = dead.id;
        world.entities.insert(id_dead, dead);
        // static 钉死造物（code_hash=creation + motion=static）
        let tower = Entity::new(
            EntityId(Uuid::from_u128(4)),
            PlayerId(Uuid::from_u128(99)),
            "creation".to_string(),
            (0.0, 0.0),
            Appearance::new("T".to_string(), "cyan".to_string(), 1.0),
            0,
            1_000_000,
        );
        let id_tower = tower.id;
        world.entities.insert(id_tower, tower);
        apply_thrust_requests(
            &mut world,
            vec![
                PendingThrust { entity: id_a, impulse: (0.0, 7.7) },
                PendingThrust { entity: id_b, impulse: (20.0, 0.0) },
                PendingThrust { entity: id_dead, impulse: (0.0, 7.7) },
                PendingThrust { entity: id_tower, impulse: (0.0, 7.7) },
                PendingThrust { entity: EntityId(Uuid::from_u128(999)), impulse: (1.0, 1.0) },
            ],
            25.0,
        );
        assert_eq!(
            world.entities.get(&id_a).unwrap().velocity(),
            (0.0, 7.7),
            "零速实体冲量后速度应为冲量本身"
        );
        assert_eq!(
            world.entities.get(&id_b).unwrap().velocity(),
            (25.0, 0.0),
            "累加超极速 (40,0) 应归一化压回 25（方向不变）"
        );
        assert_eq!(
            world.entities.get(&id_dead).unwrap().velocity(),
            (0.0, 0.0),
            "死实体不出力"
        );
        assert_eq!(
            world.entities.get(&id_tower).unwrap().velocity(),
            (0.0, 0.0),
            "static 钉死造物出力无效（速度不写）"
        );
    }

    #[test]
    fn collect_thrust_requests_deducts_fuel_and_drains() {
        // 收集即清空 + fuel 统一扣除：收集后 pending 清空、fuel_cost 归零、
        // 沙箱剩余 fuel = 原剩余 - 代价（满力 50_000 = 1_000_000 × 5%）
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.5)));
        let mut map = test_sandbox_map();
        map.create(id, &wasm(THRUSTER_WAT_HOST), 1_000_000).unwrap();
        map.run_player_code(&world, &unbounded_bb());
        let thrusts = map.collect_thrust_requests();
        assert_eq!(thrusts.len(), 1, "应收集 1 条冲量请求");
        assert_eq!(thrusts[0].entity, id, "冲量应绑定沙箱实体");
        let handle = map.sandboxes.get(&id).unwrap();
        let after_fuel = handle.store.get_fuel().unwrap();
        assert!(
            after_fuel <= 950_000,
            "满力出力应扣除 5% fuel（50_000），剩余 {after_fuel}"
        );
        assert_eq!(
            handle.store.data().thrust_fuel_cost, 0,
            "扣除后记账应归零（不跨拍残留）"
        );
        assert!(
            map.collect_thrust_requests().is_empty(),
            "收集后应清空（不跨调用残留）"
        );
    }

    #[test]
    fn thrust_jump_full_chain_ballistic_anchor() {
        // 跳跃弹道锚（D-010a Gate）：jumper WAT 首拍 thrust(0,7.7) 全链路——
        // 拍 1 升空 ≈3 格（±0.5 容差，逐拍位移 ≤ 极速 25 非瞬移），拍 2 落地
        // 贴半径线、速度全清零（vy 清零规则）。
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.5)));
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut sandbox_map = test_sandbox_map();
        sandbox_map
            .create(id, &wasm(JUMPER_ONE_SHOT_WAT), 1_000_000)
            .unwrap();
        // 拍 1：起跳（阶段二冲量入速度 → physics 速度积分带位移）
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        {
            let e = world.entities.get(&id).unwrap();
            let height = e.pos().1 - 0.5;
            assert!(
                (2.5..=3.5).contains(&height),
                "跳跃最高点应 ≈3 格（±0.5 容差），实测 {height}"
            );
            let rise = (e.pos().1 - 0.5).abs();
            assert!(rise <= 25.0, "起跳拍位移 ≤ 世界极速（非瞬移），实测 {rise}");
            assert!(e.velocity().1 < 0.0, "升空后重力已入速度（vy 转负回落）");
        }
        // 拍 2：落地清零
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut sandbox_map,
        );
        let e = world.entities.get(&id).unwrap();
        assert!(
            (e.pos().1 - 0.5).abs() < 1e-9,
            "拍 2 应落地贴半径线，实测 {:?}",
            e.pos()
        );
        assert_eq!(
            e.velocity(),
            (0.0, 0.0),
            "落地速度全清零（vy 清零规则：落地时清）"
        );
    }

    #[test]
    fn demo_jumper_wat_compiles_and_thrusts() {
        // players/jumper.wat 演示生物：语法可编译（wat::parse_str），on_tick 跑通
        // 并排队冲量（与生产加载路径同款 wat::parse_bytes，见 main.rs:594）
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/../../players/jumper.wat");
        let wat_text = std::fs::read_to_string(src).expect("应能读到 players/jumper.wat");
        let wasm_bytes = wat::parse_bytes(wat_text.as_bytes()).expect("jumper.wat 应编译通过");
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm_bytes.into_owned(), 100_000, EntityId(Uuid::from_u128(1)))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        // tick 1：jumper 的第 1 拍不跳（tick%4==0 才跳，global 从 0 起自增后 =1）
        // 造 tick%4==0 的场景：再跑 4 拍，第 4 拍应排队 1 条冲量
        let mut total = 0;
        for _ in 0..4 {
            mgr.call_on_tick(&mut handle).unwrap();
            total += handle.store.data().pending_thrusts.len();
            handle.store.data_mut().pending_thrusts.clear();
        }
        assert_eq!(total, 1, "4 拍内应恰好跳 1 次（每 4 拍一跳）");
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
        // 实体站半径线 (0,0.5)：物理无干扰（地面实体不受重力），tick 后应恰在 (1.0, 0.5)。
        let mut world = WorldState::new();
        let id = EntityId(Uuid::from_u128(1));
        world.entities.insert(id, make_entity(1, (0.0, 0.5)));
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
            (1.0, 0.5),
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
        // A 行为（#034 接触化后）：A 与 D 相向接近（D 也有接近逻辑，双向逼近），
        // 贴身（dist < 1.5）前 A 不出刀（D 拦截计数 0），贴身后刀到（拦截计数 >0）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, _a, d) =
            battle_stage(10);
        let mut prev_dist = 10.0f64;
        let mut reached = false;
        for step in 1..=15u64 {
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
            let pa = world.entities.get(&_a).map(|e| e.pos());
            let pd = world.entities.get(&d).map(|e| e.pos());
            let (Some(pa), Some(pd)) = (pa, pd) else {
                panic!("第 {step} tick 双方应都在场（贴身前互不出刀）");
            };
            let dist = ((pa.0 - pd.0).powi(2) + (pa.1 - pd.1).powi(2)).sqrt();
            if dist <= 1.5 {
                reached = true;
                break;
            }
            // 未贴身期间距离应单调不增（A 每拍朝 D 挪 1.0，D 也朝 A 挪——可能被身体挡停）
            assert!(
                dist <= prev_dist + 1e-9,
                "第 {step} tick 双方应互相逼近：{prev_dist} -> {dist}"
            );
            assert_eq!(
                call_i32_export(&mut map, &d, "get_intercepts"),
                0,
                "未贴身前 A 不出刀（距离不足的刀进不了防守询问），D 不应有拦截记录"
            );
            prev_dist = dist;
        }
        assert!(reached, "15 拍内应贴身（双向接近，间距 10 每拍缩 2）");
        run_ticks(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
            5,
        );
        assert!(
            call_i32_export(&mut map, &d, "get_intercepts") >= 1,
            "贴身后 A 的刀应到达 D 的 on_attack（拦截计数 >0）"
        );
    }

    #[test]
    fn battle_defender_counters_and_kills_attacker() {
        // 结局一（默认参数，盾=10）：A 与 D 相向接近贴身后互砍——A 的刀被盾逐刀拦截，
        // D 每 4 tick 反制，A 无 on_attack -> 反制刀命中即击杀、D 掠夺 1 点。
        // 全程走两阶段结算（拦截在收件、击杀在结算）。
        // #034 后时间线由接近速度决定（相向 2 格/拍，~t4 贴身、t8 前后收割），
        // 精确刀数随身体阻挡细节浮动，断言取行为锚（死/活/掠夺/拦过）不锚刀数。
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
        assert!(
            call_i32_export(&mut map, &d, "get_intercepts") >= 1,
            "贴身战后 D 应至少拦截过 A 一刀"
        );
        assert!(
            call_i32_export(&mut map, &d, "get_shield") < 10,
            "盾应被 A 的贴身刀消耗过（<10）"
        );
    }

    /// 冒烟辅助：世界里是否还有指定字形的实体
    fn glyph_alive(world: &WorldState, glyph: &str) -> bool {
        world.entities.values().any(|e| e.appearance.glyph == glyph)
    }

    #[test]
    fn smoke_default_viewport_four_bio_timeline_unchanged() {
        // 冒烟（#034 弹道时代的四生物剧本）：默认视口 50x25（对齐真实 Config），四生物
        // + E 类玩家 @(35,5)（PLAYER_WAT 同 players/_template.wat.txt：只 log 不动不 scan）。
        // 贴身刀+接近逻辑+H 抛射的新物理语义结局（行为锚，不锚精确刀数——
        // 接触拍与结算序随 EntityId 掷骰，#002 序确定、id 随机的老规矩）：
        // - A 与 D 相向接近（2 格/拍）~t4 贴身互砍，A 无防守被 D 反制收割，
        //   也可能提前被 H 的抛射弹收走 -> A 最迟 t12 必死
        // - D 不追悬停者（敌在头顶 2 格开外不追，塔防不下场追鸟）；H 的地面抛射
        //   弧顶 ~5 格也够不着 S@(25,12) -> S 全程存活：弹道时代悬停=真安全位，
        //   除非有人拼出对空弹道参数（#034 涌现空间）
        // - H 与 D 的地面纠缠必有接触（H 游走 ±0.5、D 逼近 1.0/拍，净闭合恒正），
        //   D 反制 4 拍一轮 vs H 的 50% 拦截 -> 80 拍内 D/H 至少倒一个
        // - D 若活下来：距离序清场会走到 E（地面目标）贴身收割 -> D 与 E 不共存
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
        assert!(glyph_alive(&world, "H"), "tick3 H 应还活着（D 尚未贴到任何敌人）");
        run_ticks(
            &mut world, &mut time, &physics, &mut renderer, &mut storage,
            &mut pending, &mut map, 9,
        );
        assert!(!glyph_alive(&world, "A"), "tick12 A 必死（贴身战 D 反制收割，或被 H 抛射弹提前收走）");
        run_ticks(
            &mut world, &mut time, &physics, &mut renderer, &mut storage,
            &mut pending, &mut map, 68,
        );
        assert!(glyph_alive(&world, "S"), "tick80 S 应存活（悬停=真安全位：D 不追空中、H 的地面弧线够不着 12 格高）");
        assert!(
            !glyph_alive(&world, "D") || !glyph_alive(&world, "H"),
            "tick80 D/H 至少倒一个（地面纠缠必有接触，反制 vs 50% 拦截必有胜负）"
        );
        assert!(
            glyph_alive(&world, "E"),
            "tick80 E 应存活（S 永久占据 D 的最近敌人槽：D 不追空中敌人就站在原地，\
             地面的 E 永远排不上号——悬停者的战略价值=冻结近战追击者，#034 物理诚实涌现）"
        );
    }

    #[test]
    fn smoke_seventh_cut_hover_stable_and_stagnation_clears() {
        // 第七刀验收冒烟（进程内 80 拍一局，等价 headless 实测；沙箱不能起进程时
        // 本测试即验收证据）：四方布阵 + 受控玩家 p1@35 + 地面阻挡者 obst@30，D 摘除。
        // a. S 坐标全程稳定 (25,12)：hover 免重力，无 12↔21.8 弹跳（全程逐拍断言）
        // c. P 点击走位贴障：p1 西行被 obst 挡死（31,0.5 全挡），2 拍窗口停滞
        //    检测自动清目标站住，不逐拍蹭动
        // 【为什么摘 D】第七刀首版此测试同时验证"D 破盾杀 S"，但 D 的清场点名序
        // 按随机 EntityId 结算：快路径（H 的 uuid 抢在 A/D 前）A 死 t4 -> obst t8
        // 即被点名 -> p1 无阻挡走满 5 拍到 (30,0.5)，贴障断言随 uuid 掷骰子
        // （基线实测 4 跑 2 挂的隐性 flaky）。停滞验证需要 obst 恒存活 -> D 缺席；
        // "D 破盾杀 S"由 smoke_default_viewport_four_bio_timeline_unchanged
        // （t28 破盾死锚，快慢分支确定性绿）+ spy_shield_intercepts_three_then_
        // fourth_kills（3 拦 1 破逐刀断言）承担。
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        spawn_test_entities(&mut world, &mut map, 10, true, true);
        let d_id = world
            .entities
            .values()
            .find(|e| e.code_hash == DEFENDER_CODE)
            .map(|e| e.id)
            .expect("四方布阵应有 D");
        map.remove(&d_id);
        world.entities.remove(&d_id);
        // 受控玩家 + 阻挡者：stems 字典序 obst < p1 -> obst@30 / p1@35（y 落半径线 0.5）
        let dir = temp_players_dir("seventh-cut");
        std::fs::write(dir.join("me.txt"), "p1").unwrap();
        std::fs::write(dir.join("obst.wat"), PLAYER_WAT).unwrap();
        std::fs::write(dir.join("p1.wat"), PLAYER_WAT).unwrap();
        map.players = PlayerDirectory::new(&dir);
        map.load_players_initial(&mut world).unwrap();
        let p1 = find_player_id(&world, "p1");
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = Renderer::new(Viewport::new((0.0, 0.0), 50.0, 25.0, 50, 80));
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let spy_id = world
            .entities
            .values()
            .find(|e| e.code_hash == SPY_CODE)
            .map(|e| e.id)
            .expect("四方布阵应有 S");
        // tick 1-9：无意图（p1 受控独占站桩；S 悬停逐拍稳定）
        let none: Vec<input::Intent> = vec![];
        let walk = vec![input::Intent::WsMove { x: 30.0, y: 0.5 }];
        // 停滞证据捕获：点击 tick10 -> 西行 4 拍（t13 到 31 贴障）-> 挡 t14 ->
        // 停滞清目标 t15 末 -> t15 末快照即贴障位（D 缺席后无人清场，确定性成立）
        let mut p1_frozen: Option<(f64, f64)> = None;
        let mut p1_target_cleared = false;
        for tick_n in 1..=80 {
            let intents = if tick_n == 10 { &walk } else { &none };
            tick_with_intents(
                &mut world, &mut time, &physics, &mut renderer, &mut storage,
                &mut pending, &mut map, intents,
            );
            // a. S 存活期内坐标恒 (25,12)（逐拍断言：无弹跳无飘移）
            if let Some(s) = world.entities.get(&spy_id) {
                assert_eq!(
                    s.pos(),
                    (25.0, 12.0),
                    "第 {tick_n} 拍 S 应稳定悬停 (25,12)（hover 免重力）"
                );
            }
            if tick_n == 15 {
                p1_frozen = world.entities.get(&p1).map(|e| e.pos());
            }
            if tick_n >= 15 && map.ws_move_target.is_none() {
                p1_target_cleared = true;
            }
        }
        // a 附：D 缺席后 S 全程存活，80 拍末坐标仍是 (25,12)（悬停不是坠落）
        assert_eq!(
            world.entities.get(&spy_id).expect("D 缺席 S 应存活").pos(),
            (25.0, 12.0),
            "tick80 S 仍稳定悬停 (25,12)"
        );
        // c. 贴障停滞：p1 西行到 31 被 obst@30 挡死，2 拍窗口停滞清目标站住
        assert_eq!(
            p1_frozen,
            Some((31.0, 0.5)),
            "p1 应停在贴障位 (31,0.5)（身体重叠全挡）"
        );
        assert!(
            p1_target_cleared,
            "贴障停滞：走位目标应已自动清除（不逐拍蹭动）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn smoke_shrunken_viewport_out_of_screen_unreachable() {
        // 冒烟（边界效应）：视口缩到 [-10,10]x[-10,10]，经典双生物 A@5/D@15
        // + E 类玩家 @(35,0)。感知诚实三路前置的屏幕语义：
        // - A 看不到屏外 D（@15 在 x>10）-> scan 返 0 站桩不动、永不出刀（D 盾不掉）
        // - D 能锁屏内 A@5 -> #034 接近逻辑走向 A，贴身后反制杀 A（~t12；屏内=看得见=合法目标，
        //   D 自己在屏外不妨碍）
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
        assert!(!glyph_alive(&world, "A"), "A 应被 D 贴身反制击杀（D 接近逻辑走向屏内的 A，15 拍内够贴身+反制）");
        assert!(glyph_alive(&world, "D"), "D 应存活（A 无防守，反制收割）");
        assert!(glyph_alive(&world, "E"), "E@35 屏外应全程存活（屏外不可锁定；D 杀完 A 后无屏内目标不再移动）");
        let d_id = world
            .entities
            .values()
            .find(|en| en.appearance.glyph == "D")
            .map(|en| en.id)
            .unwrap();
        assert!(
            call_i32_export(&mut map, &d_id, "get_shield") < 10,
            "D 盾应被消耗（#034 后 D 接近逻辑走进屏内——A 随即锁定它并出刀：\
             屏蔽语义是双向的，走进来=看得见=合法目标，感知诚实不受方向豁免）"
        );
        assert_eq!(
            world.entities.get(&e).unwrap().pos(),
            (35.0, 0.5),
            "E 应原地未动（无人锁定无人接触；落地钳半径线）"
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
        // tick 1：立塔拍——塔在 +5，P 不动（半径物理：P 落半径线 0.5，塔造物落地 y=0）
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert_eq!(world.entities.get(&p_id).unwrap().pos(), (30.0, 0.5), "立塔拍 P 不应移动（落地钳半径线）");
        let t = tower(&world).expect("立塔拍应有塔诞生");
        assert_eq!(t.pos(), (35.0, 0.0), "塔应立在 P 的 +5 格 (35,0)，造物贴地 y=0");
        assert!(map.sandboxes.contains_key(&t.id), "塔应有自己的沙箱（活造物）");
        // tick 2-5：每拍东移一步（走到塔西脸相切处：距塔面恰 0.5=半径）
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 4);
        assert_eq!(world.entities.get(&p_id).unwrap().pos(), (34.0, 0.5), "4 拍后 P 应走到塔前 (34,0.5)（贴塔脸）");
        assert_eq!(tower(&world).unwrap().pos(), (35.0, 0.0), "塔应原地站岗");
        // tick 6：P 试图进塔身体 → 碰撞裁决挡下（滑 x/y 均不行，原地贴墙，不穿塔）
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert_eq!(world.entities.get(&p_id).unwrap().pos(), (34.0, 0.5), "P 走进塔身应被挡在 (34,0.5)");
        assert_eq!(tower(&world).unwrap().pos(), (35.0, 0.0), "被挡后塔不位移（不推挤）");
        // 再跑 2 拍：P 持续被挡（贴着塔 = 接触边沿触发只报一次），塔纹丝不动
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 2);
        assert_eq!(world.entities.get(&p_id).unwrap().pos(), (34.0, 0.5), "持续被挡");
        assert_eq!(tower(&world).unwrap().pos(), (35.0, 0.0), "塔持续站岗");
        // 四生物战斗在 A/D 交接区（x~10）进行，与 P/T（x~30-35）互不干扰；
        // #034 后 D 不再远程点名（贴身刀），H 不在 tick4 被清——只锚 D 存活
        assert!(glyph_alive(&world, "D"), "D 应存活（战斗远离 P/T）");
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
        // 攻击方在 (4,0)（#034 贴身：距目标 1.0 判距内），on_tick 调 attack_at(5.0, 0.0, 0, 0)（空 payload 默认击杀）
        let attacker =
            make_entity_with_sandbox(&mut world, &mut sandbox_map, 1, (4.0, 0.0), PIPELINE_ATTACKER_WAT);
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

    // === #034 弹道系统：shoot 弹丸 + attack_at 接触化 ===

    /// 静态盾靶 WAT：不动、盾 3 拦截（观测 get_shield/get_intercepts）——弹丸拦截演示的靶子
    const SHOOT_TARGET_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (memory (export "memory") 1)
  (global $shield (mut i32) (i32.const 3))
  (global $intercepts (mut i32) (i32.const 0))
  (func (export "on_attack") (param i32) (result i32)
    global.get $shield
    if (result i32)
      global.get $shield i32.const 1 i32.sub global.set $shield
      global.get $intercepts i32.const 1 i32.add global.set $intercepts
      i32.const 7 call $log
      global.get $shield call $log
      i32.const 1
    else
      i32.const 0
    end)
  (func (export "life") (result i32) i32.const 1)
  (func (export "get_shield") (result i32) global.get $shield)
  (func (export "get_intercepts") (result i32) global.get $intercepts))"#;

    /// 一次性射手 WAT：首拍向固定方向 shoot（vx=4, vy=+14.7 两拍回弧, mass 1.0），
    /// 之后待机。配合靶位 (12.41, 0.5)（射手位 +7.41 = 1.8525×vx 两拍回弧落点）
    const SHOOTER_ONCE_WAT: &str = r#"(module
  (import "env" "shoot" (func $shoot (param f64 f64 f64 i32 i32 i32) (result i32)))
  (import "env" "log" (func $log (param i32)))
  (global $done (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_tick")
    (if (i32.eqz (global.get $done))
      (then
        (global.set $done (i32.const 1))
        (call $log (i32.const 21))
        (drop (call $shoot (f64.const 4.0) (f64.const 14.7)
                           (f64.const 1.0) (i32.const 0) (i32.const 0) (i32.const 0))))))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 构造弹丸实体（测试用：code_hash=projectile + 半径/速度）
    fn make_projectile(id_u: u128, pos: (f64, f64), vel: (f64, f64)) -> Entity {
        let mut e = Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            PlayerId(Uuid::from_u128(7)),
            codeworld_core::collision::PROJECTILE_CODE.to_string(),
            pos,
            Appearance::new(".".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        );
        e.set_radius(codeworld_core::collision::PROJECTILE_RADIUS);
        e.set_velocity(vel);
        e
    }

    #[test]
    fn smoke_ballistics_launch_to_intercept_evidence_chain() {
        // 验收#3 冒烟（进程内代 + 证据链）：射手向静态盾靶发射弹丸，两拍命中被盾拦截。
        // 证据链四事件走状态取证（battle.log 对应行：[shoot] 射击已排队 / [弹丸诞生] /
        // [弹丸命中] / [player_log] marker=7——真实起服 INFO 日志可直接 grep）：
        // 发射=射手沙箱 api_calls 含 shoot；飞行=弹丸实体离手位移；命中拦截=弹丸消亡
        // + 靶盾 3→2 + 靶沙箱 markers 含拦截 marker 7。
        // 弹道：射手(5,0.5) 初速 (4,14.7) mass1，两拍回弧：t1 (8.8,5.155) t2 落靶身。
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), SHOOTER_ONCE_WAT);
        let target = make_entity_with_sandbox(&mut world, &mut map, 2, (12.41, 0.5), SHOOT_TARGET_WAT);
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        // tick1：射击排队 -> 弹丸诞生 -> 第一拍飞行
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        // 证据链1 发射：射手沙箱调过 shoot（api_calls 元数据）
        assert!(
            map.sandboxes
                .get(&shooter)
                .unwrap()
                .store
                .data()
                .api_calls
                .contains(&"shoot".to_string()),
            "证据链1：射手应调过 shoot"
        );
        // 证据链2 飞行：弹丸实体在世且已离手位移（第一拍积分到 (8.8,5.155) 附近）
        assert_eq!(world.entities.len(), 3, "tick1 世界应有 射手+靶+弹丸");
        let proj = world
            .entities
            .values()
            .find(|e| codeworld_core::collision::is_projectile(e))
            .expect("tick1 应有 1 颗在飞弹丸");
        assert!(
            (proj.pos().0 - 8.8).abs() < 1e-9 && (proj.pos().1 - 5.155).abs() < 1e-9,
            "证据链2：弹丸第一拍应飞到 (8.8,5.155)，实际 {:?}",
            proj.pos()
        );
        // tick2：弹丸命中靶身 -> 盾拦截 -> 弹丸消亡（死亡清理移除）
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        // 证据链3 命中：弹丸消亡（死亡清理移除）
        assert!(
            world
                .entities
                .values()
                .all(|e| !codeworld_core::collision::is_projectile(e)),
            "证据链3：命中后弹丸应消亡"
        );
        // 证据链4 拦截：靶存活 + 盾 3→2 + 拦截计数 1（状态取证——markers 每拍字幕步
        // 被 drain 不可靠，沙箱内计数器才是持久证据；真实起服的 [player_log] marker=7
        // 行同样可 grep）
        assert!(world.entities.get(&target).unwrap().is_alive(), "盾拦截：靶存活");
        assert_eq!(
            call_i32_export(&mut map, &target, "get_shield"),
            2,
            "证据链4：盾 3 拦 1 刀应剩 2（弹丸结算走 on_attack 管线）"
        );
        assert_eq!(
            call_i32_export(&mut map, &target, "get_intercepts"),
            1,
            "证据链4：靶应记录 1 次拦截"
        );
        assert!(world.entities.get(&shooter).unwrap().is_alive());
    }

    #[test]
    fn attack_at_beyond_contact_misses_with_distance_reason() {
        // #034 接触门：非贴身出刀落空（新落空原因"距离不足"，与"点不在任何身体内"区分
        // ——日志可查，行为锚=目标存活不换血）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker = make_entity_with_sandbox(&mut world, &mut map, 1, (0.0, 0.5), PLAYER_WAT);
        let target = make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.5), TARGET_WAT);
        let attacks = vec![PendingAttack::strike(attacker, (5.0, 0.5), vec![])];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(world.entities.get(&target).unwrap().is_alive(), "够不着=落空");
        assert_eq!(
            world.entities.get(&attacker).unwrap().survival.points(),
            3,
            "落空无掠夺"
        );
    }

    #[test]
    fn attack_at_exact_reach_boundary_misses() {
        // #034 判距边界：dist 恰 = r攻+r目+0.5 = 1.5 -> 落空（严格 <，审查锚定）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let attacker = make_entity_with_sandbox(&mut world, &mut map, 1, (0.0, 0.5), PLAYER_WAT);
        let target = make_entity_with_sandbox(&mut world, &mut map, 2, (1.5, 0.5), TARGET_WAT);
        let attacks = vec![PendingAttack::strike(attacker, (1.5, 0.5), vec![])];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(
            world.entities.get(&target).unwrap().is_alive(),
            "恰好判距=落空（严格 <）"
        );
    }

    #[test]
    fn projectile_strike_bypasses_contact_gate() {
        // #034 弹丸结算豁免贴身判距：接触事实由弹丸完成（离体之物=手臂延伸），
        // 发射者在 5 格外弹丸命中照样结算
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (0.0, 0.5), PLAYER_WAT);
        let target = make_entity_with_sandbox(&mut world, &mut map, 2, (5.0, 0.5), TARGET_WAT);
        let attacks = vec![PendingAttack::projectile_strike(shooter, (5.0, 0.5), vec![], false)];
        process_attacks(&mut world, &mut map, &attacks);
        assert!(
            !world.entities.get(&target).unwrap().is_alive(),
            "弹丸命中不受发射者贴身判距约束"
        );
        assert_eq!(
            world.entities.get(&shooter).unwrap().survival.points(),
            4,
            "击杀掠夺归发射者"
        );
    }

    // === #036 期五第五刀：操控定装弹（WS 远程射击；驾驶档，玩家代码 shoot 才是自定义弹道）===

    /// 世界内弹丸计数（断言辅助）
    fn count_projectiles(world: &WorldState) -> usize {
        world
            .entities
            .values()
            .filter(|e| codeworld_core::collision::is_projectile(e))
            .count()
    }

    // ===== 炮塔解算器（期五第六刀）：solve_launch_vector 纯函数测试 =====

    /// 弹道模拟采样（测试辅助）：定装弹从 出发逐拍推演，与弹道步
    /// step_projectiles 同序（重力→阻力→极速钳→位移），落地 y<半径截断。
    /// 返回逐拍末坐标（含起点）。
    /// 【#044 子步宪法锚】N=1 整步积分，步数由状态决定（落地/寿命截断），
    /// 纯状态驱动零机器数据——与 solve_launch_vector 内 simulate_path 同构。
    fn sim_trajectory(sx: f64, sy: f64, mut vx: f64, mut vy: f64) -> Vec<(f64, f64)> {
        let p = codeworld_core::physics::DefaultPhysics::default();
        let r = codeworld_core::collision::PROJECTILE_RADIUS;
        let mut pts = vec![(sx, sy)];
        let (mut x, mut y) = (sx, sy);
        for _ in 0..codeworld_physics::default::PROJECTILE_MAX_AGE_TICKS {
            vy += p.gravity;
            let f = 1.0 - p.drag_coefficient / CTRL_SHOOT_MASS;
            vx *= f;
            vy *= f;
            let sp = (vx * vx + vy * vy).sqrt();
            if sp > p.max_speed {
                let k = p.max_speed / sp;
                vx *= k;
                vy *= k;
            }
            x += vx;
            y += vy;
            pts.push((x, y));
            if y < r {
                break;
            }
        }
        pts
    }

    /// 点到线段最小距离（测试辅助）：弹道拍间是直线段，仅拍末采样会漏最近点
    fn seg_min_dist(a: (f64, f64), b: (f64, f64), p: (f64, f64)) -> f64 {
        let (abx, aby) = (b.0 - a.0, b.1 - a.1);
        let len2 = abx * abx + aby * aby;
        let t = if len2 < 1e-12 {
            0.0
        } else {
            (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0)
        };
        let (cx, cy) = (a.0 + abx * t, a.1 + aby * t);
        ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
    }

    /// 弹道全程与目标的最小距离（相邻拍末点连线段到目标圆心，CCD 撞体窗口判据）
    fn traj_min_dist(pts: &[(f64, f64)], target: (f64, f64)) -> f64 {
        pts.windows(2)
            .map(|w| seg_min_dist(w[0], w[1], target))
            .fold(f64::MAX, f64::min)
    }

    #[test]
    fn solve_launch_vector_hits_elevated_target_via_high_arc() {
        // bug 回归（battle.log 四发哑弹）：地面打高空 S(25,12)——60° 定角顶点
        // ~9.6 < 12 全部落脚下；解算器高弧解顶点必须越过 S 并进入命中窗口
        //（CCD 撞体判据：r弹0.15 + r身0.5 = 0.65）
        let sol = solve_launch_vector(30.0, 0.5, 25.0, 12.0, CTRL_SHOOT_SPEED)
            .expect("高空目标 S(25,12) 必须够得着（本刀修复的核心验收）");
        let pts = sim_trajectory(30.0, 0.5, sol.0, sol.1);
        let apex = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max);
        // 顶点 ≥ 目标高度-容差（拍末采样顶点与 CCD 线段穿高的几何余量一致）
        assert!(
            apex >= 12.0 - CTRL_SOLVE_TOLERANCE,
            "弹道顶点 {apex} 必须到达 S 高度邻域 12±0.65（高弧高抛）"
        );
        let best = traj_min_dist(&pts, (25.0, 12.0));
        assert!(best < 0.65, "弹道最近点 {best} 必须进入命中窗口 0.65");
    }

    #[test]
    fn solve_launch_vector_hits_ground_target_via_low_arc() {
        // 地面目标 D（15 格外）：低弧平飞命中（|vy| ≤ |vx|——弹道压得平）
        let sol = solve_launch_vector(30.0, 0.5, 15.0, 0.5, CTRL_SHOOT_SPEED)
            .expect("15 格地面目标必须够得着（剧本 D 站桩位）");
        assert!(
            sol.1.abs() <= sol.0.abs(),
            "地面目标应取低弧平飞解（|vy|≤|vx|），实际 ({},{})",
            sol.0,
            sol.1
        );
        let pts = sim_trajectory(30.0, 0.5, sol.0, sol.1);
        let best = traj_min_dist(&pts, (15.0, 0.5));
        assert!(best < 0.65, "弹道最近点 {best} 必须进入命中窗口 0.65");
    }

    #[test]
    fn solve_launch_vector_none_when_target_unreachable() {
        // 超远目标：无解 None——解不出就明示，不打必落空的哑弹（业界惯例）
        assert!(solve_launch_vector(30.0, 0.5, 80.0, 0.5, CTRL_SHOOT_SPEED).is_none());
        // 高不可攀：火力上限内够不到的贴身高空（20 格高且水平贴脸）
        assert!(solve_launch_vector(30.0, 0.5, 30.5, 30.0, CTRL_SHOOT_SPEED).is_none());
        // 零距离（贴脸坐标）：不在此解算（前端走 attack 贴身刀分流）
        assert!(solve_launch_vector(30.0, 0.5, 30.0, 0.5, CTRL_SHOOT_SPEED).is_none());
    }

    #[test]
    fn solve_launch_vector_reasonable_for_close_target() {
        // 近贴目标（~1.5 格）：补偿档（13.8，不爬坡）+ 弹道直达（全程进入
        // 命中窗口）——近距高弧解（近乎垂直吊射自己脑门）是反直觉坏解，
        // <2 格一律低弧；离散拍下"直射"体现为弹道线段扫过目标而非拍末逼近
        let sol = solve_launch_vector(30.0, 0.5, 31.5, 0.8, CTRL_SHOOT_SPEED)
            .expect("近贴目标应有解");
        let v = (sol.0.powi(2) + sol.1.powi(2)).sqrt();
        // 上界断言：斜向目标的 vx 含水平方位缩放（世界系模长 ≤ 档位值）
        assert!(
            v <= CTRL_SHOOT_SPEED * CTRL_SHOOT_DRAG_COMP + 1e-9,
            "近贴目标应用补偿档（不爬坡），实际 {v}"
        );
        let pts = sim_trajectory(30.0, 0.5, sol.0, sol.1);
        let best = traj_min_dist(&pts, (31.5, 0.8));
        assert!(best < 0.65, "近贴弹道应直达目标，实际最近 {best}");
    }

    #[test]
    fn solve_launch_vector_muzzle_speed_bounded_and_backed() {
        // 模长下界=定装补偿档 13.8（12×1.15 阻力补偿），上界=自适应火力上限：
        // 近距目标（补偿档射程内）用首档；高空目标（12 格高）物理上需爬坡
        let near = solve_launch_vector(30.0, 0.5, 40.0, 0.5, CTRL_SHOOT_SPEED).unwrap();
        let v_near = (near.0.powi(2) + near.1.powi(2)).sqrt();
        assert!(
            (v_near - CTRL_SHOOT_SPEED * CTRL_SHOOT_DRAG_COMP).abs() < 1e-9,
            "10 格内地面目标应用补偿档 {v_near} != {}",
            CTRL_SHOOT_SPEED * CTRL_SHOOT_DRAG_COMP
        );
        let high = solve_launch_vector(30.0, 0.5, 25.0, 12.0, CTRL_SHOOT_SPEED).unwrap();
        let v_high = (high.0.powi(2) + high.1.powi(2)).sqrt();
        assert!(
            v_high > CTRL_SHOOT_SPEED * CTRL_SHOOT_DRAG_COMP,
            "高空目标允许速度爬坡（离散拍下 13.8 档射程仅 ~10 格且 vy 峰值不足）"
        );
        assert!(
            v_high <= CTRL_SHOOT_MAX_EFF_SPEED + 1e-9,
            "不得超过自适应火力上限 {CTRL_SHOOT_MAX_EFF_SPEED}"
        );
    }

    #[test]
    fn solve_launch_vector_deterministic_same_input_same_solution() {
        // #044 子步宪法·解算确定性锚（D-015）：解算器内部是"弹道积分"子步的
        // 预测侧重演（逐拍 N=1），同一输入（发射点/目标点/世界常数）必须产出
        // 逐位相同的解——判据只读仿真输入，零机器数据（墙钟/CPU/帧率）参与。
        // 三组几何形态各调两遍：低弧地面目标/高弧高空目标/无解超远——含
        // None 路径（无解也要确定性无解，两台机器一致拒绝）。
        for (sx, sy, tx, ty) in [
            (30.0, 0.5, 40.0, 0.5),   // 低弧：10 格平地
            (30.0, 0.5, 25.0, 12.0),  // 高弧：12 格高空
            (30.0, 0.5, 80.0, 0.5),   // 无解：超射程
        ] {
            let a = solve_launch_vector(sx, sy, tx, ty, CTRL_SHOOT_SPEED);
            let b = solve_launch_vector(sx, sy, tx, ty, CTRL_SHOOT_SPEED);
            assert_eq!(a, b, "同输入必须同解（#044 纯函数）：({sx},{sy})->({tx},{ty})");
        }
    }

    #[test]
    fn solver_path_pure_state_driven_tick_count() {
        // #044 子步宪法·积分步数状态驱动锚：sim_trajectory 的步数只由弹丸
        // 状态决定——同一初条件跑两遍逐位一致；且状态不同步数不同（高初速
        // 弹飞的拍数 > 低初速弹，截断条件=落地 y<半径 或寿命 30 拍，全是
        // 弹丸自身状态非机器数据）。将来解算器若引入自适应采样（tier>1），
        // 本锚必须随宪法一起更新（tier 读上拍状态+滞回带+换档记日志）。
        let slow = sim_trajectory(30.0, 0.5, 6.0, 6.0);
        let fast = sim_trajectory(30.0, 0.5, 14.0, 14.0);
        // 同一初条件重跑逐位一致（确定性）
        let slow2 = sim_trajectory(30.0, 0.5, 6.0, 6.0);
        assert_eq!(slow, slow2, "同初条件必须逐位一致（#044）");
        // 快弹飞更远、在空中更多拍（状态驱动步数的最小证据）
        assert!(
            fast.len() > slow.len(),
            "高初速弹应比低初速弹多飞拍数（{} vs {}）",
            fast.len(),
            slow.len()
        );
        // 截断上限=寿命常数（纯状态：任何弹道不超过 30 拍+起点）
        assert!(fast.len() <= 31, "弹道拍数不得超寿命上限 30");
    }

    #[test]
    fn ws_shoot_intent_spawns_projectile_with_fixed_loadout() {
        // 端到端：WsShoot 意图 -> 操控目标发射弹丸（解算初速/mass 1.0/
        // binding 0/payload 空），弹丸进世界+账本，发射位=拍末实况坐标
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![input::Intent::WsShoot { x: 45.0, y: 0.5 }];
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
        assert_eq!(count_projectiles(&world), 1, "WsShoot 诞生恰好 1 发弹丸");
        let p = world
            .entities
            .values()
            .find(|e| codeworld_core::collision::is_projectile(e))
            .unwrap();
        // 首步位置=解算初速经首拍物理（重力+阻力→位移）的消费结果——
        // 与纯函数解算逐位一致（镜像铁律的服务器内侧锚）
        let sol = solve_launch_vector(30.0, 0.5, 45.0, 0.5, CTRL_SHOOT_SPEED)
            .expect("15 格地面目标应有解");
        let pts = sim_trajectory(30.0, 0.5, sol.0, sol.1);
        let (ex, ey) = pts[1];
        let (px, py) = p.pos();
        assert!(
            (px - ex).abs() < 1e-6 && (py - ey).abs() < 1e-6,
            "首步位置应为解算初速经首拍物理（{ex},{ey}），实际 ({px},{py})"
        );
        assert!(py > 0.6, "首步 y 应离地上升（弹道离地），实际 {py}");
        assert_eq!(p.mass(), CTRL_SHOOT_MASS, "定装质量");
        assert_eq!(p.binding(), 0, "定装绑定=unbound");
        let meta = map.projectiles.values().next().expect("弹丸账本在册");
        assert_eq!(meta.shooter, pid, "发射者=操控目标");
        assert!(meta.payload.is_empty(), "定装 payload 空（默认击杀模块）");
    }

    #[test]
    fn ws_shoot_intent_unreachable_target_spawns_no_projectile() {
        // 解算无解（超远目标）-> 不发射（解不出就明示，不打必落空的哑弹）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, _pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![input::Intent::WsShoot { x: 90.0, y: 0.5 }];
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
        assert_eq!(count_projectiles(&world), 0, "无解目标不发射弹丸");
    }

    #[test]
    fn ws_shoot_rate_limited_to_one_per_tick() {
        // 同拍限流沿用（shoot import 1 发/拍在操控路径同样生效）：一批两次点击射
        // 后到折叠成单行动 -> 恰 1 发
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, _pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![
            input::Intent::WsShoot { x: 45.0, y: 0.5 },
            input::Intent::WsShoot { x: 20.0, y: 0.5 },
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
        assert_eq!(count_projectiles(&world), 1, "同拍多次点击射=后到覆盖，只发 1 发");
    }

    #[test]
    fn ws_shoot_and_move_coexist_across_ticks() {
        // 边走边射闭环：射击拍发射且不清走位目标 -> 下拍无意图拍继续向目标走（不再射）
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        let intents = vec![
            input::Intent::WsMove { x: 32.0, y: 0.5 },
            input::Intent::WsShoot { x: 45.0, y: 0.5 },
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
        assert_eq!(count_projectiles(&world), 1, "拍1射击");
        assert!(map.ws_move_target.is_some(), "走位目标保留（射击不清除）");
        assert_eq!(
            world.entities.get(&pid).unwrap().pos().0,
            30.0,
            "射击拍不移动（本拍行动=射击）"
        );
        let first_proj = world
            .entities
            .values()
            .find(|e| codeworld_core::collision::is_projectile(e))
            .map(|e| e.id);
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
            world.entities.get(&pid).unwrap().pos().0,
            31.0,
            "拍2走位恢复（边走边射：移动中开火后继续走）——走位恢复即证明拍2未再射（射拍不动）"
        );
        // 拍2无新弹丸：原弹两拍抛物线后落地消亡属正常弹道寿命，这里只证没有"另一发"
        let new_proj = world.entities.values().any(|e| {
            codeworld_core::collision::is_projectile(e) && Some(e.id) != first_proj
        });
        assert!(!new_proj, "拍2无新弹丸（无射击意图）");
    }

    #[test]
    fn terminal_client_quit_does_not_trigger_dispose_wait() {
        // #036 语义锚（验收 b）：终端 q 退出只关终端客户端（渲染静默），不动 WS 令牌——
        // 处置等待触发源=令牌翻转沿（#032），浏览器在线=操控在=无处置等待；世界照常 tick
        let (mut world, mut time, physics, mut renderer, mut storage, mut pending, mut map, pid, _me_dir) =
            control_stage(AUTO_MOVER_WAT);
        renderer.set_output_enabled(false); // 终端客户端已退出（q 路径效果）
        tick(
            &mut world,
            &mut time,
            &physics,
            &mut renderer,
            &mut storage,
            &mut pending,
            &mut map,
        );
        assert_eq!(world.tick(), 1, "世界照常推进（世界常驻）");
        assert!(world.entities.get(&pid).unwrap().is_alive(), "角色照活");
        assert!(
            map.players.dispose.get("p1").is_none(),
            "终端退出不触发处置等待（令牌未动）"
        );
        assert!(
            map.ctrl_exclusive_active,
            "浏览器连接在=操控在（操控独占不受终端退出影响）"
        );
    }

    #[test]
    fn process_shoots_spawns_projectile_entity_with_meta() {
        // 发射落地：PendingShoot -> 弹丸实体（code_hash=projectile/半径 0.15/速度/质量/
        // glyph=. / 颜色=发射者色）+ 发射账本（发射者+payload）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        world.entities.get_mut(&shooter).unwrap().appearance.color = "purple".to_string();
        map.sandboxes
            .get_mut(&shooter)
            .unwrap()
            .store
            .data_mut()
            .pending_shoots
            .push(codeworld_arena::sandbox::PendingShoot {
                shooter,
                vx: 3.0,
                vy: 4.0,
                mass: 0.5,
                binding: 0,
                payload: vec![1, 2, 3],
            });
        process_shoots(&mut world, &mut map, 7);
        assert_eq!(world.entities.len(), 2, "弹丸应入世");
        let proj = world
            .entities
            .values()
            .find(|e| codeworld_core::collision::is_projectile(e))
            .expect("应有弹丸实体");
        assert_eq!(proj.pos(), (5.0, 0.5), "从发射者当前位置出手");
        assert_eq!(proj.velocity(), (3.0, 4.0));
        assert_eq!(proj.mass(), 0.5);
        assert_eq!(proj.radius(), 0.15, "弹丸身体半径 0.15");
        assert_eq!(proj.appearance.glyph, ".", "弹丸小圆点");
        assert_eq!(proj.appearance.color, "purple", "颜色=发射者 glyph 色");
        assert_eq!(proj.created_tick, 7);
        let (pid, meta) = map.projectiles.iter().next().expect("应有发射账本");
        assert_eq!(meta.shooter, shooter, "账本记发射者");
        assert_eq!(meta.payload, vec![1, 2, 3], "账本记 payload");
        let _ = pid;
    }

    // === 发射痕迹（#034 修订三：发射即暴露，世界只保证"打了就有记录"）===

    fn push_shoot(map: &mut EntitySandboxMap, shooter: EntityId, vx: f64, vy: f64, mass: f64) {
        map.sandboxes
            .get_mut(&shooter)
            .unwrap()
            .store
            .data_mut()
            .pending_shoots
            .push(codeworld_arena::sandbox::PendingShoot {
                shooter,
                vx,
                vy,
                mass,
                binding: 0,
                payload: vec![],
            });
    }

    #[test]
    fn shoot_leaves_one_tick_trace_for_weak_shell() {
        // 弱弹（动能 5 = 速度5×质量1 < 阈值50）痕迹 1 拍：位置=发射点、记发射者与拍号
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        push_shoot(&mut map, shooter, 3.0, 4.0, 1.0);
        process_shoots(&mut world, &mut map, 7);
        assert_eq!(map.traces.len(), 1, "发射应留痕迹");
        let t = &map.traces[0];
        assert_eq!(t.pos, (5.0, 0.5), "痕迹位置=发射点");
        assert_eq!(t.shooter, shooter, "痕迹记发射者");
        assert_eq!(t.birth_tick, 7, "痕迹记发射拍号");
        assert_eq!(t.ttl, 1, "弱弹痕迹寿命 1 拍");
        // 痕迹不是实体：不进世界账面、不占碰撞
        assert!(world
            .entities
            .values()
            .all(|e| e.id != t.trace_id), "痕迹实体不进 world");
    }

    #[test]
    fn shoot_leaves_three_tick_trace_for_high_kinetic_shell() {
        // 强弹（动能 200 = 速度20×质量10 > 阈值50）痕迹 3 拍封顶
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        push_shoot(&mut map, shooter, 20.0, 0.0, 10.0);
        process_shoots(&mut world, &mut map, 7);
        assert_eq!(map.traces[0].ttl, 3, "强弹痕迹寿命封顶 3 拍");
    }

    #[test]
    fn launch_trace_expires_after_ttl() {
        // 寿命：诞生拍（阶段二）存续，之后每拍结算递减——弱弹在诞生次拍清算后消失
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        push_shoot(&mut map, shooter, 3.0, 4.0, 1.0);
        process_shoots(&mut world, &mut map, 7);
        map.expire_traces(7);
        assert_eq!(map.traces.len(), 1, "诞生拍清算不消（还没被任何一拍看见过）");
        map.expire_traces(8);
        assert!(map.traces.is_empty(), "次拍清算消亡（恰好可见一拍：第 8 拍的眼睛层）");
    }

    #[test]
    fn launch_trace_enters_visible_lists() {
        // 可探测：痕迹进 visible 列表（眼睛层），对所有沙箱可见（含发射者自己——
        // 自己当然知道自己开过火）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        push_shoot(&mut map, shooter, 3.0, 4.0, 1.0);
        process_shoots(&mut world, &mut map, 7);
        let bb = BoundingBox::new(-50.0, -25.0, 50.0, 25.0);
        map.fill_visible_snapshots(&world, &bb);
        let vis = map.sandboxes[&shooter].store.data().visible.clone();
        assert!(
            vis.contains(&(5.0, 0.5)),
            "痕迹坐标应出现在可见列表（发射者自己视角）"
        );
        // probe 桥的数据源同样带痕迹（traces 摘要与 visible 平行）
        assert_eq!(map.traces.len(), 1);
    }

    // === 绑定契约（#034 修订四：出生声明，世界每拍托管执行——遗嘱模式）===

    fn push_binding_shoot(
        map: &mut EntitySandboxMap,
        shooter: EntityId,
        vx: f64,
        vy: f64,
        mass: f64,
        binding: i32,
    ) {
        map.sandboxes
            .get_mut(&shooter)
            .unwrap()
            .store
            .data_mut()
            .pending_shoots
            .push(codeworld_arena::sandbox::PendingShoot {
                shooter,
                vx,
                vy,
                mass,
                binding,
                payload: vec![],
            });
    }

    #[test]
    fn fade_on_death_shell_dies_same_tick_owner_dies() {
        // 同生弹：主人死那一拍弹即无（托管执行不等下一拍物理）——弹丸标记死亡
        // （死亡清理链移除）+ 发射账本清空
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        push_binding_shoot(&mut map, shooter, 3.0, 0.0, 1.0, codeworld_core::entity::BINDING_FADE_ON_DEATH);
        process_shoots(&mut world, &mut map, 3);
        assert_eq!(map.projectiles.len(), 1, "弹已出手在飞");
        // 主人当场死亡（击杀/衰减/登出三路同源：实体 kill 即死）
        world.entities.get_mut(&shooter).unwrap().kill();
        enforce_binding_deaths(&mut world, &mut map);
        assert!(
            !map.projectiles.iter().any(|(pid, _)| world
                .entities
                .get(pid)
                .map(|e| e.is_alive())
                .unwrap_or(false)),
            "同生契约生效：弹丸不再有活体在册"
        );
        assert!(map.projectiles.is_empty(), "发射账本同步清空");
        let proj = world
            .entities
            .values()
            .find(|e| codeworld_core::collision::is_projectile(e))
            .expect("弹丸实体仍在账面（尸体由清理链移除）");
        assert!(!proj.is_alive(), "弹丸已标记死亡");
    }

    #[test]
    fn unbound_shell_keeps_flying_after_owner_death() {
        // unbound 回归锚：主人死弹照飞（离体即世界之物，同生不共死=现状默认）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        push_binding_shoot(&mut map, shooter, 3.0, 0.0, 1.0, codeworld_core::entity::BINDING_UNBOUND);
        process_shoots(&mut world, &mut map, 3);
        world.entities.get_mut(&shooter).unwrap().kill();
        enforce_binding_deaths(&mut world, &mut map);
        assert_eq!(map.projectiles.len(), 1, "unbound 弹不随主消亡");
        assert!(
            world
                .entities
                .values()
                .find(|e| codeworld_core::collision::is_projectile(e))
                .unwrap()
                .is_alive(),
            "unbound 弹照飞"
        );
    }

    #[test]
    fn fade_on_death_tower_dies_with_creator_unbound_tower_survives() {
        // 塔契约：fade_on_death 塔随创造者死消亡；unbound 塔照站（同生不共死=现状）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let creator = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        let owner = world.entities[&creator].owner;
        let mut fade_tower = Entity::new(
            EntityId(Uuid::from_u128(201)),
            owner,
            "creation".to_string(),
            (10.0, 0.0),
            Appearance::new("T".to_string(), "cyan".to_string(), 1.0),
            3,
            1_000_000,
        );
        fade_tower.set_binding(codeworld_core::entity::BINDING_FADE_ON_DEATH);
        fade_tower.set_bound_to(Some(creator));
        world.entities.insert(fade_tower.id, fade_tower);
        let unbound_tower = Entity::new(
            EntityId(Uuid::from_u128(202)),
            owner,
            "creation".to_string(),
            (12.0, 0.0),
            Appearance::new("T".to_string(), "cyan".to_string(), 1.0),
            3,
            1_000_000,
        );
        world.entities.insert(unbound_tower.id, unbound_tower);
        // 创造者死亡 -> 托管清算
        world.entities.get_mut(&creator).unwrap().kill();
        enforce_binding_deaths(&mut world, &mut map);
        assert!(
            !world.entities.get(&EntityId(Uuid::from_u128(201))).unwrap().is_alive(),
            "同生塔随主消亡"
        );
        assert!(
            world.entities.get(&EntityId(Uuid::from_u128(202))).unwrap().is_alive(),
            "unbound 塔照站（同生不共死=unbound 现状默认）"
        );
    }

    #[test]
    fn grace_owner_landing_does_not_settle_shooter() {
        // 攻击豁免条款（声明式）：grace_owner 弹落地结算不伤发射者（P1-B 定案：
        // 世界不豁免，豁免是声明——unbound 落地砸自己合法）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        let grace = PendingAttack {
            attacker: shooter,
            target_pos: (5.0, 0.5), // 落点在发射者自己身体内
            payload: vec![],
            contact_exempt: true,
            grace_owner: true,
        };
        let strikes = deliver_attacks(&world, &mut map, &[grace]);
        assert!(strikes.is_empty(), "grace_owner 落地弹不结算发射者（哑弹）");
        assert!(world.entities.get(&shooter).unwrap().is_alive());
    }

    #[test]
    fn unbound_landing_still_settles_shooter() {
        // P1-B 定案回归锚：unbound 弹落地砸自己合法（世界不豁免，豁免是声明）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        let unbound = PendingAttack {
            attacker: shooter,
            target_pos: (5.0, 0.5),
            payload: vec![],
            contact_exempt: true,
            grace_owner: false,
        };
        let strikes = deliver_attacks(&world, &mut map, &[unbound]);
        assert_eq!(strikes.len(), 1, "unbound 落地砸自己照常进结算");
        assert_eq!(strikes[0].target, shooter, "目标就是发射者自己");
    }

    #[test]
    fn grace_owner_upward_shell_lands_at_feet_without_self_kill() {
        // 端到端冒烟（进程内代 headless）：grace_owner 上抛弹（binding=2）垂直升空
        // -> CCD 落回脚下（落点在自己身体内）-> 落地结算被豁免 -> 发射者无伤
        const GRACE_UPWARD_WAT: &str = r#"(module
  (import "env" "shoot" (func $shoot (param f64 f64 f64 i32 i32 i32) (result i32)))
  (global $done (mut i32) (i32.const 0))
  (memory (export "memory") 1)
  (func (export "on_tick")
    (if (i32.eqz (global.get $done))
      (then
        (global.set $done (i32.const 1))
        (drop (call $shoot (f64.const 0.0) (f64.const 14.7)
                           (f64.const 1.0) (i32.const 0) (i32.const 0) (i32.const 2))))))
  (func (export "life") (result i32) i32.const 1))"#;
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), GRACE_UPWARD_WAT);
        let points_before = world.entities.get(&shooter).unwrap().survival.points();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        for _ in 0..8 {
            tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        }
        assert!(
            world
                .entities
                .values()
                .all(|e| !codeworld_core::collision::is_projectile(e)),
            "弹丸已落地消亡"
        );
        let me = world.entities.get(&shooter).expect("发射者存活未清理");
        assert!(me.is_alive(), "grace_owner 上抛弹落回脚下不炸自己");
        assert_eq!(
            me.survival.points(),
            points_before,
            "豁免结算无掠夺（点数不动）"
        );
    }


    #[test]
    fn process_shoots_dead_shooter_and_cap_void_requests() {
        // 发射防御：发射者已死作废；达世界实体上限作废
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        map.sandboxes
            .get_mut(&shooter)
            .unwrap()
            .store
            .data_mut()
            .pending_shoots
            .push(codeworld_arena::sandbox::PendingShoot {
                shooter,
                vx: 1.0,
                vy: 0.0,
                mass: 1.0,
                binding: 0,
                payload: vec![],
            });
        world.entities.get_mut(&shooter).unwrap().kill();
        process_shoots(&mut world, &mut map, 1);
        assert_eq!(world.entities.len(), 1, "死者不出手（弹丸不诞生）");
        // 上限：cap=1 已满
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        map.max_entities = 1;
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), PLAYER_WAT);
        map.sandboxes
            .get_mut(&shooter)
            .unwrap()
            .store
            .data_mut()
            .pending_shoots
            .push(codeworld_arena::sandbox::PendingShoot {
                shooter,
                vx: 1.0,
                vy: 0.0,
                mass: 1.0,
                binding: 0,
                payload: vec![],
            });
        process_shoots(&mut world, &mut map, 1);
        assert_eq!(world.entities.len(), 1, "达上限弹丸不诞生");
    }

    #[test]
    fn projectile_flies_on_after_shooter_death_and_strike_voided() {
        // owner 死亡时在飞弹丸照飞（已离体归世界），命中结算因攻击者已死作废
        //（#002 死者刀作废既有法）：弹丸消亡、目标存活
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let shooter = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), SHOOTER_ONCE_WAT);
        let target = make_entity_with_sandbox(&mut world, &mut map, 2, (12.41, 0.5), TARGET_WAT);
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        // 弹丸已出手（tick1 诞生在飞），发射者当场阵亡
        world.entities.get_mut(&shooter).unwrap().kill();
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert!(
            world
                .entities
                .values()
                .all(|e| !codeworld_core::collision::is_projectile(e)),
            "弹丸照飞并已消亡（命中/落地路径收口）"
        );
        assert!(
            world.entities.get(&target).unwrap().is_alive(),
            "发射者已死，弹丸命中结算作废（死者刀作废）目标存活"
        );
        assert!(!world.entities.contains_key(&shooter), "死者被清理");
    }

    #[test]
    fn find_nearest_and_hit_target_ignore_projectiles() {
        // 弹丸不可锁定（scan 不指向弹丸）也不可被攻击命中（点命中永不指向弹丸）
        let mut world = WorldState::new();
        let me = EntityId(Uuid::from_u128(1));
        let enemy = EntityId(Uuid::from_u128(3));
        world.entities.insert(me, make_entity(1, (0.0, 0.5)));
        world.entities.insert(enemy, make_entity(3, (5.0, 0.5)));
        let proj = make_projectile(2, (1.0, 0.5), (2.0, 0.0));
        let proj_id = proj.id;
        world.entities.insert(proj_id, proj);
        assert_eq!(
            find_nearest_enemy(&world, &me, &unbounded_bb()),
            Some((5.0, 0.5)),
            "scan 锁定敌人而非更近的弹丸"
        );
        assert_eq!(
            find_hit_target(&world, (1.0, 0.5)),
            None,
            "点在弹丸身体内也不可被命中（弹丸非攻击目标）"
        );
        assert_eq!(
            find_hit_target(&world, (5.0, 0.5)),
            Some(enemy),
            "敌人照常可命中"
        );
    }

    #[test]
    fn hunter_shoots_projectile_at_eighth_tick_and_hits_static_victim() {
        // H 新行为：每 8 拍向最近敌人抛射（vx=dx/1.71、vy=+14.7、mass 0.5）。
        // H@9.5（游走 7 拍后）对 10.5 格外的静止靶：两拍回弧落点恰在靶身 -> t9 命中击杀
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let h = make_entity_with_sandbox(&mut world, &mut map, 1, (10.0, 0.5), HUNTER_WAT);
        let victim = make_entity_with_sandbox(&mut world, &mut map, 2, (20.0, 0.5), TARGET_WAT);
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut saw_projectile = false;
        for _ in 0..10 {
            tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
            saw_projectile |= world
                .entities
                .values()
                .any(|e| codeworld_core::collision::is_projectile(e) && e.is_alive());
        }
        assert!(saw_projectile, "10 拍内应见到 H 抛出的弹丸实体");
        assert!(
            !world.entities.contains_key(&victim),
            "静止靶在 H 的两拍回弧弹道上 -> 弹丸命中击杀（无防守放行，死亡清理移除）"
        );
        assert!(world.entities.get(&h).unwrap().is_alive(), "H 存活");
    }

    #[test]
    fn defender_approaches_ground_enemy_then_strikes_in_contact() {
        // D 接近逻辑：贴身前每拍向地面敌人挪 1.0，贴身后反制拍出刀（距离不足的刀
        // 进不了防守询问）——5 格外的静态靶在 ~t4 被贴身反制击杀
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let d = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), &defender_wat(10));
        let victim = make_entity_with_sandbox(&mut world, &mut map, 2, (10.0, 0.5), TARGET_WAT);
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 12);
        assert!(
            !world.entities.contains_key(&victim),
            "D 应接近到贴身并反制击杀（死亡清理移除）"
        );
        assert!(
            world.entities.get(&d).unwrap().pos().0 > 5.0,
            "D 应挪过窝（接近逻辑生效）"
        );
    }

    #[test]
    fn defender_ignores_airborne_enemy() {
        // D 不追悬停者：敌在头顶 2 格开外=空中够不着（塔防不下场追鸟）——
        // D 原地不动，悬停者全程安全（高度=物理诚实的防御）
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let d = make_entity_with_sandbox(&mut world, &mut map, 1, (5.0, 0.5), &defender_wat(10));
        let hoverer = make_entity_with_sandbox(&mut world, &mut map, 2, (9.0, 12.0), ALWAYS_HOVER_WAT);
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 8);
        assert_eq!(
            world.entities.get(&d).unwrap().pos(),
            (5.0, 0.5),
            "D 不应向空中敌人挪一步"
        );
        assert_eq!(
            world.entities.get(&hoverer).unwrap().pos(),
            (9.0, 12.0),
            "悬停者全程安全"
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
        // 负面路径：A 的 marker（0/2/3）、H 拦截 6、S 侦察 10、S 替身拦截 11
        // 都不含盾上报 -> None（S 的 11 不许撞 D 的 7/8 字幕协议）
        assert_eq!(parse_shield_markers(&[0, 2, 3]), None);
        assert_eq!(
            parse_shield_markers(&[6, 10, 11]),
            None,
            "S/H 的 marker 不进盾量字幕"
        );
    }

    #[test]
    fn spy_disguise_intercept_count_counts_marker_11() {
        // 替身台词触发源计数：只数 S 拦截 marker 11，其他 marker（含 D 的 7）不算
        assert_eq!(spy_disguise_intercept_count(&[11, 11, 11]), 3);
        assert_eq!(spy_disguise_intercept_count(&[10, 7, 8, 6]), 0);
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
        // A 被击杀后距离段显式 A:阵亡 / alive=1（期五第六刀：dist=-- 分不清谁没了）
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
        // 开局字幕（第 1 tick 后：alive=2；#034 接近逻辑后 A/D 双向逼近 dist=8.0）
        run(&mut world, &mut time, &mut renderer, &mut storage, &mut pending, &mut map);
        assert!(
            renderer_row0(&renderer).contains("alive=2"),
            "开局字幕应显示存活 2，实际 {:?}",
            renderer_row0(&renderer)
        );
        assert!(
            renderer_row0(&renderer).contains("dist=8.0"),
            "第 1 tick A/D 相向逼近后距离应恰 8.0，实际 {:?}",
            renderer_row0(&renderer)
        );
        // 15 tick：A 被 D 反制击杀（默认盾 10 时间线）-> A:阵亡 / alive=1
        for _ in 0..14 {
            run(&mut world, &mut time, &mut renderer, &mut storage, &mut pending, &mut map);
        }
        assert!(
            !world.entities.contains_key(&a),
            "回归前提：15 tick 时 A 应已被击杀"
        );
        assert!(
            renderer_row0(&renderer).contains("A:阵亡"),
            "A 死后距离段应显式标 A:阵亡，实际 {:?}",
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
        // 集成：贴身战后 D 拦截 A 的刀，字幕盾量经 marker 上报显示（盾量=拦截配额余量，
        // 来自玩家代码自愿上报，host 不读沙箱内部）。
        // #034 后贴身时间线 ~t4（精确刀数随身体阻挡浮动），断言取"有上报"行为锚。
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
            !line.contains("shield=?"),
            "D 拦截过就应有盾量上报（不是占位符），实际 {:?}",
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
        assert_eq!(p.pos(), (30.0, 0.0), "第 1 名玩家出生点 (30,0)（d0 进场在物理步后，本拍不落位）");
        assert_eq!(p.appearance.glyph, "E");
        assert_eq!(p.appearance.color, "green");
        assert!(map.sandboxes.contains_key(&p.id), "进场玩家必有沙箱");
        let pid = p.id;
        // tick 2：玩家代码第一次执行（on_tick 右移 1.0；上拍落地钳制已站半径线）
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (31.0, 0.5),
            "玩家 on_tick 应真实执行（每拍右移 1.0，站半径线）"
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
            (31.0, 0.5),
            "存活期玩家代码应每拍右移 1.0（站半径线）"
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
            &[PendingAttack::strike(enemy, (30.0, 0.0), stun)],
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
        // 击杀刀打残影（空 payload = 默认击杀；#030 点命中：残影已落半径线 (30,0.5)）
        let enemy = make_entity_with_sandbox(&mut world, &mut map, 201, (29.0, 0.0), TARGET_WAT);
        process_attacks(
            &mut world,
            &mut map,
            &[PendingAttack::strike(enemy, (30.0, 0.5), vec![])],
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
            &[PendingAttack::strike(pid, (99.0, 99.0), vec![])],
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
            &[PendingAttack::strike(enemy, (30.0, 0.5), stun)],
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
    /// D-003：第 10 参 motion（默认 0=static）
    fn birth_spawner_wat(code: &str, x: &str, y: &str) -> String {
        birth_motion_spawner_wat(code, x, y, "i32.const 0")
    }

    /// 指定 motion 的造物方 WAT（D-003 spawn 第 10 参；本任务 14 参：
    /// 初速 vx/vy 第 11/12 参 + blob_ptr/blob_len 第 13/14 参默认零速无包）
    fn birth_motion_spawner_wat(code: &str, x: &str, y: &str, motion: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const {x} f64.const {y}
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    {motion}
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    call $spawn drop)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
            x = x,
            y = y,
            motion = motion,
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

    // === 出生参数批：出生初速 + 出生数据包（spawn 第 11-14 参全链路）===

    #[test]
    fn process_spawns_writes_birth_velocity_into_creation() {
        // 出生初速写进造物数据（velocity 字段所有实体都有）：static 造物钉死不吃
        // 速度物理，初速照存不裁（物理意义仅对 dynamic 造物生效——D-018 通用
        // 有速度实体物理的活，本刀只管"出生时把速度写进数据"）
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(21));
        world
            .entities
            .insert(creator, make_entity_of_owner(21, 70, (10.0, 0.0)));
        let mut map = test_sandbox_map();
        // 创造者沙箱（直构请求绕过 import 排队路径，直测阶段二落库）
        map.create(creator, &wasm(PLAYER_WAT), 1_000_000).unwrap();
        map.sandboxes
            .get_mut(&creator)
            .unwrap()
            .store
            .data_mut()
            .pending_spawns
            .push(codeworld_arena::sandbox::PendingSpawn {
                creator,
                pos: (12.0, 0.0),
                code: "(module (func (export \"life\") (result i32) i32.const 1))".to_string(),
                glyph: "T".to_string(),
                color: "cyan".to_string(),
                radius: 0.5,
                mass: 1.0,
                binding: 0,
                motion: 0,
                vx: 3.0,
                vy: -4.0,
                blob: Vec::new(),
                fuel_limit: 1_000_000,
            });
        process_spawns(&mut world, &mut map, 7);
        let creation = world
            .entities
            .values()
            .find(|e| e.code_hash == "creation")
            .expect("造物应诞生");
        assert_eq!(
            creation.velocity(),
            (3.0, -4.0),
            "出生初速应写进造物 velocity 字段"
        );
    }

    #[test]
    fn process_spawns_delivers_birth_blob_end_to_end() {
        // 端到端：创造者 spawn 带 blob + 初速排队 -> 阶段二造物诞生 ->
        // 造物下一拍 on_tick 经 env.birth_blob 原样读回字节（数据流两端可串：
        // spawn 排队 blob_len 日志 ↔ 造物诞生 ↔ birth_blob 读回日志）
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(22));
        world
            .entities
            .insert(creator, make_entity_of_owner(22, 71, (10.0, 0.0)));
        // 造物代码：on_tick 调 birth_blob(1024, 4096)，返回值可观测
        let creation_code = r#"(module
  (import "env" "birth_blob" (func $birth_blob (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (global $blob_ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    (global.set $blob_ret (call $birth_blob (i32.const 1024) (i32.const 4096))))
  (func (export "get_blob_ret") (result i32) (global.get $blob_ret))
  (func (export "life") (result i32) (i32.const 1)))"#;
        let blob: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42];
        let blob_escaped: String = blob.iter().map(|b| format!("\\{b:02x}")).collect();
        let escaped = wat_escape(creation_code);
        let spawner = format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (data (i32.const 8500) "{blob_escaped}")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 12.0 f64.const 0.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 3.0 f64.const -4.0
    i32.const 8500 i32.const 5
    call $spawn drop)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = creation_code.len(),
            blob_escaped = blob_escaped,
        );
        let mut map = test_sandbox_map();
        map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
        // 阶段一：创造者排队 spawn（带 blob + 初速）
        map.run_player_code(&world, &unbounded_bb());
        // 阶段二：造物诞生（初速/blob 应落进造物）
        process_spawns(&mut world, &mut map, 7);
        let creation = world
            .entities
            .values()
            .find(|e| e.code_hash == "creation")
            .expect("造物应诞生");
        assert_eq!(creation.velocity(), (3.0, -4.0), "初速应随造物落库");
        let creation_id = creation.id;
        // 造物下一拍自己跑 on_tick：birth_blob 读回 blob
        map.run_player_code(&world, &unbounded_bb());
        assert_eq!(
            call_i32_export(&mut map, &creation_id, "get_blob_ret"),
            5,
            "birth_blob 应返回 N=5（blob 已写入造物 memory）"
        );
        // 端到端字节一致：造物 memory [1024..1029) == 创造者 data 段携带的 blob
        let handle = map.sandboxes.get_mut(&creation_id).unwrap();
        let memory = handle
            .instance
            .get_memory(&mut handle.store, "memory")
            .expect("造物应导出 memory");
        assert_eq!(
            &memory.data(&handle.store)[1024..1029],
            &blob[..],
            "造物经 birth_blob 读回的字节应与出生数据包逐字节一致"
        );
        assert!(
            handle.store.data().api_calls.contains(&"birth_blob".to_string()),
            "birth_blob 调用应记入造物 api_calls 元数据"
        );
    }

    #[test]
    fn process_spawns_allows_adjacent_edge_touching_birth() {
        // 相邻不重叠边界锚（#030 严格 < 判据）：出生格 (12,0) 的 AABB 是
        // [12,13]×[0,1]（格对齐），占据者圆 r=0.5 圆心挪半格到 (13.5,0.5)
        // ——距格边恰 0.5 = 相切（严格 < 不满足）→ 不算重叠 → 正常诞生。
        // 与"出生格被占=拒绝"（上方 occupied 用例）构成边界两侧的双锚
        let mut world = WorldState::new();
        let creator = EntityId(Uuid::from_u128(23));
        world
            .entities
            .insert(creator, make_entity_of_owner(23, 72, (10.0, 0.0)));
        let occupant = make_entity_of_owner(24, 73, (13.5, 0.5));
        world.entities.insert(occupant.id, occupant);
        let mut map = test_sandbox_map();
        let spawner = birth_spawner_wat(CREATION_LOG_WAT, "12.0", "0.0");
        map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
        map.run_player_code(&world, &unbounded_bb());
        process_spawns(&mut world, &mut map, 7);
        assert!(
            world.entities.values().any(|e| e.code_hash == "creation"),
            "贴边相邻（相切不重叠）应正常诞生"
        );
        assert_eq!(world.entities.len(), 3, "1 创造者 + 1 占据者 + 1 造物");
    }

    // === D-003 motion 出生声明（spawn 第 10 参全链路）===

    #[test]
    fn process_spawns_dynamic_motion_birth_precheck_uses_circle() {
        // D-003 出生预检按声明档分流：dynamic 造物身体=r=0.5 的圆（圆-圆判据），
        // static 造物身体=出生格 AABB（圆-AABB 判据）。occupant 固定 (0,0.5)，
        // 造物出生 (d,0)：圆-圆重叠 ⟺ d²+0.25<1 ⟺ d<0.866；
        // 圆-AABB 重叠 ⟺ 圆心(0,0.5)到出生格[floor(d),floor(d)+1]钳距<0.5 ⟺ floor(d)=0 ⟺ d<1。
        // d=0.9 是分流核心证据：dynamic 放行（1.06>1.0）而 static 被挡（圆心贴盒边钳距 0）
        // ——dynamic 若误用 AABB 判据即被挡下，测试即红。
        for (dx, dyn_ok, label) in [
            (0.75, false, "0.75 圆判距内两档都挡"),
            (0.9, true, "0.9 圆外格内=分流证据"),
        ] {
            let mut world = WorldState::new();
            let creator = EntityId(Uuid::from_u128(4));
            world
                .entities
                .insert(creator, make_entity_of_owner(4, 60, (10.0, 0.0)));
            let occupant = make_entity_of_owner(5, 61, (0.0, 0.5)); // 圆 r=0.5，圆心(0,0.5)
            world.entities.insert(occupant.id, occupant);
            let mut map = test_sandbox_map();
            let spawner = birth_motion_spawner_wat(
                CREATION_LOG_WAT,
                &format!("{dx}"),
                "0.0",
                "i32.const 1",
            );
            map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
            map.run_player_code(&world, &unbounded_bb());
            process_spawns(&mut world, &mut map, 7);
            let born = world.entities.values().any(|e| e.code_hash == "creation");
            assert_eq!(born, dyn_ok, "dynamic 档 d={label} 出生结果应为 {dyn_ok}");
            if born {
                let c = world
                    .entities
                    .values()
                    .find(|e| e.code_hash == "creation")
                    .unwrap();
                assert_eq!(c.motion(), codeworld_core::entity::MOTION_DYNAMIC, "motion=1 应落库");
            }
        }
        // static 档分流对锚：d=1.2 被 AABB 挡下（dynamic 同点放行——两档判据确已分流）；
        // d=1.6 两档都放行（远处零回归）
        for (dx, born_expect, label) in [(0.9, false, "0.9 AABB覆盖内挡下"), (1.6, true, "1.6 两档判距外放行")] {
            let mut world = WorldState::new();
            let creator = EntityId(Uuid::from_u128(6));
            world
                .entities
                .insert(creator, make_entity_of_owner(6, 60, (10.0, 0.0)));
            let occupant = make_entity_of_owner(7, 61, (0.0, 0.5));
            world.entities.insert(occupant.id, occupant);
            let mut map = test_sandbox_map();
            let spawner = birth_spawner_wat(CREATION_LOG_WAT, &format!("{dx}"), "0.0");
            map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
            map.run_player_code(&world, &unbounded_bb());
            process_spawns(&mut world, &mut map, 7);
            let born = world.entities.values().any(|e| e.code_hash == "creation");
            assert_eq!(born, born_expect, "static 档 d={label} 出生结果应为 {born_expect}");
        }
    }

    #[test]
    fn process_spawns_dynamic_creation_falls_in_tick_physics() {
        // 全链路：motion=1 空中出生（y=5）-> 插入世界 -> 同拍 physics.step 圆管线
        // 一拍落到半径线 0.5（生物同款）；static 档同场景落地 y=0（AABB 贴地）。
        // 锚定 tick 内顺序：process_spawns(a1) 先于 physics.step(c)——出生拍即受物理
        for (motion_wat, want_y, label) in
            [("i32.const 1", 0.5, "dynamic 落半径线"), ("i32.const 0", 0.0, "static 落地面")]
        {
            let mut world = WorldState::new();
            let creator = EntityId(Uuid::from_u128(8));
            world
                .entities
                .insert(creator, make_entity_of_owner(8, 62, (0.0, 0.0)));
            let mut map = test_sandbox_map();
            let spawner =
                birth_motion_spawner_wat(CREATION_LOG_WAT, "3.0", "5.0", motion_wat);
            map.create(creator, &wasm(&spawner), 1_000_000).unwrap();
            map.run_player_code(&world, &unbounded_bb());
            process_spawns(&mut world, &mut map, 7);
            assert!(
                world.entities.values().any(|e| e.code_hash == "creation"),
                "motion={motion_wat} 造物应诞生"
            );
            let physics = DefaultPhysicsEngine::new();
            physics.step(&mut world, &std::collections::HashSet::new());
            let c = world
                .entities
                .values()
                .find(|e| e.code_hash == "creation")
                .unwrap();
            assert_eq!(
                c.pos().1, want_y,
                "{label}（D-003 出生拍即受物理）"
            );
        }
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
                radius: 0.5,
                mass: 1.0,
                binding: 0,
                motion: 0,
                vx: 0.0,
                vy: 0.0,
                blob: Vec::new(),
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
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32) (result i32)))
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
        (f64.const 0.5)
        (f64.const 1.0)
        (i32.const 0)
        (i32.const 0)
        (f64.const 0.0)
        (f64.const 0.0)
        (i32.const 0)
        (i32.const 0)
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

    // === D-007 三兄弟：on_contact 接触回调 + on_death 遗嘱钩子 + 够不着镜像锚 ===

    /// 接触记录模块（main 侧同款）：on_contact 记对方坐标 + 调用计数
    const CONTACT_RECORDER_WAT: &str = r#"(module
  (global $gx (mut f64) (f64.const -999))
  (global $gy (mut f64) (f64.const -999))
  (global $calls (mut i32) (i32.const 0))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_contact") (param $ox f64) (param $oy f64)
    local.get $ox global.set $gx
    local.get $oy global.set $gy
    global.get $calls i32.const 1 i32.add global.set $calls)
  (func (export "get_gx") (result f64) global.get $gx)
  (func (export "get_gy") (result f64) global.get $gy)
  (func (export "get_calls") (result i32) global.get $calls))"#;

    /// 遗嘱造物模块：life=0（首拍即死）+ on_death 排队造遗物（观测点：
    /// 死后世界出现 creation 实体=遗嘱被世界托管执行——遗物是死后仍可观测的证据）
    fn dying_will_spawn_wat(relic_x: &str, relic_y: &str) -> String {
        let relic_code = r#"(module (func (export "life") (result i32) i32.const 1))"#;
        let escaped = wat_escape(relic_code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64 i32 i32 f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "R\00")
  (data (i32.const 8256) "gold\00")
  (func (export "life") (result i32) i32.const 0)
  (func (export "on_death")
    i32.const 0 i32.const {len}
    f64.const {rx} f64.const {ry}
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    i32.const 0
    i32.const 0
    f64.const 0.0
    f64.const 0.0
    i32.const 0
    i32.const 0
    call $spawn drop))"#,
            escaped = escaped,
            len = relic_code.len(),
            rx = relic_x,
            ry = relic_y,
        )
    }

    /// 活体遗嘱模块：life=1（正常活着）+ on_death 排队造遗物——残影消散测试用
    ///（玩家文件必须 life=1，否则活不过一拍根本进不了残影路径）
    fn living_will_spawn_wat(relic_x: &str, relic_y: &str) -> String {
        let body = dying_will_spawn_wat(relic_x, relic_y);
        // 复用死体生成器，仅把 life 从 0 改回 1（遗物段完全一致，观测同构）
        body.replace(
            r#"(func (export "life") (result i32) i32.const 0)"#,
            r#"(func (export "life") (result i32) i32.const 1)"#,
        )
    }

    /// 遗嘱射击模块：life=0 + on_death 排队 shoot（烟花：遗嘱排队的弹当拍发射）
    const DYING_WILL_SHOOT_WAT: &str = r#"(module
  (import "env" "shoot" (func $shoot (param f64 f64 f64 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "life") (result i32) i32.const 0)
  (func (export "on_death")
    f64.const 3.0 f64.const 9.8 f64.const 1.0 i32.const 0 i32.const 0 i32.const 0
    call $shoot drop))"#;

    /// 遗嘱攻击模块：life=0 + on_death 排队 attack_at（死人不出刀：队列应丢弃）
    const DYING_WILL_ATTACK_WAT: &str = r#"(module
  (import "env" "attack_at" (func $attack (param f64 f64 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "life") (result i32) i32.const 0)
  (func (export "on_death")
    f64.const 3.0 f64.const 0.5 i32.const 0 i32.const 0
    call $attack drop))"#;

    /// 拦截计数防守模块（遗嘱攻击丢弃的观测点：on_attack 被问过才计数）
    const DEFEND_COUNTER_WAT: &str = r#"(module
  (global $calls (mut i32) (i32.const 0))
  (func (export "life") (result i32) i32.const 1)
  (func (export "on_attack") (param i32) (result i32)
    global.get $calls i32.const 1 i32.add global.set $calls
    i32.const 0)
  (func (export "get_calls") (result i32) global.get $calls))"#;

    #[test]
    fn shoot_reach_sim_ticks_mirror_physics() {
        // D-007 ①镜像约束锚：arena 侧够不着判定的弹道模拟寿命必须与 physics
        // 弹丸寿命上限（30 拍超时哑弹）同值——判定模拟的就是真实弹丸的一生
        assert_eq!(
            codeworld_arena::sandbox::SHOOT_REACH_SIM_TICKS,
            codeworld_physics::default::PROJECTILE_MAX_AGE_TICKS,
            "arena 弹道模拟寿命与 physics 弹丸寿命镜像（改一处必改另一处）"
        );
    }

    #[test]
    fn notify_contacts_both_sides_receive_opposite_coords() {
        // D-007 ②直接单测：新进入接触的对，双方都收通知且坐标=对方实况位置
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 60, (0.5, 0.5), CONTACT_RECORDER_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 61, (1.5, 0.5), CONTACT_RECORDER_WAT);
        let contacts = vec![ContactPair { a, b, pos: (0.5, 0.5), source: "touch" }];
        notify_contacts(&mut world, &mut map, &contacts);
        assert_eq!(call_i32_export(&mut map, &a, "get_calls"), 1, "a 应收到通知");
        assert_eq!(call_f64_export(&mut map, &a, "get_gx"), 1.5, "a 收到 b 的 x");
        assert_eq!(call_f64_export(&mut map, &a, "get_gy"), 0.5, "a 收到 b 的 y");
        assert_eq!(call_i32_export(&mut map, &b, "get_calls"), 1, "b 应收到通知");
        assert_eq!(call_f64_export(&mut map, &b, "get_gx"), 0.5, "b 收到 a 的 x");
        assert_eq!(call_f64_export(&mut map, &b, "get_gy"), 0.5, "b 收到 a 的 y");
    }

    #[test]
    fn notify_contacts_skips_pair_with_dead_side() {
        // 边界锚定：接触对一方死亡 -> 整对跳过（与 handle_collisions 死亡过滤同语义），
        // 活方不收通知、不 Err
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 62, (0.5, 0.5), CONTACT_RECORDER_WAT);
        let mut dead = make_entity(63, (1.5, 0.5));
        dead.kill();
        let b = dead.id;
        world.entities.insert(b, dead);
        let contacts = vec![ContactPair { a, b, pos: (0.5, 0.5), source: "touch" }];
        notify_contacts(&mut world, &mut map, &contacts);
        assert_eq!(call_i32_export(&mut map, &a, "get_calls"), 0, "含死亡方的对整对跳过");
    }

    #[test]
    fn notify_contacts_notifies_only_sandboxed_side() {
        // 边界锚定：一方无沙箱（框架实体）-> 该侧跳过，有沙箱的活方照常收通知，不 Err
        let mut world = WorldState::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 64, (0.5, 0.5), CONTACT_RECORDER_WAT);
        let bare = make_entity(65, (1.5, 0.5));
        let b = bare.id;
        world.entities.insert(b, bare);
        let contacts = vec![ContactPair { a, b, pos: (0.5, 0.5), source: "touch" }];
        notify_contacts(&mut world, &mut map, &contacts);
        assert_eq!(call_i32_export(&mut map, &a, "get_calls"), 1, "有沙箱侧照常收通知");
        assert!(!map.sandboxes.contains_key(&b), "无沙箱侧无沙箱可调（前置自证）");
    }

    #[test]
    fn contact_notification_is_edge_triggered_full_cycle() {
        // 边沿触发全周期（tick 级）：碰（双方各 1 次）-> 持续贴（不重复）->
        // 分开再碰（再触发）
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        let a = make_entity_with_sandbox(&mut world, &mut map, 66, (0.5, 0.5), CONTACT_RECORDER_WAT);
        let b = make_entity_with_sandbox(&mut world, &mut map, 67, (1.5, 0.5), CONTACT_RECORDER_WAT);
        // 拍 1：新进入接触 -> 双方各收 1 次
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert_eq!(call_i32_export(&mut map, &a, "get_calls"), 1, "拍 1 a 首触");
        assert_eq!(call_i32_export(&mut map, &b, "get_calls"), 1, "拍 1 b 首触");
        // 拍 2：持续贴着 -> 不再触发（防刷屏）
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert_eq!(call_i32_export(&mut map, &a, "get_calls"), 1, "持续贴不再触发");
        // 拍 3：a 搬走（SetPos 排队，阶段二 a 步应用）-> 分开
        pending.push(PendingChange::SetPos { entity: a, new_pos: (5.5, 0.5) });
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert_eq!(call_i32_export(&mut map, &a, "get_calls"), 1);
        // 拍 4：a 搬回 -> 分开后再碰再触发
        pending.push(PendingChange::SetPos { entity: a, new_pos: (0.5, 0.5) });
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert_eq!(call_i32_export(&mut map, &a, "get_calls"), 2, "分开再碰再触发");
        assert_eq!(call_i32_export(&mut map, &b, "get_calls"), 2, "b 侧同拍再触发");
    }

    #[test]
    fn contact_ghost_side_is_not_notified() {
        // 残影休眠不通知：残影是身体会被碰到（对成立），但其沙箱不被唤醒
        //（残影不执行玩家代码既有不变量）；活方照常收到残影坐标
        let dir = temp_players_dir("contact-ghost");
        std::fs::write(dir.join("willer.wat"), CONTACT_RECORDER_WAT).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let ghost_id = find_player_id(&world, "willer");
        std::fs::remove_file(dir.join("willer.wat")).unwrap();
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(map.players.is_ghost(&ghost_id), "前置：残影已登记");
        // 活方贴着残影（残影首进场 x=30 落地 y=0.5）
        let neighbor = make_entity_with_sandbox(&mut world, &mut map, 68, (31.0, 0.5), CONTACT_RECORDER_WAT);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert_eq!(call_i32_export(&mut map, &neighbor, "get_calls"), 1, "活方收到接触通知");
        assert_eq!(call_f64_export(&mut map, &neighbor, "get_gx"), 30.0, "对方坐标=残影实况位置");
        assert_eq!(call_f64_export(&mut map, &neighbor, "get_gy"), 0.5);
        assert_eq!(call_i32_export(&mut map, &ghost_id, "get_calls"), 0, "残影侧不被唤醒");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn killed_entity_runs_will_and_relic_spawns() {
        // D-007 ③验收：实体死亡（life=0）-> on_death 被世界托管调用 ->
        // 遗嘱排队的 spawn 放行（创造者已死例外，产权链照常）-> 死后世界出现遗物
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        let dying_id = EntityId(Uuid::from_u128(70));
        let dying = make_entity(70, (2.0, 0.5));
        map.create(dying_id, &wasm(&dying_will_spawn_wat("5.5", "0.5")), 1_000_000).unwrap();
        world.entities.insert(dying_id, dying);
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert!(!world.entities.contains_key(&dying_id), "死者应被死亡清理移除");
        assert!(!map.sandboxes.contains_key(&dying_id), "死者沙箱应销毁");
        let relic = world
            .entities
            .values()
            .find(|e| e.code_hash == "creation")
            .expect("遗嘱造物应存在（创造者已死放行，遗产归属生效）");
        assert_eq!(relic.pos(), (5.5, 0.5), "遗物出生在遗嘱指定位置");
    }

    #[test]
    fn will_shoot_fires_same_tick() {
        // 验收：遗嘱排队的 shoot 当拍发射（烟花）——弹丸实体+账本当拍出现
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        let dying_id = EntityId(Uuid::from_u128(71));
        let dying = make_entity(71, (2.0, 0.5));
        map.create(dying_id, &wasm(DYING_WILL_SHOOT_WAT), 1_000_000).unwrap();
        world.entities.insert(dying_id, dying);
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert!(!world.entities.contains_key(&dying_id), "死者应被移除");
        let fireworks = world
            .entities
            .values()
            .filter(|e| e.code_hash == codeworld_core::collision::PROJECTILE_CODE)
            .count();
        assert_eq!(fireworks, 1, "遗嘱烟花应当拍发射（弹丸实体已落地）");
        assert_eq!(map.projectiles.len(), 1, "在飞弹丸账本应有 1 条");
    }

    #[test]
    fn will_attack_discarded_dead_men_strike_no_blows() {
        // 验收：遗嘱排队的 attack 丢弃（死人不出刀）——目标无恙且 on_attack 未被问
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        let dying_id = EntityId(Uuid::from_u128(72));
        let dying = make_entity(72, (2.0, 0.5));
        map.create(dying_id, &wasm(DYING_WILL_ATTACK_WAT), 1_000_000).unwrap();
        world.entities.insert(dying_id, dying);
        let target = make_entity_with_sandbox(&mut world, &mut map, 73, (3.0, 0.5), DEFEND_COUNTER_WAT);
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert!(!world.entities.contains_key(&dying_id), "死者应被移除");
        assert!(world.entities.get(&target).unwrap().is_alive(), "目标应无恙");
        assert_eq!(call_i32_export(&mut map, &target, "get_calls"), 0, "死人不出刀：目标收件未发生");
    }

    #[test]
    fn dead_without_on_death_export_no_regression() {
        // 零回归锚：无 on_death 导出的死亡路径与现状完全一致（实体移除、无遗物、不 Err）
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        let dying_id = EntityId(Uuid::from_u128(74));
        let dying = make_entity(74, (2.0, 0.5));
        // 无 on_death 的普通死亡模块（零回归对照：只有 life 导出）
        map.create(dying_id, &wasm("(module (func (export \"life\") (result i32) i32.const 0))"), 1_000_000).unwrap();
        world.entities.insert(dying_id, dying);
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert!(!world.entities.contains_key(&dying_id), "死者照常移除");
        assert!(
            !world.entities.values().any(|e| e.code_hash == "creation"),
            "无遗嘱不应有遗物"
        );
    }

    #[test]
    fn ghost_disperse_does_not_trigger_will() {
        // 验收：残影消散不触发遗嘱（消散≠死亡谢幕）——遗嘱若被触发会留下遗物，
        // 消散后世界不应有任何 creation
        let dir = temp_players_dir("will-ghost");
        std::fs::write(dir.join("willer.wat"), living_will_spawn_wat("40.0", "0.5")).unwrap();
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        map.players = PlayerDirectory::new(&dir);
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        let pid = find_player_id(&world, "willer");
        std::fs::remove_file(dir.join("willer.wat")).unwrap();
        // 下线进残影（拍 2）+ 残影期 5 拍（拍 3..6 仍在）
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 5);
        assert!(world.entities.contains_key(&pid), "残影第 5 拍末仍应在世界");
        run_ticks(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map, 1);
        assert!(!world.entities.contains_key(&pid), "残影应消散");
        assert!(
            !world.entities.values().any(|e| e.code_hash == "creation"),
            "残影消散不触发遗嘱（无遗物）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_take_does_not_trigger_will() {
        // 登出 take 路径不触发遗嘱（会话级离场非死亡）：dispose_take kill 的实体
        // 落进 session_takes 集合，遗嘱窗口据此跳过（集合消费后清空）
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        let mut pending: Vec<PendingChange> = Vec::new();
        let mut map = test_sandbox_map();
        let taken_id = EntityId(Uuid::from_u128(75));
        let taken = make_entity(75, (2.0, 0.5));
        map.create(taken_id, &wasm(&dying_will_spawn_wat("9.5", "0.5")), 1_000_000).unwrap();
        world.entities.insert(taken_id, taken);
        world.entities.get_mut(&taken_id).unwrap().kill(); // dispose_take 同款 kill
        map.session_takes.insert(taken_id); // dispose_take 同款登记
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage, &mut pending, &mut map);
        assert!(!world.entities.contains_key(&taken_id), "被带走的角色照常移除");
        assert!(
            !world.entities.values().any(|e| e.code_hash == "creation"),
            "会话级离场不触发遗嘱（无遗物）"
        );
        assert!(map.session_takes.is_empty(), "session_takes 应被遗嘱窗口消费清空");
    }

    #[test]
    fn run_death_wills_clears_stale_session_take_entries() {
        // P2-1 收尾：战斗死亡（实体已 remove）后玩家登出 take——remove_online 返回
        // 旧 id，session_takes 登记永不进 dead_ids 的死条目，永久残留（无界增长）。
        // 语义定案："遗嘱窗口已过，未消费的登记全是死条目"——run_death_wills
        // 消费循环结束后 clear() 清场。本测试锚定残留路径：登记不在 dead_ids 里
        //（空表=无死者），条目照样被清。
        let mut map = test_sandbox_map();
        let stale = EntityId(Uuid::from_u128(77));
        map.session_takes.insert(stale); // 战斗死亡后的 take 残留（实体早已不在世界）
        let (will_shoots, will_spawns) = run_death_wills(&mut map, &[]);
        assert!(will_shoots.is_empty(), "无死者应无遗嘱产物（shoot）");
        assert!(will_spawns.is_empty(), "无死者应无遗嘱产物（spawn）");
        assert!(map.session_takes.is_empty(), "跨拍死条目应在遗嘱窗口后清场");
    }

    #[test]
    fn entity_sandbox_map_new_with_reach_physics_forwards_drag() {
        // P1-3 收尾装配锚：EntitySandboxMap::new_with_reach_physics 把装配阻力
        // 透传给 SandboxManager 够不着判定——同参数 lob，drag=0.05 够得着的
        // 快照在 drag=0.3 下改判够不着（数值预验证：min 距离 0.0 vs 1.8752，
        // 容差 0.65 两侧）。main() 装配点传 config.drag_coefficient，
        // CODEWORLD_DRAG 覆盖后判定与真实弹道不分叉
        let shooter_wat = r#"(module
  (import "env" "shoot" (func $shoot (param f64 f64 f64 i32 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    (f64.const 4.0) (f64.const 9.8) (f64.const 0.5) (i32.const 0) (i32.const 0) (i32.const 0)
    call $shoot global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;
        let verdict = |drag: f64, eid: u128| -> i32 {
            let mut map = EntitySandboxMap::new_with_reach_physics(25.0, drag).unwrap();
            let sid = EntityId(Uuid::from_u128(eid));
            map.create(sid, &wasm(shooter_wat), 100_000).unwrap();
            {
                let handle = map.sandboxes.get_mut(&sid).unwrap();
                handle.store.data_mut().pos = (0.0, 0.5);
                handle.store.data_mut().nearest_enemy = Some((3.5, 0.5));
            }
            map.manager
                .call_on_tick(map.sandboxes.get_mut(&sid).unwrap())
                .unwrap();
            call_i32_export(&mut map, &sid, "get_ret")
        };
        assert_eq!(
            verdict(0.05, 78),
            1,
            "drag=0.05（默认档）3.5 格快照应够得着（正序号，现状不变）"
        );
        assert_eq!(
            verdict(0.3, 79),
            -1,
            "drag=0.3 同参数应改判够不着（警告码 -N）——装配阻力必须透传"
        );
    }
}
