//! WASM 沙箱管理：SandboxManager/SandboxHandle/SandboxData
//!
//! 架构文档六节（WASM 沙箱设计）+ 6.3（安全防线）。
//! 基于 wasmtime，启用 fuel 指令计量。spike/route_a 已验证 API。

use anyhow::Result;
use std::collections::HashMap;
use uuid::Uuid;
use wasmtime::{Caller, Config, Engine, Instance, Linker, Module, Store};

use crate::attack::PendingAttack;
use crate::channel::ChannelManager;
use codeworld_core::entity::EntityId;
use codeworld_core::pending::PendingChange;

/// attack payload 最大字节数（1MB，架构 4.2 技术保护）
///
/// import 层拒绝超限请求：防一次性拷贝大块线性内存（最大可到 64MB）+
/// 超大模块编译长时间阻塞 tick（编译不受 fuel 约束）。
pub const MAX_PAYLOAD_LEN: usize = 1024 * 1024;

/// 造物出生点相对创造者的最大偏移（门二 spawn 位置起点限制）
///
/// 出生坐标必须在创造者快照坐标 ±10 格（每轴盒式判定）内——防远程投射刷屏。
/// MVP 一刀切的简单规则；规则由宪法允许后续玩法化（decisions.md #012）。
pub const SPAWN_RANGE: f64 = 10.0;

/// shoot 初速模长上限（#034 修订一：MAX_SHOOT_SPEED=50 硬编码退役——发射钳制
/// 改用世界极速常数 WORLD_MAX_SPEED（默认 25，视觉可读性常数），经
/// SandboxManager::new_with_max_speed 装配，与飞行中钳制（DefaultPhysics.max_speed）
/// 同源一个世界常数。此常量仅作 new() 的缺省值）
pub const MAX_SHOOT_SPEED: f64 = codeworld_core::physics::WORLD_MAX_SPEED;

/// spawn 外观字符串（glyph/color，NUL 结尾）最大字节数
///
/// import 层扫描上限：内存里没有 NUL 时最多读这么多字节就拒绝，
/// 防无界扫描线性内存。
pub const MAX_META_LEN: usize = 64;

/// 本 tick 排队的 spawn 请求（env.spawn 推入，阶段二收集统一处理）
///
/// import 层只做零 world 校验（限流/有限性/钻地/±10 格/外观格式），
/// 产权链 owner 继承与世界实体上限在阶段二判（import 拿不到 world）。
#[derive(Debug, Clone)]
pub struct PendingSpawn {
    /// 创造者（沙箱所属实体）
    pub creator: EntityId,
    /// 造物出生坐标（import 层已校验：有限、y>=0、创造者快照 ±10 格内）
    pub pos: (f64, f64),
    /// 造物 WAT 源码文本（阶段二编译；编译失败作废不崩世界）
    pub code: String,
    /// 造物外观字形（import 层已校验：恰好 1 个字符）
    pub glyph: String,
    /// 造物外观颜色（import 层已校验：非空）
    pub color: String,
    /// 身体半径（#030 spawn 第 7 参；import 层已校验钳制 0.05..=25.0；
    /// creation 身体=占地格 AABB 不用它，快照协议携带）
    pub radius: f64,
    /// 质量（#034 spawn 第 8 参；import 层已钳制 0.05..=100.0，物理人格属性）
    pub mass: f64,
    /// 造物 fuel 配额（继承创造者沙箱的 fuel_limit）
    pub fuel_limit: u64,
}

/// 本 tick 排队的射击请求（#034 env.shoot 推入，阶段二收集统一处理）
///
/// import 层只做零 world 校验（限流 1 发/tick、速度模长 ≤50、mass 钳制、
/// payload ≤1MB）；发射位置在阶段二读发射者当拍坐标（阶段一快照坐标会被
/// move_by 移动更新，从哪里出手以移动后的实况为准——与攻击坐标解析同哲学）。
#[derive(Debug, Clone)]
pub struct PendingShoot {
    /// 发射者（沙箱所属实体）
    pub shooter: EntityId,
    /// 初速 x 分量（import 层已校验：有限、模长 ≤50）
    pub vx: f64,
    /// 初速 y 分量
    pub vy: f64,
    /// 弹丸质量（import 层已钳制 0.05..=100.0）
    pub mass: f64,
    /// 弹丸携带的攻击字节（同 attack_at payload 语义：空=默认击杀模块）
    pub payload: Vec<u8>,
}

/// 本 tick 排队的 destroy 请求（env.destroy 推入，阶段二产权校验后应用）
///
/// 产权约束（宪法 #015）：MVP 只允许销毁自家造物（owner 匹配，阶段二用 world 判）；
/// 击杀掠夺式的销毁走攻击通路，不归 destroy（destroy 无掠夺）。
#[derive(Debug, Clone)]
pub struct PendingDestroy {
    /// 销毁发起者
    pub destroyer: EntityId,
    /// 目标实体（i64 两半重建）
    pub target: EntityId,
}

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
    /// 最近敌人的坐标快照（host 每 tick 阶段一【前】填充；None=视野内没有其他存活实体，
    /// env.scan 返回 0）。填充按渲染同款 viewport 过滤（架构 4.2"视距内"前置条件：
    /// 屏幕外不可锁定；超视距手段玩家自行构建，#018），单敌人取最近者
    /// （架构 3.2 感知接口的最简版）。
    pub nearest_enemy: Option<(f64, f64)>,
    /// 本 tick 排队的移动请求（env.move_by 推入 PendingChange::SetPos，
    /// 阶段二收集后并入主 pending 队列统一应用，与攻击排队同模式）
    pub pending_moves: Vec<PendingChange>,
    /// 本 tick env.log 的 marker 值序列（按调用顺序记录）。
    /// 战况字幕的盾量上报通路：玩家代码主动 log（如 D 拦截时 log(7) 后跟 log(余量)），
    /// host 侧解析该序列更新字幕——玩家自愿上报，不破坏沙箱隔离（架构 4.2 铁律：
    /// 字幕=渲染层展示，只读，不写回 world）。
    pub log_markers: Vec<i32>,
    /// 可见实体坐标快照（三层感知 Phase A 眼睛列表，架构 3.2 get_visible_entities
    /// 的 import 数据通路）：host 每 tick 阶段一执行玩家代码【前】按 viewport 过滤
    /// 存活实体填充，EntityId 升序稳定排序、排除自己（env.visible_count/visible_at
    /// 的数据源；import 回调拿不到 world，同 pos/nearest_enemy 快照模式）。
    pub visible: Vec<(f64, f64)>,
    /// 本 tick 排队的造物请求（env.spawn 推入，阶段二收集后 drain 清空；
    /// 每实体每 tick 限流 1 个在 import 层保证）
    pub pending_spawns: Vec<PendingSpawn>,
    /// 本 tick 排队的销毁请求（env.destroy 推入，阶段二产权校验后 drain 清空）
    pub pending_destroys: Vec<PendingDestroy>,
    /// 本 tick 排队的射击请求（#034 env.shoot 推入，阶段二收集后 drain 清空；
    /// 每实体每拍限 1 发在 import 层保证）
    pub pending_shoots: Vec<PendingShoot>,
    /// 该沙箱累计已成功排队的射击发数（#034 shoot 返回值：弹丸序号，跨拍递增）
    pub shots_fired: i32,
    /// 本 tick 的悬停声明（env.hover 置位，host 阶段一收件并入免重力集合后清空；
    /// 每拍重申语义——玩家代码要持续对抗世界规则，上一拍声明不延续）
    pub hover_declared: bool,
    /// probe 通路桥指针（三层感知 Phase A，见 ProbeBridge 的 SAFETY 契约）。
    /// 仅在主循环阶段一执行玩家代码的窗口内非空（挂载 -> call -> 置空同一循环体
    /// 作用域），窗口外恒为 null——import 层判空作废探测。
    pub probe_bridge: *mut ProbeBridge,
}

impl SandboxData {
    /// 创建空沙箱数据
    pub fn new() -> Self {
        Self::default()
    }
}

/// 发射痕迹摘要（#034 修订三：痕迹不是实体不进沙箱表，probe 桥携带的最小证据）
#[derive(Debug, Clone, Copy)]
pub struct TraceProbe {
    /// 痕迹 id（与 visible_ids 中该痕迹的条目对应）
    pub trace_id: EntityId,
    /// 发射者
    pub shooter: EntityId,
    /// 发射拍号
    pub birth_tick: u64,
}

