//! 攻击请求与默认击杀模块（架构 4.2）
//!
//! PendingAttack：玩家代码调 env.attack_at import 时排队的攻击请求（阶段二统一处理）。
//! DEFAULT_KILL_WAT/WASM：payload 为空时框架用的默认击杀模块（life()=false）。
//! 【#045 替代链口径】命中=接触通道代码攻：一切出刀都从实体出（env.attack_at 的
//! attacker=出刀实体自身，造物弹的独立沙箱自飞自攻），贴身判距门由物理事实天然
//! 成立（CCD 撞体截断停点落进接触窗口，deliver 只复核距离）；无弹丸专属结算路径，
//! contact_exempt/grace_owner 出刀携带字段已随 D-018 链路退役——grace_owner 豁免
//! 改读攻击者实体的出生声明（#035 修订四，deliver 现场查 binding/bound_to）。

use codeworld_core::entity::EntityId;
use std::sync::LazyLock;

/// 排队的攻击请求（架构 4.2：import 只排队，阶段二 process_attacks 统一执行）
///
/// import 函数 env.attack_at 在 on_tick 执行期间被调，此时 world 被借用不可改，
/// 故只把请求推入 SandboxData.pending_attacks，阶段二统一处理（与 PendingChange 模式一致）。
#[derive(Debug, Clone)]
pub struct PendingAttack {
    /// 攻击方实体ID（import 函数从 SandboxData.entity_id 获知攻击者是谁——
    /// #045 替代链：造物弹命中时 attacker=弹实体自身，独立沙箱自飞自攻）
    pub attacker: EntityId,
    /// 目标坐标（架构 4.2：attack_at 攻击坐标不是 EntityId，目标移动可能 miss）
    pub target_pos: (f64, f64),
    /// 攻击 payload：WASM 模块字节码（路线A整模块替换）。空=用 DEFAULT_KILL_WASM 默认击杀
    pub payload: Vec<u8>,
}

impl PendingAttack {
    /// 出刀（attack_at/操控攻击/接触通道造物弹同款：attacker=出刀实体自身）。
    /// 贴身判距门只约束物理上的"够不够得着"（deliver 复核 melee_in_reach）——
    /// 接触通道的出刀由 CCD 撞体截断天然贴身，门当拍成立；grace_owner 豁免
    /// 归 deliver 现场读攻击者实体出生声明（binding/bound_to），不在刀上携带。
    pub fn strike(attacker: EntityId, target_pos: (f64, f64), payload: Vec<u8>) -> Self {
        Self {
            attacker,
            target_pos,
            payload,
        }
    }
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
        // 创建：PendingAttack 字段正确存取（attacker/坐标/payload——#045 替代链：
        // 刀只带身份/坐标/payload 三件，豁免归实体出生声明，不在刀上携带）
        let atk = PendingAttack::strike(
            EntityId(Uuid::from_u128(1)),
            (5.0, 5.0),
            vec![1, 2, 3],
        );
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
