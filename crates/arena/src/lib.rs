//! code-arena：WASM 沙箱通道模块
//!
//! 架构文档六节（WASM 沙箱设计）+ 3.2（沙箱通道）+ 4.2/4.3/4.4（攻击/探测/算力）。
//! 子模块：sandbox（沙箱管理）/ observe（被动观测）/ channel（probe/attack/call）/ compute（算力）/ attack（攻击请求与默认击杀模块）。

pub mod assets;
pub mod attack;
pub mod channel;
pub mod compute;
pub mod observe;
pub mod sandbox;
