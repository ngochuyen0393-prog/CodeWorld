//! 进场仪式（D-021b，t_be0de36c）：浏览器"创建你的角色"面板的服务端半边。
//!
//! - 新手模板三选（空白木桩/吃草彷徨者/小猎手）：官方示例内容**以玩家身份进场**
//!   的最小实现（#011 合理解法——模板=玩家自己代码的起点，非世界预设生物；
//!   模板 WAT 存 static/templates/ 全文可抄可改，GET /templates/*.wat 同源伺服）
//! - create_player 上行（net-protocol v2.6）：名字+模板 → 写 players/<name>.wat
//!   → 复用门一目录轮询机当拍进场（零新进场路径）
//! - 起名只约束文件名安全（本机单机阶段；不做唯一性服务器注册——重名=已存在
//!   结构化拒绝，前端红字提示换名）
//! - 旧档归档：恢复快照把上一局世界带回来时，首次 create_player 自动把
//!   codeworld.db 挪去 archive/（世界干净开局=新玩家仪式的一部分；归档可找回）
//!
//! 本模块只放纯逻辑与文件动作；意图落地接线在 main.rs（需访问 sandbox_map/world）。

use std::path::{Path, PathBuf};

use codeworld_core::world::WorldState;

use crate::net::{RosterEntry, ROSTER_OFFLINE_COLOR};
use crate::players::{glyph_for, PlayerDirectory};

/// 新手模板定义（静态表；WAT 源在 static/templates/<file>，启动读入装配）。
/// 面板显示名/副标题/预览字形在前端 index.html 静态写死（渲染是浏览器的家）；
/// 服务器侧只需 id 与文件名的映射
pub(crate) struct NewbieTemplate {
    /// 协议模板 id（上行 template 字段值；稳定字符串勿改——前端/协议锚）
    pub id: &'static str,
    /// 模板文件名（static/templates/ 下）
    pub file: &'static str,
}

/// 模板静态表（面板顺序即此表顺序；id 与前端模板卡一一对应）
pub(crate) const TEMPLATES: [NewbieTemplate; 4] = [
    NewbieTemplate {
        id: "blank",
        file: "blank.wat",
    },
    NewbieTemplate {
        id: "grazer",
        file: "grazer.wat",
    },
    NewbieTemplate {
        id: "hunter",
        file: "hunter.wat",
    },
    // D-022 造物工坊新增箭塔骨架（scan+贴身刀+on_attack 盾拦）；create_player
    // 模板选择同步可取（无害——create 与工坊共用同一张模板表）
    NewbieTemplate {
        id: "tower",
        file: "tower.wat",
    },
];

/// 模板目录名（STATIC_DIR 下；伺服路径 GET /templates/*.wat 与装配同源）
pub(crate) const TEMPLATES_DIR_NAME: &str = "templates";

/// 起名长度上限（字符数；与前端 maxlength 同步——协议两端的同一道门）
pub(crate) const NAME_MAX_CHARS: usize = 24;

/// 名字清理/校验（纯函数，net.rs 解析层与本模块落地共用同一实现）：
/// - trim 首尾空白
/// - 长度 1..=24 字符（字符数非字节数，中文名友好）
/// - 文件名安全字符集：字母/数字/下划线/连字符/中文等非 ASCII 字母
///   （拒绝路径分隔符、点号（防 ../ 与扩展名注入）、控制字符、空白）
/// - 返回 Err(原因码) 供结构化拒绝（wire 值见 reason_wire）
pub(crate) fn sanitize_name(raw: &str) -> Result<String, NameReject> {
    let name = raw.trim();
    let chars: Vec<char> = name.chars().collect();
    if chars.is_empty() {
        return Err(NameReject::Empty);
    }
    if chars.len() > NAME_MAX_CHARS {
        return Err(NameReject::TooLong);
    }
    for &c in &chars {
        let ok = c.is_ascii_alphanumeric()
            || c == '_'
            || c == '-'
            || (c.is_alphabetic() && !c.is_ascii());
        if !ok {
            return Err(NameReject::BadChar);
        }
    }
    Ok(name.to_string())
}

/// 起名拒绝原因（wire 值进 create_player_result 事件 + api.md 码表）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameReject {
    Empty,
    TooLong,
    BadChar,
}

impl NameReject {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Empty => "NAME_EMPTY",
            Self::TooLong => "NAME_TOO_LONG",
            Self::BadChar => "NAME_BAD_CHAR",
        }
    }
    // 人话提示在前端 CREATE_REJECT_TEXT（api.md 码表同源）；服务器侧回执只发
    // wire 码——R1 改法 B 后解析层不再调用本地人话（原 text() 成死代码已删）
}

