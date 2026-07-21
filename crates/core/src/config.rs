//! 接口约束与配额（Constraints）
//!
//! 框架所有配额参数集中在此。MVP阶段是静态常量配置，
//! 用 struct + impl 常量函数定义（参考架构文档 3.3 节）。

use serde::{Deserialize, Serialize};

/// 框架接口约束与配额参数
///
/// 所有数值来自架构文档 3.3 节，MVP阶段为静态常量。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraints {
    // === spawn 配额 ===
    /// 每玩家每tick最多spawn数
    pub spawn_per_player_per_tick: u32,
    /// 每玩家累计最多存活实体数
    pub spawn_total_per_player: u32,

    // === 指令配额（防死循环，非游戏数值） ===
    /// 每tick每实体最大指令数（100万条）
    pub tick_callback_instruction_limit: u64,
    /// 每tick每实体超时（100ms）
    pub tick_callback_timeout_ms: u64,

    // === probe ===
    /// probe消耗指令数（10万条）
    pub probe_instruction_cost: u64,
    /// 被探测方on_probe预算（严格小于cost的1/10）
    pub probe_target_instruction_limit: u64,
    /// probe超时（50ms）
    pub probe_timeout_ms: u64,
    /// probe最大调用深度（防跨沙箱死锁）
    pub probe_max_call_depth: u32,

    // === attack ===
    /// attack消耗指令数（50万条）
    pub attack_instruction_cost: u64,
    /// attack payload最大字节数（64KB）
    pub attack_payload_max_size: u64,
    /// 被攻击方on_attack预算
    pub attack_target_instruction_limit: u64,
    /// attack超时（200ms）
    pub attack_timeout_ms: u64,

    // === call ===
    /// call超时（100ms）
    pub call_timeout_ms: u64,
    /// call响应最大字节数（64KB）
    pub call_response_max_size: u64,
    /// 每tick最多call次数
    pub call_per_tick: u32,

    // === compute ===
    /// 外接算力超时（5秒）
    pub compute_timeout_ms: u64,
    /// 超时是否降级基础算力
    pub compute_fallback: bool,

    // === render ===
    /// 渲染状态最大字节数（64KB）
    pub render_state_max_size: u64,
    /// 每tick最多提交渲染状态次数
    pub render_per_tick: u32,

    // === physics override ===
    /// 每实体最多注册覆盖规则数
    pub physics_override_per_entity: u32,
    /// 每条规则每tick执行预算
    pub physics_override_instruction_limit: u64,
    /// 规则执行超时（50ms）
    pub physics_override_timeout_ms: u64,
    /// 全世界最多全局覆盖数
    pub physics_global_override_total: u32,
}

impl Constraints {
    /// 默认配额（架构文档 3.3 节的数值）
    ///
    /// 注意：const fn 无法加 tracing 日志，Constraints 是静态常量配置，
    /// 创建时不产生运行时事件，故不加日志（这是日志规则的合理例外）。
    pub const fn default() -> Self {
        Self {
            // spawn 配额
            spawn_per_player_per_tick: 1,
            spawn_total_per_player: 100,

            // 指令配额
            tick_callback_instruction_limit: 1_000_000,
            tick_callback_timeout_ms: 100,

            // probe
            probe_instruction_cost: 100_000,
            probe_target_instruction_limit: 10_000,
            probe_timeout_ms: 50,
            probe_max_call_depth: 3,

            // attack
            attack_instruction_cost: 500_000,
            attack_payload_max_size: 64 * 1024,
            attack_target_instruction_limit: 200_000,
            attack_timeout_ms: 200,

            // call
            call_timeout_ms: 100,
            call_response_max_size: 64 * 1024,
            call_per_tick: 10,

            // compute
            compute_timeout_ms: 5000,
            compute_fallback: true,

            // render
            render_state_max_size: 64 * 1024,
            render_per_tick: 1,

            // physics override
            physics_override_per_entity: 10,
            physics_override_instruction_limit: 200_000,
            physics_override_timeout_ms: 50,
            physics_global_override_total: 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_constraints_match_architecture_doc() {
        // 创建：默认配额必须严格等于架构文档 3.3 节的数值
        let c = Constraints::default();
        assert_eq!(c.spawn_per_player_per_tick, 1);
        assert_eq!(c.spawn_total_per_player, 100);
        assert_eq!(c.tick_callback_instruction_limit, 1_000_000);
        assert_eq!(c.tick_callback_timeout_ms, 100);
        assert_eq!(c.probe_instruction_cost, 100_000);
        assert_eq!(c.probe_target_instruction_limit, 10_000);
        assert_eq!(c.probe_timeout_ms, 50);
        assert_eq!(c.probe_max_call_depth, 3);
        assert_eq!(c.attack_instruction_cost, 500_000);
        assert_eq!(c.attack_payload_max_size, 64 * 1024);
        assert_eq!(c.attack_target_instruction_limit, 200_000);
        assert_eq!(c.attack_timeout_ms, 200);
        assert_eq!(c.call_timeout_ms, 100);
        assert_eq!(c.call_response_max_size, 64 * 1024);
        assert_eq!(c.call_per_tick, 10);
        assert_eq!(c.compute_timeout_ms, 5000);
        assert!(c.compute_fallback);
        assert_eq!(c.render_state_max_size, 64 * 1024);
        assert_eq!(c.render_per_tick, 1);
        assert_eq!(c.physics_override_per_entity, 10);
        assert_eq!(c.physics_override_instruction_limit, 200_000);
        assert_eq!(c.physics_override_timeout_ms, 50);
        assert_eq!(c.physics_global_override_total, 5);
    }

    #[test]
    fn probe_target_budget_is_at_most_tenth_of_cost() {
        // 边界：被探测方 on_probe 预算应不大于 probe 指令消耗的 1/10
        // （架构文档 4.3 节安全性约束：防被探测方反耗攻击者）
        //
        // ⚠️ 文档不一致：3.3 节定 probe_target_instruction_limit=10_000、
        //    probe_instruction_cost=100_000，恰为 1/10（相等而非"严格小于"）；
        //    4.3 节文字要求"严格小于 1/10"。任务规定按 3.3 节数值为准，
        //    故此处断言 <=（相等成立），并在报告中标注该文档矛盾待澄清。
        let c = Constraints::default();
        assert!(
            c.probe_target_instruction_limit * 10 <= c.probe_instruction_cost,
            "on_probe 预算 * 10 应 <= probe 消耗，实际 {} * 10 = {} > {}",
            c.probe_target_instruction_limit,
            c.probe_target_instruction_limit * 10,
            c.probe_instruction_cost
        );
    }

    #[test]
    fn constraints_clone_serialize_roundtrip() {
        // 读写：clone 与 serde 序列化往返不丢字段
        let c = Constraints::default();
        let cloned = c.clone();
        assert_eq!(c.tick_callback_instruction_limit, cloned.tick_callback_instruction_limit);

        let json = serde_json::to_string(&c).unwrap();
        let back: Constraints = serde_json::from_str(&json).unwrap();
        assert_eq!(c.attack_payload_max_size, back.attack_payload_max_size);
    }
}
