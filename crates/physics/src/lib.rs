//! physics：默认物理规则 + 覆盖接口 + 冲突执行
//!
//! 架构文档 1.4 节物理模块子结构：
//! - 默认规则（default）-- 重力/碰撞/地面约束（厚版本，让新手能上手）
//! - 规则覆盖接口（override）-- 玩家注册自定义物理规则（自身/区域/全局）
//! - 冲突执行（conflict）-- 多规则冲突时统一执行（规则碰撞=攻击）

pub mod conflict;
pub mod default;
// D-012 力场：field 出生声明家族的物理结算（FieldEffect/叠加 v2/登记制收集）
pub mod fields;
// D-009 水体：liquid 出生声明家族的物理结算（LiquidDomain/浮沉涌现/登记制收集）
pub mod liquid;
// `override` 是 Rust 保留关键字，用 r# 转义（文件名仍为 override.rs，符合架构文档十）
pub mod r#override;
