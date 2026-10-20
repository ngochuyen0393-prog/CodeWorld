//! 接口约束与配额（Constraints）
//!
//! 框架所有配额参数集中在此。MVP阶段是静态常量配置，
//! 用 struct + impl 常量函数定义（参考架构文档 3.3 节）。
//!
//! 设计原则：框架不提供任何游戏资源（无金币/点卡/能量），只提供基础设施。
//! 以下约束都是【技术保护】（防死循环/防内存爆炸/防DoS）或【物理限制】
//! （payload 大小/渲染频率），玩家不应感知，不影响游戏内数值。
//! probe/attack 不扣手续费，占用的是 on_tick 的执行时间。

use serde::{Deserialize, Serialize};

/// 框架接口约束与配额参数
///
/// 所有数值来自架构文档 3.3 节，MVP阶段为静态常量。
///
/// 这些参数全部是技术保护或物理限制，不是游戏资源：
/// - 技术保护：防死循环、防内存爆炸、防DoS（玩家不应感知）
/// - 物理限制：payload 大小、渲染频率等（防服务器资源耗尽）
///
/// 框架不收任何"手续费"，probe/attack 只占用 on_tick 的执行时间。
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

    // === probe（技术保护：防死循环+防DoS，不是游戏消耗） ===
    // probe 不扣手续费，占用的是 on_tick 的执行时间（见 3.3 节）
    /// 被探测方 on_probe 预算（防被探测方死循环）
    pub probe_target_instruction_limit: u64,
    /// probe超时（50ms）
    pub probe_timeout_ms: u64,
    /// probe最大调用深度（防跨沙箱死锁）
    pub probe_max_call_depth: u32,

    // === attack（技术保护：防死循环+防内存爆炸，不是游戏消耗） ===
    // attack 不扣手续费，占用的是 on_tick 的执行时间（见 3.3 节）
    /// attack payload最大字节数（64KB，防内存爆炸）
    pub attack_payload_max_size: u64,
    /// 被攻击方 on_attack 预算（防被攻击方死循环）
    pub attack_target_instruction_limit: u64,
    /// attack超时（200ms）
    pub attack_timeout_ms: u64,

    // === contact / death（D-007 三兄弟，同族技术保护，不是游戏消耗） ===
    // 与 probe/attack 同哲学：不扣手续费，host 侧独立小预算包裹
    /// 被接触方 on_contact 预算（防被接触方死循环；与 probe 同档——
    /// 地雷/哨戒圈是轻量应答）
    pub contact_target_instruction_limit: u64,
    /// 死亡谢幕 on_death 预算（遗嘱有字数上限=预算即上限；与 on_attack 同档）
    pub death_instruction_limit: u64,

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

            // probe（不扣手续费，占用 on_tick 执行时间）
            probe_target_instruction_limit: 10_000,
            probe_timeout_ms: 50,
            probe_max_call_depth: 3,

            // attack（不扣手续费，占用 on_tick 执行时间）
            attack_payload_max_size: 64 * 1024,
            attack_target_instruction_limit: 200_000,
            attack_timeout_ms: 200,

            // contact / death（D-007，不扣手续费，host 侧独立预算包裹）
            contact_target_instruction_limit: 10_000,
            death_instruction_limit: 200_000,

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
        // probe/attack 不再扣手续费（3.3 节：框架不提供游戏资源），
        // 故此处不再断言 probe_instruction_cost / attack_instruction_cost。
        assert_eq!(c.probe_target_instruction_limit, 10_000);
        assert_eq!(c.probe_timeout_ms, 50);
        assert_eq!(c.probe_max_call_depth, 3);
        assert_eq!(c.attack_payload_max_size, 64 * 1024);
        assert_eq!(c.attack_target_instruction_limit, 200_000);
        assert_eq!(c.attack_timeout_ms, 200);
        assert_eq!(c.contact_target_instruction_limit, 10_000);
        assert_eq!(c.death_instruction_limit, 200_000);
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
    fn default_contact_and_death_instruction_limits_match_d007() {
        // D-007 三兄弟预算默认值（架构 3.3 同族技术保护，非游戏消耗）：
        // contact=10_000 与 probe 同档（地雷/哨戒圈是轻量应答）；
        // death=200_000 与 on_attack 同档（遗嘱有字数上限=预算即上限）。
        let c = Constraints::default();
        assert_eq!(
            c.contact_target_instruction_limit, 10_000,
            "接触回调预算默认应为 10_000（与 probe 同档）"
        );
        assert_eq!(
            c.death_instruction_limit, 200_000,
            "遗嘱钩子预算默认应为 200_000（与 on_attack 同档）"
        );
    }

    #[test]
    fn constraints_has_no_game_resource_cost_fields() {
        // 设计原则（架构文档 3.3 节）：框架不提供任何游戏资源（无金币/点卡/能量），
        // 只提供基础设施。probe/attack 不扣手续费，占用的是 on_tick 的执行时间。
        // 因此 Constraints 不应再包含任何 *_instruction_cost 字段（那是"手续费"概念）。
        //
        // 通过 serde 序列化后的 JSON key 来断言"手续费"概念已彻底移除--
        // 若字段仍存在于结构体，序列化结果必然包含对应 key。
        let c = Constraints::default();
        let json = serde_json::to_string(&c).expect("Constraints 序列化失败");
        assert!(
            !json.contains("probe_instruction_cost"),
            "Constraints 不应再包含 probe_instruction_cost（probe 不扣手续费，见 3.3 节），实际 JSON: {}",
            json
        );
        assert!(
            !json.contains("attack_instruction_cost"),
            "Constraints 不应再包含 attack_instruction_cost（attack 不扣手续费，见 3.3 节），实际 JSON: {}",
            json
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