/// 旧档归档（D-021b 验收补充）：把 db 挪去 <db 同目录>/archive/<原名>.<时间戳>.db。
/// 归档而非删除（世界历史可找回）；失败不阻断创建流程（老档留着只影响下次
/// 重启，玩家本局照常）——失败原因日志留痕。返回 Some(归档路径)=已归档。
pub(crate) fn archive_old_world(db_path: &str) -> Option<PathBuf> {
    let db = Path::new(db_path);
    if !db.exists() {
        return None;
    }
    let dir = db.parent()?;
    let file_name = db.file_name()?.to_string_lossy().to_string();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let archive_dir = dir.join("archive");
    if let Err(e) = std::fs::create_dir_all(&archive_dir) {
        tracing::warn!(error = %e, dir = %archive_dir.display(), "[onboarding] 归档目录创建失败（旧档保留，世界重启会恢复上一局）");
        return None;
    }
    let dest = archive_dir.join(format!("{file_name}.{stamp}.db"));
    match std::fs::rename(db, &dest) {
        Ok(()) => {
            // sqlite sidecar 一并挪走（journal/wal/shm：崩溃残留的旧锁文件）
            for ext in ["-journal", "-wal", "-shm"] {
                let side = PathBuf::from(format!("{db_path}{ext}"));
                if side.exists() {
                    let side_dest = archive_dir.join(format!(
                        "{file_name}.{stamp}.db{ext}"
                    ));
                    let _ = std::fs::rename(&side, &side_dest);
                }
            }
            tracing::info!(
                from = %db.display(),
                to = %dest.display(),
                "[onboarding] 旧世界已归档（新玩家进场仪式：干净开局；归档可找回）"
            );
            Some(dest)
        }
        Err(e) => {
            tracing::warn!(error = %e, "[onboarding] 旧世界归档失败（文件被锁？旧档保留，不影响本局）");
            None
        }
    }
}

/// 本机角色档案文件名（players/ 下隐藏文件，UTF-8 一行一名）。
/// D-021e 角色列表私有化：登录页只列"我曾创建/使用过的角色"——players/ 里的
/// 开发测试生物（pioneer/bridge-demo 等）不进档案就不列。档案是本机单机阶段的
/// 概念（多人时代"我的角色"走账号，届时换轨）；身份权威仍是 players/<名>.wat
/// 文件锚，档案只服务登录页体验。
pub(crate) const MY_CHARACTERS_FILE: &str = ".my-characters";

/// 读本机角色档案（内部）：一行一名，trim 每行、跳过空行；文件不存在=空表
fn read_my_characters(players_dir: &Path) -> std::io::Result<Vec<String>> {
    let text = match std::fs::read_to_string(players_dir.join(MY_CHARACTERS_FILE)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// 读本机角色档案（roster 过滤用）：读失败=warn+空表（宁可空也不泄露他人角色）
pub(crate) fn my_characters(players_dir: &Path) -> Vec<String> {
    match read_my_characters(players_dir) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %players_dir.join(MY_CHARACTERS_FILE).display(),
                "[onboarding] 本机角色档案读取失败（按空表处理）"
            );
            Vec::new()
        }
    }
}

/// 首建判定：档案为空 = 本机第一个角色 = 归档旧世界的触发口径。
/// 档案不存在/可读但无有效行 = 空白；**读失败 ≠ 空白**——档案被锁时宁可不归档
/// 也不错归档（错归档=老玩家重启世界被挪走，而读失败恰是 Windows 锁文件的
/// 常见姿势，与本文件 archive_old_world 的 os error 32 同族）
pub(crate) fn my_characters_blank(players_dir: &Path) -> bool {
    if !players_dir.join(MY_CHARACTERS_FILE).exists() {
        return true;
    }
    matches!(read_my_characters(players_dir), Ok(v) if v.is_empty())
}

/// 追加本机角色档案（create/select 成功路径调用："我曾创建/使用过的角色"）。
/// 重复名字不重复追加（追加前查含）；写失败只 warn 不阻断进场——档案是登录页
/// 体验，身份权威仍是 players/<名>.wat 文件锚
pub(crate) fn append_my_character(players_dir: &Path, name: &str) {
    match read_my_characters(players_dir) {
        Ok(v) if v.iter().any(|n| n == name) => return,
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "[onboarding] 档案读取失败（跳过去重直接追加）");
        }
    }
    let path = players_dir.join(MY_CHARACTERS_FILE);
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(format!("{name}\n").as_bytes())
        });
    if let Err(e) = res {
        tracing::warn!(
            error = %e,
            path = %path.display(),
            "[onboarding] 本机角色档案追加失败（不阻断进场）"
        );
    }
}