/// probe 通路桥（三层感知 Phase A：import 闭包访问目标沙箱的桥接上下文）
///
/// 为什么需要：import 闭包是 'static 的，无法持有 host 引用；probe 又必须同步
/// 调用目标沙箱的 on_probe（玩家代码要当场拿到响应，不能像 attack_at 那样排队）。
/// 桥接方案：主循环阶段一执行玩家代码的窗口内，把窗口上下文挂到每个沙箱的
/// SandboxData.probe_bridge，窗口结束置空。
///
/// # SAFETY 契约（挂载方必须遵守）
/// - **悬垂**：probe_bridge 仅在 host 栈上 ProbeBridge 值存活期间非空（挂载 ->
///   call -> 置空发生在同一循环体作用域），窗口外恒为 null，import 层判空作废。
/// - **aliasing**：caller 自身的 `&mut SandboxHandle` 由外层循环持有；桥里的
///   sandboxes 指针只访问**其他实体**的 entry——坐标查找已排除 caller_id，
///   HashMap 不同 entry 的值指向互不重叠的独立分配。
pub struct ProbeBridge {
    /// 可见实体 id 列表（viewport 过滤 + 存活 + EntityId 升序；含 caller 自己——
    /// probe 的坐标查找在此列表上排除自己，作为 host 快照遗漏时的第二道防线。
    /// #034 修订三起也包含发射痕迹的条目——痕迹坐标后追加）
    pub visible_ids: Vec<EntityId>,
    /// 与 visible_ids 一一平行的坐标列表
    pub visible_positions: Vec<(f64, f64)>,
    /// 沙箱表指针（仅阶段一窗口内有效）
    pub sandboxes: *mut HashMap<EntityId, SandboxHandle>,
    /// 沙箱管理器指针（call_on_probe 经由它走既有 probe 通道，仅窗口内有效）
    pub manager: *const SandboxManager,
    /// 探测者 id（坐标查找排除自己 + 日志）
    pub caller_id: EntityId,
    /// on_probe 独立小 fuel 预算（架构 3.3 probe_target_instruction_limit，config 可调）
    pub probe_fuel: u64,
    /// 本拍可见的发射痕迹（#034 修订三；probe 命中痕迹坐标返回发射者身份，
    /// 与沙箱表无交集——痕迹不建沙箱）
    pub traces: Vec<TraceProbe>,
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
    /// 创建管理器：初始化 Engine（启用 fuel）和 Linker（注册 env.log）。
    /// 世界极速用缺省常数 WORLD_MAX_SPEED（测试与默认装配用）
    pub fn new() -> Result<Self> {
        Self::new_with_max_speed(codeworld_core::physics::WORLD_MAX_SPEED)
    }

