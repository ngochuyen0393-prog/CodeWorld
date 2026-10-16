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
    /// #034 接触门豁免：贴身刀判距只约束 attack_at 直出之手（出手者与目标身体相触
    /// 才砍得着）；弹丸命中结算不走本门——弹丸撞身体本身就是接触事实，离体之物是
    /// 攻击者手臂的延伸（env.attack_at 传 false，弹丸结算传 true）。
    pub contact_exempt: bool,
    /// #034 修订四攻击豁免条款（声明式）：true=弹丸结算命中发射者本人时豁免
    /// （落地不砸自己；飞行撞体本就全程豁免发射者，#034 既有口径）。
    /// 由弹丸绑定契约 grace_owner 携带；unbound 弹落地砸自己合法（世界不豁免，
    /// 豁免是声明）
    pub grace_owner: bool,
}

impl PendingAttack {
    /// 普通出刀（attack_at/操控攻击：走 #034 接触门）
    pub fn strike(attacker: EntityId, target_pos: (f64, f64), payload: Vec<u8>) -> Self {
        Self {
            attacker,
            target_pos,
            payload,
            contact_exempt: false,
            grace_owner: false,
        }
    }

    /// 弹丸命中结算（#034：接触事实已由弹丸完成，豁免贴身判距）。
    /// `grace_owner`：发射者是否声明了对自己的豁免（绑定契约 grace_owner）
    pub fn projectile_strike(
        attacker: EntityId,
        target_pos: (f64, f64),
        payload: Vec<u8>,
        grace_owner: bool,
    ) -> Self {
        Self {
            attacker,
            target_pos,
            payload,
            contact_exempt: true,
            grace_owner,
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
        // 创建：PendingAttack 字段正确存取（attacker/坐标/payload/接触门豁免）
        let atk = PendingAttack::strike(
            EntityId(Uuid::from_u128(1)),
            (5.0, 5.0),
            vec![1, 2, 3],
        );
        assert_eq!(atk.attacker, EntityId(Uuid::from_u128(1)));
        assert_eq!(atk.target_pos, (5.0, 5.0));
        assert_eq!(atk.payload, vec![1, 2, 3]);
        assert!(!atk.contact_exempt, "普通出刀走接触门");
        assert!(!atk.grace_owner, "普通出刀无发射者豁免");
        let proj = PendingAttack::projectile_strike(
            EntityId(Uuid::from_u128(2)),
            (6.0, 6.0),
            vec![],
            false,
        );
        assert!(proj.contact_exempt, "弹丸命中豁免贴身判距");
        let grace = PendingAttack::projectile_strike(
            EntityId(Uuid::from_u128(2)),
            (6.0, 6.0),
            vec![],
            true,
        );
        assert!(grace.grace_owner, "弹丸契约可携带发射者豁免");
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
