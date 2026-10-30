//! 技能栏（D-020b 语义重定义，t_bdb1da48）：技能=玩家自己造物的服务器侧记忆，
//! 不再是静态表。D-020a 的"预置四模板"（投掷器/飞剑/护盾场/侦察弹）退役——
//! 开发期测试内容冒充产品内容（违 #011"开局四生物"同病）；D-020a 拍板原文
//! "技能=玩家自己的造物模板快捷方式"，本模块回归该语义。
//!
//! 数据链：玩家 spawn 成功的造物在阶段二落地那刻自动入列（同名去重保留首次，
//! 按首次出生顺序排列，上限 9 格）；玩家按技能格上行 `skill{slot:N}`，服务端
//! 取出该格模板重新 spawn（出生点=操控实体旁重定位，#030 预检/上限/编译全适用）。
//! 账本随进程存活不持久化——重启=空栏重来，与会话令牌 v2.9 同生命周期（api.md 载明）。

use codeworld_arena::sandbox::PendingSpawn;
use std::collections::HashMap;

/// 技能格数上限（上行 slot=1..=9；数据源派生的自然上限——超出的不显示不入列，
/// 与实体上限 64 同族护栏；前端数字键 1..9 同域）
pub(crate) const SKILL_SLOTS_MAX: usize = 9;

/// 单个技能格模板（玩家某次 spawn 成功的造物档案：name+code+全部出生参数，
/// 再 spawn 时原样重放——只有出生点由 handle_skill_intents 按操控实体实况重定位）
#[derive(Debug, Clone)]
pub(crate) struct SkillTemplate {
    /// 造物实名（技能格显示名；同格键控字段）
    pub name: String,
    /// 造物 WAT 源码（再 spawn 重放）
    pub code: String,
    /// 外观字形（恰好 1 字符，import 层校验过）
    pub glyph: String,
    /// 外观颜色
    pub color: String,
    /// 身体半径（spawn 第 7 参）
    pub radius: f64,
    /// 质量（第 8 参）
    pub mass: f64,
    /// 绑定契约（第 9 参）
    pub binding: i32,
    /// 运动身份（第 10 参）
    pub motion: i32,
    /// 出生初速（第 11/12 参）
    pub vx: f64,
    pub vy: f64,
    /// 出生数据包（第 13/14 参）
    pub blob: Vec<u8>,
    /// 力场三参（第 15/16/17 参）
    pub field_radius: f64,
    pub field_gravity_scale: f64,
    pub field_drag_delta: f64,
    /// 液体两参（第 18/19 参）
    pub liquid_density: f64,
    pub liquid_drag: f64,
    /// 链声明（第 20 参；再 spawn 时母体多半已不在——链只在出生那一刻可挂，
    /// 重放仍带原声明，process_spawn_requests 的"母体已死按放生落地"兜底）
    pub link: i32,
    /// 弹性/摩擦（第 21/22 参）
    pub restitution: f64,
    pub friction: f64,
    /// 寿命声明（第 23 参，>=1）
    pub lifespan_ticks: u64,
    /// fuel 配额（继承原创造者沙箱）
    pub fuel_limit: u64,
}

impl SkillTemplate {
    /// 从 PendingSpawn 提取模板（阶段二落地成功那刻调用——import 层拒绝/阶段二
    /// 作废的失败品不进此路径，正确：失败品不上栏）
    pub(crate) fn from_pending(req: &PendingSpawn) -> Self {
        Self {
            name: req.name.clone().unwrap_or_default(),
            code: req.code.clone(),
            glyph: req.glyph.clone(),
            color: req.color.clone(),
            radius: req.radius,
            mass: req.mass,
            binding: req.binding,
            motion: req.motion,
            vx: req.vx,
            vy: req.vy,
            blob: req.blob.clone(),
            field_radius: req.field_radius,
            field_gravity_scale: req.field_gravity_scale,
            field_drag_delta: req.field_drag_delta,
            liquid_density: req.liquid_density,
            liquid_drag: req.liquid_drag,
            link: req.link,
            restitution: req.restitution,
            friction: req.friction,
            lifespan_ticks: req.lifespan_ticks,
            fuel_limit: req.fuel_limit,
        }
    }
}

/// 按玩家的技能格账本（服务端内存态：每玩家一个模板清单，格下标=清单下标+1）
#[derive(Default)]
pub(crate) struct SkillMemory {
    per_player: HashMap<String, Vec<SkillTemplate>>,
}

