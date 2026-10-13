# CodeWorld 技术方案文档 v2.1

> 代码即现实的开放世界游戏框架
> 日期：2026-07-23
> 状态：Phase 1 MVP 核心循环完成（8 crate / 291 测试），tick 循环完整跑通

### v2.1 更新（2026-07-23）
- 渲染模块借鉴寒霜 FrameGraph 架构（GDC 2017, Yuriy O'Donnell），三阶段管线：Setup声明 -> Compile裁剪+排序 -> Execute绘制
- 物理模块 V2 设计完成（docs/physics-v2-design.md），借鉴 PhysX 5 & Chaos，Phase 2 实施
- 主程序借鉴 Screeps 两阶段 tick 模型 + AzerothCore 管理器链 + Bevy 插件式组合
- 渲染模块文件结构更新：graph.rs/buffer.rs/viewport.rs/module.rs（原 ascii.rs/layer.rs/protocol.rs 已废弃）
- MVP 缺口记录：WASM 玩家代码执行/PendingChanges/终端 ANSI 渲染/生存点数扣减/接触触发通道/击杀掠夺

---

## 一、系统架构总览

### 1.1 模块化设计原则

系统采用模块化设计，每个原语是独立模块，模块下按能力拆子模块。

核心原则：**子模块按"能力"拆，不按"规则"拆。**

- 能力 = 框架能做什么（probe能探测、attack能覆写、snapshot能存历史）
- 规则 = 怎么用、谁能用、代价多大（这些是玩家代码的事）

子模块对外只暴露接口，内部实现随便换。接口是稳定契约，实现可替换。
改实现（怎么做的）不影响其他模块；改接口（做什么的）才影响调用方。

对标传统游戏引擎的模块化（Bevy的Plugin系统、Unreal的Subsystem），
但CodeWorld的模块边界是"存在性原语"而非"功能域"：
- 传统引擎模块是胖的（塞满规则：重力=9.8、碰撞=弹开、质量影响速度）
- CodeWorld模块是瘦的（只有接口+默认实现，规则由玩家代码定义）

### 1.2 五大原语模块

```
┌─────────────────────────────────────────────────────────────┐
│                    玩家代码层 (WASM)                          │
│  实体定义 / 技能 / 攻防 / 服务 / 区域规则 / 时空篡改           │
├─────────────────────────────────────────────────────────────┤
│                   code-arena (代码竞技场)                     │
│  ┌──────────┬──────────┬──────────┬───────────────────────┐ │
│  │ sandbox  │ sandbox  │ sandbox  │  compute-broker       │ │
│  │ entity-A │ entity-B │ entity-C │  (算力调度/验证)       │ │
│  └──────────┴──────────┴──────────┴───────────────────────┘ │
│         沙箱通道: probe / attack / call                      │
│         被动观测: gas/内存/API调用元数据                      │
├─────────────┬─────────────┬──────────────┬─────────────────┤
│ time-engine │ space-engine│  physics     │    renderer      │
│ 时钟+快照    │ 坐标+变换    │ 规则+覆盖     │  状态->图形(ECS) │
├─────────────┴─────────────┴──────────────┴─────────────────┤
│                    world-core                                │
│  时间存在 · 空间存在 · 渲染存在 · 沙箱通道 · 物理规则 · 持久化 │
├─────────────────────────────────────────────────────────────┤
│              storage (MVP: SQLite全管 / Phase2+: SQLite+RocksDB+Git) │
└─────────────────────────────────────────────────────────────┘
```

### 1.3 分层职责

| 层 | 职责 | 不做的事 |
|---|---|---|
| world-core | 提供存在性原语（时间/空间/渲染/通道/物理），持久化，配额 | 不定义玩法、不判断合法性、不广播事件 |
| time-engine | 维护全局tick，快照，时间篡改支持 | 不规定回溯代价、不限制谁可以篡改 |
| space-engine | 维护坐标系，变换管线，区域边界 | 不规定扭曲规则、不预设安全区 |
| physics | 提供基础物理规则（重力/碰撞/地面约束），规则覆盖接口，冲突执行 | 不判断规则公平、不裁决规则碰撞（视为攻击） |
| code-arena | WASM沙箱管理，沙箱通道，被动观测，算力调度 | 不判断攻防公平、不干预代码执行 |
| renderer | 把state对象画出来 | 不验证state合法性、不干预游戏逻辑 |
| 玩家代码 | 一切游戏内容 | 受沙箱物理隔离约束 |

### 1.4 模块子结构

每个模块按能力拆子模块：

**时间模块 (time-engine)**
- tick计数器 -- 能力：提供"现在第几tick"这个数字
- 快照存储 -- 能力：存世界状态/恢复世界状态
- 时间篡改接口 -- 能力：回溯/加速/冻结的接口

**空间模块 (space-engine)**
- 坐标系 -- 能力：get_pos/set_pos，读写坐标
- 变换管线 -- 能力：注册一个坐标变换函数
- 区域注册 -- 能力：注册一个带规则的区域

**物理规则模块 (physics)**
- 默认规则 -- 能力：框架自带重力/碰撞/地面约束（厚版本，让新手能上手）
- 规则覆盖接口 -- 能力：玩家注册自定义物理规则（自身/区域/全局三种作用域）
- 冲突执行 -- 能力：多条规则产生状态变更时统一执行（不裁决，视为攻击）

**渲染模块 (renderer)**
- 状态采集 -- 能力：从其他模块读当前世界状态
- 渲染后端 -- 能力：把状态画出来（ASCII/Canvas/Bevy可换）
- 可见性筛选 -- 能力：只画可见区域
- 图层排序 -- 能力：按图层优先级+Y轴深度排序绘制（背景<实体<特效<UI，层内近的遮远的）

**沙箱通道模块 (code-arena)**
- 沙箱管理 -- 能力：WASM实例化/LRU缓存/内存限制
- probe通道 -- 能力：主动探测+被动观测
- attack通道 -- 能力：跨沙箱覆写（当前阻塞点）
- call通道 -- 能力：服务调用
- 算力调度 -- 能力：外接算力

---

## 二、核心数据结构

### 2.1 世界状态

```rust
struct WorldState {
    tick: u64,                          // 全局tick计数器
    entities: HashMap<EntityId, Entity>, // 所有存活实体
    zones: HashMap<ZoneId, Zone>,       // 注册的区域
    services: HashMap<ServiceId, Service>, // 注册的服务
    transforms: Vec<TransformEntry>,    // 空间变换注册表
    physics_overrides: Vec<PhysicsOverride>, // 物理规则覆盖注册表
    default_physics: DefaultPhysics,    // 默认物理参数
    snapshots: SnapshotStore,           // 历史快照
}
```

### 2.2 实体

```rust
struct EntityId(pub Uuid);

struct Entity {
    id: EntityId,
    owner: PlayerId,        // 创建者
    code_hash: String,      // WASM模块的git commit hash
    // 注意：wasmtime::Module和Instance不在Entity里
    // 由code-arena层管理（Module全局缓存，Instance按LRU管理）
    // world-core只存元数据
    
    // 位置（逻辑坐标，经过变换管线后的）
    pos: (f64, f64),
    velocity: (f64, f64),   // 速度（物理规则使用）
    
    // 外观与动作状态（角色登录门槛要求）
    appearance: Appearance,  // 渲染数据
    action_state: ActionState, // 当前动作状态
    
    // 指令配额（纯技术保护）+ 生存点数
    instruction_guard: InstructionGuard,
    survival: SurvivalPoints,
    
    // 状态
    alive: bool,
    created_tick: u64,
    
    // WASM内存由code-arena管理
    // 内存上限由框架配置（如64MB），超过则报错（玩家代码写得差，自己的事）
    // 不活跃的实例内存换到磁盘（LRU缓存）
}

/// 外观数据（角色登录门槛：必须有，渲染模块画什么就靠这个）
struct Appearance {
    glyph: String,          // ASCII字符（MVP）或形状标识（后续Canvas/Bevy）
    color: String,          // 颜色
    size: f64,              // 大小
}

/// 动作状态（影响外观显示，如弯腰/蹲下/挥手）
enum ActionState {
    Idle,                   // 待机
    Walking,                // 走路
    Jumping,                // 跳跃
    Crouching,              // 蹲下
    Waving,                 // 挥手
    Custom(String),         // 玩家自定义动作
}

/// 渲染图层（参考Unity Sorting Layer / Unreal Translucency Sort）
/// 图层只管渲染顺序，不影响物理逻辑
/// 同图层内按Y轴排序（画家算法：Y值大的后画=近的遮远的）
enum RenderLayer {
    Background = 0,         // 世界层：地形、地面、背景
    Entity = 1,             // 实体层：玩家、NPC、可交互物体（默认）
    Effect = 2,             // 效果层：特效、粒子、时空扭曲可视化
    UI = 3,                 // 界面层：叠加UI（MVP不用）
}

/// 渲染请求（实体提交给渲染队列的完整数据）
struct RenderRequest {
    layer: RenderLayer,         // 目标图层
    y_sort_value: f64,          // Y轴排序值（通常=实体y坐标，用于层内深度排序）
    appearance: Appearance,     // 外观数据
    pos: (f64, f64),            // 屏幕坐标
    action_state: ActionState,  // 当前动作（影响外观帧）
    priority: i32,              // 同图层同Y值时的微调优先级（默认0）
}

/// 视口内可见实体（感知接口返回）
/// 只包含"眼睛能看到"的信息，不包含代码层面信息
struct VisibleEntity {
    id: EntityId,
    pos: (f64, f64),            // 坐标（可见所以可知）
    appearance: Appearance,     // 外观（可见所以可知）
}

/// 攻击结果
struct AttackResult {
    hit: bool,                  // 是否命中（坐标上有实体）
    intercepted: bool,          // 是否被on_attack拦截
    success: bool,              // 是否击杀成功（life()被覆写为false）
}
```