    /// 创建管理器（指定世界极速，#034 修订一）：发射钳制与飞行钳制同源。
    /// 极速值经 move 闭包进 shoot import，管理器本体不再存副本
    pub fn new_with_max_speed(max_speed: f64) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        // 内存限制：每个 WASM 实例最大 64MB（CLAUDE.md 技术约束）
        // static_memory_maximum_size 设静态内存上限；完整动态内存硬上限用 ResourceLimiter（Phase 2）
        config.static_memory_maximum_size(64 * 1024 * 1024);
        let engine = Engine::new(&config)?;
        let linker = Self::make_linker(&engine, max_speed)?;
        tracing::debug!(max_speed, "SandboxManager 创建");
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
    fn make_linker(engine: &Engine, max_speed: f64) -> Result<Linker<SandboxData>> {
        let mut linker = Linker::new(engine);
        // env.log(v)：玩家代码日志（v 为玩家自定义 marker，含义见各生物 WAT 注释）。
        // 记 api_calls 元数据 + marker 值序列（log_markers，host 字幕解析用）
        // + tracing::info 输出到 stderr（战斗节点可见性）。
        linker.func_wrap("env", "log", |mut caller: Caller<SandboxData>, v: i32| {
            let data = caller.data_mut();
            data.api_calls.push("log".to_string());
            data.log_markers.push(v);
            tracing::info!(
                entity_id = %data.entity_id.0,
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
                    tracing::debug!(entity_id = %entity.0, "[scan] 视野内无目标");
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
        // env.hover()：玩家代码声明"本拍我要悬停"（期四第七刀）。无参无返回，只置位
        // SandboxData.hover_declared——host 阶段一执行完玩家代码后收集声明并入本拍
        // 免重力集合（与操控悬停同集合同机制，decisions.md #030 升力独立）。
        // 每拍重申语义：声明不跨拍延续，玩家代码要悬停就得每拍出力声明（#029 无税，
        // 但"持续对抗世界规则"以代码持续声明体现）。
        linker.func_wrap("env", "hover", |mut caller: Caller<SandboxData>| {
            let entity = caller.data().entity_id;
            caller.data_mut().api_calls.push("hover".to_string());
            caller.data_mut().hover_declared = true;
            tracing::info!(
                entity_id = %entity.0,
                "[hover] 悬停声明（本拍免重力，每拍需重申）"
            );
        })?;
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
                caller
                    .data_mut()
                    .pending_attacks
                    .push(PendingAttack::strike(attacker, (x, y), payload));
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
        // env.visible_count() -> i32：眼睛列表（三层感知 Phase A，架构 3.2
        // get_visible_entities 的 import 数据通路）。返回 host 填充的可见实体
        // 坐标快照长度（viewport 过滤 + EntityId 升序 + 排除自己）。
        linker.func_wrap("env", "visible_count", |mut caller: Caller<SandboxData>| -> i32 {
            caller.data_mut().api_calls.push("visible_count".to_string());
            let n = caller.data().visible.len() as i32;
            tracing::debug!(
                entity_id = %caller.data().entity_id.0,
                count = n,
                "[visible_count] 可见敌人数"
            );
            n
        })?;
        // env.visible_at(idx, x_ptr, y_ptr) -> i32：按 idx 取第 N 个可见实体坐标
        //（写 caller memory，同 scan 模式：x 8字节@x_ptr、y 8字节@y_ptr）。
        // 返回 1 成功 / 0 越界（idx 非法）或写入指针非法。
        linker.func_wrap(
            "env",
            "visible_at",
            |mut caller: Caller<SandboxData>, idx: i32, x_ptr: i32, y_ptr: i32| -> i32 {
                caller.data_mut().api_calls.push("visible_at".to_string());
                let entity = caller.data().entity_id;
                // idx<0 as usize 符号扩展成巨大值 -> get 返回 None，与越界同语义
                let Some(p) = caller.data().visible.get(idx as usize).copied() else {
                    tracing::debug!(
                        entity_id = %entity.0,
                        idx,
                        "[visible_at] 越界，无该可见实体"
                    );
                    return 0;
                };
                // 原子语义预检：两个指针先全量校验（只查不写），全部合法才落笔
                //（同 scan：避免其一非法时另一半坐标已写入的半截副作用）
                let ptrs_ok = can_write_f64_to_caller(&mut caller, x_ptr)
                    && can_write_f64_to_caller(&mut caller, y_ptr);
                if !ptrs_ok {
                    tracing::warn!(
                        entity_id = %entity.0,
                        idx,
                        x_ptr,
                        y_ptr,
                        "[visible_at] 写入指针非法，读取作废"
                    );
                    return 0;
                }
                write_f64_to_caller(&mut caller, x_ptr, p.0);
                write_f64_to_caller(&mut caller, y_ptr, p.1);
                tracing::info!(
                    entity_id = %entity.0,
                    idx,
                    x = p.0,
                    y = p.1,
                    "[visible_at] 读取可见实体坐标"
                );
                1
            },
        )?;
        // env.probe(tx, ty, req_ptr, req_len, resp_ptr, resp_cap) -> i32：探测
        //（三层感知 Phase B，架构 3.2 probe 通道 + 4.3 被动观测）。
        // - 按坐标在可见快照（桥）里找目标（架构 3.2 通道开启条件：视距内），
        //   排除自己；MVP 不引入 EntityId 传递（玩家代码持有坐标即可）。
        // - 调 channel.rs 既有 probe 通道（on_probe + meta 采集 + 容错），复用不重写。
        // - 被探测方 on_probe 用 host 侧独立小 fuel 包裹（存剩余 -> set 预算 ->
        //   调 -> 记消耗 -> 恢复原 fuel；已知限制：wasmtime 25 + Windows 下 fuel
        //   耗尽 trap = 进程 abort（.cc-bugs #5），预算把最坏情形缩到快速中止）。
        // - response 之后追加 24 字节被动体徵（host 采集，玩家不可伪造）：
        //   [0..8] 目标 api_calls 计数 u64 LE；[8..16] 目标 fuel 剩余 u64 LE
        //   （探测前快照）；[16..24] 本次 on_probe 消耗 u64 LE。
        // - 返回：>=0 实际写入字节数（超 resp_cap 截断）；-1 坐标无实体/桥未挂载/
        //   resp 不可写；-2 目标无 on_probe（响应位置已写默认被动体徵）。
        linker.func_wrap(
            "env",
            "probe",
            |mut caller: Caller<SandboxData>,
             tx: f64,
             ty: f64,
             req_ptr: i32,
             req_len: i32,
             resp_ptr: i32,
             resp_cap: i32|
             -> i32 {
                caller.data_mut().api_calls.push("probe".to_string());
                let caller_id = caller.data().entity_id;
                let bridge_ptr = caller.data().probe_bridge;
                if bridge_ptr.is_null() {
                    tracing::warn!(entity_id = %caller_id.0, "[probe] 通路桥未挂载，探测作废");
                    return -1;
                }
                // SAFETY: 桥仅在 run_player_code 执行窗口内非空（挂载 -> call ->
                // 置空同一循环体作用域），本函数只在窗口内被调，ProbeBridge 存活。
                let bridge = unsafe { &mut *bridge_ptr };
                // 坐标查找（可见列表内 + 排除自己：host 快照已排除，此处为第二道防线）。
                // #034 修订三：同坐标实体与痕迹并存时实体优先（痕迹作兜底证据）
                let is_trace_id =
                    |id: &EntityId| bridge.traces.iter().any(|t| &t.trace_id == id);
                let coordinate_match = |(id, p): (&EntityId, (f64, f64))| {
                    *id != caller_id && p == (tx, ty)
                };
                let target_id = bridge
                    .visible_ids
                    .iter()
                    .zip(bridge.visible_positions.iter().copied())
                    .find(|(id, p)| coordinate_match((id, *p)) && !is_trace_id(id))
                    .or_else(|| {
                        bridge
                            .visible_ids
                            .iter()
                            .zip(bridge.visible_positions.iter().copied())
                            .find(|(id, p)| coordinate_match((id, *p)) && is_trace_id(id))
                    })
                    .map(|(id, _)| *id);
                let Some(target_id) = target_id else {
                    tracing::info!(entity_id = %caller_id.0, tx, ty, "[probe] 坐标无可见实体");
                    return -1;
                };
                // 痕迹证据分支（#034 修订三）：坐标上是痕迹不是实体——不建沙箱，
                // 直接构造 24 字节身份响应（发射者 id u128 LE + 发射拍号 u64 LE），
                // 返回 -3（与 -1 无实体 / -2 无 on_probe 同族负值语义区分）
                if let Some(trace) = bridge.traces.iter().find(|t| t.trace_id == target_id) {
                    let mut body = Vec::with_capacity(24);
                    body.extend_from_slice(&trace.shooter.0.as_u128().to_le_bytes());
                    body.extend_from_slice(&trace.birth_tick.to_le_bytes());
                    if resp_cap <= 0 {
                        tracing::warn!(entity_id = %caller_id.0, resp_cap, "[probe] resp_cap 非正，痕迹证据丢弃");
                        return -1;
                    }
                    let written = write_bytes_to_caller(&mut caller, resp_ptr, &body, resp_cap as usize);
                    if written == 0 {
                        tracing::warn!(entity_id = %caller_id.0, resp_ptr, "[probe] 痕迹证据写入失败（指针非法/无 memory）");
                        return -1;
                    }
                    tracing::info!(
                        prober = %caller_id.0,
                        trace = %trace.trace_id.0,
                        shooter = %trace.shooter.0,
                        birth_tick = trace.birth_tick,
                        tx,
                        ty,
                        "[probe] 命中发射痕迹：返回发射者身份（攻击留下痕迹）"
                    );
                    return -3;
                }
                // SAFETY: sandboxes 指针窗口内有效；目标 != caller（上一步排除），
                // 不会与外层持有的 caller 自身 &mut handle aliasing（不同 entry）。
                let sandboxes = unsafe { &mut *bridge.sandboxes };
                let Some(target) = sandboxes.get_mut(&target_id) else {
                    tracing::warn!(
                        entity_id = %caller_id.0,
                        target = %target_id.0,
                        "[probe] 目标无沙箱（死者或框架实体）"
                    );
                    return -1;
                };
                // SAFETY: manager 指针窗口内有效（SandboxManager 与沙箱表同生命周期）。
                let manager = unsafe { &*bridge.manager };
                // request 字节从 caller memory 读（同 attack_at 的 payload 模式）
                let request = read_payload_from_caller(&mut caller, req_ptr, req_len);
                // host 侧独立小 fuel 包裹（架构 3.3 probe_target_instruction_limit）
                let prev_fuel = target.store.get_fuel().unwrap_or(0);
                if let Err(e) = target.store.set_fuel(bridge.probe_fuel) {
                    tracing::warn!(
                        target = %target_id.0,
                        error = %e,
                        "[probe] 预算设置失败，探测作废"
                    );
                    return -1;
                }
                let had_on_probe = manager.has_export(target, "on_probe").unwrap_or(false);
                let probe_result = ChannelManager::probe(manager, target, &request);
                let fuel_left = target.store.get_fuel().unwrap_or(bridge.probe_fuel);
                let probe_fuel_used = bridge.probe_fuel.saturating_sub(fuel_left);
                if let Err(e) = target.store.set_fuel(prev_fuel) {
                    tracing::warn!(target = %target_id.0, error = %e, "[probe] 目标 fuel 恢复失败");
                }
                // update_fuel 在包裹期间用预算尺度算过 fuel_consumed（假象），
                // 恢复后按原尺度重算，目标元数据不被探测污染
                let limit = target.store.data().fuel_limit;
                target.store.data_mut().fuel_consumed = limit.saturating_sub(prev_fuel);
                // 组装响应：目标 on_probe 返回 + 24 字节被动体徵
                let mut body = probe_result.response.clone();
                let api_count = probe_result.meta.api_calls.len() as u64;
                body.extend_from_slice(&api_count.to_le_bytes());
                body.extend_from_slice(&prev_fuel.to_le_bytes());
                body.extend_from_slice(&probe_fuel_used.to_le_bytes());
                if resp_cap <= 0 {
                    tracing::warn!(entity_id = %caller_id.0, resp_cap, "[probe] resp_cap 非正，响应丢弃");
                    return -1;
                }
                let written = write_bytes_to_caller(&mut caller, resp_ptr, &body, resp_cap as usize);
                if written == 0 {
                    tracing::warn!(
                        entity_id = %caller_id.0,
                        resp_ptr,
                        "[probe] 响应写入失败（指针非法/无 memory）"
                    );
                    return -1;
                }
                // response 摘要（前 4 字节 i32 hex；玩家内容照录——框架只透传不审查，
                // 撒谎是否成立由探测者比对 response 与体徵自行判断）
                let resp_head = if probe_result.response.len() >= 4 {
                    format!("0x{:08X}", i32::from_le_bytes(
                        probe_result.response[..4].try_into().unwrap_or([0; 4]),
                    ))
                } else if probe_result.response.is_empty() {
                    "-".to_string()
                } else {
                    format!("{} 字节", probe_result.response.len())
                };
                tracing::info!(
                    prober = %caller_id.0,
                    target = %target_id.0,
                    resp_len = written,
                    resp_head = %resp_head,
                    api_calls = api_count,
                    fuel_used = probe_fuel_used,
                    "[probe] 探测完成（response+被动体徵）"
                );
                if had_on_probe {
                    written as i32
                } else {
                    tracing::info!(
                        target = %target_id.0,
                        "[probe] 目标无 on_probe，仅返回默认被动体徵"
                    );
                    -2
                }
            },
        )?;
        // env.spawn(code_ptr, code_len, x, y, glyph_ptr, color_ptr, radius, mass) -> i32：造物
        //（门二开放，world-foundation 门二 + decisions.md #012 能力统一：造物再造造物合法）。
        // 玩家代码提交一段 WAT 文本 + 出生坐标 + 外观，请求造一个新实体。
        // import 层只做零 world 校验（同 move_by 模式：快照坐标 host 填充，玩家伪造不了）：
        //   - 限流：每实体每 tick 最多 1 个请求，多余拒绝
        //   - 代码：len>0 且 <= MAX_PAYLOAD_LEN（编译不受 fuel 约束，防超大模块阻塞 tick），
        //     从 memory [ptr..ptr+len) 读 UTF-8 文本
        //   - 出生点：x/y 有限、y>=0（不钻地）、创造者快照 ±10 格内（SPAWN_RANGE，
        //     防远程投射刷屏；规则由宪法允许后续玩法化）
        //   - 外观：glyph/color 从 memory 读 NUL 结尾字符串（≤ MAX_META_LEN），
        //     glyph 恰 1 字符、color 非空
        //   - radius 第 7 参钳制 0.05..=25.0（#030）；mass 第 8 参钳制 0.05..=100.0
        //     （#034 物理人格属性，除零护栏）
        // 产权链 owner 继承、世界实体上限、WAT 编译都在阶段二判（返回 1=已排队，
        // 阶段二拒绝以日志为准，api.md 载明）。
        linker.func_wrap(
            "env",
            "spawn",
            |mut caller: Caller<SandboxData>,
             code_ptr: i32,
             code_len: i32,
             x: f64,
             y: f64,
             glyph_ptr: i32,
             color_ptr: i32,
             radius: f64,
             mass: f64|
             -> i32 {
                caller.data_mut().api_calls.push("spawn".to_string());
                let creator = caller.data().entity_id;
                // 限流：每实体每 tick 最多 1 个造物请求
                if !caller.data().pending_spawns.is_empty() {
                    tracing::warn!(
                        entity_id = %creator.0,
                        "[spawn] 本 tick 已有造物请求，限流拒绝（1 个/tick）"
                    );
                    return 0;
                }
                // 代码长度预检：在读 memory 前拦下超限/非法长度（同 attack_at）
                if code_len <= 0 || code_len as usize > MAX_PAYLOAD_LEN {
                    tracing::warn!(
                        entity_id = %creator.0,
                        code_len,
                        max = MAX_PAYLOAD_LEN,
                        "[spawn] 造物代码长度非法，拒绝"
                    );
                    return 0;
                }
                let code_bytes = read_payload_from_caller(&mut caller, code_ptr, code_len);
                let Ok(code) = String::from_utf8(code_bytes) else {
                    tracing::warn!(
                        entity_id = %creator.0,
                        "[spawn] 造物代码非 UTF-8 文本，拒绝"
                    );
                    return 0;
                };
                // 出生点校验：有限 + 不钻地 + 创造者快照 ±10 格内
                if !x.is_finite() || !y.is_finite() {
                    tracing::warn!(entity_id = %creator.0, x, y, "[spawn] 出生点非有限，拒绝");
                    return 0;
                }
                if y < 0.0 {
                    tracing::warn!(entity_id = %creator.0, y, "[spawn] 出生点钻地（y<0），拒绝");
                    return 0;
                }
                let (px, py) = caller.data().pos;
                if (x - px).abs() > SPAWN_RANGE || (y - py).abs() > SPAWN_RANGE {
                    tracing::warn!(
                        entity_id = %creator.0,
                        creator_pos = ?(px, py),
                        birth = ?(x, y),
                        range = SPAWN_RANGE,
                        "[spawn] 出生点超出创造者 ±10 格，拒绝"
                    );
                    return 0;
                }
                // 外观校验：glyph 恰 1 字符、color 非空（NUL 结尾字符串从 memory 读）
                let Some(glyph) = read_cstring_from_caller(&mut caller, glyph_ptr, MAX_META_LEN)
                else {
                    tracing::warn!(
                        entity_id = %creator.0,
                        glyph_ptr,
                        "[spawn] 字形读取失败（指针非法/无 NUL/非 UTF-8），拒绝"
                    );
                    return 0;
                };
                if glyph.chars().count() != 1 {
                    tracing::warn!(
                        entity_id = %creator.0,
                        glyph = %glyph,
                        "[spawn] 字形必须是单字符，拒绝"
                    );
                    return 0;
                }
                let Some(color) = read_cstring_from_caller(&mut caller, color_ptr, MAX_META_LEN)
                else {
                    tracing::warn!(
                        entity_id = %creator.0,
                        color_ptr,
                        "[spawn] 颜色读取失败（指针非法/无 NUL/非 UTF-8），拒绝"
                    );
                    return 0;
                };
                if color.is_empty() {
                    tracing::warn!(entity_id = %creator.0, "[spawn] 颜色为空，拒绝");
                    return 0;
                }
                // fuel 配额继承创造者（造物与创造者同算力档）
                let fuel_limit = caller.data().fuel_limit;
                // #030 第 7 参 radius 校验钳制（0.05..=25.0，非有限回退 0.5，超界 warn）
                let radius = codeworld_core::collision::clamp_radius(radius);
                // #034 第 8 参 mass 校验钳制（0.05..=100.0，非有限回退 1.0，超界 warn）
                let mass = codeworld_core::collision::clamp_mass(mass);
                tracing::info!(
                    creator = %creator.0,
                    birth = ?(x, y),
                    glyph = %glyph,
                    color = %color,
                    radius,
                    mass,
                    code_len = code.len(),
                    "[spawn] 造物请求已排队（阶段二统一处理）"
                );
                caller
                    .data_mut()
                    .pending_spawns
                    .push(PendingSpawn {
                        creator,
                        pos: (x, y),
                        code,
                        glyph,
                        color,
                        radius,
                        mass,
                        fuel_limit,
                    });
                1
            },
        )?;
        // env.shoot(vx, vy, mass, payload_ptr, payload_len) -> i32：射击（#034 第 12 个
        // 玩家 import，弹道系统）：从调用者当前位置发射弹丸。初速/质量由代码定，
        // payload 同 attack_at 语义（空=默认击杀模块）。离体即世界物体（#034 立法第 1 条）。
        // import 层只做零 world 校验（排队模式同 attack_at）：
        //   - 限流：每实体每 tick 最多 1 发（防弹幕灌爆）
        //   - 速度：vx/vy 有限且模长 ≤ 50（防一拍穿世界）
        //   - mass：钳制 0.05..=100.0（除零护栏，#034）
        //   - payload：len > MAX_PAYLOAD_LEN 直接拒绝（同 attack_at）
        //   - 返回：弹丸序号（该实体累计第几发，1 起）或 0=拒绝。
        //     发射位置与世界实体上限在阶段二判（import 拿不到 world）。
        linker.func_wrap(
            "env",
            "shoot",
            move |mut caller: Caller<SandboxData>,
                  vx: f64,
                  vy: f64,
                  mass: f64,
                  payload_ptr: i32,
                  payload_len: i32|
                  -> i32 {
                caller.data_mut().api_calls.push("shoot".to_string());
                let shooter = caller.data().entity_id;
                // 限流：同实体一拍至多 1 发
                if !caller.data().pending_shoots.is_empty() {
                    tracing::warn!(
                        entity_id = %shooter.0,
                        "[shoot] 本 tick 已射击，限流拒绝（1 发/tick）"
                    );
                    return 0;
                }
                // 速度校验：有限 + 模长 ≤ 世界极速（#034 修订一，原硬编码 50 退役）
                if !vx.is_finite() || !vy.is_finite() {
                    tracing::warn!(
                        entity_id = %shooter.0,
                        vx,
                        vy,
                        "[shoot] 初速非有限，拒绝"
                    );
                    return 0;
                }
                let speed2 = vx * vx + vy * vy;
                if speed2 > max_speed * max_speed {
                    tracing::warn!(
                        entity_id = %shooter.0,
                        speed = speed2.sqrt(),
                        max = max_speed,
                        "[shoot] 初速超世界极速，拒绝（视觉可读性常数：攻击必须看得见飞过来）"
                    );
                    return 0;
                }
                // payload 超限拒绝：在读 memory 之前拦下（同 attack_at）
                if payload_len > 0 && payload_len as usize > MAX_PAYLOAD_LEN {
                    tracing::warn!(
                        entity_id = %shooter.0,
                        actual_len = payload_len,
                        max = MAX_PAYLOAD_LEN,
                        "[shoot] payload 超限，拒绝入队"
                    );
                    return 0;
                }
                let payload = read_payload_from_caller(&mut caller, payload_ptr, payload_len);
                // mass 钳制（除零护栏，#034）
                let mass = codeworld_core::collision::clamp_mass(mass);
                caller.data_mut().shots_fired += 1;
                let serial = caller.data().shots_fired;
                caller.data_mut().pending_shoots.push(PendingShoot {
                    shooter,
                    vx,
                    vy,
                    mass,
                    payload,
                });
                tracing::info!(
                    entity_id = %shooter.0,
                    vx,
                    vy,
                    mass,
                    serial,
                    "[shoot] 射击已排队（阶段二统一发射）"
                );
                serial
            },
        )?;
        // env.destroy(id_lo: i64, id_hi: i64) -> i32：销毁（门二开放，产权链 #015）。
        // EntityId(Uuid u128) 拆 lo/hi 两个 i64 传（WAT 侧负 i64 经 as u64 恢复位型）。
        // import 只排队：产权校验（只能销毁自家造物，owner 匹配）需要 world，
        // 在阶段二 process_destroys 判；destroy 不触发掠夺（不是击杀，宪法铁律）。
        // 自毁走 life()=0，本接口对自家实体同样放行（owner 恒匹配，语义等价无掠夺）。
        linker.func_wrap(
            "env",
            "destroy",
            |mut caller: Caller<SandboxData>, id_lo: i64, id_hi: i64| -> i32 {
                caller.data_mut().api_calls.push("destroy".to_string());
                let destroyer = caller.data().entity_id;
                // i64 -> u64 用 as 截断恢复位型（符号扩展陷阱：WAT 侧 -1 = u64::MAX），
                // 拼回 u128 再转 Uuid
                let id_u = ((id_hi as u64 as u128) << 64) | (id_lo as u64 as u128);
                let target = EntityId(Uuid::from_u128(id_u));
                caller
                    .data_mut()
                    .pending_destroys
                    .push(PendingDestroy { destroyer, target });
                tracing::info!(
                    destroyer = %destroyer.0,
                    target = %target.0,
                    "[destroy] 销毁请求已排队（阶段二产权校验）"
                );
                1
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

/// 向 caller 的 memory export 写入字节切片（env.probe 的 response+体徵输出）
///
/// 模块未导出 memory / ptr<0 / cap=0 / 写入范围越界 -> 0（不写，防半截数据）。
/// bytes.len() > cap 时只写前 cap 字节（超容量截断），返回实际写入字节数。
fn write_bytes_to_caller(
    caller: &mut Caller<SandboxData>,
    ptr: i32,
    bytes: &[u8],
    cap: usize,
) -> usize {
    if ptr < 0 || cap == 0 {
        return 0;
    }
    let Some(memory) = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
    else {
        tracing::debug!("[write_bytes] 模块未导出 memory，写入失败");
        return 0;
    };
    let start = ptr as usize;
    let want = bytes.len().min(cap);
    let Some(end) = start.checked_add(want) else {
        tracing::warn!(ptr, want, "[write_bytes] 写入地址回绕，写入失败");
        return 0;
    };
    if end > memory.data(&*caller).len() {
        tracing::warn!(ptr, want, mem_len = memory.data(&*caller).len(), "[write_bytes] 写入越界，写入失败");
        return 0;
    }
    memory.data_mut(&mut *caller)[start..end].copy_from_slice(&bytes[..want]);
    want
}

/// 从 caller 的 memory export 读取 NUL 结尾字符串（env.spawn 的 glyph/color 参数）
///
/// 从 ptr 起扫描到首个 NUL（最多扫 max_len 字节，防无界扫描线性内存），
/// UTF-8 解码失败 / ptr<0 / 无 memory / max_len 内无 NUL -> None。
fn read_cstring_from_caller(
    caller: &mut Caller<SandboxData>,
    ptr: i32,
    max_len: usize,
) -> Option<String> {
    if ptr < 0 {
        return None;
    }
    let memory = caller
        .get_export("memory")
        .and_then(|e| e.into_memory())?;
    let data = memory.data(&*caller);
    let start = ptr as usize;
    let window = data.get(start..)?.get(..max_len)?;
    let nul = window.iter().position(|&b| b == 0)?;
    String::from_utf8(window[..nul].to_vec()).ok()
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

    // === env.log marker 值记录（战况字幕的盾量上报通路：玩家代码 log(7) 后跟 log(余量)）===

    /// 连发两个 marker 的 WAT：on_tick 调 log(7) 再 log(5)（模拟 D 拦截上报盾量 5）
    const DOUBLE_LOG_WAT: &str = r#"(module
  (import "env" "log" (func $log (param i32)))
  (func (export "on_tick")
    i32.const 7 call $log
    i32.const 5 call $log)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn log_import_records_marker_values_in_order() {
        // env.log：除 api_calls 元数据与 stderr 日志外，把 marker 值按序记入
        // SandboxData.log_markers（host 侧字幕解析盾量的数据源，玩家代码零改动）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(DOUBLE_LOG_WAT), 100_000, id(601))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(
            handle.store.data().log_markers,
            vec![7, 5],
            "env.log 的 marker 值应按调用顺序记录"
        );
    }

    #[test]
    fn log_markers_stay_empty_when_no_log_called() {
        // 负面路径：不调 log 的模块 -> log_markers 保持为空
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(LIFE_TRUE_WAT), 100_000, id(602))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert!(
            handle.store.data().log_markers.is_empty(),
            "未调 env.log 时 log_markers 应为空"
        );
    }

    // === env.visible_count / env.visible_at（三层感知 Phase A：眼睛列表）===

    /// 眼睛列表消费者 WAT：on_tick 先 visible_count 存全局，再对 idx=0 调
    /// visible_at(0, 0, 8)（x 写 memory[0..8]、y 写 [8..16]）
    const EYES_WAT: &str = r#"(module
  (import "env" "visible_count" (func $visible_count (result i32)))
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (global $count (mut i32) (i32.const -1))
  (global $got (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    call $visible_count global.set $count
    i32.const 0 i32.const 0 i32.const 8 call $visible_at global.set $got)
  (func (export "get_count") (result i32) global.get $count)
  (func (export "get_got") (result i32) global.get $got)
  (func (export "get_x") (result f64) (f64.load (i32.const 0)))
  (func (export "get_y") (result f64) (f64.load (i32.const 8)))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 眼睛列表越界读取 WAT：对 idx=5 调 visible_at（列表只有 1 项）
    const EYES_OUT_OF_RANGE_WAT: &str = r#"(module
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (global $got (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 5 i32.const 0 i32.const 8 call $visible_at global.set $got)
  (func (export "get_got") (result i32) global.get $got)
  (func (export "get_x") (result f64) (f64.load (i32.const 0)))
  (func (export "life") (result i32) i32.const 1))"#;

    /// 眼睛列表负索引 WAT：对 idx=-1 调 visible_at
    const EYES_NEGATIVE_IDX_WAT: &str = r#"(module
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (global $got (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const -1 i32.const 0 i32.const 8 call $visible_at global.set $got)
  (func (export "get_got") (result i32) global.get $got)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 眼睛列表坏指针 WAT：x_ptr 越界（memory 1 页 = 64KB，ptr=65536）
    const EYES_BAD_PTR_WAT: &str = r#"(module
  (import "env" "visible_at" (func $visible_at (param i32 i32 i32) (result i32)))
  (global $got (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    i32.const 0 i32.const 65536 i32.const 8 call $visible_at global.set $got)
  (func (export "get_got") (result i32) global.get $got)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn visible_count_returns_snapshot_len() {
        // visible_count：返回 host 填充的可见快照长度（host 侧已排除自己）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(EYES_WAT), 100_000, id(701)).unwrap();
        handle.store.data_mut().visible = vec![(1.0, 2.0), (3.0, 4.0)];
        mgr.call_on_tick(&mut handle).unwrap();
        let count = call_i32_export(&mut handle, "get_count");
        assert_eq!(count, 2, "visible_count 应返回快照长度");
        assert!(
            handle.store.data().api_calls.contains(&"visible_count".to_string()),
            "visible_count 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn visible_count_zero_when_list_empty() {
        // 负面路径：可见列表为空（周围无敌人）-> 返回 0
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(EYES_WAT), 100_000, id(702)).unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        let count = call_i32_export(&mut handle, "get_count");
        assert_eq!(count, 0, "空列表应返回 0");
    }

    #[test]
    fn visible_at_reads_coordinates_into_memory() {
        // visible_at：idx=0 命中 -> 坐标写 memory（x@0..8 y@8..16），返回 1
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(EYES_WAT), 100_000, id(703)).unwrap();
        handle.store.data_mut().visible = vec![(7.5, -2.25), (100.0, 200.0)];
        mgr.call_on_tick(&mut handle).unwrap();
        let got = call_i32_export(&mut handle, "get_got");
        assert_eq!(got, 1, "idx=0 应命中");
        assert_eq!(call_f64_export(&mut handle, "get_x"), 7.5, "x 应写入 memory[0..8]");
        assert_eq!(call_f64_export(&mut handle, "get_y"), -2.25, "y 应写入 memory[8..16]");
        assert!(
            handle.store.data().api_calls.contains(&"visible_at".to_string()),
            "visible_at 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn visible_at_out_of_range_returns_zero() {
        // 负面路径：idx 越界（列表 1 项取 idx=5）-> 返回 0、不写 memory
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(EYES_OUT_OF_RANGE_WAT), 100_000, id(704))
            .unwrap();
        handle.store.data_mut().visible = vec![(7.5, -2.25)];
        mgr.call_on_tick(&mut handle).unwrap();
        let got = call_i32_export(&mut handle, "get_got");
        assert_eq!(got, 0, "越界应返回 0");
        let x = call_f64_export(&mut handle, "get_x");
        assert_eq!(x, 0.0, "越界不应写 memory（保持初始 0）");
    }

    #[test]
    fn visible_at_negative_idx_returns_zero() {
        // 负面路径：idx 负数 -> 返回 0（负索引不是合法查询）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(EYES_NEGATIVE_IDX_WAT), 100_000, id(705))
            .unwrap();
        handle.store.data_mut().visible = vec![(7.5, -2.25)];
        mgr.call_on_tick(&mut handle).unwrap();
        let got = call_i32_export(&mut handle, "get_got");
        assert_eq!(got, 0, "负索引应返回 0");
    }

    #[test]
    fn visible_at_bad_ptr_returns_zero() {
        // 负面路径：写入指针越界 -> 返回 0 不 panic（同 scan 坏指针语义）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(EYES_BAD_PTR_WAT), 100_000, id(706))
            .unwrap();
        handle.store.data_mut().visible = vec![(7.5, -2.25)];
        mgr.call_on_tick(&mut handle).unwrap();
        let got = call_i32_export(&mut handle, "get_got");
        assert_eq!(got, 0, "指针非法应返回 0");
    }

    // === env.probe（三层感知 Phase A：探测层 + 撒谎机制 + 被动体徵）===

    /// 被探测目标 WAT（撒谎者）：on_probe 固定回 "LIE9"（i32 LE 字节序列
    /// [0x4C,0x49,0x45,0x39]——谎称友军、life=9 的可识别假串，4 字节为
    /// on_probe i32->i32 签名的 MVP 上限，Phase 2 扩内存传递）
    const LIAR_WAT: &str = r#"(module
  (func (export "on_probe") (param i32) (result i32) i32.const 0x3945494C)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 无 on_probe 的目标 WAT（哑巴：被探测只能暴露默认被动体徵）
    const NO_PROBE_WAT: &str = r#"(module
  (func (export "life") (result i32) i32.const 1))"#;

    /// 探测者 WAT 模板：on_tick 调 probe(tx, ty, 100, 4, 200, 64)，返回值存全局。
    /// memory 布局：[100..104]=request 字节、[200..264]=response 缓冲（cap 64）。
    fn prober_wat(tx: &str, ty: &str) -> String {
        format!(
            r#"(module
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -999))
  (memory (export "memory") 1)
  (data (i32.const 100) "\01\02\03\04")
  (func (export "on_tick")
    f64.const {tx} f64.const {ty} i32.const 100 i32.const 4 i32.const 200 i32.const 64
    call $probe global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "get_resp0") (result i32) (i32.load (i32.const 200)))
  (func (export "get_resp8") (result i32) (i32.load (i32.const 208)))
  (func (export "get_resp12") (result i32) (i32.load (i32.const 212)))
  (func (export "get_resp16") (result i32) (i32.load (i32.const 216)))
  (func (export "get_resp20") (result i32) (i32.load (i32.const 220)))
  (func (export "life") (result i32) i32.const 1))"#
        )
    }

    /// 从实体当前沙箱调用无参 i32 导出（测试观测探针全局变量）
    fn call_i32_export(handle: &mut SandboxHandle, name: &str) -> i32 {
        let func = handle
            .instance
            .get_typed_func::<(), i32>(&mut handle.store, name)
            .unwrap();
        func.call(&mut handle.store, ()).unwrap()
    }

    /// probe 测试挂载：目标沙箱入表 + 探测者建桥（bridge 由调用方持有保活，
    /// 测试结束前用 null 断开，符合 ProbeBridge 的 SAFETY 契约）
    #[test]
    fn probe_hit_returns_liar_response_plus_vitals() {
        // probe 命中：目标 on_probe 的假串原样透传（撒谎=玩家代码自由，框架只透传），
        // 之后追加 24 字节被动体徵（api_count/fuel 剩余/fuel 消耗，host 采集不可伪造）。
        // 总响应 4+24=28 字节 <= resp_cap 64，返回 28。
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(2), mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap());
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        prober.store.data_mut().visible = vec![(7.0, 8.0)];
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
            traces: Vec::new(),
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, 28, "命中应写入 4 字节假串 + 24 字节体徵");
        // 假串原样到达探测者（撒谎透传）
        let lie = call_i32_export(&mut prober, "get_resp0");
        assert_eq!(lie, 0x3945494C, "response 前 4 字节应是目标的假串 LIE9");
        // 体徵块：api_count（u64 LE @+4，目标没调过 api = 0）
        // 用 +12 的 i32 读 fuel 剩余低 32 位（prev_fuel=100_000 未跑过任何调用）
        let fuel_remaining = call_i32_export(&mut prober, "get_resp12");
        assert_eq!(fuel_remaining, 100_000, "体徵应含目标探测前 fuel 剩余量");
        // fuel 消耗 > 0（on_probe 至少执行了几条指令）
        let fuel_used = call_i32_export(&mut prober, "get_resp20");
        assert!(fuel_used > 0, "体徵应含本次 on_probe 实际消耗");
        assert!(
            prober.store.data().api_calls.contains(&"probe".to_string()),
            "probe 调用应记入探测者 api_calls 元数据"
        );
    }

