//! world-core：CodeWorld 世界内核数据结构
//!
//! 定义所有其他模块依赖的核心数据结构：
//! - 实体（Entity/EntityId）、世界状态（WorldState）
//! - 生存点数（SurvivalPoints）、指令配额（InstructionGuard）
//! - 物理规则（PhysicsOverride/DefaultPhysics 等）
//! - 渲染（RenderLayer/RenderRequest 等）
//! - 接口约束（Constraints）
//!
//! 本 crate 只定义数据结构与基本方法，不实现 tick 循环或 WASM 沙箱逻辑。

pub mod config;
pub mod entity;
pub mod guard;
pub mod pending;
pub mod physics;
pub mod render;
pub mod survival;
pub mod world;
