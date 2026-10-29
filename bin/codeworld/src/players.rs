//! 玩家目录：门一进场 + 在线锚合同 + 战斗残影期（world-foundation.md 二/三章）
//!
//! - 一个 `.wat` 文件 = 一名玩家；文件存在 = 在线信号，删除 = 下线
//! - 玩家名 = 文件名（去扩展名）= code_hash 身份承载
//! - 下线实体进"残影状态"：不执行玩家代码、仍可被攻击/掠夺，到期消散
//! - #013 铁律：最近 30 拍内参战过的实体下线，残影期延长（不可豁免，防拔线逃课）
//!
//! 本模块只放纯逻辑与状态机；与沙箱/世界的编排（spawn/poll/expire 接线）在 main.rs
//! 的 EntitySandboxMap impl 中（需访问沙箱表与 world，跨模块字段不可见）。

use codeworld_core::entity::EntityId;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::dispose::{DisposeBook, DEFAULT_DISPOSE_WAIT_TICKS};

/// 玩家颜色轮（按进场顺序轮转；A红/D青/H黄/S紫 已占用，玩家从 green 起轮）
pub(crate) const COLOR_WHEEL: [&str; 7] = [
    "red", "cyan", "yellow", "purple", "green", "magenta", "blue",
];
/// 玩家色轮起始下标（green；前 4 个已被开局四生物占用）
const COLOR_OFFSET: usize = 4;
/// 出生点 x 轮转序列（y 恒为 0；确定性分配，不引 rand 依赖）
const SPAWN_XS: [f64; 4] = [30.0, 35.0, 40.0, 45.0];
/// 普通残影期拍数（#013 基线：下线后在世界里留 5 拍，可被攻击/掠夺）
pub(crate) const GHOST_BASE_TICKS: u64 = 5;
/// 战斗窗口拍数：最近 N 拍内参战过的实体下线，残影期延长（#013 防拔线逃课）
pub(crate) const COMBAT_WINDOW_TICKS: u64 = 30;

/// 玩家名首字符大写 = 渲染字形（"example" -> "E"；非 ASCII 首字符原样大写化）
pub(crate) fn glyph_for(name: &str) -> String {
    let glyph = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".to_string());
    tracing::debug!(name, glyph = %glyph, "[players] 字形生成");
    glyph
}

/// 第 idx 名进场玩家的颜色（色轮从 green 起轮转，越界回绕）
pub(crate) fn color_for(join_index: usize) -> &'static str {
    COLOR_WHEEL[(COLOR_OFFSET + join_index) % COLOR_WHEEL.len()]
}

/// 第 idx 名进场玩家的出生点 x（[30,35,40,45] 循环）
pub(crate) fn spawn_x_for(join_index: usize) -> f64 {
    SPAWN_XS[join_index % SPAWN_XS.len()]
}

/// 残影期时长（#013）：普通 5 拍；最近 30 拍内参战 -> max(5, 30 - 距上次参战拍数)。
/// `now - t == 30` 恰好出窗（< 30 才算战斗状态）。
pub(crate) fn ghost_duration_ticks(last_combat: Option<u64>, now: u64) -> u64 {
    match last_combat {
        Some(t) if now.saturating_sub(t) < COMBAT_WINDOW_TICKS => {
            (COMBAT_WINDOW_TICKS - (now - t)).max(GHOST_BASE_TICKS)
        }
        _ => GHOST_BASE_TICKS,
    }
}

/// me 声明解析（#024 身份绑定）：操控者声明的玩家名。
///
/// - 环境变量 `CODEWORLD_ME` 优先（trim 后非空才生效，空串=未通过 env 声明，回落文件）
/// - 其次 `<dir>/me.txt` 一行内容（trim；空文件/文件缺失 = 未声明）
/// - 返回 None = 未声明（观战模式）；名字是否对应在线玩家由 [`PlayerDirectory::controlled_target`] 校验
pub(crate) fn read_me_declaration(env_value: Option<&str>, dir: &Path) -> Option<String> {
    if let Some(v) = env_value {
        let t = v.trim();
        if !t.is_empty() {
            tracing::debug!(source = "env", name = t, "[players] me 声明来自 CODEWORLD_ME");
            return Some(t.to_string());
        }
    }
    let path = dir.join("me.txt");
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let t = content.trim();
            if t.is_empty() {
                tracing::debug!(path = %path.display(), "[players] me.txt 为空，未声明操控者");
                None
            } else {
                tracing::debug!(path = %path.display(), name = t, "[players] me 声明来自 me.txt");
                Some(t.to_string())
            }
        }
        Err(_) => None,
    }
}