    #[test]
    fn probe_miss_returns_minus_one() {
        // 负面路径：坐标上没有可见实体 -> 返回 -1，不写响应
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(2), mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap());
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("99.0", "99.0")), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
            traces: Vec::new(),
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, -1, "坐标无人应返回 -1");
    }

    #[test]
    fn probe_self_pos_returns_minus_one() {
        // 负面路径（防御）：bridge 被污染含 caller 自己的坐标时，probe 自己仍被拒。
        // 正常 host 填充会排除自己，此测试锁定 import 层的第二道防线。
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(1), mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(1)).unwrap());
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("3.0", "3.0")), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1)],
            visible_positions: vec![(3.0, 3.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
            traces: Vec::new(),
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, -1, "probe 自己应被拒绝");
    }

    #[test]
    fn probe_target_without_on_probe_returns_minus2_with_vitals() {
        // 负面路径：目标无 on_probe 导出 -> 返回 -2，响应位置写默认被动体徵（24 字节）
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(3), mgr.create_sandbox(&wasm(NO_PROBE_WAT), 100_000, id(3)).unwrap());
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(3)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
            traces: Vec::new(),
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, -2, "目标无 on_probe 应返回 -2");
        // 默认被动体徵仍写入：-2 时响应为空，体徵块从 offset 0 开始
        //（api_count 在 [0..8]、fuel 在 [8..16]）——fuel 低 4 字节在 memory[208..212]
        let fuel_remaining = call_i32_export(&mut prober, "get_resp8");
        assert_eq!(fuel_remaining, 100_000, "-2 时也应写入默认体徵");
    }

    #[test]
    fn probe_truncates_response_to_resp_cap() {
        // 边界：resp_cap 小于 body（28 字节）-> 只写 cap 字节，返回 cap
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(id(2), mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap());
        // cap=10：模板的 resp_ptr=200 cap=64，此测试用内联小 cap 模块
        const PROBER_SMALL_CAP_WAT: &str = r#"(module
  (import "env" "probe" (func $probe (param f64 f64 i32 i32 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -999))
  (memory (export "memory") 1)
  (data (i32.const 100) "\01\02\03\04")
  (func (export "on_tick")
    f64.const 7.0 f64.const 8.0 i32.const 100 i32.const 4 i32.const 200 i32.const 10
    call $probe global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;
        let mut prober = mgr
            .create_sandbox(&wasm(PROBER_SMALL_CAP_WAT), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
            traces: Vec::new(),
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, 10, "超容量应截断为 resp_cap 字节");
    }

    #[test]
    fn probe_restores_target_fuel_and_records_consumption() {
        // fuel 包裹（host 侧预算控制）：探测前目标 fuel=55_555 -> 探测后必须恢复
        // 55_555（probe 不扣目标手续费，架构 3.3）；消耗记进体徵；
        // fuel_consumed 元数据按原尺度修正（update_fuel 在包裹期间用预算尺度算过假象）。
        // 注：本测试不构造 on_probe 烧穿预算的场景——wasmtime 25 + Windows 下
        // fuel 耗尽 trap = 进程 abort（.cc-bugs #5），预算中断行为由 host 侧
        // set_fuel 包裹逻辑保证，测试只验证记账与恢复。
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        let mut liar = mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap();
        liar.store.set_fuel(55_555).unwrap();
        map.insert(id(2), liar);
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2)],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
            traces: Vec::new(),
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        let ret = call_i32_export(&mut prober, "get_ret");
        drop(bridge);
        assert_eq!(ret, 28, "命中应正常返回");
        // 目标 fuel 恢复到探测前值
        let map_handle = map.get(&id(2)).unwrap();
        assert_eq!(
            map_handle.store.get_fuel().unwrap(),
            55_555,
            "探测后目标 fuel 应恢复原值（probe 不扣手续费）"
        );
        // fuel_consumed 元数据按原尺度修正（limit - prev_fuel）
        assert_eq!(
            map_handle.store.data().fuel_consumed,
            100_000 - 55_555,
            "fuel_consumed 应按探测前尺度重算，不被包裹污染"
        );
        // 体徵记录：fuel 剩余 = 55_555（探测前快照）
        let fuel_remaining = call_i32_export(&mut prober, "get_resp12");
        assert_eq!(fuel_remaining, 55_555, "体徵 fuel 剩余应是探测前快照");
        let fuel_used = call_i32_export(&mut prober, "get_resp20");
        assert!(fuel_used > 0 && fuel_used < 10_000, "消耗应在预算内且大于 0");
    }

    #[test]
    fn probe_with_null_bridge_returns_minus_one() {
        // 负面路径（防御）：桥未挂载（非阶段一窗口调用）-> 返回 -1 不崩
        let mgr = SandboxManager::new().unwrap();
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        mgr.call_on_tick(&mut prober).unwrap();
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, -1, "桥未挂载应返回 -1");
    }

    // === probe 命中发射痕迹（#034 修订三：痕迹不是实体，最小证据路径）===

    #[test]
    fn probe_on_trace_coordinate_returns_shooter_identity() {
        // probe 命中痕迹坐标 -> 返回 -3，resp 前 24 字节 = 发射者 id（u128 LE 16 字节）
        // + 发射拍号（u64 LE 8 字节）——"此处发射过弹丸"的最小证据（不建沙箱）。
        // 断言 resp0 = shooter u128 低 32 位（=42）、resp16 = 拍号低 32 位（=7）
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new(); // 空沙箱表：痕迹不占沙箱
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        prober.store.data_mut().visible = vec![(7.0, 8.0)];
        let trace_id = id(77);
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), trace_id],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
            traces: vec![TraceProbe {
                trace_id,
                shooter: id(42),
                birth_tick: 7,
            }],
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        assert_eq!(
            call_i32_export(&mut prober, "get_ret"),
            -3,
            "痕迹命中应返回 -3（痕迹证据语义，与 -1 无实体/-2 无 on_probe 区分）"
        );
        assert_eq!(
            call_i32_export(&mut prober, "get_resp0"),
            42,
            "resp[0..4] 应为发射者 id 低 32 位"
        );
        assert_eq!(
            call_i32_export(&mut prober, "get_resp16"),
            7,
            "resp[16..20] 应为发射拍号低 32 位"
        );
    }

    #[test]
    fn probe_prefers_real_entity_over_trace_at_same_coordinate() {
        // 同坐标实体与痕迹并存（发射者原地不动的常见态）：按实体结算（on_probe
        // 假串照回），不吃痕迹证据——实体优先于痕迹
        let mgr = SandboxManager::new().unwrap();
        let mut map = std::collections::HashMap::new();
        map.insert(
            id(2),
            mgr.create_sandbox(&wasm(LIAR_WAT), 100_000, id(2)).unwrap(),
        );
        let mut prober = mgr
            .create_sandbox(&wasm(&prober_wat("7.0", "8.0")), 100_000, id(1))
            .unwrap();
        prober.store.data_mut().visible = vec![(7.0, 8.0)];
        let trace_id = id(77);
        let mut bridge = ProbeBridge {
            visible_ids: vec![id(1), id(2), trace_id],
            visible_positions: vec![(0.0, 0.0), (7.0, 8.0), (7.0, 8.0)],
            sandboxes: &mut map,
            manager: &mgr,
            caller_id: id(1),
            probe_fuel: 10_000,
            traces: vec![TraceProbe {
                trace_id,
                shooter: id(2),
                birth_tick: 3,
            }],
        };
        prober.store.data_mut().probe_bridge = &mut bridge;
        mgr.call_on_tick(&mut prober).unwrap();
        prober.store.data_mut().probe_bridge = std::ptr::null_mut();
        drop(bridge);
        let ret = call_i32_export(&mut prober, "get_ret");
        assert_eq!(ret, 28, "同坐标并存应按实体 probe 结算（4 假串+24 体徵）");
        assert_eq!(
            call_i32_export(&mut prober, "get_resp0"),
            0x3945494C,
            "响应应是实体的假串而非痕迹证据"
        );
    }

    // === env.spawn / env.destroy（门二：代码造物，decisions.md #012/#015）===

    /// WAT 字符串字面量转义（按字节）：\ 与 " 转义、换行转 \0a（WAT 禁裸换行）、
    /// 非 ASCII 字节逐字节转 \XX（裸高字节 wat crate 会错误解码）。
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

    /// 造物方 WAT 生成器：data 段放造物源码（[0..)）+ 字形 T（[8192]）+ 颜色 cyan（[8256]
    /// ——外观段远离代码段，防大体积造物源码与外观 data 段重叠相互覆盖），
    /// on_tick 调 spawn(0, code.len(), x, y, 256, 300)，返回值存全局 get_ret。
    /// code_len 用 Rust 侧 code.len() 计算——手数 WAT 字符串长度天然易错（截断坑）。
    fn spawner_wat(code: &str, x: &str, y: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    {x} {y}
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    call $spawn global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
            x = x,
            y = y,
        )
    }

    /// 连发两次 spawn 的 WAT：同 tick 第二次应被限流拒绝（每实体每 tick 最多 1 个）
    fn double_spawner_wat(code: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64) (result i32)))
  (global $r1 (mut i32) (i32.const -1))
  (global $r2 (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    call $spawn global.set $r1
    i32.const 0 i32.const {len}
    f64.const 4.0 f64.const 4.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    call $spawn global.set $r2)
  (func (export "get_r1") (result i32) global.get $r1)
  (func (export "get_r2") (result i32) global.get $r2)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
        )
    }

    /// 空 code WAT：code_len=0 应拒绝（没有代码造不出活的造物）
    const EMPTY_CODE_SPAWNER_WAT: &str = r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const 0
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    call $spawn global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    /// 坏字形 WAT：glyph 两个字符 "TT" 应拒绝（渲染层一字形一实体）
    fn bad_glyph_spawner_wat(code: &str) -> String {
        let escaped = wat_escape(code);
        format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "TT\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1.0
    call $spawn global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = code.len(),
        )
    }

    /// 造物方 WAT 的合法造物源码（最简 life 模块，data 段内容与此一致）
    const SPAWNED_CODE: &str = r#"(module (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn spawn_import_queues_pending_spawn() {
        // env.spawn：合法请求（±10 内、限流未满、代码/外观合法）-> 排队 PendingSpawn，
        // 返回 1。owner 不在 import 层解析（import 拿不到 world），阶段二按产权链继承。
        let mgr = SandboxManager::new().unwrap();
        let creator = id(801);
        let wat_text = spawner_wat(SPAWNED_CODE, "f64.const 2.0", "f64.const 3.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, creator)
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let ret = call_i32_export(&mut handle, "get_ret");
        assert_eq!(ret, 1, "合法 spawn 应返回 1（已排队）");
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns.len(), 1, "应排队 1 个造物请求");
        let s = &spawns[0];
        assert_eq!(s.creator, creator, "creator 应为沙箱绑定实体");
        assert_eq!(s.pos, (2.0, 3.0), "出生点应保存请求传入的绝对坐标");
        assert_eq!(s.code, SPAWNED_CODE, "造物代码应从 memory data 段读取（NUL 截断）");
        assert_eq!(s.glyph, "T", "字形应从 memory 读取");
        assert_eq!(s.color, "cyan", "颜色应从 memory 读取");
        assert_eq!(s.fuel_limit, 100_000, "fuel 配额应继承创造者沙箱的 fuel_limit");
        assert_eq!(s.mass, 1.0, "#034 第 8 参 mass 应透传（import 层已钳制）");
        assert!(
            handle.store.data().api_calls.contains(&"spawn".to_string()),
            "spawn 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn spawn_import_clamps_mass_out_of_bounds() {
        // #034 第 8 参 mass 护栏：1000 超上限钳到 100，0.001 低下限钳到 0.05（除零护栏）
        let mgr = SandboxManager::new().unwrap();
        let escaped = wat_escape(SPAWNED_CODE);
        // 两次调用只排队第一次（限流）——本测试只关心 mass 钳制，单发即可
        let wat_text = format!(
            r#"(module
  (import "env" "spawn" (func $spawn (param i32 i32 f64 f64 i32 i32 f64 f64) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}\00")
  (data (i32.const 8192) "T\00")
  (data (i32.const 8256) "cyan\00")
  (func (export "on_tick")
    i32.const 0 i32.const {len}
    f64.const 1.0 f64.const 1.0
    i32.const 8192 i32.const 8256
    f64.const 0.5
    f64.const 1000.0
    call $spawn drop)
  (func (export "life") (result i32) i32.const 1))"#,
            escaped = escaped,
            len = SPAWNED_CODE.len(),
        );
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(808))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        let spawns = &handle.store.data().pending_spawns;
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].mass, 100.0, "超上限 mass 应钳到 100.0");
    }

    // === env.shoot import（#034 第 12 个玩家 import：发射弹丸）===

    /// 射手 WAT 生成器：on_tick 调 shoot(vx, vy, mass, 0, payload_len)（payload 从
    /// data 段 [0..4) 读 [1,2,3,4]），返回值存全局 get_ret。
    fn shooter_wat(vx: &str, vy: &str, mass: &str, payload_len: &str) -> String {
        format!(
            r#"(module
  (import "env" "shoot" (func $shoot (param f64 f64 f64 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (data (i32.const 0) "\01\02\03\04")
  (func (export "on_tick")
    {vx} {vy} {mass} i32.const 0 i32.const {payload_len}
    call $shoot global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#,
            vx = vx,
            vy = vy,
            mass = mass,
            payload_len = payload_len,
        )
    }

    #[test]
    fn shoot_import_queues_pending_shoot() {
        // env.shoot：合法请求（速度/质量合法、限流未满）-> 排队 PendingShoot，
        // 返回弹丸序号（该实体累计第 1 发）
        let mgr = SandboxManager::new().unwrap();
        let shooter_id = id(901);
        let wat_text = shooter_wat("f64.const 3.0", "f64.const 4.0", "f64.const 0.5", "4");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, shooter_id)
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "首发射击序号应为 1");
        let shoots = &handle.store.data().pending_shoots;
        assert_eq!(shoots.len(), 1, "应排队 1 次射击");
        let s = &shoots[0];
        assert_eq!(s.shooter, shooter_id, "shooter 应为沙箱绑定实体");
        assert_eq!(s.vx, 3.0);
        assert_eq!(s.vy, 4.0);
        assert_eq!(s.mass, 0.5, "#034 弹丸质量由代码定（0.5 合法域内透传）");
        assert_eq!(s.payload, vec![1, 2, 3, 4], "payload 应从 memory data 段读取");
        assert!(
            handle.store.data().api_calls.contains(&"shoot".to_string()),
            "shoot 调用应记入 api_calls 元数据"
        );
    }

    #[test]
    fn shoot_import_rejects_second_shot_same_tick() {
        // 限流：同实体一拍至多 1 发（防弹幕灌爆），第二次返回 0 不入队
        let mgr = SandboxManager::new().unwrap();
        let wat_text = r#"(module
  (import "env" "shoot" (func $shoot (param f64 f64 f64 i32 i32) (result i32)))
  (global $r1 (mut i32) (i32.const -1))
  (global $r2 (mut i32) (i32.const -1))
  (memory (export "memory") 1)
  (func (export "on_tick")
    f64.const 1.0 f64.const 0.0 f64.const 1.0 i32.const 0 i32.const 0 call $shoot global.set $r1
    f64.const 1.0 f64.const 0.0 f64.const 1.0 i32.const 0 i32.const 0 call $shoot global.set $r2)
  (func (export "get_r1") (result i32) global.get $r1)
  (func (export "get_r2") (result i32) global.get $r2)
  (func (export "life") (result i32) i32.const 1))"#;
        let mut handle = mgr
            .create_sandbox(&wasm(wat_text), 100_000, id(902))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_r1"), 1, "首发应排队成功");
        assert_eq!(call_i32_export(&mut handle, "get_r2"), 0, "同拍第二发应被限流拒绝");
        assert_eq!(handle.store.data().pending_shoots.len(), 1, "同拍只应有 1 发入队");
    }

    #[test]
    fn shoot_import_serial_increments_across_ticks() {
        // 序号跨拍递增：tick1 返 1、drain 后 tick2 返 2（该实体累计第几发）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = shooter_wat("f64.const 1.0", "f64.const 9.8", "f64.const 1.0", "0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(903))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1);
        handle.store.data_mut().pending_shoots.clear(); // 主程序收集后清空（drain 语义）
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 2, "次拍射击序号应递增");
    }

    #[test]
    fn shoot_import_rejects_overspeed() {
        // 世界极速上限（#034 修订一：MAX_SHOOT_SPEED=50 退役，改用 WORLD_MAX_SPEED=25）：
        // (40, 40) 模长 ~56.6 > 25 -> 拒绝
        let mgr = SandboxManager::new().unwrap();
        let wat_text = shooter_wat("f64.const 40.0", "f64.const 40.0", "f64.const 1.0", "0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(904))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "超速射击应被拒绝");
        assert!(handle.store.data().pending_shoots.is_empty(), "超速请求不应入队");
    }

    #[test]
    fn shoot_import_allows_exactly_world_max_speed() {
        // 边界：初速模长恰等于世界极速 25 不拒（严格 > 超限判据，审查锚定）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = shooter_wat("f64.const 25.0", "f64.const 0.0", "f64.const 1.0", "0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(908))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "恰等于极速应放行");
        assert_eq!(handle.store.data().pending_shoots.len(), 1);
    }

    #[test]
    fn shoot_import_rejects_just_above_world_max_speed() {
        // 边界：刚过极速一点（25.5）即拒
        let mgr = SandboxManager::new().unwrap();
        let wat_text = shooter_wat("f64.const 25.5", "f64.const 0.0", "f64.const 1.0", "0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(909))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "刚过极速应拒绝");
        assert!(handle.store.data().pending_shoots.is_empty());
    }

    #[test]
    fn shoot_import_uses_manager_max_speed_not_hardcoded() {
        // 口径同源：new_with_max_speed(10) 的管理器按 10 判超限——发射钳制与
        // 飞行钳制（DefaultPhysics.max_speed）同源一个世界常数，不再各自硬编码
        let mgr = SandboxManager::new_with_max_speed(10.0).unwrap();
        let wat_text = shooter_wat("f64.const 12.0", "f64.const 0.0", "f64.const 1.0", "0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(910))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "12 > 自定义极速 10 应拒绝");
    }

    #[test]
    fn shoot_import_rejects_non_finite_velocity() {
        // NaN 初速（0/0 除零）-> 拒绝，防污染世界状态
        let mgr = SandboxManager::new().unwrap();
        let wat_text = shooter_wat(
            "f64.const 0.0 f64.const 0.0 f64.div",
            "f64.const 1.0",
            "f64.const 1.0",
            "0",
        );
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(905))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "NaN 初速应被拒绝");
        assert!(handle.store.data().pending_shoots.is_empty());
    }

    #[test]
    fn shoot_import_clamps_mass_out_of_bounds() {
        // mass 护栏（与 spawn 第 8 参同口径）：0.001 -> 0.05，1000 -> 100
        let mgr = SandboxManager::new().unwrap();
        let wat_text = shooter_wat("f64.const 1.0", "f64.const 0.0", "f64.const 0.001", "0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(906))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1);
        assert_eq!(handle.store.data().pending_shoots[0].mass, 0.05, "低下限应钳到 0.05");
    }

    #[test]
    fn shoot_import_rejects_oversized_payload() {
        // payload 超限（>1MB）-> 拒绝不入队（与 attack_at 同上限 MAX_PAYLOAD_LEN）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = r#"(module
  (import "env" "shoot" (func $shoot (param f64 f64 f64 i32 i32) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (memory (export "memory") 24)
  (func (export "on_tick")
    f64.const 1.0 f64.const 0.0 f64.const 1.0 i32.const 0 i32.const 1048577
    call $shoot global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;
        let mut handle = mgr
            .create_sandbox(&wasm(wat_text), 100_000, id(907))
            .unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "超限 payload 应被拒绝");
        assert!(handle.store.data().pending_shoots.is_empty());
    }

    #[test]
    fn spawn_import_rejects_second_request_same_tick() {
        // 限流：每实体每 tick 最多 1 个 spawn 请求，第二次调用返回 0 不入队
        let mgr = SandboxManager::new().unwrap();
        let wat_text = double_spawner_wat(SPAWNED_CODE);
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(802))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_r1"), 1, "第一次应排队成功");
        assert_eq!(call_i32_export(&mut handle, "get_r2"), 0, "第二次应被限流拒绝");
        assert_eq!(
            handle.store.data().pending_spawns.len(),
            1,
            "同 tick 只应有 1 个请求入队"
        );
    }

    #[test]
    fn spawn_import_rejects_birth_out_of_creator_range() {
        // 位置起点限制：出生点超出创造者快照 ±10 格 -> 拒绝（防远程投射刷屏，
        // MVP 一刀切简单规则；规则由宪法允许后续玩法化，#012）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = spawner_wat(SPAWNED_CODE, "f64.const 21.0", "f64.const 10.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(803))
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "越界出生点应被拒绝");
        assert!(
            handle.store.data().pending_spawns.is_empty(),
            "被拒绝的造物请求不应入队"
        );
    }

    #[test]
    fn spawn_import_rejects_underground_birth() {
        // 出生点 y<0 钻地 -> 拒绝（默认物理地面 y=0，与 move_by 同语义）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = spawner_wat(SPAWNED_CODE, "f64.const 12.0", "f64.const -1.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(804))
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "钻地出生点应被拒绝");
    }

    #[test]
    fn spawn_import_rejects_non_finite_birth() {
        // 出生点非有限（NaN）-> 拒绝，防污染世界状态（与 move_by NaN 同款防御）。
        // WAT 无 NaN 字面量直接写法缺口：0.0/0.0 产生 NaN（浮点除零不 trap）
        let mgr = SandboxManager::new().unwrap();
        let wat_text = spawner_wat(SPAWNED_CODE, "f64.const 0.0 f64.const 0.0 f64.div", "f64.const 0.0");
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(805))
            .unwrap();
        handle.store.data_mut().pos = (10.0, 10.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "NaN 出生点应被拒绝");
        assert!(handle.store.data().pending_spawns.is_empty(), "NaN 请求不应入队");
    }

    #[test]
    fn spawn_import_rejects_empty_code() {
        // code_len=0（没有代码）-> 拒绝：造物是活实体，必须有可编译的代码
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr
            .create_sandbox(&wasm(EMPTY_CODE_SPAWNER_WAT), 100_000, id(806))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "空代码应被拒绝");
        assert!(handle.store.data().pending_spawns.is_empty(), "空代码请求不应入队");
    }

    #[test]
    fn spawn_import_rejects_multi_char_glyph() {
        // 字形必须是单字符（渲染层一实体一字符），"TT" -> 拒绝
        let mgr = SandboxManager::new().unwrap();
        let wat_text = bad_glyph_spawner_wat(SPAWNED_CODE);
        let mut handle = mgr
            .create_sandbox(&wasm(&wat_text), 100_000, id(807))
            .unwrap();
        handle.store.data_mut().pos = (0.0, 0.0);
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 0, "多字符字形应被拒绝");
        assert!(handle.store.data().pending_spawns.is_empty(), "坏字形请求不应入队");
    }

    /// 销毁方 WAT：on_tick 调 destroy(id_lo, id_hi)，返回值存全局。
    /// 目标 id = 0x0102030405060708_FFFFFFFFFFFFFFFF（hi 半含全 1 低半，
    /// 锁定 i64 -> u64 的 as 截断语义：WAT 侧 -1 表示 u64::MAX）。
    const DESTROYER_WAT: &str = r#"(module
  (import "env" "destroy" (func $destroy (param i64 i64) (result i32)))
  (global $ret (mut i32) (i32.const -1))
  (func (export "on_tick")
    i64.const -1 i64.const 0x0102030405060708 call $destroy global.set $ret)
  (func (export "get_ret") (result i32) global.get $ret)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn destroy_import_queues_pending_destroy() {
        // env.destroy：EntityId 以 i64 两半传递（u128 拆 lo/hi；WAT 侧负 i64 经 as
        // 恢复位型）。import 只排队（产权校验在阶段二，import 拿不到 world）。
        let mgr = SandboxManager::new().unwrap();
        let destroyer = id(808);
        let mut handle = mgr.create_sandbox(&wasm(DESTROYER_WAT), 100_000, destroyer).unwrap();
        mgr.call_on_tick(&mut handle).unwrap();
        assert_eq!(call_i32_export(&mut handle, "get_ret"), 1, "合法 destroy 应返回 1");
        let destroys = &handle.store.data().pending_destroys;
        assert_eq!(destroys.len(), 1, "应排队 1 个销毁请求");
        assert_eq!(destroys[0].destroyer, destroyer, "发起者应为沙箱绑定实体");
        assert_eq!(
            destroys[0].target,
            EntityId(Uuid::from_u128(0x0102_0304_0506_0708_FFFF_FFFF_FFFF_FFFF)),
            "目标 id 应由 i64 两半无失真重建"
        );
        assert!(
            handle.store.data().api_calls.contains(&"destroy".to_string()),
            "destroy 调用应记入 api_calls 元数据"
        );
    }

    // === env.hover import（期四第七刀：玩家代码每拍悬停声明，接入物理免重力集合）===

    /// 悬停声明 WAT：on_tick 调 hover()（无参无返回）
    const HOVER_WAT: &str = r#"(module
  (import "env" "hover" (func $hover))
  (global $called (mut i32) (i32.const 0))
  (func (export "on_tick")
    call $hover
    global.get $called i32.const 1 i32.add global.set $called)
  (func (export "get_called") (result i32) global.get $called)
  (func (export "life") (result i32) i32.const 1))"#;

    #[test]
    fn hover_import_marks_declaration_for_current_tick() {
        // hover()：玩家代码声明"本拍我要悬停"——import 置位 SandboxData.hover_declared，
        // host 阶段一收件后并入本拍免重力集合（与操控悬停同集合同机制，#030 升力独立；
        // #029 无税：悬停是持续对抗世界规则的代码声明，每拍需重申，不延续）
        let mgr = SandboxManager::new().unwrap();
        let mut handle = mgr.create_sandbox(&wasm(HOVER_WAT), 100_000, id(901)).unwrap();
        assert!(
            !handle.store.data().hover_declared,
            "未调用 hover 前声明位应为 false"
        );
        mgr.call_on_tick(&mut handle).unwrap();
        assert!(
            handle.store.data().hover_declared,
            "on_tick 调 hover 后声明位应为 true"
        );
        assert!(
            handle.store.data().api_calls.contains(&"hover".to_string()),
            "hover 调用应记入 api_calls 元数据"
        );
    }
}
