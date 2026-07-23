# 主程序设计：借鉴游戏服务器框架架构

> 状态：设计提案（2026-07-23）
> 来源：Screeps + AzerothCore + Skynet + NoahGameFrame + Nano + Bevy Headless
> 目标：设计 bin/codeworld 主程序，把 7 个 crate 串成可运行的 tick 循环

## 一、参考项目架构总结

### 1.1 Screeps（编程MMO，最接近 CodeWorld 的设计）

Screeps 是"代码即现实"游戏的鼻祖，架构文档 4.1 节已参考它。

**服务器架构**：
- Node.js + MongoDB + Redis
- 40 台四核服务器（160 核）并行计算玩家脚本
- 24 核 MongoDB 机器，每秒 3 万次更新

**Tick 两阶段模型**（与 CodeWorld 4.1 节一致）：
1. **玩家脚本计算**：所有活跃玩家放 Redis 队列，各机器并行取任务执行 main 模块
2. **命令处理**：玩家产生的命令放队列，各机器并行处理每个房间的命令
- 一个 tick 必须等所有玩家 main 执行完 + 所有命令处理完才开始下一个
- 玩家代码操作的是 tick 开始时的快照状态，变更在 tick 末统一应用
- 超时不优雅退出则杀整个 fork 进程

**关键设计**：
- 每个玩家一个独立 fork 进程（沙箱），vm.runInContext 执行
- 编译缓存（V8 code cache）加速后续执行
- 玩家间隔离：一个玩家崩溃不影响其他
- DB 批量写入：两阶段结束后统一 bulk write

### 1.2 AzerothCore（MMORPG 服务器）

**主循环**（WorldRunnable::run）：
```
while (!World::IsStopped()) {
    diff = getMSTimeDiff(prev, now);  // 计算距上次的时间差
    sWorld->Update(diff);              // 传 diff 给世界更新
    // 统计执行时间
}
```

**World::Update(diff)**：
- 更新定时器（拍卖/天气/尸体清理/事件等，各有不同刷新频率）
- 调用各 Manager 的 Update(diff)：BattlegroundMgr/OutdoorPvPMgr/BattlefieldMgr...
- 调用脚本钩子 OnWorldUpdate(diff)（模块系统，第三方扩展）

**关键设计**：
- diff 驱动：不是固定 tick，而是按实际时间差驱动（变长时间步）
- 管理器链：World::Update 依次调用各 Manager::Update，Manager 再调用子系统
- 模块钩子：OnWorldUpdate 是扩展点，第三方模块挂载自定义逻辑

### 1.3 Skynet（Actor 模型，C+Lua）

**架构**：
- 每个 Actor 是一个独立的 Lua State（隔离内存）
- Actor 间通过消息传递通信（不共享内存）
- 每个 Actor 有自己的消息队列，框架调度器轮询分发
- 热更新：替换 Actor 的代码不用停服务

**与 CodeWorld 的对应**：
- Skynet Actor = CodeWorld WASM 沙箱
- Skynet 消息传递 = CodeWorld probe/attack/call 通道
- Skynet 热更新 = CodeWorld attack 覆写（整模块替换）
- Skynet Actor 隔离 = CodeWorld 沙箱隔离

### 1.4 NoahGameFrame（插件框架，C++）

**架构**：
- 受 OGRE 和 Bigworld 启发
- 插件框架：所有功能是插件，核心只管插件加载/卸载/调度
- IPlugin 接口：Init/Update/Shutdown
- 插件间通过事件总线通信

**与 CodeWorld 的对应**：
- NF 插件 = CodeWorld 的 crate（core/time/space/physics/arena/render/storage）
- NF IPlugin::Update = CodeWorld 各引擎的 step/tick
- NF 事件总线 = CodeWorld 的 PendingChanges 队列

### 1.5 Nano（Go，Entity-Component）

**架构**：
- 应用 = Component 集合
- Component = Handler 束
- 注册 Component 后自动注册所有 Handler
- 分布式：agent 分发消息到 backend 服务器

### 1.6 Bevy Headless（Rust ECS）

**架构**：
- App = Plugin 集合
- 不注册 RenderPlugin = headless 服务器
- MinimalPlugins = 只留核心调度+事件循环
- Schedule 驱动：系统按依赖关系自动调度