impl SkillMemory {
    /// 模板入列（阶段二 spawn 落地成功时调用）：同名去重保留首次（重复 spawn
    /// 同款不占新格）；满 9 格且名字新 → 拒绝入列返回 false（调用方 INFO）；
    /// 入列成功返回 true。空名（阶段二回退默认名之前不该发生——调用方保证
    /// 先落名再入列）防御性按名字面入列
    pub(crate) fn record(&mut self, player: &str, tpl: SkillTemplate) -> bool {
        let list = self.per_player.entry(player.to_string()).or_default();
        if list.iter().any(|t| t.name == tpl.name) {
            tracing::debug!(
                player = %player,
                name = %tpl.name,
                "[skill] 同名模板已在格（保留首次），不重复入列"
            );
            return false;
        }
        if list.len() >= SKILL_SLOTS_MAX {
            tracing::info!(
                player = %player,
                name = %tpl.name,
                cap = SKILL_SLOTS_MAX,
                "[skill] 技能格已满 9，超出不入列（数据源派生自然上限）"
            );
            return false;
        }
        tracing::info!(
            player = %player,
            name = %tpl.name,
            slot = list.len() + 1,
            "[skill] 造物模板入列（技能=我的造物，spawn 成功自动上栏）"
        );
        list.push(tpl);
        true
    }

    /// 取格模板（slot 1..=9 → 清单下标 0..=8；越界/空格=None，调用方 warn 丢弃）
    pub(crate) fn template(&self, player: &str, slot: u8) -> Option<&SkillTemplate> {
        self.per_player
            .get(player)?
            .get((slot as usize).checked_sub(1)?)
    }

    /// 该玩家当前格数（测试/调试观测口；生产链路 record 日志自带 slot 序号）
    #[allow(dead_code)] // 测试专用观测口，非死代码（events.rs reason 码表同款先例）
    pub(crate) fn count(&self, player: &str) -> usize {
        self.per_player.get(player).map_or(0, Vec::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试模板构造（只填键控字段，其余零值）
    fn tpl(name: &str) -> SkillTemplate {
        SkillTemplate {
            name: name.to_string(),
            code: "(module)".to_string(),
            glyph: "T".to_string(),
            color: "cyan".to_string(),
            radius: 0.5,
            mass: 1.0,
            binding: 0,
            motion: 0,
            vx: 0.0,
            vy: 0.0,
            blob: Vec::new(),
            field_radius: 0.0,
            field_gravity_scale: 1.0,
            field_drag_delta: 0.0,
            liquid_density: 0.0,
            liquid_drag: 0.0,
            link: 0,
            restitution: 0.0,
            friction: 0.6,
            lifespan_ticks: 100,
            fuel_limit: 1_000_000,
        }
    }

    #[test]
    fn t_d020b_record_dedup_keeps_first() {
        // 同名去重保留首次：首录入列 true；同名再录 false 且保留首次的 code
        //（"历史模板不是活体计数"——重复 spawn 同款不占新格）
        let mut mem = SkillMemory::default();
        assert!(mem.record("alice", tpl("箭塔")), "首录入列");
        let mut dup = tpl("箭塔");
        dup.code = "(module (func))".to_string(); // 同名不同码（玩家改过代码再 spawn）
        assert!(!mem.record("alice", dup), "同名再录应拒绝");
        assert_eq!(mem.count("alice"), 1, "仍 1 格");
        assert_eq!(
            mem.template("alice", 1).unwrap().code,
            "(module)",
            "保留首次的代码"
        );
        // 异名照常入列，按首次出生顺序排列
        assert!(mem.record("alice", tpl("炮台")));
        assert_eq!(mem.template("alice", 2).unwrap().name, "炮台");
        // 玩家间隔离：bob 的账本独立
        assert_eq!(mem.count("bob"), 0, "bob 无模板");
        assert!(mem.record("bob", tpl("箭塔")));
        assert_eq!(mem.count("bob"), 1);
        assert_eq!(mem.count("alice"), 2, "alice 不受 bob 影响");
    }

    #[test]
    fn t_d020b_cap_nine_slots() {
        // 上限 9：9 格内照常入列；第 10 个异名拒绝（超出不显示不入列，护栏同族）
        let mut mem = SkillMemory::default();
        for i in 1..=9 {
            assert!(
                mem.record("alice", tpl(&format!("造物{i}"))),
                "第 {i} 格应入列"
            );
        }
        assert_eq!(mem.count("alice"), 9);
        assert!(!mem.record("alice", tpl("第十格")), "满 9 后应拒绝");
        assert_eq!(mem.count("alice"), 9, "格数不变");
    }

    #[test]
    fn t_d020b_slot_lookup_bounds() {
        // slot 语义=玩家技能格下标：1..=9 → 清单下标 0..=8；0/10/空玩家=None
        let mut mem = SkillMemory::default();
        assert!(mem.record("alice", tpl("甲")));
        assert!(mem.record("alice", tpl("乙")));
        assert_eq!(mem.template("alice", 1).unwrap().name, "甲");
        assert_eq!(mem.template("alice", 2).unwrap().name, "乙");
        assert!(mem.template("alice", 3).is_none(), "空格=None（warn 丢弃）");
        assert!(mem.template("alice", 0).is_none(), "slot 0 非法");
        assert!(mem.template("alice", 10).is_none(), "slot 10 越界");
        assert!(mem.template("bob", 1).is_none(), "无账本玩家=None");
    }
}