### 2.3 生存点数（框架管理的唯一硬数值）

生存点数是框架管理的唯一游戏内数值。玩家代码不能直接修改，只能通过框架接口操作。

```rust
struct SurvivalPoints {
    points: u64,            // 当前生存点数（单位：天）
    created_tick: u64,      // 创建时间（用于计算每日消耗）
    last_decay_tick: u64,   // 上次扣减的tick
}
```

框架行为（每tick自动执行）：
```
每 86400 tick（=1天）自动: points -= 1
points == 0 时: 实体死亡（life强制返回false，不论玩家代码怎么写）
```

框架接口（玩家代码通过import调用）：
```rust
import fn get_survival() -> u64;                    // 读自己的生存点数
import fn transfer_survival(to: EntityId, n: u64) -> bool;  
                                                     // 转移点数给别人
                                                     // 只能转自己的
                                                     // 击杀掠夺=破解后调transfer
import fn harvest(input: Bytes) -> bool;            // 获取生存点数（预留，先不开放）
                                                     // 开放时机看世界进度
```

设计原则：
- 初始3天，每天-1，归零死亡
- 击杀其他玩家可掠夺1天（通过transfer_survival）
- harvest接口做好但先不开放，看世界进度决定是否开放
- 不限制harvest获得上限（哪怕永生也没关系）
- 生存点数是唯一框架硬管理的数值，其它一切数值由玩家代码自定义

### 2.4 指令配额（纯技术保护，非游戏数值）

```rust
struct InstructionGuard {
    // 防止玩家代码跑死服务器的技术保护
    // 玩家不应感知到，不影响任何游戏内数值
    per_tick_limit: u64,        // 每tick每实体最大指令数（如100万条）
    consumed_this_tick: u64,
}
```

注意：这不是"能量条"，不是"蓝量"，不是游戏内资源。纯粹是防止死循环/恶意代码吃满CPU的技术措施。超额=本tick跳过该实体（不死亡，下次tick继续）。

### 2.5 区域

```rust
struct ZoneId(pub Uuid);

struct Zone {
    id: ZoneId,
    owner: PlayerId,
    bounds: BoundingBox,       // 区域边界
    transform: Option<TransformFn>, // 区域内的坐标变换
    rules_code_hash: String,   // 区域规则代码（on_enter/on_cross等）
    created_tick: u64,
}

struct BoundingBox {
    x_min: f64, y_min: f64,
    x_max: f64, y_max: f64,
}
```

### 2.6 服务

```rust
struct ServiceId(pub Uuid);

struct Service {
    id: ServiceId,
    owner: EntityId,           // 提供方实体（死亡则服务消失）
    name: String,              // 服务名（用于call查找）
    handler_code_hash: String, // 处理函数代码
    created_tick: u64,
}
```

### 2.7 空间变换

```rust
struct TransformEntry {
    id: TransformId,
    owner: EntityId,
    scope: TransformScope,     // 作用域
    transform_fn: WasmModule,  // 变换函数（WASM编译）
    priority: i32,             // 优先级（多个变换叠加时的顺序）
    created_tick: u64,
}

enum TransformScope {
    Entity(EntityId),          // 只影响自己
    Zone(ZoneId),              // 影响某区域内所有实体
    // 注意：MVP不包含 Global（全局扭曲）
}
```

### 2.8 物理规则

```rust
/// 物理规则覆盖注册条目
struct PhysicsOverride {
    id: PhysicsOverrideId,
    owner: EntityId,           // 注册者（死亡则覆盖失效）
    scope: PhysicsScope,       // 作用域
    rule_code: WasmModule,     // 规则代码（跑在WASM沙箱里）
    priority: i32,             // 优先级（高=先执行）
    created_tick: u64,
}

enum PhysicsScope {
    Entity(EntityId),          // 只影响自己的实体
    Zone(ZoneId),              // 影响某区域内所有实体
    Global,                    // 影响整个世界（改了=暴露面大=可被攻击）
}

/// 默认物理规则参数（框架内置，不可删除，可通过覆盖替换行为）
struct DefaultPhysics {
    gravity: f64,              // 重力加速度（如 -9.8）
    ground_y: f64,             // 地面高度（如 0.0）
    collision_enabled: bool,   // 基础碰撞（两实体不能占同一坐标）
    max_velocity: f64,         // 最大移动速度
}

/// 物理规则执行产生的状态变更（放入PendingChanges队列）
struct PhysicsChange {
    entity: EntityId,
    new_pos: (f64, f64),       // 物理计算后的新坐标
    reason: PhysicsReason,     // 重力/碰撞/覆盖规则
    source_rule: Option<PhysicsOverrideId>, // 哪条覆盖规则产生的
}

enum PhysicsReason {
    Gravity,                   // 默认重力
    Collision,                 // 默认碰撞
    Override(PhysicsOverrideId), // 玩家自定义规则
}
```

设计原则：
- 默认规则不可删除，但可通过覆盖接口替换行为（厚版本）
- 覆盖规则跑在WASM沙箱里，崩溃/超时只影响该规则本身
- 多条规则同时产生状态变更时不裁决，视为攻击（规则互相attack，胜者留下）
- 都活下来则后执行的覆盖先执行的（last-write-wins）
- 改默认物理规则=注册全局覆盖=暴露面大=可被其他玩家probe到并attack

---

## 三、框架接口定义

### 3.1 玩家代码暴露的函数（WASM export）

玩家代码必须/可选导出以下函数，框架通过这些函数与玩家代码交互：

```rust
// === 必须 ===
export fn life() -> bool;
// 返回false = 死亡。框架每tick调用检查。
// 攻击者的目标是让这个返回false。

export fn on_tick(world: WorldHandle) -> void;
// 每tick调用。玩家的主要逻辑入口。
// 通过world句柄调用框架API。

// === 可选 ===
export fn on_probe(request: Bytes) -> Bytes;
// 被探测时调用。返回任意数据（可造假）。
// 如果不导出，probe返回框架默认的被动观测数据。

export fn on_attack(payload: Bytes) -> AttackResult;
// 被攻击时调用。处理攻击payload。
// 如果不导出，attack直接作用于life()函数。
// 返回 AttackResult { intercepted: bool, damage: ... }

export fn on_enter_zone(zone: ZoneHandle) -> bool;
// 进入区域时调用。返回true=允许进入，false=拒绝。
// 如果不导出，区域边界由zone owner的规则判定。

export fn on_entity_destroyed(entity: EntityId) -> void;
// 某实体死亡时调用（仅当你的代码注册了依赖）。
// 注意：这不是广播，只有显式依赖（如正在call该实体服务）才会收到。
```

### 3.2 框架提供给玩家代码的API（WASM import）

玩家代码通过 `world` 句柄调用框架API。所有API都是WASM import函数：