/// 旧角色归档（D-021e 首建配套）：players/*.wat 挪 players/archive/players-<stamp>/
///（归档而非删除，可找回）。me.txt / .my-characters / 非 .wat 文件一律不挪。
/// 必须*与 db 归档同窗*执行：世界重开后 load_players_initial + 每拍目录轮询会把
/// players/ 里所有 .wat 自动拉进场——留原地=干净世界被旧角色污染。返回挪动数量。
pub(crate) fn archive_player_wats(players_dir: &Path) -> usize {
    let wats: Vec<PathBuf> = match std::fs::read_dir(players_dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "wat").unwrap_or(false))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, dir = %players_dir.display(), "[onboarding] 旧角色归档：目录读失败");
            return 0;
        }
    };
    if wats.is_empty() {
        return 0;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest_dir = players_dir.join("archive").join(format!("players-{stamp}"));
    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        tracing::warn!(error = %e, dir = %dest_dir.display(), "[onboarding] 旧角色归档目录创建失败（旧角色留在原地）");
        return 0;
    }
    let mut moved = 0usize;
    for src in &wats {
        let Some(file_name) = src.file_name() else { continue };
        match std::fs::rename(src, dest_dir.join(file_name)) {
            Ok(()) => moved += 1,
            Err(e) => tracing::warn!(
                error = %e,
                from = %src.display(),
                "[onboarding] 旧角色归档失败（该文件留在原地）"
            ),
        }
    }
    tracing::info!(
        count = moved,
        to = %dest_dir.display(),
        "[onboarding] 旧角色已归档（干净世界开局；归档可找回）"
    );
    moved
}

