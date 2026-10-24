//! 技能栏（D-020a，t_06b4efc6）：驾驶舱快捷键 = 替操控玩家 spawn 预置模板造物。
//! UI 便利性非世界玩法供给——模板能力全部来自已开放 spawn 参数，零新原语；
//! 模板 WAT 全文可查可抄（static/skills/ 随静态页伺服，GET /skills/*.wat）。
//! 每个模板必须带合法 lifespan_ticks 声明（D-019 R1 强制：域 [1,+]）。

/// 技能槽位数（上行 slot=1..=4）
pub(crate) const SKILL_SLOT_COUNT: u8 = 4;

/// 技能定义（静态表；WAT 源单独放 static/skills/ 由启动读入——全文可抄是卖点）
pub(crate) struct SkillDef {
    /// 槽位 1..=4
    pub slot: u8,
    /// 面板显示名
    pub name: &'static str,
    /// 造物字形（ASCII 单字符：终端等宽渲染安全）
    pub glyph: &'static str,
    /// 造物颜色（颜色名，同 spawn 第 5 参域）
    pub color: &'static str,
    /// 模板文件名（static/skills/ 下；=creature 源码，spawn 出来的实体跑的就是它）
    pub file: &'static str,
    /// 身体半径（spawn 第 7 参）
    pub radius: f64,
    /// 质量（第 8 参）
    pub mass: f64,
    /// 运动身份（第 10 参：dynamic=吃完整物理会飞）
    pub motion: i32,
    /// 出生初速（第 11/12 参）
    pub vx: f64,
    pub vy: f64,
    /// 力场三参（第 15/16/17 参：0/1.0/0=无场）
    pub field_radius: f64,
    pub field_gravity_scale: f64,
    pub field_drag_delta: f64,
    /// 声明寿命（第 23 参，D-019 R1 强制 >=1）
    pub lifespan_ticks: u64,
}

/// 四格技能静态表（slot 1..=4；初速全部 ≤ 世界极速 25、寿命全部 >=1）
pub(crate) const SKILLS: [SkillDef; SKILL_SLOT_COUNT as usize] = [
    SkillDef {
        slot: 1,
        name: "投掷器",
        glyph: "o",
        color: "orange",
        file: "thrower.wat",
        radius: 0.3,
        mass: 0.5,
        motion: codeworld_core::entity::MOTION_DYNAMIC,
        vx: 8.0,
        vy: 0.0,
        field_radius: 0.0,
        field_gravity_scale: 1.0,
        field_drag_delta: 0.0,
        lifespan_ticks: 10,
    },
    SkillDef {
        slot: 2,
        name: "飞剑",
        glyph: "+",
        color: "magenta",
        file: "sword.wat",
        radius: 0.35,
        mass: 1.0,
        motion: codeworld_core::entity::MOTION_DYNAMIC,
        vx: 6.0,
        vy: 3.0,
        field_radius: 0.0,
        field_gravity_scale: 1.0,
        field_drag_delta: 0.0,
        lifespan_ticks: 15,
    },
    SkillDef {
        slot: 3,
        name: "护盾场",
        glyph: "#",
        color: "cyan",
        file: "shield.wat",
        radius: 0.5,
        mass: 1.0,
        motion: codeworld_core::entity::MOTION_STATIC,
        vx: 0.0,
        vy: 0.0,
        field_radius: 4.0,
        field_gravity_scale: 1.0,
        field_drag_delta: 5.0,
        lifespan_ticks: 10000,
    },
    SkillDef {
        slot: 4,
        name: "侦察弹",
        glyph: "^",
        color: "yellow",
        file: "scout.wat",
        radius: 0.25,
        mass: 0.5,
        motion: codeworld_core::entity::MOTION_DYNAMIC,
        vx: 3.0,
        vy: 0.0,
        field_radius: 0.0,
        field_gravity_scale: 1.0,
        field_drag_delta: 0.0,
        lifespan_ticks: 30,
    },
];

/// 技能书：启动时读入的模板源（下标=slot-1；缺文件/编译失败=该槽位不可用，
/// warn 不拖垮世界——技能栏是便利层不是玩法命脉）
pub(crate) struct SkillBook {
    sources: Vec<Option<String>>,
}

impl SkillBook {
    /// 从 static/skills/ 读入四模板（读不到/编不过的槽位禁用并 WARN）
    pub(crate) fn load(dir: &std::path::Path) -> Self {
        let mut sources = Vec::with_capacity(SKILL_SLOT_COUNT as usize);
        for def in SKILLS.iter() {
            let path = dir.join(def.file);
            let src = match std::fs::read_to_string(&path) {
                Ok(text) => match wat::parse_str(&text) {
                    Ok(_) => {
                        tracing::info!(slot = def.slot, name = def.name, "[skill] 模板装配完成（{}）", def.file);
                        Some(text)
                    }
                    Err(e) => {
                        tracing::warn!(slot = def.slot, file = def.file, errMsg = %e, "[skill] 模板编译失败，槽位禁用");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(slot = def.slot, file = %path.display(), errMsg = %e, "[skill] 模板文件缺失，槽位禁用");
                    None
                }
            };
            sources.push(src);
        }
        Self { sources }
    }

    /// 禁用态（测试脚手架/显式无技能场景用：全槽位不可用）
    pub(crate) fn disabled() -> Self {
        Self {
            sources: vec![None; SKILL_SLOT_COUNT as usize],
        }
    }

    /// 槽位定义（slot 1..=4 之外=None）
    pub(crate) fn def(&self, slot: u8) -> Option<&'static SkillDef> {
        SKILLS.get((slot as usize).checked_sub(1)?)
    }

    /// 槽位模板源（槽位合法且模板已装配才 Some）
    pub(crate) fn source(&self, slot: u8) -> Option<&str> {
        let idx = (slot as usize).checked_sub(1)?;
        self.sources.get(idx)?.as_deref()
    }
}

/// 模板目录名（STATIC_DIR 下；伺服路径 GET /skills/*.wat 同源）
pub(crate) const SKILLS_DIR_NAME: &str = "skills";

/// 便捷：从静态页根目录装配技能书（main 启动用）
pub(crate) fn load_from_static(static_dir: &std::path::Path) -> SkillBook {
    SkillBook::load(&static_dir.join(SKILLS_DIR_NAME))
}