```rust
// === 时间接口 ===
import fn tick() -> u64;                        // 读当前tick
import fn register_tick_callback(cb: FnHandle) -> void;
import fn get_history_snapshot(target_tick: u64) -> SnapshotHandle;
                                                // 只读历史快照（MVP）
                                                // v2: 支持状态替换（真正回溯）

// === 空间接口 ===
import fn get_pos() -> (f64, f64);
// 读取自己的坐标。不能读其他实体的坐标。
import fn set_pos(x: f64, y: f64) -> void;
// 设置自己的坐标。经过物理规则校验（碰撞挤压等）和Space Engine变换管线。
import fn register_transform(fn: FnHandle, scope: TransformScope, priority: i32) -> TransformId;
import fn remove_transform(id: TransformId) -> void;

// === 感知接口 ===
import fn get_visible_entities() -> Vec<VisibleEntity>;
// 获取当前视口内可见的实体列表（渲染模块已在画的那些）。
// 不暴露屏幕外实体的任何信息。
// VisibleEntity { id: EntityId, pos: (f64, f64), appearance: Appearance }
// 这是"眼睛"--你能看到远处有个人，但不知道他在想什么。
// 要看代码层面的信息需要probe。

// === 渲染接口 ===
import fn render(request: RenderRequest) -> void;
// 提交渲染请求。框架按图层优先级+Y轴深度排序后绘制。
// 图层优先级：Background(0) < Entity(1) < Effect(2) < UI(3)
// 同图层内按y_sort_value排序（画家算法：Y值大的后画=近的遮远的）
// 同图层同Y值时按priority微调
// 玩家代码默认提交到Entity层，可通过RenderRequest指定其他图层

import fn set_render_layer(layer: RenderLayer) -> void;
// 设置实体默认渲染图层（不调用则默认Entity层）
// 例：切换到Effect层可实现半透明特效状态

// === 实体接口 ===
import fn spawn(code: Bytes) -> EntityId;
// 注入新实体。受配额限制。
import fn destroy(entity: EntityId) -> void;
// 销毁实体。只能destroy自己spawn的或通过attack击杀的。

// === 沙箱通道 ===
// 通道开启条件：
// - 视距内：通过get_visible_entities()发现目标 -> probe/attack_at
// - 接触式：物理碰撞触发 -> 框架自动为双方开启通道 -> 双方获得对方EntityId和坐标
// - 超视距：玩家自行定位坐标 -> attack_at(推测坐标) -> 可能miss

import fn probe(target: EntityId, request: Bytes) -> ProbeResult;
// 探测目标。需要先通过感知或接触获得EntityId。
// 返回 {
//   response: Bytes,           // 目标on_probe返回的数据（可造假）
//   meta: ProbeMeta,           // 被动观测（不可伪造）
// }
// ProbeMeta包含: gas_consumed, mem_read_count, mem_write_count, api_calls

import fn attack_at(pos: (f64, f64), payload: Bytes) -> AttackResult;
// 攻击指定坐标。不是攻击EntityId，是攻击一个位置。
// 框架检查该坐标上有没有实体：
// - 有 -> 命中，执行attack流程（销毁旧实例+用payload重建）
// - 没有 -> 空打，攻击落空（可能miss）
// payload是完整的WASM模块，替换目标的整个代码。
// 返回 { hit: bool, intercepted: bool, success: bool }
// hit=false表示坐标上没有实体（miss）
// success=true表示击杀成功（life()被覆写为false）

import fn compute(task: ComputeTask) -> ComputeResult;
// 提交计算任务给外接算力
// ComputeTask { task_type: TaskType, input: Bytes }
// TaskType: HashCrack | BruteForce | PatternAnalysis | Custom
// 不接外接算力时用基础算力慢跑

import fn call(service_name: String, params: Bytes) -> CallResult;
// 调用服务。返回 {
//   data: Option<Bytes>,       // 服务返回的数据
//   status: CallStatus,        // Ok | Void | Timeout
// }
// 目标死亡/服务不存在 -> 返回Void（不报错，不泄露信息）

// === 注册接口 ===
import fn register_zone(bounds: BoundingBox, rules_code: Bytes) -> ZoneId;
import fn register_service(name: String, handler_code: Bytes) -> ServiceId;
// 注册服务。服务名可重复（不同实体可注册同名服务）
// 框架内部用 EntityId:name 作为唯一标识
// call时可以指定 "EntityId:name" 精确调用，也可以只传name获取所有同名服务列表

// === 物理规则接口 ===
import fn register_physics_override(
    scope: PhysicsScope,
    rule_code: Bytes,
    priority: i32
) -> PhysicsOverrideId;
// 注册物理规则覆盖。
// scope=Entity: 只影响自己（"我免疫重力"）
// scope=Zone: 影响区域内所有实体（"我的领地重力横着拉"）
// scope=Global: 影响整个世界（暴露面大，可被probe到并attack）
// rule_code是WASM字节码，跑在沙箱里。
// 注册后该规则每tick被执行，产生的状态变更放入PendingChanges队列。
// 改默认物理规则=注册覆盖=代码在沙箱里=可被attack覆写。

import fn remove_physics_override(id: PhysicsOverrideId) -> void;
// 注销自己注册的物理规则覆盖。

import fn get_default_physics() -> DefaultPhysics;
// 读取当前默认物理参数（重力/地面/碰撞/速度上限）。
// 玩家代码可以读取这些值来决定如何适应或覆盖。

import fn get_active_overrides(entity: EntityId) -> Vec<PhysicsOverrideId>;
// 查询某实体当前受哪些覆盖规则影响。
// 可用于probe时判断对方是否修改了物理规则。

// === 持久化 ===
import fn save_state() -> void;    // 手动触发状态保存
// 框架也会每100tick自动保存

// === 生存点数（框架管理的唯一硬数值） ===
import fn get_survival() -> u64;                    // 读自己的生存点数
import fn transfer_survival(to: EntityId, n: u64) -> bool;
                                                    // 转移点数给别人（只能转自己的）
                                                    // 击杀掠夺=击杀成功时框架自动从死者转移1天给杀手
                                                    // 玩家代码不需要也不能"以受害者身份调transfer"
import fn harvest(input: Bytes) -> bool;            // 获取生存点数（预留，先不开放）
                                                    // 开放时机看世界进度决定
```

### 3.3 接口约束与配额

设计原则：框架不提供任何游戏资源（无金币/点卡/能量），只提供基础设施（沙箱/通道/渲染/基础世界架构）。
以下约束分两类：
- 技术保护：防死循环、防内存爆炸、防DoS。玩家不应感知，不影响游戏内数值。
- 物理限制：payload大小、渲染频率等，防止服务器资源耗尽。

```rust
struct Constraints {
    // spawn配额（防刷屏）
    spawn_per_player_per_tick: 1,
    spawn_total_per_player: 100,
    
    // 指令配额（技术保护：防死循环，非游戏数值）
    // 这是每tick每实体的总执行预算，玩家在on_tick里自由分配
    // 花在移动/probe/attack上全由玩家代码决定，框架不收手续费
    tick_callback_instruction_limit: 1_000_000,  // 每tick每实体100万条指令
    tick_callback_timeout_ms: 100,       // 100ms超时
    
    // probe（技术保护：防死循环+防DoS，不是游戏消耗）
    // probe不扣手续费，占用的是on_tick的执行时间
    probe_target_instruction_limit: 10_000,     // 被探测方on_probe预算（技术保护）
    probe_timeout_ms: 50,
    probe_max_call_depth: 3,             // 防止跨沙箱死锁
    
    // attack（技术保护：防死循环，不是游戏消耗）
    // attack不扣手续费，占用的是on_tick的执行时间
    attack_payload_max_size: 64 * 1024,  // 64KB（防内存爆炸）
    attack_target_instruction_limit: 200_000,  // 被攻击方on_attack预算（技术保护）
    attack_timeout_ms: 200,
    
    // call（技术保护）
    call_timeout_ms: 100,
    call_response_max_size: 64 * 1024,
    call_per_tick: 10,                   // 防DoS
    
    // compute（技术保护）
    compute_timeout_ms: 5000,            // 外接算力5秒超时
    compute_fallback: true,              // 超时降级基础算力
    
    // render（物理限制）
    render_state_max_size: 64 * 1024,
    render_per_tick: 1,                  // 每tick最多提交1次渲染状态
    
    // physics override（物理限制）
    physics_override_per_entity: 10,     // 每实体最多注册10条覆盖规则
    physics_override_instruction_limit: 200_000, // 每条规则执行预算（技术保护）
    physics_override_timeout_ms: 50,     // 超时则该规则本tick不生效
    physics_global_override_total: 5,    // 全世界最多5条全局覆盖
}
```

---

## 四、核心流程

### 4.1 Tick 主循环

```
参考架构：Screeps（编程MMO）的任务队列 + 多机并行模型

每个tick执行流程：

1. time-engine: tick++

2. 阶段一：玩家代码执行（并行）
   a. 把所有存活实体放进任务队列
   b. 线程池从队列并行取任务（每个实体一个独立线程）
   c. 每个线程内：
      - 重置指令配额（InstructionGuard）
      - 生存点数检查：用区间计算 (current_tick - last_decay_tick) / 86400
        归零则标记死亡
      - 调用 entity.on_tick(world)
        - 执行玩家代码
        - 消耗指令配额
        - 超时/超额 -> 强制终止该线程（不死亡，下次tick继续）
      - 调用 entity.life()
        - 返回false -> 标记死亡
      - 如果实体提交了render(request) -> 存入渲染队列
   d. 所有线程完成后进入阶段二
   
   注意：
   - 玩家代码执行期间产生的所有状态变更（spawn/destroy/attack等）
     放入延迟队列（PendingChanges），不立即应用
   - 玩家代码读取的是tick开始时的快照状态
   - 同tick内的变更在阶段二统一应用

3. 阶段二：状态更新（串行）
   a. 应用 PendingChanges（spawn/destroy/attack结果等）
   b. physics: 执行物理规则
      - 执行默认物理规则（重力/碰撞/地面约束）
        * 重力：对每个实体 y -= gravity * dt
        * 地面约束：if y < ground_y then y = ground_y
        * 碰撞：检测坐标重叠，产生挤压（一方被挤走）
          坐标不能有多个实体，挤压由物理规则强制执行
      - 接触/挤压触发：框架自动为碰撞双方开启沙箱通道
        * 双方获得对方的EntityId和坐标
        * 是否probe/attack由玩家代码决定，框架只负责开通道
      - 执行覆盖规则（按priority排序，高优先级先执行）
        * 每条覆盖规则跑在WASM沙箱里
        * 超时/崩溃 -> 该规则本tick不生效，其他规则不受影响
        * 产生的PhysicsChange放入PendingChanges队列
      - 多条规则对同一实体产生冲突变更时：
        * 规则碰撞视为攻击，胜者的变更生效
        * 都活下来则后执行的覆盖先执行的（last-write-wins）
   c. space-engine: 处理所有set_pos请求
      - 遍历所有transform（按priority排序）
      - 应用变换: physical_pos = transform_n(...transform_1(logical_pos))
      - 检查zone边界（zone规则判定）
      - 写入最终物理坐标
   d. 死亡清理：移除所有标记死亡的实体
      - 静默删除代码和状态
      - 击杀掠夺：框架自动从死者转移1天生存点数给杀手
      - 联系断裂：service/transform/zone/physics_override移除，call返回Void
   e. 渲染：从渲染队列取RenderRequest，按图层排序后绘制
      - 按图层优先级分组：Background < Entity < Effect < UI
      - 同图层内按y_sort_value排序（画家算法：Y值大的后画=近的遮远的）
      - 同图层同Y值时按priority微调
      - 绘制顺序：从底到顶，后画的覆盖先画的

4. 每100tick: 异步快照（不阻塞主循环）
```

