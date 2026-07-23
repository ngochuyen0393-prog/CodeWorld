//! codeworld 主程序：把 7 个 crate 串成可运行的 tick 循环
//!
//! 借鉴 Screeps 两阶段 tick 模型 + AzerothCore 管理器链 Update 模式 + Bevy 插件式组合。
//! 主程序只编排不实现逻辑：main() 负责 init/循环/信号，tick() 负责单 tick 的串行状态更新。
//! MVP 跳过玩家代码执行（Phase 2 接入 WASM）与并行。
//!
//! 架构文档 4.1 节 MVP 简化版 tick 流程：
//! 1. time-engine: tick++
//! 2. 阶段一：玩家代码执行（MVP 跳过）
//! 3. 阶段二：状态更新（串行）
//!    a. physics.step(world)    b. 死亡清理    c. renderer.render(world)    d. 每100tick存快照
//! 4. sleep 到下一个 tick

mod config;

use anyhow::Result;
use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};
use codeworld_core::world::WorldState;
use codeworld_physics::default::DefaultPhysicsEngine;
use codeworld_render::{Renderer, Viewport};
use codeworld_storage::StorageEngine;
use codeworld_time::clock::TimeEngine;
use config::Config;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing_subscriber::filter::LevelFilter;

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

    // 新世界或恢复后无实体：放测试实体让 MVP 有东西渲染
    if world.entities.is_empty() {
        spawn_test_entities(&mut world);
    }

    // 信号处理：后台线程监听 stdin，按 Enter/EOF 置退出标志（零依赖，符合 MVP 最简方案）
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        r.store(false, Ordering::SeqCst);
    });

    tracing::info!("tick 循环开始（按 Enter 退出）");
    while running.load(Ordering::SeqCst) {
        tick(
            &mut world,
            &mut time_engine,
            &physics,
            &mut renderer,
            &mut storage,
        );
        std::thread::sleep(config.tick_interval());
    }

    // 关闭：强制存快照（架构文档 4.1 收尾）
    tracing::info!("收到退出信号，正在保存世界快照...");
    storage.force_save(&world)?;
    tracing::info!(tick = world.tick(), entities = world.entities.len(), "主程序已关闭");
    Ok(())
}

/// 初始化 tracing 日志，级别从配置字符串读取（非法时回退 info）
fn init_tracing(log_level: &str) {
    let level = log_level.parse::<LevelFilter>().unwrap_or(LevelFilter::INFO);
    tracing_subscriber::fmt().with_max_level(level).init();
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
) {
    // ① 时间推进（time-engine 计数 + 世界 tick 同步推进）
    let should_snapshot = time_engine.advance_tick();
    world.advance_tick();
    let current_tick = world.tick();

    // ② 阶段一：玩家代码执行（MVP 跳过，Phase 2 接入 WASM）

    // ③ 阶段二：状态更新（串行）
    //   a. 物理规则（重力/地面/速度/碰撞，架构 4.1 b 步）
    let changes = physics.step(world);
    if !changes.is_empty() {
        tracing::debug!(tick = current_tick, change_count = changes.len(), "物理变更");
    }

    //   b. 死亡清理：移除死亡实体
    let before = world.entities.len();
    world.entities.retain(|_, e| e.is_alive());
    let removed = before - world.entities.len();
    if removed > 0 {
        tracing::info!(tick = current_tick, removed, "死亡实体清理");
    }

    //   c. 渲染输出（架构 4.1 e 步；有差异才 flush）
    let diffs = renderer.render(world);
    if !diffs.is_empty() {
        renderer.flush();
        tracing::debug!(tick = current_tick, diff_count = diffs.len(), "渲染输出");
    }

    //   d. 快照存储（每 100 tick，架构 4.1 d 步）
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

/// 生成测试实体塞入世界（让 MVP 有东西渲染）
///
/// 两个实体分别置于 (0, 10) 与 (5, 20)，都不站地面（y=0），
/// 这样默认重力规则下能看到下落效果，日志比实体贴在地面更有信息量。
fn spawn_test_entities(world: &mut WorldState) {
    for pos in [(0.0, 10.0), (5.0, 20.0)] {
        let e = create_test_entity(pos, world.tick());
        tracing::info!(entity_id = %e.id.0, pos = ?e.pos(), "创建测试实体");
        world.entities.insert(e.id, e);
    }
}

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

    #[test]
    fn tick_advances_world_tick() {
        // 修改：连续 tick 推进世界与时间引擎计数
        let mut world = WorldState::new();
        let mut time = TimeEngine::new();
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        for _ in 0..5 {
            tick(&mut world, &mut time, &physics, &mut renderer, &mut storage);
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
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage);
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
        tick(&mut world, &mut time, &physics, &mut renderer, &mut storage);
        assert_eq!(world.entities.len(), 1, "死亡实体应被清理");
        assert!(world.entities.contains_key(&alive));
        assert!(!world.entities.contains_key(&dead));
    }

    #[test]
    fn tick_snapshots_at_interval_100() {
        // 集成：每 100 tick 自动存快照（架构 4.1 d 步）
        let mut world = WorldState::new();
        let mut time = TimeEngine::new(); // 默认间隔 100
        let physics = DefaultPhysicsEngine::new();
        let mut renderer = test_renderer();
        let mut storage = StorageEngine::open_in_memory().unwrap();
        for _ in 0..100 {
            tick(&mut world, &mut time, &physics, &mut renderer, &mut storage);
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
        spawn_test_entities(&mut world);
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
        for _ in 0..5 {
            tick(&mut world, &mut time, &physics, &mut renderer, &mut storage);
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
}
