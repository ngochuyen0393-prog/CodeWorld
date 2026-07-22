//! space-engine：坐标系 + 变换管线 + 区域管理
//!
//! 架构文档 1.4 节空间模块子结构：
//! - 坐标系（coord）-- get_pos/set_pos，读写坐标
//! - 变换管线（transform）-- 注册坐标变换函数，按优先级叠加
//! - 区域注册（zone）-- 注册带规则的区域，查询坐标所属区域

pub mod coord;
pub mod transform;
pub mod zone;