并行模型说明：
```
  ┌──────────────┐
  │  任务队列     │  所有活跃实体
  │  [A][B][C]... │  
  └──────┬───────┘  
         │          
    ┌────┼────┬────┬────┐
    ▼    ▼    ▼    ▼    ▼   线程池并行取任务
   核心A 核心B 核心C 核心D 核心E
    │    │    │    │    │
    ▼    ▼    ▼    ▼    ▼
   A执行 B执行 C执行 D执行 E执行
    │    │    │    │    │
    └────┴────┴────┴────┘
         │
         ▼
  ┌──────────────┐
  │  延迟队列     │  收集所有变更
  │  统一应用     │
  └──────────────┘

  - 每个实体在独立线程执行，互不阻塞
  - 某个实体代码卡住只影响自己（超时被杀）
  - 100组玩家同时攻击 = 100个线程同时跑
  - 线程池大小 = CPU核心数，超出部分排队等待
```

### 4.2 攻击流程

```
攻击前置条件：攻击方必须知道目标坐标
- 视距内：get_visible_entities()看到目标 -> 获得坐标
- 接触式：物理碰撞触发 -> 框架自动开通道 -> 双方获得对方坐标
- 超视距：玩家自行定位（分析probe数据/移动痕迹等）-> 推测坐标

玩家A调用 attack_at((x, y), payload):

1. A调用attack_at，占用A的on_tick执行时间（无额外手续费）
3. 框架检查坐标(x,y)上有没有实体：
   a. 没有 -> 返回 { hit: false } （攻击落空，miss）
   b. 有实体B -> 命中，继续执行attack流程
4. 在B的沙箱中执行：
   a. 如果B导出了on_attack:
      - 给on_attack分配attack_target_instruction_limit预算
      - 执行 on_attack(payload)
      - on_attack可以:
        - 拦截 (intercepted=true) -> 攻击失败
        - 部分拦截 -> 减轻伤害
        - 反制 -> 在A的沙箱触发事件
        - 卡死/超时 -> on_attack本身指令配额耗尽
          -> 框架使用默认行为：life()维持原值（不被覆写）
          -> B被"囚禁"（本tick无法行动），但不死
   b. 如果B没导出on_attack:
      - payload直接替换B的整个WASM模块（路线A：整模块替换）
      - 替换后life()返回false -> B死亡
5. 返回AttackResult给A
6. 如果B死亡:
   a. 框架自动从B转移1天生存点数给A（击杀掠夺）
   b. 静默删除B的代码和状态
   c. B注册的所有service标记为Void
   d. B注册的所有transform移除
   e. 正在call B的实体收到Void
   f. 不广播，不记录公共日志

注意：attack_at是攻击坐标不是EntityId
- 目标移动后坐标变化，攻击可能miss
- 这天然实现了"攻击可以躲避"
- payload内容决定攻击效果：击杀(life=false)/击晕(life=true但on_tick瘫痪)/绑架(on_tick改成移动)
```

#### 攻击语义铁律（2026-10-13 定）

1. 攻击只伤对方的**代码**（换血/覆写）与**算力**（防守烧 fuel），不碰生存点数。
2. 生存点数唯一变动路径：时间流逝衰减 + 击杀瞬间掠夺转移。攻击路径无任何 survival 写操作。
3. 血条/血量概念只允许存在于渲染层展示，不存在于数据模型。
4. 掠夺只属于击杀：无击杀=无掠夺（无吸血式）。

#### 两阶段结算模型（2026-10-13 定，取代逐刀即时结算）

玩家代码在 on_tick 期间调 env.attack_at 只排队；tick 阶段二 a2 步按两阶段处理：

1. 收件阶段（deliver_attacks）：过滤已死攻击者 -> 坐标解析目标 -> 对每个目标的
   【当前沙箱】逐刀调用 on_attack。收件期间不换血、不击杀；同一防守者面对全部
   压力源，每次 on_attack 消耗防守者 fuel（围殴=磨算力，机制自然成立，无额外代码）。
   防守询问失败（如 fuel 耗尽）按默认行为处理：该刀作废，life 维持原值不被覆写。
2. 结算阶段（settle_attacks）：未拦截的刀【全局按攻击者 EntityId 升序】依次执行
   换血/击杀；每刀执行前再查攻防双方存活——攻击者已被同批更早的刀击杀则其刀作废
   （点数不蒸发）；目标已被先执行的刀击杀则后到的刀跳过（先执行者的换血生效）。
   击杀成功才掠夺 1 生存点数。

技术保护（同日定）：
- payload 上限 1MB：env.attack_at import 层拒绝超限请求（返回 0，不入队），
  防大块内存拷贝与不受 fuel 约束的编译阻塞 tick。
- 换血 fuel 继承：新沙箱配额取旧沙箱剩余量（非原配额上限），防反复被攻击回满刷算力。
- 先建后拆：沙箱替换先编译新模块、成功才销毁旧沙箱；payload 非法时旧沙箱原样保留，
  "存活实体必有沙箱"不变量不破（防僵尸实体）。

占有权与防守权：on_attack 由目标坐标上的【当前占有者】的代码触发。后到的攻击
触发的是当前占有者的 on_attack（防守权跟随占有权），与攻击发起时坐标上是谁无关。


### 4.3 探测流程

```
玩家A调用 probe(B, request):

1. A调用probe，占用A的on_tick执行时间（无额外手续费）
2. 在B的沙箱中执行：
   a. 如果B导出了on_probe:
      - 给on_probe分配probe_target_instruction_limit预算（技术保护防死循环）
      - 执行 on_probe(request)
      - B可以返回任意数据（假数据/真数据/部分数据）
      - B的on_probe超时/卡死 -> 返回默认空响应
   b. 如果B没导出on_probe:
      - 返回空响应
3. 框架收集被动观测数据（B无法伪造）：
   - B执行on_probe消耗的gas
   - B的内存读写次数
   - B调用的API列表
4. 返回 ProbeResult {
     response: B返回的数据（可造假）,
     meta: 被动观测数据（不可伪造）,
   }
5. A拿到ProbeResult后自己决定怎么用
   - 小白: 把response+meta喂给AI分析
   - 程序员: 自己写分析代码

注意：probe能否成功取决于双方代码能力，不消耗游戏资源。
攻击方分析代码强就探测得到，防御方on_probe代码强就探测不到。
框架只提供通道和超时保护，不收费不限制次数（次数由on_tick总预算自然约束）。
```

### 4.4 算力流程

```
玩家A调用 compute(task):

1. 检查A是否注册了外接算力
   a. 有外接算力:
      - 把task发给算力节点
      - 等待结果（超时5秒）
      - 验证结果（见下文）
      - 验证通过 → 返回结果
      - 验证失败 → 返回错误
      - 超时 → 降级为基础算力执行
   b. 无外接算力:
      - 用服务器基础算力执行task
      - 速度慢但能跑完
      - 返回结果

2. 结果验证策略（按task类型）:
   - HashCrack: 直接验证（算出来的hash对不对）
   - BruteForce: 抽样验证（随机抽10%的结果检查）
   - PatternAnalysis: 重放验证（用基础算力重跑一小段对比）
   - Custom: 不验证（玩家自担风险）
   
   注意：外接算力只跑计算任务，不跑角色核心代码
   角色核心代码始终在服务器沙箱内，不存在代码泄露
```

### 4.5 死亡与清理

```
实体B死亡时：

1. 标记B.alive = false
2. 从WorldState.entities中移除B
3. 遍历services: B注册的服务 → 标记Void
   - 正在call这些服务的实体 → 收到CallResult{status: Void}
4. 遍历transforms: B注册的变换 → 移除
   - 空间扭曲消失，受影响区域恢复默认坐标
5. 遍历zones: B注册的区域 → 移除
   - 区域内的实体不再受该区域规则约束
6. 删除B的WASM实例和内存快照
7. 保留B的git历史（审计用，不影响游戏世界）
8. 不广播，不记录公共日志

第三方发现B死亡的方式:
- probe(B) → 返回Void（与"不存在""超时""拒绝"相同）
- call(B.service) → 返回Void
- 框架不区分"死了""不存在""隐身了"
```

### 4.6 时间成长流程

```
玩家C种花示例：

// player_c.wasm (AssemblyScript伪代码)
let birth_tick = world.tick()
let mature = false

export function on_tick(world: WorldHandle): void {
    let age = world.tick() - birth_tick
    if (age >= 36000 && !mature) {   // 10小时 = 36000 tick
        mature = true
        world.register_service("heal_aura", heal_aura_code)
        world.render({ type: "flower", state: "mature", color: "gold" })
    } else if (!mature) {
        world.render({ 
            type: "flower", 
            state: "growing", 
            size: age / 36000,
            color: "green" 
        })
    }
}

export function life(): bool {
    return true   // 花不会自己死，除非被攻击
}
```

