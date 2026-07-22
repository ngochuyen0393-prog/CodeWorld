//! physics：默认物理规则 + 覆盖接口 + 冲突执行
//!
//! 架构文档 1.4 节物理模块子结构：
//! - 默认规则（default）-- 重力/碰撞/地面约束（厚版本，让新手能上手）
//! - 规则覆盖接口（override）-- 玩家注册自定义物理规则（自身/区域/全局）
//! - 冲突执行（conflict）-- 多规则冲突时统一执行（规则碰撞=攻击）

pub mod conflict;
pub mod default;
// `override` 是 Rust 保留关键字，用 r# 转义（文件名仍为 override.rs，符合架构文档十）
pub mod r#override;