/// 下线事件（玩家删文件产出，供日志与接线）
pub(crate) struct OfflineEvent {
    pub(crate) entity_id: EntityId,
    /// 残影期拍数（普通 5 / 战斗延长）
    pub(crate) ghost_ticks: u64,
    /// 是否战斗残影（30 拍内参战过）
    pub(crate) in_combat: bool,
}

/// 玩家目录状态机（players/ 目录 + 在线锚 + 残影期）。
///
/// 字段私有，main.rs 经方法访问；目录 IO 只在 [`PlayerDirectory::scan_dir`]。
pub(crate) struct PlayerDirectory {
    dir: PathBuf,
    /// 进场门开关：默认关（测试基线零副作用），main 按配置开启
    enabled: bool,
    /// 在线玩家：玩家名 -> 角色实体 ID
    online: HashMap<String, EntityId>,
    /// 残影中：实体 ID -> 消散 tick（到期拍阶段二标记死亡，走死亡清理通路）
    ghosts: HashMap<EntityId, u64>,
    /// 最近参战 tick：实体 ID -> tick（战斗残影期计算；#013）
    combat: HashMap<EntityId, u64>,
    /// 编译失败黑名单：进场失败不再逐拍重试刷屏；文件消失即解除（删了重放可重试）
    failed: HashSet<String>,
    /// 会话级离场标记（#025 登出带走）：在册名字不参与目录自动重生；
    /// 操控连接重新认领时解除（重新进场）。文件锚语义不动（文件在=声明在）
    absent: HashSet<String>,
    /// 下线处置账本（#025 期五第一刀，dispose.rs 状态机；会话级不持久化）
    pub(crate) dispose: DisposeBook,
    /// 累计进场人次（颜色/出生点轮转计数）
    join_count: usize,
}