玩家D获得花的加成：

```
// player_d.wasm
export function on_tick(world: WorldHandle): void {
    // 调用花的服务
    let result = world.call("heal_aura", { target: self })
    if (result.status == CallStatus.Ok) {
        // 加成生效
        self.defense_boost = result.data.parse()
    } else {
        // Void = 花死了或服务消失了
        self.defense_boost = 0
        // D想在新角色带上？得自己拿到heal_aura的代码
    }
}
```

---

## 五、技术选型详述

### 5.1 运行时

| 组件 | 选型 | 理由 |
|---|------|---|
| 内核语言 | Rust | 内存安全 + WASM原生支持 + 零成本抽象 |
| WASM运行时 | wasmtime v25+ | 指令计量 + 内存限制 + 成熟稳定 |
| 异步运行时 | tokio | Rust生态标准，处理并发IO |
| 序列化 | rkyv / MessagePack | 零拷贝序列化，跨沙箱高效传数据 |

### 5.2 存储

| 组件 | MVP选型 | Phase2+选型 | 用途 |
|---|---|---|---|
| 元数据 | SQLite + WAL | SQLite + WAL | 实体注册表、区域、服务、配置 |
| WASM内存快照 | SQLite（BLOB字段） | RocksDB (LSM-Tree) | 高频写入的内存页差分存储 |
| 玩家代码 | Git仓库 (bare repo) | Git仓库 (bare repo) | 每个玩家一个git仓库，代码版本管理 |
| 快照索引 | SQLite | SQLite | 快照元数据（tick范围、实体覆盖） |

MVP先用SQLite全管，理由：
- MVP只有几百个实体，SQLite完全够
- RocksDB的Rust绑定编译时间长，增加环境复杂度
- 先跑通核心循环，再优化存储

Phase 2引入RocksDB的切换理由（来自Council审查）：
- SQLite的B+树处理高频二进制增量会退化
- RocksDB的LSM-Tree天然适合高频写
- Git全局只存一份代码，实体状态只存commit hash

### 5.3 渲染

| 阶段 | 方案 | 说明 |
|---|---|---|
| MVP | 终端ASCII | 验证逻辑，零依赖 |
| v2 | Web Canvas (浏览器) | 2D图形，WebSocket实时推送 |
| v3 | Bevy (Rust引擎) | 3D，ECS原生支持 |

渲染标准协议（ECS风格state + 世界单位 + 挂载系统）：

```typescript
// === 基础单位 ===
// 所有尺寸使用世界单位（world unit），1 world unit = 1米
// 渲染器负责将世界单位映射到屏幕像素（根据缩放比例）

// === RenderState：每tick提交给渲染器的完整状态 ===
interface RenderState {
    version: 1,                    // 协议版本号
    entity_id: string,             // 提交方实体ID
    components: {
        // 基础变换
        position?: { x: number, y: number, z?: number },
        rotation?: { angle: number, axis?: "z" },
        scale?: { x: number, y: number, z?: number },

        // 形状（世界单位）
        shape?: {
            type: "circle" | "rect" | "polygon" | "line" | "sprite" | "text" | "particle",
            // circle: { type:"circle", radius: 0.5 }  // 半径0.5米
            // rect:   { type:"rect", width: 1.2, height: 0.8 }  // 1.2米×0.8米
            // polygon: { type:"polygon", points: [[0,0],[1,0],[0.5,1]] }
            // sprite: { type:"sprite", src: "url_or_fileid", width: 1.5, height: 2.0 }
            // text:   { type:"text", content: "Hello", size: 0.3, color: "#FFF" }
            // particle: { type:"particle", count: 20, spread: 0.5, color: "#F00" }
            [key: string]: any,
        },

        // 图层（渲染顺序）
        layer?: number,            // 0=背景, 10=地面, 20=实体, 30=物品, 40=特效, 50=UI

        // 挂载声明（见下方挂载系统）
        anchors?: AnchorDecl[],    // 本实体声明的挂载点
        mounted_on?: MountInfo,    // 本实体挂载到谁身上

        // 效果
        effect?: { kind: "explosion" | "aura" | "trail" | "shake", frame: number, ...params },

        // 玩家自定义渲染数据（渲染器不解释，原样传递给自定义渲染扩展）
        custom?: Record<string, any>,
    }
}

// === 挂载系统 ===

// 挂载点声明：实体声明自己身上有哪些挂载位置
interface AnchorDecl {
    name: string,              // 挂载点名，如 "hand_r", "head", "back"
                               // 可以用标准名，也可以自定义（如 "left_horn"）
    position: { x: number, y: number, z?: number },
                               // 挂载点在本实体局部坐标系中的位置（世界单位）
    rotation?: { angle: number },
    scale_limit?: { min: number, max: number },
                               // 可选：挂载物尺寸限制（世界单位）
                               // 框架不强制检查，仅作为元数据
                               // 检查与否由玩家代码决定
}

// 挂载信息：当实体A挂载到实体B的某个挂载点上
interface MountInfo {
    parent_entity_id: string,  // 宿主实体ID
    anchor_name: string,       // 挂到宿主的哪个挂载点
    offset?: { x: number, y: number },  // 可选：在挂载点基础上的偏移
    scale_mode: "absolute" | "relative",
                               // absolute: 用物品自己的尺寸（世界单位）
                               // relative: 按宿主尺寸比例缩放
    relative_scale?: number,   // scale_mode="relative"时，物品尺寸=宿主尺寸×此值
}
```

渲染协议设计原则：

1. 框架只定义协议（数据格式），不判断内容
   - 框架不检查"巨剑是否合理"，只负责把state画出来
   - 小人拿百米巨剑会正常渲染，这是开放世界的特性

2. 挂载点完全由玩家代码自定义
   - 框架不提供任何标准挂载点
   - 玩家在角色/物品代码中自己声明挂载点（名字、位置随意）
   - 可以叫 "hand_r" 也可以叫 "left_horn" 或任何名字
   - 由此产生的奇葩造型（小人拿百米巨剑、头上卡座山）不需要考虑
   - 挂载时按名匹配，框架只负责定位，不判断合不合理

3. 尺寸检查是玩家代码的事
   - 框架在 AnchorDecl 中提供 scale_limit 字段，但仅为元数据，不强制
   - 玩家代码在 mount(item) 时自己决定是否检查尺寸
   - 不检查 = 可能被挂百米巨剑（视觉奇葩但逻辑合法）
   - 检查了 = 拒绝过大/过小的物品

4. 挂载关系是代码层面的
   - mount/unmount 需要框架提供接口（见下方新增接口）
   - 挂载关系持久化在世界状态中
   - 宿主死亡时，挂载的物品释放（掉落到宿主位置，或随宿主一起删除，由宿主代码决定）

新增框架接口（挂载系统）：
```rust
import fn mount(item_entity_id: EntityId, target_anchor: String) -> MountResult;
// 把一个已存在的实体挂到目标的某个挂载点上
// 只接受EntityId，不接受原始代码（防止绕过spawn配额注入恶意代码）
// 返回 { success: bool, anchor_name: String }
// 失败原因：挂载点不存在 / 目标已死亡 / 挂载点已被占用（除非目标代码允许多个）

import fn unmount(mount_id: MountId) -> void;
// 从挂载点卸下

import fn get_anchors(entity: EntityId) -> Vec<AnchorDecl>;
// 查询目标实体声明的挂载点
// 注意：这会暴露对方的锚点布局（可被探测，符合"不物理隔离"原则）

import fn set_mount_policy(policy: MountPolicy) -> void;
// 设置自己的挂载策略
// MountPolicy { allow_multiple: bool, require_scale_check: bool, ... }
// 玩家代码自定义挂载规则
```

### 5.4 玩家代码语言

| 语言 | 定位 | 优势 | 劣势 |
|---|---|---|---|
| AssemblyScript | 入门首选 | 类TypeScript，AI生成友好 | 底层控制弱 |
| Rust | 进阶 | 性能+底层控制+生态 | 门槛高 |
| C/C++ | 进阶 | 最大控制力 | 内存安全靠人 |
| Zig | 探索 | 简洁+性能 | 生态小 |

框架提供"防御原语标准库"（Council建议）：
- 内置加密/混淆内存操作API
- 让AS开发者也能调用底层防御能力
- 不需要手写底层防御代码

### 5.5 算力接入

```
玩家本地运行 compute-daemon（轻量进程）

连接方式:
  MVP: 本地子进程 (stdin/stdout)
  v2: WebSocket (支持远程)
  v3: 独立客户端 (GUI)

协议:
  框架 → daemon: { task_id, task_type, input, difficulty }
  daemon → 框架: { task_id, output, pow_proof }

任务类型:
  HashCrack:     给定hash和字符集，找到原文
  BruteForce:    给定验证函数，枚举输入空间
  PatternAnalysis: 给定数据集，找模式/特征
  Custom:        玩家自定义任务（不验证，自担风险）
```

---

## 六、WASM 沙箱设计

### 6.1 沙箱边界

