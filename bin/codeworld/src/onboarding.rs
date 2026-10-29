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
pub(crate) const TEMPLATES: [NewbieTemplate; 3] = [
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
    /// 人话提示（前端兜底文案；正常走前端本地同款校验，此为服务器侧回执）
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::Empty => "名字不能为空",
            Self::TooLong => "名字太长（最多 24 个字符）",
            Self::BadChar => "名字只能用字母、数字、中文、_ 和 -",
        }
    }
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

/// 首建判定：玩家目录里没有任何 .wat（模板文件 _ 前缀/.txt 不算）——
/// "这是本局第一个角色"= 归档旧档的触发口径（复用 scan 的加载口径，
/// 不引入第二套"什么算玩家文件"定义）
pub(crate) fn players_dir_is_fresh(players_dir: &Path) -> bool {
    let mut has_wat = false;
    if let Ok(entries) = std::fs::read_dir(players_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "wat").unwrap_or(false) {
                has_wat = true;
                break;
            }
        }
    }
    !has_wat
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!TEMPLATES.iter().any(|t| t.id == "dragon"));
    }

    #[test]
    fn fresh_dir_detection_ignores_template_files() {
        let dir = std::env::temp_dir().join(format!("cw-onboard-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 模板文件（_ 前缀 wat / .txt）不算玩家——fresh 仍真
        std::fs::write(dir.join("_template.wat.txt"), "x").unwrap();
        std::fs::write(dir.join("README.md"), "x").unwrap();
        assert!(players_dir_is_fresh(&dir), "只有模板/说明=首建");
        std::fs::write(dir.join("pioneer.wat"), "x").unwrap();
        assert!(!players_dir_is_fresh(&dir), "出现 .wat=非首建");
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
}