impl PlayerDirectory {
    /// 创建启用状态的玩家目录（main 启动用；目录不存在时 scan 自动创建）
    pub(crate) fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            enabled: true,
            online: HashMap::new(),
            ghosts: HashMap::new(),
            combat: HashMap::new(),
            failed: HashSet::new(),
            absent: HashSet::new(),
            dispose: DisposeBook::new(DEFAULT_DISPOSE_WAIT_TICKS),
            join_count: 0,
        }
    }

    /// 默认禁用状态（"./players" 但不扫描；EntitySandboxMap::new 用，
    /// 保证既有测试跑 tick 不碰文件系统）
    pub(crate) fn disabled() -> Self {
        Self {
            dir: PathBuf::from("./players"),
            enabled: false,
            online: HashMap::new(),
            ghosts: HashMap::new(),
            combat: HashMap::new(),
            failed: HashSet::new(),
            absent: HashSet::new(),
            dispose: DisposeBook::new(DEFAULT_DISPOSE_WAIT_TICKS),
            join_count: 0,
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 实体是否处于残影期（run_player_code 跳过 / deliver_attacks 跳过防守的依据）
    pub(crate) fn is_ghost(&self, id: &EntityId) -> bool {
        self.ghosts.contains_key(id)
    }

    /// 玩家名是否已在线
    pub(crate) fn is_online(&self, name: &str) -> bool {
        self.online.contains_key(name)
    }

    /// 玩家名的在场实体（D-021d 角色列表：online 条目查世界里活实体外观用）
    pub(crate) fn online_entity(&self, name: &str) -> Option<EntityId> {
        self.online.get(name).copied()
    }

    /// 在线玩家实体判定（id 键控）：免疫 code_hash 撞名——玩家可命名
    /// "creation.wat"/"attacker.wat" 进场，事件归属只认实体 id 不认代码
    /// 身份字符串（D-020a 双审 P0 修复：字符串判归属=身份碰撞绕穿可见性）
    pub(crate) fn is_online_entity(&self, id: &EntityId) -> bool {
        self.online.values().any(|v| v == id)
    }

    /// 玩家名是否在编译失败黑名单中
    pub(crate) fn is_failed(&self, name: &str) -> bool {
        self.failed.contains(name)
    }

    // === 会话级离场标记（#025 登出带走；与文件锚并存的两条下线路径之一）===

    /// 装配处置等待拍数（main 启动时从 CODEWORLD_DISPOSE_WAIT 配置）
    pub(crate) fn set_dispose_wait(&mut self, ticks: u64) {
        self.dispose.set_wait_ticks(ticks);
    }

    /// 名字是否处于会话级离场（登出带走后、重新认领前）
    pub(crate) fn is_absent(&self, name: &str) -> bool {
        self.absent.contains(name)
    }

    /// 标记会话级离场（take 落地时调用；目录扫描跳过该名字不自动重生）
    pub(crate) fn mark_absent(&mut self, name: &str) {
        self.absent.insert(name.to_string());
    }

    /// 解除会话级离场（操控连接重新认领=重新进场）；返回此前是否有标记
    pub(crate) fn clear_absent(&mut self, name: &str) -> bool {
        self.absent.remove(name)
    }

    /// 摘除在场登记（#025 会话级 take 专用）：不走残影（区别于 note_offline），
    /// 返回被摘除的实体 ID 供调用方清算。文件锚不动（文件在=声明在）
    pub(crate) fn remove_online(&mut self, name: &str) -> Option<EntityId> {
        let id = self.online.remove(name)?;
        tracing::debug!(name, entity_id = %id.0, "[players] 在场登记摘除（会话级离场）");
        Some(id)
    }

    /// 处置账本房间打扫：只留在场玩家的处置记录（#025，防无界增长）
    pub(crate) fn prune_dispose(&mut self) {
        self.dispose.prune(&self.online);
    }

    /// 扫描目录，返回 .wat 文件名列表（去扩展名，按文件名排序保证确定性）。
    /// 目录不存在时自动创建并返回空列表（空目录 = 无玩家进场）。
    pub(crate) fn scan_dir(&self) -> std::io::Result<Vec<String>> {
        std::fs::create_dir_all(&self.dir)?;
        let mut stems = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "wat").unwrap_or(false) {
                if let Some(stem) = path.file_stem() {
                    let name = stem.to_string_lossy().to_string();
                    if !name.is_empty() {
                        stems.push(name);
                    }
                }
            }
        }
        stems.sort();
        tracing::debug!(dir = %self.dir.display(), count = stems.len(), "[players] 目录扫描");
        Ok(stems)
    }

    /// 登记玩家上线（spawn 成功后调用）
    pub(crate) fn register_online(&mut self, name: &str, entity_id: EntityId) {
        tracing::debug!(name, entity_id = %entity_id.0, "[players] 玩家登记上线");
        self.online.insert(name.to_string(), entity_id);
    }

    /// 操控目标（#024 身份绑定，修正 #023 进场序语义）：me 声明的玩家名 -> 校验已进场在线
    /// -> (名字, 实体ID)。无声明/名字无效（拼错/未进场/已下线）返回 None = 观战模式。
    pub(crate) fn controlled_target(&self, me: Option<&str>) -> Option<(String, EntityId)> {
        let name = me?;
        let id = self.online.get(name).copied()?;
        Some((name.to_string(), id))
    }

    /// 是否有玩家在线（字幕 ctrl 段显示依据：有人但非你 -> ctrl:--；无人 -> 不显示段）
    pub(crate) fn has_online(&self) -> bool {
        !self.online.is_empty()
    }

    /// 登记编译失败（不再逐拍重试）
    pub(crate) fn mark_failed(&mut self, name: &str) {
        self.failed.insert(name.to_string());
    }

    /// 取下一个进场序号（颜色/出生点轮转用），内部计数自增
    pub(crate) fn next_join_index(&mut self) -> usize {
        let idx = self.join_count;
        self.join_count += 1;
        idx
    }

    /// 下线检测：online 中不在 stems 里的玩家名单
    pub(crate) fn offline_names(&self, stems: &[String]) -> Vec<String> {
        self.online
            .keys()
            .filter(|n| !stems.contains(n))
            .cloned()
            .collect()
    }

    /// 处理下线：从在线表摘除、按 #013 计算残影时长、登记残影到期拍。
    /// 返回 None 表示名字不在在线表（不该发生，防御）。
    pub(crate) fn note_offline(&mut self, name: &str, now: u64) -> Option<OfflineEvent> {
        let entity_id = self.online.remove(name)?;
        let last_combat = self.combat.get(&entity_id).copied();
        let in_combat = last_combat
            .map(|t| now.saturating_sub(t) < COMBAT_WINDOW_TICKS)
            .unwrap_or(false);
        let ghost_ticks = ghost_duration_ticks(last_combat, now);
        let expire_tick = now + ghost_ticks;
        self.ghosts.insert(entity_id, expire_tick);
        // 下线日志由编排层（main.rs step_online_anchor）消费事件打出
        Some(OfflineEvent {
            entity_id,
            ghost_ticks,
            in_combat,
        })
    }

    /// 残影到期：返回本拍应消散的实体 ID 列表并摘除记录（调用方标记死亡走清理通路）
    pub(crate) fn expire_due_ghosts(&mut self, now: u64) -> Vec<EntityId> {
        let due: Vec<EntityId> = self
            .ghosts
            .iter()
            .filter(|(_, &expire)| expire <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in &due {
            self.ghosts.remove(id);
            tracing::info!(entity_id = %id.0, "[players] 残影到期消散");
        }
        due
    }

    /// 单个实体的参战记录（#013：出刀即战斗，落空刀也记——堵"打空再拔线"）
    pub(crate) fn record_combat_id(&mut self, id: EntityId, tick: u64) {
        self.combat.insert(id, tick);
    }

    /// 房间打扫：清除已不在世界中的实体的残影/参战记录（防无界增长）
    pub(crate) fn prune(&mut self, entities: &HashMap<EntityId, codeworld_core::entity::Entity>) {
        self.ghosts.retain(|id, _| entities.contains_key(id));
        self.combat.retain(|id, _| entities.contains_key(id));
    }

    /// 黑名单解除：文件消失（被删除）的名字从黑名单移除（删了重放可重试）
    pub(crate) fn clear_failed_not_in(&mut self, stems: &[String]) {
        self.failed.retain(|n| stems.contains(n));
    }

    /// 单名解除黑名单（D-021d 选角）：显式选择=允许重试一次，失败会重新拉黑
    /// 不刷屏；与 clear_failed_not_in 正交（选角只动被选的那一个）
    pub(crate) fn clear_failed(&mut self, name: &str) {
        self.failed.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn eid(n: u128) -> EntityId {
        EntityId(Uuid::from_u128(n))
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cw-players-{}-{}-{}",
            tag,
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // === 纯函数：字形/颜色/出生点/残影时长 ===

    #[test]
    fn glyph_takes_first_char_uppercased() {
        // 规则：玩家名首字符大写 = 字形
        assert_eq!(glyph_for("example"), "E");
        assert_eq!(glyph_for("Alice"), "A");
        assert_eq!(glyph_for("豆豆"), "豆", "非 ASCII 首字符大写化后原样");
    }

    #[test]
    fn color_wheel_starts_green_and_wraps() {
        // 规则：A红/D青/H黄/S紫已占用，玩家从 green 起轮：green->magenta->blue->red 回绕
        assert_eq!(color_for(0), "green");
        assert_eq!(color_for(1), "magenta");
        assert_eq!(color_for(2), "blue");
        assert_eq!(color_for(3), "red", "轮转 7 步回绕到 red");
        assert_eq!(color_for(7), "green", "整轮回绕");
    }

    #[test]
    fn spawn_x_cycles_over_grid() {
        // 规则：出生点 x 在 [30,35,40,45] 循环，y 恒 0
        assert_eq!(spawn_x_for(0), 30.0);
        assert_eq!(spawn_x_for(1), 35.0);
        assert_eq!(spawn_x_for(2), 40.0);
        assert_eq!(spawn_x_for(3), 45.0);
        assert_eq!(spawn_x_for(4), 30.0, "循环回绕");
    }

    #[test]
    fn ghost_duration_plain_five_combat_extended() {
        // 规则（#013）：普通下线残影 5 拍；30 拍内参战 -> max(5, 30 - 距参战拍数)
        assert_eq!(ghost_duration_ticks(None, 100), 5, "无参战记录 -> 5 拍");
        assert_eq!(ghost_duration_ticks(Some(65), 100), 5, "参战已出窗(35>30) -> 5 拍");
        assert_eq!(ghost_duration_ticks(Some(70), 100), 5, "恰好 30 拍出窗 -> 5 拍");
        assert_eq!(ghost_duration_ticks(Some(100), 100), 30, "当拍参战当拍下线 -> 30 拍");
        assert_eq!(ghost_duration_ticks(Some(90), 100), 20, "10 拍前参战 -> 20 拍");
        assert_eq!(ghost_duration_ticks(Some(75), 100), 5, "25 拍前参战 -> max(5,5)=5");
        assert_eq!(ghost_duration_ticks(Some(74), 100), 5, "26 拍前参战 -> max(5,4)=5（地板 5）");
    }

    // === 状态机：在线/下线/残影 ===

    #[test]
    fn register_and_offline_plain_ghost_lifecycle() {
        // 状态机：进场登记 -> 删文件下线 -> 普通残影 5 拍 -> 到期消散
        let mut dir = PlayerDirectory::new("unused");
        let id = eid(1);
        dir.register_online("example", id);
        assert!(dir.is_online("example"));
        assert!(!dir.is_ghost(&id), "在线实体不是残影");

        let ev = dir.note_offline("example", 100).expect("在线玩家下线应产出事件");
        assert_eq!(ev.entity_id, id);
        assert_eq!(ev.ghost_ticks, 5, "无参战记录 -> 普通 5 拍");
        assert!(!ev.in_combat);
        assert!(!dir.is_online("example"), "下线后移出在线表");
        assert!(dir.is_ghost(&id), "下线后进残影");

        // 到期前不消散，到期拍消散
        assert!(dir.expire_due_ghosts(104).is_empty(), "第 104 拍未到期");
        assert!(dir.is_ghost(&id));
        let expired = dir.expire_due_ghosts(105);
        assert_eq!(expired, vec![id], "100+5=105 拍到期消散");
        assert!(!dir.is_ghost(&id), "消散后记录摘除");
    }

    #[test]
    fn note_offline_combat_ghost_extends_and_logs_flag() {
        // 规则（#013）：参战记录在册 -> 下线残影延长 + in_combat 标记
        let mut dir = PlayerDirectory::new("unused");
        let id = eid(2);
        dir.register_online("fighter", id);
        dir.record_combat_id(id, 90); // fighter 被打
        let ev = dir.note_offline("fighter", 100).expect("下线应产出事件");
        assert!(ev.in_combat, "10 拍前参战 = 战斗残影");
        assert_eq!(ev.ghost_ticks, 20, "max(5, 30-10)=20 拍");
        // 到期拍 = 100 + 20 = 120
        assert!(dir.expire_due_ghosts(119).is_empty());
        assert_eq!(dir.expire_due_ghosts(120), vec![id]);
    }

    #[test]
    fn note_offline_unknown_name_returns_none() {
        // 防御：不在在线表的名字下线 -> None（不 panic、不改状态）
        let mut dir = PlayerDirectory::new("unused");
        assert!(dir.note_offline("ghost-name", 0).is_none());
    }

    #[test]
    fn offline_names_detects_deleted_files() {
        // 规则：在线表与目录文件名的差集 = 下线名单
        let mut dir = PlayerDirectory::new("unused");
        dir.register_online("a", eid(1));
        dir.register_online("b", eid(2));
        let stems: Vec<String> = vec!["a".to_string(), "new".to_string()];
        assert_eq!(dir.offline_names(&stems), vec!["b".to_string()], "b 的文件已删");
        let full: Vec<String> = vec!["a".to_string(), "b".to_string()];
        assert!(dir.offline_names(&full).is_empty(), "文件都在则无人下线");
    }

    #[test]
    fn record_combat_keeps_latest_tick() {
        // 规则：多次参战取最近 tick（残影时长按最近一次算）
        let mut dir = PlayerDirectory::new("unused");
        let id = eid(3);
        dir.register_online("p", id);
        dir.record_combat_id(id, 10);
        dir.record_combat_id(id, 50);
        let ev = dir.note_offline("p", 60).expect("下线");
        assert_eq!(ev.ghost_ticks, 20, "按最近参战 50 算：30-(60-50)=20");
    }

    #[test]
    fn prune_removes_records_of_gone_entities() {
        // 房间打扫：实体已不在世界 -> 残影/参战记录清除（防无界增长）；
        // 仍在世界的记录保留
        let mut entities = HashMap::new();
        let alive = codeworld_core::entity::Entity::new(
            eid(10),
            codeworld_core::entity::PlayerId(Uuid::from_u128(1)),
            "x".to_string(),
            (0.0, 0.0),
            codeworld_core::entity::Appearance::new("@".to_string(), "red".to_string(), 1.0),
            0,
            1_000_000,
        );
        entities.insert(eid(10), alive);

        let mut dir = PlayerDirectory::new("unused");
        dir.register_online("gone", eid(11));
        let _ = dir.note_offline("gone", 0); // eid(11) 进残影
        dir.combat.insert(eid(11), 0);
        dir.combat.insert(eid(10), 5);
        dir.prune(&entities);
        assert!(!dir.is_ghost(&eid(11)), "已不在世界的实体残影记录应清除");
        assert!(!dir.combat.contains_key(&eid(11)), "已不在世界的实体参战记录应清除");
        assert!(dir.combat.contains_key(&eid(10)), "存活实体的参战记录应保留");
    }

    // === 目录扫描与黑名单 ===

    #[test]
    fn scan_dir_lists_wat_stems_sorted() {
        // 规则：只收 .wat，名字取文件名去扩展名，排序输出（确定性颜色/出生点分配）
        let dir_path = temp_dir("scan");
        std::fs::write(dir_path.join("b.wat"), "(module)").unwrap();
        std::fs::write(dir_path.join("a.wat"), "(module)").unwrap();
        std::fs::write(dir_path.join("notes.txt"), "not a player").unwrap();
        std::fs::write(dir_path.join("c.wat.bak"), "(module)").unwrap();
        std::fs::write(dir_path.join("_template.wat.txt"), "(module)").unwrap();
        std::fs::write(dir_path.join("me.txt"), "a").unwrap();
        let dir = PlayerDirectory::new(&dir_path);
        let stems = dir.scan_dir().unwrap();
        assert_eq!(
            stems,
            vec!["a".to_string(), "b".to_string()],
            "只收 .wat 且排序；.wat.txt 模板与 me.txt 不进场（#024）"
        );
        let _ = std::fs::remove_dir_all(&dir_path);
    }

    #[test]
    fn scan_dir_missing_dir_creates_and_returns_empty() {
        // 规则（A.5）：目录不存在自动创建，空目录 = 无玩家进场
        let dir_path = temp_dir("missing");
        let nested = dir_path.join("players");
        let dir = PlayerDirectory::new(&nested);
        let stems = dir.scan_dir().unwrap();
        assert!(stems.is_empty(), "空目录应无玩家");
        assert!(nested.is_dir(), "目录应被自动创建");
        let _ = std::fs::remove_dir_all(&dir_path);
    }

    #[test]
    fn failed_blacklist_blocks_retry_until_file_disappears() {
        // 规则（B.4）：编译失败的名字不再逐拍重试（不刷屏）；
        // 文件消失（删除）后黑名单解除，"删了再放"可重试
        let mut dir = PlayerDirectory::new("unused");
        dir.mark_failed("broken");
        assert!(dir.is_failed("broken"));
        let stems: Vec<String> = vec!["broken".to_string()];
        dir.clear_failed_not_in(&stems);
        assert!(dir.is_failed("broken"), "文件仍在 -> 黑名单维持");
        let empty: Vec<String> = vec![];
        dir.clear_failed_not_in(&empty);
        assert!(!dir.is_failed("broken"), "文件已删 -> 黑名单解除");
    }

    // === 开关语义 ===

    #[test]
    fn clear_failed_releases_single_name_only() {
        // 规则（D-021d 选角）：显式选择=单名解除黑名单（允许重试一次，失败会
        // 重新拉黑不刷屏）；黑名单里别的名字不受牵连（与 clear_failed_not_in
        // 的"按目录清单清理"正交——选角只动被选的那一个）
        let mut dir = PlayerDirectory::new("unused");
        dir.mark_failed("broken");
        dir.mark_failed("other");
        dir.clear_failed("broken");
        assert!(!dir.is_failed("broken"), "被选名字黑名单解除");
        assert!(dir.is_failed("other"), "别的名字黑名单不牵连");
    }

    #[test]
    fn online_entity_returns_registered_id_by_name() {
        // 规则（D-021d 角色列表）：名字 -> 在场实体 ID（在线表键控；未登记/
        // 已下线名字返回 None）——build_roster 据此查世界里活实体外观
        let mut dir = PlayerDirectory::new("unused");
        assert!(dir.online_entity("pioneer").is_none(), "未登记=None");
        dir.register_online("pioneer", eid(7));
        assert_eq!(dir.online_entity("pioneer"), Some(eid(7)), "登记后返回实体 ID");
        let _ = dir.note_offline("pioneer", 0);
        assert!(dir.online_entity("pioneer").is_none(), "下线后摘除");
    }

    #[test]
    fn disabled_directory_is_not_enabled() {
        // 规则：默认禁用（既有测试跑 tick 零文件系统副作用）；new() 为启用
        assert!(!PlayerDirectory::disabled().is_enabled());
        assert!(PlayerDirectory::new("any").is_enabled());
    }

    // === 操控目标（#024 身份绑定：me 声明 -> 校验在线，非进场序）===

    #[test]
    fn controlled_target_follows_me_declaration_not_join_order() {
        // 规则（#024，修正 #023）：操控目标 = me 声明的名字，与进场顺序无关；
        // 两个玩家在册时声明后进场者 -> 操控后进场者
        let mut dir = PlayerDirectory::new("unused");
        assert!(
            dir.controlled_target(None).is_none(),
            "无声明 = 无操控目标（观战模式）"
        );
        dir.register_online("pioneer", eid(1));
        dir.register_online("second", eid(2));
        let (name, id) = dir
            .controlled_target(Some("second"))
            .expect("声明的名字在线应有操控目标");
        assert_eq!(name, "second");
        assert_eq!(id, eid(2), "操控目标=声明的 second，不是先进场的 pioneer");
    }

    #[test]
    fn controlled_target_none_when_declared_name_not_online() {
        // 名字无效（拼错/未进场/已下线）-> None（观战模式；warn 由调用方负责）
        let mut dir = PlayerDirectory::new("unused");
        dir.register_online("pioneer", eid(1));
        assert!(
            dir.controlled_target(Some("pioner")).is_none(),
            "拼错的名字不对应任何在线玩家"
        );
        let _ = dir.note_offline("pioneer", 0);
        assert!(
            dir.controlled_target(Some("pioneer")).is_none(),
            "已下线的名字无效（观战）"
        );
    }

    // === me 声明解析（#024：CODEWORLD_ME 环境变量优先 -> players/me.txt -> 未声明）===

    #[test]
    fn me_env_beats_file() {
        // env 有值（trim 后非空）-> 优先于 me.txt，文件内容被忽略
        assert_eq!(
            read_me_declaration(Some("beta"), Path::new("unused")).as_deref(),
            Some("beta")
        );
    }

    #[test]
    fn me_env_empty_falls_to_file() {
        // env 空串/纯空白 = 未通过 env 声明 -> 回落 me.txt
        assert_eq!(
            read_me_declaration(Some("  "), Path::new("unused")).as_deref(),
            None,
            "env 空且无 me.txt -> 未声明"
        );
    }

    #[test]
    fn me_file_trimmed_empty_or_missing_is_none() {
        // me.txt 内容 trim 后生效；空白文件/文件缺失 = 未声明
        let dir_path = temp_dir("me-file");
        assert!(
            read_me_declaration(None, &dir_path).is_none(),
            "无 me.txt = 未声明"
        );
        std::fs::write(dir_path.join("me.txt"), "  pioneer \n").unwrap();
        assert_eq!(
            read_me_declaration(None, &dir_path).as_deref(),
            Some("pioneer"),
            "文件内容 trim 后生效"
        );
        std::fs::write(dir_path.join("me.txt"), "   \n").unwrap();
        assert!(
            read_me_declaration(None, &dir_path).is_none(),
            "空白 me.txt = 未声明"
        );
        let _ = std::fs::remove_dir_all(&dir_path);
    }
}