```
┌─────────────── 玩家A的WASM沙箱 ───────────────┐
│                                               │
│  Export (可被attack覆写):                     │
│    life(), on_tick(), on_probe(),            │
│    on_attack(), on_enter_zone()              │
│                                               │
│  Linear Memory (玩家A私有内存)                │
│    [数据/变量/堆栈]                           │
│                                               │
│  Import (系统API，不可覆写):                  │
│    tick(), get_pos(), set_pos(),             │
│    probe(), attack(), compute(), call(),     │
│    spawn(), destroy(), render(), ...         │
│                                               │
└───────────────────────────────────────────────┘
                    ↕
            code-arena (沙箱通道)
                    ↕
┌─────────────── 玩家B的WASM沙箱 ───────────────┐
│  同上结构                                      │
└───────────────────────────────────────────────┘
```

关键设计：
- **Export函数**：玩家代码导出的函数，attack只能覆写这些
- **Import函数**：框架提供的系统API，绝对不可被attack覆写
- **Linear Memory**：玩家私有内存，跨沙箱不可直接访问
- **通道**：所有跨沙箱交互必须经过code-arena的通道API（probe/attack/call）

### 6.2 attack 覆写机制

```
attack(target, payload) 的技术实现:

1. payload是一段WASM字节码（或新函数的字节码）
2. 框架在target的沙箱中:
   a. 解析payload，验证它只包含export函数的覆写
   b. 如果payload试图覆写import → 拒绝，attack失败
   c. 如果payload合法 → 替换target的export函数表
3. 替换后:
   a. 如果target导出了on_attack → 先执行on_attack
      - on_attack可以拒绝覆写（intercepted=true）
      - on_attack可以恢复覆写（把life改回去）
      - on_attack超时 → 默认保留原life（不覆写）
   b. 如果target没导出on_attack → 直接覆写
4. 调用target.life()检查是否已被覆写为false
5. 返回结果给攻击方
```

### 6.3 沙箱安全防线

```
第一层：WASM类型安全
  - wasmtime强制类型检查
  - 玩家代码无法越界访问内存

第二层：指令计量(InstructionGuard)
  - 每个操作有指令配额
  - 超额立即终止

第三层：import/export隔离
  - attack只能改export，不能改import
  - 系统API绝对安全

第四层（可选）：进程级隔离
  - 关键操作（代码热替换）由Rust直接执行
  - 不通过WASM
  - wasmtime进程本身受seccomp限制

第五层（可选）：容器隔离
  - 整个游戏服务器跑在Docker中
  - 限制网络/文件系统访问
```

---

## 七、持久化与恢复

### 7.1 持久化策略

```
┌────────────────────────────────────────────┐
│              持久化时间线                   │
├────────────────────────────────────────────┤
│                                            │
│  tick 0    tick 100   tick 200   tick 300  │
│    │          │          │          │      │
│    ▼          ▼          ▼          ▼      │
│  全量快照   全量快照   全量快照   全量快照   │
│    │          │          │          │      │
│    ├──WAL──WAL──WAL──┤  ├──WAL──WAL──┤    │
│    │  增量日志         │  │  增量日志     │
│    └──────────────────┘  └──────────────┘ │
│                                            │
│  保留策略:                                  │
│  - 7天内: 每100tick全量 + 每tick增量        │
│  - 7天后: 只保留每1000tick的全量快照         │
│  - 30天后: 只保留每10000tick的全量快照       │
└────────────────────────────────────────────┘
```

### 7.2 存储内容

```
全量快照包含:
  - WorldState.tick
  - 所有存活实体的元数据 (id, owner, pos, survival_points, alive)
  - 所有实体的WASM内存dump (压缩存储)
  - 所有实体的code_hash (指向Git仓库)
  - 所有区域/服务/变换的注册表
  - 全局配置

增量日志(WAL)包含:
  - 本tick内实体的创建/删除
  - 实体位置变化
  - 实体内存页变化 (diff)
  - 区域/服务/变换的注册/移除

代码存储:
  - Git bare repo: /data/codeworld/code/<player_id>.git
  - 每次代码提交 = 一个commit
  - 实体的code_hash = commit hash
  - 加载实体时: 从Git拉取commit → 编译WASM → LRU缓存
```

### 7.3 崩溃恢复

```
恢复流程:
1. 加载最近的完整快照
2. 重放WAL日志到目标tick
3. 从Git仓库拉取所有实体的代码
4. 编译WASM模块（LRU缓存优化）
5. 创建wasmtime实例
6. 恢复WASM线性内存
7. 恢复完成，继续tick循环

恢复时间目标:
  - MVP: < 60秒（单机，<100实体）
  - v2: < 30秒（优化，<1000实体）
  - v3: < 10秒（分布式，分片恢复）
```

---

## 八、物理规则与角色登录门槛

### 8.1 物理规则设计：厚版本

框架自带基础物理规则，让新手玩家不写任何物理代码就能让角色站在地上、走来走去。

**默认规则（框架内置，不可删除，可覆盖）：**
- 地面约束：实体不能掉到y<0以下（角色站得住）
- 基础碰撞：两个实体不能占同一坐标（不会叠在一起）
- 重力：每tick y坐标减少固定值（东西会掉到地上）
- 移动速度上限：防止瞬移

**覆盖机制：**
玩家通过register_physics_override注册自定义规则，覆盖默认规则。
- 自身覆盖：只影响自己（"我免疫重力"）
- 区域覆盖：影响区域内所有实体（"我的领地重力横着拉"）
- 全局覆盖：影响整个世界（暴露面大，可被probe到并attack）

**改规则=可被攻击：**
覆盖规则跑在WASM沙箱里，跟角色代码一样可被probe和attack。
改得越多越激进，暴露给攻击的面越大。这构成了风险-收益平衡：
- 新手：用默认物理，安全但能力有限
- 高手：深度修改规则，获得能力优势，但暴露给攻击

**规则碰撞处理：**
当两条规则对同一实体产生冲突状态变更时，框架不裁决谁对：
- 规则碰撞视为攻击，通过现有沙箱通道（probe/attack）互相攻击
- 胜者的变更生效，败者的变更丢弃
- 如果两条都活下来（都没被对方attack掉），后执行的覆盖先执行的
- 都不崩系统：规则代码在WASM沙箱里，崩溃/超时只影响该规则本身

### 8.2 角色登录门槛

玩家生成角色时，代码必须包含最低限度的结构才能登录世界。
框架提供默认模板，新手直接用模板就能玩。

**物理存在层（模板提供，必须有）：**
- 外观数据 -- 渲染模块需要知道画什么（ASCII字符/形状/颜色）
- 移动能力 -- 调用框架物理API实现走路（基础位移）
- 跳跃能力 -- 调用框架物理API实现跳跃（对抗重力的瞬时速度）
- 动作状态 -- 弯腰/蹲下/挥手等状态标记，影响外观数据

**交互存在层（模板提供默认，可覆盖）：**
- life() -> bool -- 默认返回true。生存点数归零时框架强制覆写为false（不可改）
- on_tick() -- 默认空转。玩家写自己的逻辑（移动/AI/资源采集）
- on_probe(request) -- 默认返回基本信息（外观/位置）。可覆盖为返回假数据/加密/什么都不返回
- on_attack(payload) -- 默认不防御，接受攻击。可覆盖为拦截/偏转/反制

**框架硬管的（不在WASM里，玩家改不了）：**
- 生存点数扣减（每天-1，归零强制死亡）
- 沙箱隔离（WASM边界）
- tick调度（什么时候跑你的代码）
- 物理规则执行（默认重力/碰撞/地面约束）

**三层配合效果：**
- 新手：用默认模板+默认物理，能走能跳能被看到，安全
- 进阶：改on_attack加防御代码，attack他需要先破解防御
- 高手：改on_probe返回假数据，改物理规则获得能力优势，但全部暴露给attack

**为什么life/on_probe/on_attack必须放在WASM里（玩家代码）而不是框架里：**
attack的核心是覆写对方代码。如果life()是框架代码，攻击者没法覆写它，
"攻击=破解"这个核心玩法就不存在了。on_probe也一样--如果框架控制探测响应，
玩家就没法造假数据欺骗，信息战玩法也没了。
框架提供默认实现放在模板里，玩家不改就用默认的，改了就成为自己的代码（可被attack）。

### 8.3 小白可玩性设计

#### 8.3.1 AI辅助破解流程

```
小白玩家的典型流程:

1. probe对手
   → 拿到 { response: 乱码, meta: { gas: 12345, mem_reads: 678, ... } }
   → 自己看不懂

2. 把数据喂给AI
   → AI分析: "对手的on_probe响应看似随机，但gas消耗稳定
     在12000-13000之间，说明on_probe内部有固定循环结构
     mem_reads远高于平均值，说明在读取大量预存数据
     可能在做查表式防御，建议攻击时机：on_probe执行后
     内存还未重新加密的窗口期"

3. AI生成攻击代码
   → 小白复制代码到自己的角色

4. 执行attack
   → 成功 → 开心
   → 失败 → AI根据返回的AttackResult调整
   → 重复

5. 失败太多？
   → 转行种花/开商店/当情报贩子
   → 框架不要求每个人都是战士
```

#### 8.3.2 非战斗玩法路径