/// 模板书：启动读入的模板源（id -> WAT 全文；缺文件=该模板禁用，warn 不拖垮世界）
pub(crate) struct TemplateBook {
    sources: Vec<(&'static str, Option<String>)>,
}

impl TemplateBook {
    /// 从 static/templates/ 读入全部模板（读不到的模板禁用并 WARN）
    pub(crate) fn load(dir: &Path) -> Self {
        let mut sources = Vec::with_capacity(TEMPLATES.len());
        for def in TEMPLATES.iter() {
            let path = dir.join(def.file);
            let src = match std::fs::read_to_string(&path) {
                Ok(text) => match wat::parse_str(&text) {
                    Ok(_) => {
                        tracing::info!(id = def.id, "[onboarding] 新手模板装配完成（{}）", def.file);
                        Some(text)
                    }
                    Err(e) => {
                        tracing::warn!(id = def.id, file = def.file, errMsg = %e, "[onboarding] 模板编译失败，禁用");
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(id = def.id, path = %path.display(), errMsg = %e, "[onboarding] 模板文件缺失，禁用");
                    None
                }
            };
            sources.push((def.id, src));
        }
        Self { sources }
    }

    /// 模板源（id 合法且模板已装配才 Some）
    pub(crate) fn source(&self, id: &str) -> Option<&str> {
        self.sources
            .iter()
            .find(|(sid, _)| *sid == id)
            .and_then(|(_, s)| s.as_deref())
    }
}

/// 便捷：从静态页根目录装配模板书（main 启动用；skills.rs 同款）
pub(crate) fn load_templates_from_static(static_dir: &Path) -> TemplateBook {
    TemplateBook::load(&static_dir.join(TEMPLATES_DIR_NAME))
}

/// 角色列表组装（D-021d 登录/选角页；D-021e 私有化）：本机角色档案
///（players/.my-characters）登记 ∩ players/*.wat 实存，投影为下行条目
///（纯函数，main 每拍刷新写共享槽——players 目录小，1Hz 全量重建白菜价）。
/// - 档案过滤：登录页只列"我曾创建/使用过的角色"——开发测试生物（pioneer/
///   bridge-demo 等）不进档案就不列；档案不存在/读失败 = 空列表（宁可空也
///   不泄露他人角色）
/// - online 条目：在线表 name→EntityId→世界里活实体查实际 glyph/color
/// - 离线条目：glyph=名字首字符大写（既有 glyph_for），color=中性灰
///   （颜色依赖出生序号，离线时不可知，灰=诚实）
/// - 排序：scan_dir 已按名排序，保持确定性；目录不可读=空列表（不拖垮世界）
pub(crate) fn build_roster(
    players: &PlayerDirectory,
    world: &WorldState,
) -> Vec<RosterEntry> {
    let mine = my_characters(players.dir());
    if mine.is_empty() {
        return Vec::new();
    }
    let stems = match players.scan_dir() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "[onboarding] 角色列表目录扫描失败（本轮发空列表）");
            return Vec::new();
        }
    };
    stems
        .into_iter()
        .filter(|name| mine.iter().any(|m| m == name))
        .map(|name| match players
            .online_entity(&name)
            .and_then(|id| world.entities.get(&id))
        {
            Some(e) => RosterEntry {
                name,
                glyph: e.appearance.glyph.clone(),
                color: e.appearance.color.clone(),
                online: true,
            },
            None => RosterEntry {
                glyph: glyph_for(&name),
                color: ROSTER_OFFLINE_COLOR.to_string(),
                name,
                online: false,
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeworld_core::entity::{Appearance, Entity, EntityId, PlayerId};

    #[test]
    fn sanitize_accepts_normal_names() {
        assert_eq!(sanitize_name("pioneer"), Ok("pioneer".to_string()));
        assert_eq!(sanitize_name("  pioneer \n"), Ok("pioneer".to_string()));
        assert_eq!(sanitize_name("土豆"), Ok("土豆".to_string()));
        assert_eq!(sanitize_name("Player_1-x"), Ok("Player_1-x".to_string()));
    }

    #[test]
    fn sanitize_rejects_dangerous_names() {
        // 空/纯空白
        assert_eq!(sanitize_name(""), Err(NameReject::Empty));
        assert_eq!(sanitize_name("   "), Err(NameReject::Empty));
        // 路径注入三件套与扩展名
        assert_eq!(sanitize_name("a/b"), Err(NameReject::BadChar));
        assert_eq!(sanitize_name("a\\b"), Err(NameReject::BadChar));
        assert_eq!(sanitize_name(".."), Err(NameReject::BadChar));
        assert_eq!(sanitize_name("a.wat"), Err(NameReject::BadChar));
        assert_eq!(sanitize_name("a b"), Err(NameReject::BadChar));
        // 超长（25 字符）
        assert_eq!(sanitize_name(&"a".repeat(25)), Err(NameReject::TooLong));
        // 24 字符恰好放行
        assert_eq!(sanitize_name(&"a".repeat(24)), Ok("a".repeat(24)));
    }

    #[test]
    fn template_ids_resolve() {
        assert!(TEMPLATES.iter().any(|t| t.id == "blank"));
        assert!(TEMPLATES.iter().any(|t| t.id == "grazer"));
        assert!(TEMPLATES.iter().any(|t| t.id == "hunter"));
        assert!(TEMPLATES.iter().any(|t| t.id == "tower"), "D-022 箭塔模板在册");
        assert!(!TEMPLATES.iter().any(|t| t.id == "dragon"));
    }

    #[test]
    fn tower_template_loads_and_parses_from_static() {
        // D-022 工坊 tower 模板：static/templates/tower.wat 真装配（文件在+编译过
        // ——与 main.rs onboard 测试同款 cwd=bin/codeworld 口径；坏模板会被
        // TemplateBook::load 禁用为 None）
        let book = load_templates_from_static(Path::new("static"));
        let src = book
            .source("tower")
            .expect("tower 模板应装配成功（文件缺失/WAT 编译失败都会禁用）");
        assert!(src.contains("attack_at"), "箭塔骨架含贴身出刀");
        assert!(src.contains("on_attack"), "箭塔骨架含拦截位");
        assert!(src.contains("scan"), "箭塔骨架含扫描");
    }

    #[test]
    fn my_characters_blank_semantics_decides_first_build() {
        // 首建判定（D-021e）：档案不存在/可读但无有效行 = 空白（首建）；
        // 有有效行 = 非空白；**读失败 ≠ 空白**（档案被锁时宁可不归档也不错归档
        // ——错归档=老玩家重启世界被挪走，而读失败恰是 Windows 锁文件的常见姿势）
        let dir = archive_dir("blank");
        assert!(my_characters_blank(&dir), "档案不存在=首建");
        std::fs::write(dir.join(MY_CHARACTERS_FILE), "").unwrap();
        assert!(my_characters_blank(&dir), "空文件=首建");
        std::fs::write(dir.join(MY_CHARACTERS_FILE), "\n   \n").unwrap();
        assert!(my_characters_blank(&dir), "纯空行=首建");
        std::fs::write(dir.join(MY_CHARACTERS_FILE), "a\n").unwrap();
        assert!(!my_characters_blank(&dir), "有角色=非首建");
        std::fs::remove_dir_all(&dir).ok();
        // 读失败模拟：.my-characters 是目录（read_to_string 必失败）
        std::fs::create_dir_all(dir.join(MY_CHARACTERS_FILE)).unwrap();
        assert!(!my_characters_blank(&dir), "读失败≠空白（不归档）");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn my_characters_reads_lines_trims_and_skips_empty() {
        // 档案格式：UTF-8 一行一名，trim 每行、跳过空行；档案不存在=空表
        let dir = archive_dir("read");
        assert!(my_characters(&dir).is_empty(), "档案不存在=空表");
        std::fs::write(dir.join(MY_CHARACTERS_FILE), "a\n\n b \n土豆\n").unwrap();
        assert_eq!(
            my_characters(&dir),
            vec!["a".to_string(), "b".to_string(), "土豆".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_my_character_dedupes_by_name() {
        // 追加幂等：重复名字不重复追加（追加前查含）
        let dir = archive_dir("append");
        append_my_character(&dir, "a");
        append_my_character(&dir, "b");
        append_my_character(&dir, "a");
        assert_eq!(
            my_characters(&dir),
            vec!["a".to_string(), "b".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_player_wats_moves_wat_files_only() {
        // 归档口径（D-021e 首建配套）：players/*.wat 挪 players/archive/players-<stamp>/
        // 可找回；me.txt / .my-characters / 非 .wat 文件（模板 .wat.txt）一律不挪
        let dir = archive_dir("wats");
        std::fs::write(dir.join("a.wat"), "(module)").unwrap();
        std::fs::write(dir.join("b.wat"), "(module)").unwrap();
        std::fs::write(dir.join("me.txt"), "a\n").unwrap();
        std::fs::write(dir.join(MY_CHARACTERS_FILE), "a\n").unwrap();
        std::fs::write(dir.join("_t.wat.txt"), "x").unwrap();
        let moved = archive_player_wats(&dir);
        assert_eq!(moved, 2, "只挪两个 .wat");
        assert!(
            !dir.join("a.wat").exists() && !dir.join("b.wat").exists(),
            "顶层 .wat 已挪走"
        );
        assert!(dir.join("me.txt").exists(), "me 声明不挪");
        assert!(dir.join(MY_CHARACTERS_FILE).exists(), "本机档案不挪");
        assert!(dir.join("_t.wat.txt").exists(), "非 .wat 文件不挪");
        let archive_root = dir.join("archive");
        let stamps: Vec<PathBuf> = std::fs::read_dir(&archive_root)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .map(|e| e.path())
            .collect();
        assert_eq!(stamps.len(), 1, "一个时间戳归档目录");
        assert!(
            stamps[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("players-"),
            "目录名 players-<stamp> 形态"
        );
        assert!(
            stamps[0].join("a.wat").exists() && stamps[0].join("b.wat").exists(),
            "两个 .wat 落归档目录"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_moves_db_and_sidecars() {
        let dir = std::env::temp_dir().join(format!("cw-onboard-arch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("codeworld.db");
        std::fs::write(&db, b"old-world").unwrap();
        std::fs::write(dir.join("codeworld.db-wal"), b"wal").unwrap();
        let db_str = db.to_string_lossy().to_string();
        let dest = archive_old_world(&db_str).expect("应归档成功");
        assert!(!db.exists(), "原 db 已挪走");
        assert!(dest.exists(), "归档文件存在");
        assert!(dest.to_string_lossy().contains("archive"), "落 archive/ 目录");
        assert!(
            !dir.join("codeworld.db-wal").exists(),
            "wal sidecar 一并挪走"
        );
        // 不存在的 db：no-op
        assert!(archive_old_world(&db_str).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    // === D-021d 登录/选角页：角色列表（player_roster 下行数据源）===

    /// roster 测试台：临时玩家目录 + .wat 文件清单（与 players.rs temp_dir 同款）
    fn roster_dir(tag: &str, names: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cw-onboard-roster-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for n in names {
            std::fs::write(d.join(format!("{n}.wat")), "(module)").unwrap();
        }
        d
    }

    /// 档案单测台：空临时目录（my_characters/append/wats 单测共用）
    fn archive_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cw-onboard-mine-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 往临时玩家目录写本机角色档案（一行一名）
    fn write_archive(dir: &Path, names: &[&str]) {
        std::fs::write(
            dir.join(MY_CHARACTERS_FILE),
            names.iter().map(|n| format!("{n}\n")).collect::<String>(),
        )
        .unwrap();
    }

    #[test]
    fn build_roster_lists_only_my_characters_sorted() {
        // 规则（D-021e 角色列表私有化）：角色列表 = 本机角色档案登记 ∩ players/*.wat
        // 实存——开发测试生物（c.wat）不在档案就不列；顺序仍按 scan_dir 字母序
        //（确定性）；非 .wat 文件不列
        let dir = roster_dir("sorted", &["b", "a", "c"]);
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        std::fs::write(dir.join("_tpl.wat.txt"), "x").unwrap();
        write_archive(&dir, &["b", "a"]);
        let players = PlayerDirectory::new(&dir);
        let world = WorldState::new();
        let roster = build_roster(&players, &world);
        let names: Vec<&str> = roster.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"], "只列档案内角色且按名字排序");
        assert!(roster.iter().all(|e| !e.online), "无在线登记=全部离线");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_roster_without_archive_is_empty_even_with_wats() {
        // 规则：档案不存在/为空 = 空列表（读失败同款 warn+空）——宁可空也不泄露
        // 他人角色；目录里有再多 .wat 也不列
        let dir = roster_dir("noarchive", &["a", "b"]);
        let players = PlayerDirectory::new(&dir);
        let world = WorldState::new();
        assert!(build_roster(&players, &world).is_empty(), "无档案=空列表");
        write_archive(&dir, &[]);
        assert!(build_roster(&players, &world).is_empty(), "空档案=空列表");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_roster_online_entries_carry_live_appearance() {
        // 规则：online=true 条目 glyph/color 取世界里活实体实际值（在线表 name
        // → EntityId → world.entities 查 appearance）；online=false（离线/被带走/
        // 残影）：glyph=glyph_for(name)（首字符大写），color=中性灰 #8b949e
        //（颜色依赖出生序号，离线时不可知，灰=诚实）
        let dir = roster_dir("appear", &["a", "土豆"]);
        write_archive(&dir, &["a", "土豆"]);
        let mut players = PlayerDirectory::new(&dir);
        let mut world = WorldState::new();
        let id = EntityId::new();
        world.entities.insert(
            id,
            Entity::new(
                id,
                PlayerId(id.0),
                "土豆".to_string(),
                (30.0, 0.5),
                Appearance::new("土".to_string(), "green".to_string(), 1.0),
                0,
                1000,
            ),
        );
        players.register_online("土豆", id);
        let roster = build_roster(&players, &world);
        let on = roster.iter().find(|e| e.name == "土豆").expect("在线条目在列");
        assert!(on.online, "在场登记=online");
        assert_eq!(on.glyph, "土", "glyph=活实体实际值");
        assert_eq!(on.color, "green", "color=活实体实际值");
        let off = roster.iter().find(|e| e.name == "a").expect("离线条目也在列");
        assert!(!off.online);
        assert_eq!(off.glyph, "A", "离线 glyph=名字首字符大写");
        assert_eq!(off.color, "#8b949e", "离线 color=中性灰（出生序号不可知）");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_roster_online_without_world_entity_falls_back_gray() {
        // 防御：在线表登记了但世界里查不到实体（异常时序）——按离线灰兜底，
        // 不 panic 不发空条目
        let dir = roster_dir("ghost", &["lonely"]);
        write_archive(&dir, &["lonely"]);
        let mut players = PlayerDirectory::new(&dir);
        let world = WorldState::new();
        players.register_online("lonely", EntityId::new());
        let roster = build_roster(&players, &world);
        assert_eq!(roster.len(), 1);
        assert!(!roster[0].online, "查不到实体按离线兜底");
        assert_eq!(roster[0].color, "#8b949e");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_roster_skips_archive_names_without_wat() {
        // 规则：档案里登记但 .wat 已不在的角色自然不列（先 scan_dir 再按档案交集
        // ——档案是"我曾用过"的流水账，.wat 才是角色实存）
        let dir = roster_dir("gone", &["a"]);
        write_archive(&dir, &["gone", "a"]);
        let players = PlayerDirectory::new(&dir);
        let world = WorldState::new();
        let roster = build_roster(&players, &world);
        let names: Vec<&str> = roster.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a"], "档案有但 .wat 不在的角色不列");
        std::fs::remove_dir_all(&dir).ok();
    }
}