**与 CodeWorld 的对应**：
- Bevy Plugin = CodeWorld crate
- Bevy Schedule = CodeWorld tick 循环
- Bevy 不注册渲染插件 = CodeWorld MVP 先跑无渲染的 tick 循环

## 二、CodeWorld 主程序应该怎么设计

### 2.1 借鉴什么

| 借鉴项 | 来源 | 为什么适合 CodeWorld |
|--------|------|---------------------|
| Tick 两阶段模型 | Screeps | 架构文档 4.1 已采用：阶段一玩家代码并行，阶段二状态串行 |
| diff 时间驱动 | AzerothCore | MVP 用固定 tick 间隔，但保留 diff 概念为变长时间步预留 |
| 管理器链 Update | AzerothCore | World::Update 依次调用各 Manager::Update，CodeWorld 主循环依次调用各引擎 |
| 插件式组合 | NoahGameFrame / Bevy | 7 个 crate 像插件一样组合，主程序只编排不实现逻辑 |
| Actor 隔离 | Skynet | 每个实体一个沙箱，崩溃不影响其他（arena 已实现） |
| 编译缓存 | Screeps V8 code cache | CodeWorld 的 wasmtime Module 已全局缓存（arena 实现） |
| 批量写入 | Screeps bulk write | tick 末统一写 DB，不在执行中写（storage 已支持） |
| 优雅关闭 | AzerothCore World::IsStopped | Ctrl+C 信号处理，当前 tick 完成后退出，退出前存快照 |

### 2.2 不借鉴什么

| 不借鉴项 | 原因 |
|---------|------|
| 分布式多机 | MVP 单机，Phase 3 才多机 |
| Redis 队列 | MVP 单机不需要消息队列，直接用 Rust 线程池 |
| MongoDB | MVP 用 SQLite |
| 变长时间步 | MVP 固定 1 tick/秒，变长留 Phase 2 |
| 网络层 | MVP 终端运行，无网络 |
| Lua 脚本 | CodeWorld 用 WASM，不用 Lua |

### 2.3 主程序架构

```
┌──────────────────────── bin/codeworld ────────────────────────┐
│                                                                │
│  main()                                                        │
│    ├─ 初始化日志（tracing-subscriber）                          │
│    ├─ 初始化存储（StorageEngine::open）                         │
│    ├─ 尝试加载快照（load_latest_world）或创建空世界              │
│    ├─ 初始化各引擎：                                             │
│    │   TimeEngine / SpaceEngine / DefaultPhysicsEngine /        │
│    │   SandboxManager / Renderer / StorageEngine                │
│    ├─ 注册信号处理（Ctrl+C -> graceful shutdown）               │
│    │                                                            │
│    ├─ ┌─── tick 循环（while !stopped）───┐                     │
│    │   │                                    │                     │
│    │   │  ① time_engine.advance_tick()      │  时间推进            │
│    │   │                                    │                     │
│    │   │  ② 阶段一：实体执行（MVP 跳过）     │  玩家代码            │
│    │   │     MVP 无 WASM 玩家代码执行        │  （Phase 2 接入）     │
│    │   │                                    │                     │
│    │   │  ③ 阶段二：状态更新（串行）          │                     │
│    │   │     a. physics.step(world)          │  物理规则            │
│    │   │     b. space 处理（MVP 跳过）        │  坐标变换            │
│    │   │     c. 死亡清理                      │  移除死亡实体         │
│    │   │     d. renderer.render(world)       │  渲染输出            │
│    │   │     e. storage.save_snapshot(world) │  每100tick存快照     │
│    │   │                                    │                     │
│    │   │  ④ sleep 到下一个 tick（1秒间隔）    │  帧率控制            │
│    │   └────────────────────────────────────┘                     │
│    │                                                            │
│    └─ 关闭：force_save 快照 -> 关闭 storage -> 退出              │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

### 2.4 文件结构

```
bin/codeworld/
├── src/
│   ├── main.rs      # main() 入口 + tick 循环 + 信号处理
│   └── config.rs    # 运行配置（tick 间隔/视口大小/数据库路径/日志级别）
└── Cargo.toml
```

### 2.5 main.rs 设计

```rust
fn main() -> anyhow::Result<()> {
    // ① 初始化
    init_logging();
    let config = Config::load();
    
    // ② 初始化存储，尝试恢复或创建世界
    let mut storage = StorageEngine::open(&config.db_path)?;
    let mut world = storage.load_latest_world()?
        .unwrap_or_else(WorldState::new);
    
    // ③ 初始化各引擎
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
    
    // ④ 信号处理
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc_handler(move || r.store(false, Ordering::SeqCst));
    
    // ⑤ tick 循环
    tracing::info!("CodeWorld 启动，tick={}", world.tick());
    while running.load(Ordering::SeqCst) {
        tick(&mut world, &mut time_engine, &physics, &mut renderer, &mut storage);
        sleep(config.tick_interval);
    }
    
    // ⑥ 优雅关闭
    tracing::info!("正在关闭...");
    storage.force_save(&world)?;
    tracing::info!("快照已保存，退出");
    Ok(())
}