```
CodeWorld不强制战斗。玩家可以选择:

情报贩子:
  - 专注probe，收集其他玩家的代码特征
  - 把情报包装成service出售
  - "想知道A的防御弱点？call我的情报service"

种花/制药:
  - 写随时间成长的代码
  - 成熟后注册service提供加成
  - 开商店卖效果

建筑师:
  - 注册区域，定义规则
  - 开竞技场/市场/银行
  - 收费（自己定义货币/交易规则）

破解者:
  - 专注分析别人代码
  - 出售"破解工具"（一段帮助分析的代码）
  - 或直接受雇攻击

驯兽师:
  - spawn多个子实体
  - 写AI控制它们
  - 组团探险/战斗
```

---

## 九、分阶段实现计划

### Phase 1: MVP - 核心循环验证 (4-6周)

目标：验证"代码即生命，攻击=破解"的核心循环

### 基础设施层完成状态（2026-07-23）

8 个 crate 全部编译通过，271 测试全绿，主程序可运行：

| crate | 测试数 | 状态 | 说明 |
|-------|--------|------|------|
| core | 97 | ✅ | WorldState/Entity/Physics数据结构/SurvivalPoints/RenderRequest/PendingChange |
| time | 16 | ✅ | TimeEngine tick计数器 + SnapshotManager |
| space | 30 | ✅ | SpaceEngine坐标 + TransformPipeline变换 + ZoneManager区域 |
| physics | 28 | ✅ | DefaultPhysicsEngine重力/碰撞/地面约束 + OverrideManager + ConflictResolver |
| arena | 23 | ✅ | SandboxManager(wasmtime) + ChannelManager(probe/attack/call) + ComputeExecutor |
| render | 36 | ✅ | RenderGraph三阶段(借鉴寒霜FrameGraph) + TerminalBuffer + Viewport + AsciiRenderModule |
| storage | 26 | ✅ | SqliteStorage(3表) + SnapshotManager(serde_json序列化) + RetentionPolicy |
| bin | 35 | ✅ | 主程序tick循环 + WASM执行接入 + PendingChanges + 生存点数扣减 + 接触触发通道 |
| 合计 | 291 | | 全绿 |

### MVP 核心缺口（全部完成，2026-07-23/24）

| 缺口 | 状态 | 实现 |
|------|------|------|
| WASM 玩家代码执行 | ✅ 完成 | EntitySandboxMap + tick 阶段一调 on_tick/life，每 tick 复位 fuel |
| PendingChanges 队列 | ✅ 完成 | core/pending.rs（Spawn/Destroy/SetPos/SetVelocity），tick 阶段二 a 步 apply |
| 生存点数扣减 | ✅ 完成 | tick 阶段二 b 步 decay_survival，归零 kill |
| 接触触发沙箱通道 | ✅ 完成 | tick 阶段二 c 步先 detect_collisions 再 step 再 handle_collisions |
| 终端 ANSI 渲染输出 | ❌ 推迟 | 渲染图算出 diff 但不写终端，Phase 2 |
| 击杀掠夺 | ❌ 推迟 | Phase 2 |
| 多线程并行执行 | ❌ 推迟 | MVP 单线程，Phase 2 |

### MVP 功能范围

```
  ✅ world-core: tick时钟 + 2D坐标 + ASCII渲染（基础设施）
  ✅ code-arena: WASM沙箱管理 + probe/attack/call通道（沙箱能力已有，未接入tick循环）
  ✅ 被动观测: gas/内存/API调用元数据
  ✅ physics: 默认物理规则（重力/碰撞/地面约束）+ 覆盖接口
  ✅ 角色登录门槛: 默认模板（examples/ 三个 .as 文件）
  ✅ 生存点数: 数据结构已有（3天初始 + 每天消耗 + 归零死亡 + 掠夺），未接入tick循环
  ✅ compute: LocalExecutor 空壳
  ✅ 死亡: 静默删除 + 联系断裂（死亡清理已有，掠夺未实现）
  ✅ 持久化: SQLite全管（元数据+内存快照BLOB+快照索引）
  
  ❌ WASM玩家代码执行接入tick循环（P0，Phase 2 优先）
  ❌ PendingChanges延迟队列（P0）
  ❌ 终端ANSI渲染输出（P1）
  ❌ 生存点数扣减接入tick循环（P1）
  ❌ 接触触发沙箱通道（P1）
  ❌ 时间篡改（Phase 2）
  ❌ 空间扭曲（Phase 2）
  ❌ 玩家自建区域（Phase 2）
  ❌ 网络多人（Phase 3）
  ❌ 图形渲染（Phase 3）

MVP验证场景:
  1. 两个玩家各写一段AS代码，通过probe互相探测
  2. 根据探测结果生成attack payload
  3. 一方成功覆写另一方life() = 击杀
  4. 被杀方静默删除
  5. 杀人方独享战果

  6. 玩家C种花，10小时后成熟
  7. 成熟后注册heal service
  8. 玩家D调用service获得加成
  9. C被杀 → D的call返回Void

  10. 玩家A接本地算力，compute任务快5倍
  11. 玩家B不接算力，但代码写得好也能赢A

技术产出:
  - codeworld-core (Rust crate): 世界内核
  - codeworld-arena (Rust crate): 沙箱+通道
  - codeworld-stdio (Rust binary): 终端运行器
  - compute-daemon (Rust binary): 本地算力守护进程
  - 示例玩家代码 (AssemblyScript): 3-5个示例
  - API文档 + 玩家代码编写指南
```

### Phase 2: 扩展原语 (4-6周)

```
功能:
  ✅ 时间篡改: 只读历史探查 → 状态回溯
  ✅ 空间扭曲: register_transform + 变换管线
  ✅ 玩家自建区域: register_zone + 区域规则
  ✅ 服务系统: register_service + call
  ✅ 经济基础: 玩家自定义货币/交易
  ✅ WASM模块热替换: 代码热更新
  ✅ RocksDB内存快照: 从SQLite切换到RocksDB优化高频写入存储
```

### Phase 3: 多人网络 (6-8周)

```
功能:
  ✅ WebSocket服务器: 持久多人世界
  ✅ Web Canvas渲染: 浏览器2D图形
  ✅ 在线IDE: 浏览器编写/编译AS代码
  ✅ 算力节点远程接入
  ✅ 视锥剔除: 只渲染附近实体
  ✅ 分区tick调度: 不同区域不同tick频率
  ✅ 账户系统: 注册/认证/配额
```

### Phase 4: 生态 (持续)

```
功能:
  ✅ Bevy 3D渲染
  ✅ 玩家代码市场 (npm/git包管理)
  ✅ 观战系统 (分享session)
  ✅ 排行榜/赛事
  ✅ 移动端客户端
  ✅ AI助手集成 (帮小白分析/生成代码)
```

---

## 十、项目结构

```
codeworld/
├── Cargo.toml                 # workspace根
├── crates/
│   ├── core/                  # world-core
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── world.rs       # WorldState
│   │   │   ├── entity.rs      # Entity, EntityId
│   │   │   ├── fuel.rs        # InstructionGuard + SurvivalPoints
│   │   │   └── config.rs      # Constraints
│   │   └── Cargo.toml
│   ├── arena/                 # code-arena
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── sandbox.rs     # WASM沙箱管理
│   │   │   ├── channel.rs     # probe/attack/call通道
│   │   │   ├── observe.rs     # 被动观测
│   │   │   └── compute.rs     # 算力调度
│   │   └── Cargo.toml
│   ├── time/                  # time-engine
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── clock.rs       # tick时钟
│   │   │   └── snapshot.rs    # 快照管理
│   │   └── Cargo.toml
│   ├── space/                 # space-engine
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── coord.rs       # 坐标系
│   │   │   ├── transform.rs   # 变换管线
│   │   │   └── zone.rs        # 区域管理
│   │   └── Cargo.toml
│   ├── physics/               # 物理规则模块
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── default.rs     # 默认物理规则（重力/碰撞/地面约束）
│   │   │   ├── override.rs    # 规则覆盖接口
│   │   │   └── conflict.rs    # 冲突执行（规则碰撞=攻击）
│   │   └── Cargo.toml
│   ├── render/                # renderer（借鉴寒霜 FrameGraph 三阶段管线）
│   │   ├── src/
│   │   │   ├── lib.rs         # Renderer 编排器 + 三阶段入口
│   │   │   ├── graph.rs       # RenderGraph + RenderNode（Setup/Compile/Execute）
│   │   │   ├── buffer.rs      # TerminalBuffer + Cell（双缓冲 + diff flush）
│   │   │   ├── viewport.rs    # Viewport（世界坐标<->终端行列映射 + 视口裁剪）
│   │   │   └── module.rs      # RenderModule trait + AsciiRenderModule
│   │   └── Cargo.toml
│   └── storage/               # 持久化
│       ├── src/
│       │   ├── lib.rs
│       │   ├── sqlite.rs      # 元数据+内存快照BLOB(MVP全用SQLite)
│       │   ├── rocksdb.rs     # 内存快照(Phase2从SQLite切换)
│       │   └── git.rs         # 代码仓库
│       └── Cargo.toml
├── bin/
│   ├── codeworld/             # 主程序(MVP终端运行器)
│   │   ├── src/main.rs
│   │   └── Cargo.toml
│   └── compute-daemon/        # 算力守护进程
│       ├── src/main.rs
│       └── Cargo.toml
├── examples/                  # 示例玩家代码
│   ├── attacker.as            # 攻击型(AS)
│   ├── defender.as            # 防御型(AS)
│   ├── flower.as              # 种花型(AS)
│   └── README.md
├── docs/
│   ├── api-reference.md       # 完整API文档
│   ├── player-guide.md        # 玩家代码编写指南
│   └── architecture.md        # 架构文档(本文档)
└── tests/
    ├── integration/           # 集成测试
    └── combat/                # 攻防测试场景
```

