//! 攻击请求与默认击杀模块（架构 4.2）
//!
//! PendingAttack：玩家代码调 env.attack_at import 时排队的攻击请求（阶段二统一处理）。
//! DEFAULT_KILL_WAT/WASM：payload 为空时框架用的默认击杀模块（life()=false）。

use codeworld_core::entity::EntityId;
use std::sync::LazyLock;

/// 排队的攻击请求（架构 4.2：import 只排队，阶段二 process_attacks 统一执行）
///
/// import 函数 env.attack_at 在 on_tick 执行期间被调，此时 world 被借用不可改，
/// 故只把请求推入 SandboxData.pending_attacks，阶段二统一处理（与 PendingChange 模式一致）。
#[derive(Debug, Clone)]
pub struct PendingAttack {
    /// 攻击方实体ID（import 函数从 SandboxData.entity_id 获知攻击者是谁）
    pub attacker: EntityId,
    /// 目标坐标（架构 4.2：attack_at 攻击坐标不是 EntityId，目标移动可能 miss）
    pub target_pos: (f64, f64),
    /// 攻击 payload：WASM 模块字节码（路线A整模块替换）。空=用 DEFAULT_KILL_WASM 默认击杀
    pub payload: Vec<u8>,
}

/// 默认击杀 WAT 文本：life()=false 的最小模块（payload 为空时框架用此替换目标沙箱）
pub const DEFAULT_KILL_WAT: &str = r#"(module (func (export "life") (result i32) i32.const 0))"#;

/// 默认击杀 WASM 字节码（预编译，避免每次空 payload 攻击重复 parse WAT）
pub static DEFAULT_KILL_WASM: LazyLock<Vec<u8>> = LazyLock::new(|| {
    wat::parse_bytes(DEFAULT_KILL_WAT.as_bytes())
        .expect("默认击杀 WAT 编译失败（静态常量，不应失败）")
        .into_owned()
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxManager;
    use uuid::Uuid;

    #[test]
    fn pending_attack_stores_fields() {
        // 创建：PendingAttack 字段正确存取（attacker/坐标/payload）
        let atk = PendingAttack {
            attacker: EntityId(Uuid::from_u128(1)),
            target_pos: (5.0, 5.0),
            payload: vec![1, 2, 3],
        };
        assert_eq!(atk.attacker, EntityId(Uuid::from_u128(1)));
        assert_eq!(atk.target_pos, (5.0, 5.0));
        assert_eq!(atk.payload, vec![1, 2, 3]);
    }

    #[test]
    fn default_kill_wasm_life_returns_false() {
        // 默认击杀模块 life()=false（payload 为空时的攻击效果：替换后目标死亡）
        let mgr = SandboxManager::new().unwrap();
        let mut h = mgr
            .create_sandbox(&DEFAULT_KILL_WASM, 1000, EntityId(Uuid::from_u128(1)))
            .unwrap();
        assert!(!mgr.call_life(&mut h).unwrap(), "默认击杀模块 life() 应为 false");
    }
}