fn tick(
    world: &mut WorldState,
    time_engine: &mut TimeEngine,
    physics: &DefaultPhysicsEngine,
    renderer: &mut Renderer,
    storage: &mut StorageEngine,
) {
    // ① 时间推进
    let should_snapshot = time_engine.advance_tick();
    world.advance_tick();
    
    // ② 阶段一：玩家代码执行（MVP 跳过，Phase 2 接入 WASM）
    
    // ③ 阶段二：状态更新（串行）
    //   a. 物理规则
    physics.step(world);
    
    //   b. 坐标变换（MVP 跳过）
    
    //   c. 死亡清理
    world.entities.retain(|_, e| e.is_alive());
    
    //   d. 渲染
    let diffs = renderer.render(&world);
    if !diffs.is_empty() {
        renderer.flush();  // swap 缓冲
        // MVP: 只打印 diff 数量，不实际写终端
        tracing::debug!("渲染 {} 个变化格子", diffs.len());
    }
    
    //   e. 快照存储
    if should_snapshot {
        storage.save_world_snapshot(world).ok();
    }
}
```

### 2.6 config.rs 设计

```rust
pub struct Config {
    /// tick 间隔（毫秒），MVP 默认 1000（1秒/tick）
    pub tick_interval_ms: u64,
    /// 数据库路径，默认 "codeworld.db"
    pub db_path: String,
    /// 视口中心
    pub viewport_center: (f64, f64),
    /// 视口半宽
    pub viewport_half_width: f64,
    /// 视口半高
    pub viewport_half_height: f64,
    /// 终端行数
    pub term_rows: usize,
    /// 终端列数
    pub term_cols: usize,
    /// 日志级别
    pub log_level: String,
}

impl Config {
    /// 默认配置
    pub fn default() -> Self { ... }
    /// 从环境变量加载
    pub fn load() -> Self { ... }
}
```

### 2.7 Cargo.toml 依赖

```toml
[dependencies]
codeworld-core = { path = "../../crates/core" }
codeworld-time = { path = "../../crates/time" }
codeworld-space = { path = "../../crates/space" }
codeworld-physics = { path = "../../crates/physics" }
codeworld-arena = { path = "../../crates/arena" }
codeworld-render = { path = "../../crates/render" }
codeworld-storage = { path = "../../crates/storage" }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

## 三、MVP 范围说明

主程序 MVP 实现：
- ✅ 初始化所有引擎
- ✅ tick 循环（时间推进 -> 物理 -> 死亡清理 -> 渲染 -> 快照存储）
- ✅ 快照恢复（从 SQLite 加载）
- ✅ 优雅关闭（Ctrl+C 存快照退出）
- ✅ 日志输出

MVP 不实现：
- ❌ 玩家代码执行（WASM 沙箱执行，Phase 2 接入）
- ❌ 多线程并行（Phase 2 用 tokio 线程池）
- ❌ 终端 ANSI 渲染输出（MVP 只日志输出 diff 数量）
- ❌ 网络层（Phase 3 WebSocket）
- ❌ 命令行参数解析（用环境变量或默认值）

## 四、设计哲学对照

| CodeWorld 铁律 | 本设计如何遵守 |
|---------------|--------------|
| 框架只提供能力不规定玩法 | 主程序只编排各引擎，不实现游戏逻辑 |
| 传统引擎模块胖，CodeWorld模块瘦 | 主程序 ~100 行，逻辑全在各 crate |
| 不提前抽象 | 不引入网络/多线程/分布式，MVP 单线程跑通 |
| 存在性原语只提供能力 | 主程序是"能力管道"的编排器 |