---

## 十一、开放问题（待讨论）

### 11.1 时间回溯的限制机制

```
已确认：允许波及他人（神仙打架凡人遭殃）
待定：具体限制

候选方案:
  A. 算力代价: 回溯消耗大量compute（外接算力才能支撑）
  B. 代码代价: 回溯需要消耗自身代码的一部分（永久损失）
  C. 冷却: 回溯后N tick内不能再次回溯
  D. 范围递减: 第1次可回溯1000 tick，第2次500，递减

注意: 这些限制由玩家代码实现还是框架强制？
  原则: 框架只提供 get_history_snapshot() 接口
        代价由框架在接口层强制（如消耗生存点数或算力）
        具体限制参数可配置
```

### 11.2 attack覆写机制 -- spike验证结果（2026-07-21）

```
三条路线已完成技术spike验证，16个测试全部通过。

验证环境：Rust 1.97.1 + wasmtime v25 + wat
代码位置：spike/route_a, spike/route_b, spike/route_c

┌──────────┬──────────┬──────────┬──────────┬──────────┐
│ 路线     │ 可行性   │ 内存保留 │ 性能     │ 推荐度   │
├──────────┼──────────┼──────────┼──────────┼──────────┤
│ A 整模块  │ 可行     │ 丢失     │ 0.188ms  │ MVP首选  │
│ B funcref │ 可行*    │ 保留     │ 0.718us  │ v2升级   │
│ C HostHook│ 可行     │ 保留     │ ~0ns开销 │ MVP首选  │
└──────────┴──────────┴──────────┴──────────┴──────────┘

* 路线B跨沙箱注入被wasmtime拒绝（type mismatch），
  需要攻击者与受害者在同一Store内，或框架中转Func引用。

路线A：整模块替换
  原理: 销毁目标旧WASM实例，用攻击者payload编译新模块替换
  优点: wasmtime原生支持，实现最简单，import绑定可复用
  缺点: 旧内存状态全部丢失（角色"失忆"）
  性能: 销毁+重建 0.188ms（可接受）
  适用: attack=完全接管对方角色（彻底覆写）

路线B：funcref表间接调用
  原理: export函数通过funcref table间接调用，attack时改表项
  优点: 精准替换单个函数，不丢失内存状态，性能极好(0.718us)
  缺点: 需要定制编译工具链（玩家WASM必须包含table+call_indirect）
        AssemblyScript/Rust默认不这样编译
        跨Store(沙箱)注入被wasmtime拒绝，需框架在同一Store内中转
  适用: v2升级方案（需要定制编译流程后才有意义）

路线C：Host Hook拦截
  原理: 框架在host调用层加中间层，attack时改覆写标记
  优点: 不改WASM模块，host层完全可控，按实例隔离，内存保留
        性能开销约0ns（一个bool判断，可忽略）
  缺点: 只能拦截返回值，不能改函数内部逻辑
  适用: attack=影响函数返回值（life()从true变false足够）

MVP决策：只用路线A（一个方案搞定所有场景）
  - 路线A是唯一的attack机制：销毁旧实例 + 用payload重建
  - 玩家attack：payload来自攻击者（击杀/击晕/绑架都由payload内容决定）
    击杀=payload里life()=false
    击晕=payload里life()=true但on_tick被改成不能行动
    绑架=payload里on_tick改成往攻击者坐标移动
    这些都是玩家代码层面的事，框架不管
  - 框架强制死亡：payload来自框架（life()=false的最小模块）
  - 路线B留作Phase 2：需定制编译工具链让attackable export走funcref表
    跨沙箱注入被wasmtime拒绝，需框架broker模型(攻击者代码编进受害者Store)
  - 路线C不采用：不需要额外的host hook机制，路线A已覆盖所有场景
```

### 11.3 算力验证的Bootstrap问题

```
新世界只有一个玩家时，没有多节点比对。
候选方案:
  A. Bootstrap阶段强制只用PoW类型任务（可直接验证）
  B. 新算力供应商有"低信誉期"，只能跑小任务
  C. 框架提供基础算力作为"验证基准"
```

### 11.4 WASM模块热替换的状态迁移

```
attack覆写life()后，旧模块的状态如何迁移到新模块？
候选方案:
  A. 不迁移: 覆写后内存清零（简单但损失状态）
  B. 自动迁移: 新模块必须导出migrate(old_state)函数
  C. 玩家自管: 框架不管状态迁移，玩家代码自己处理

MVP建议: A（不迁移），v2再考虑B
```

### 11.5 跨沙箱死锁

```
A probe B, B同时probe A → 互相等待
候选方案:
  A. 调用深度限制(max_depth=3) → 超过返回Void
  B. 异步模型 → probe返回Future，不阻塞
  C. 超时强制断开 → 50ms超时返回Void

MVP建议: A+C（深度限制+超时）
```

---

## 附录A: 一次完整的攻防演示

```
=== 初始状态 ===
Tick 0: 世界创建
  Entity A (owner: player1, pos: (0,0))
  Entity B (owner: player2, pos: (100,0))

=== Tick 1-10: A通过视口发现B并探测 ===
A.on_tick:
  visible = get_visible_entities()
  -> 发现B在(100,0)，外观为战士
  -> A获得B的EntityId和坐标
  result = probe(B, "scan")
  -> B.on_probe("scan") 返回 "defense: shield"
  -> 被动观测: gas=8000, mem_reads=234, mem_writes=12
  -> A的代码记录: B声称有护盾，但gas消耗低，可能是假的

=== Tick 11-20: A分析 ===
A.on_tick:
  result = probe(B, "deep_scan")
  -> B.on_probe("deep_scan") 返回 "defense: shield, armor: 50"
  -> 被动观测: gas=8500, mem_reads=245, mem_writes=15
  -> A分析: 两次probe结果一致，但gas消耗几乎不变
  -> A判断: B的on_probe可能是查表返回固定值，不是实时计算
  -> A生成payload: 完整的WASM模块(life()=false)

=== Tick 21: A发起攻击 ===
A.on_tick:
  attack_at((100, 0), payload)
  -> 框架检查坐标(100,0)：有实体B -> 命中(hit=true)
  -> B.on_attack(payload) 执行:
    - B检测到攻击
    - B的防御代码尝试拦截
    - 但payload绕过了查表防御
    - B.on_attack拦截失败 (intercepted=false)
  -> payload替换B的整个WASM模块
  -> 新模块life()返回false
  -> B死亡

=== Tick 22: B死亡后 ===
  B从世界状态中移除
  A的attack_at返回 success=true
  无广播
  A独享战果
  其他玩家get_visible_entities()不再看到B
```

---

## 附录B: 玩家代码示例 (AssemblyScript)

```typescript
// attacker.as - 一个简单的攻击型角色

import { tick, get_pos, get_visible_entities, probe, attack_at, render } from "codeworld"

let target: EntityId | null = null
let target_pos: (f64, f64) | null = null
let probe_data: ProbeResult[] = []

export function on_tick(): void {
    // 查找视口内可见的敌人
    let visible = get_visible_entities();
    if visible.length > 0 {
        target = visible[0].id;
        target_pos = visible[0].pos;
    }
    
    if (target && target_pos) {
        // 阶段1: 探测
        let result = probe(target, "scan")
        probe_data.push(result)
        
        // 阶段2: 分析（简化版，真实场景用AI辅助）
        if (probe_data.length >= 5) {
            let pattern = analyze_pattern(probe_data)
            if (pattern.weakness_found) {
                // 阶段3: 攻击坐标（不是攻击EntityId）
                let payload = generate_payload(pattern)
                let result = attack_at(target_pos, payload)
                if (result.success) {
                    target = null  // 目标已死，找下一个
                    target_pos = null
                }
            }
        }
    }
    
    // 渲染（默认提交到Entity层，按Y轴排序绘制）
    render({
        layer: RenderLayer.Entity,
        y_sort_value: get_pos(self)[1],  // Y坐标用于深度排序
        appearance: { glyph: "A", color: "red", size: 10 },
        pos: get_pos(self),
        action_state: ActionState.Idle,
        priority: 0,
    })
}

export function life(): bool {
    return true  // 我还活着
}

export function on_attack(payload: Bytes): AttackResult {
    // 简单防御: 随机概率拦截
    if (Math.random() < 0.3) {
        return { intercepted: true, damage: 0 }
    }
    return { intercepted: false, damage: 0 }
}

function analyze_pattern(data: ProbeResult[]): Pattern {
    // 分析探测数据的模式
    // 小白: 把data序列化后喂给AI
    // 程序员: 自己写分析逻辑
    return { weakness_found: data.length >= 5 }
}

function generate_payload(pattern: Pattern): Bytes {
    // 生成攻击payload
    // 覆写目标life()为always-false
    return life_override(false)
}
```

---

*文档结束。v2更新：新增物理规则模块、模块化设计原则、角色登录门槛。等待讨论和迭代。*
