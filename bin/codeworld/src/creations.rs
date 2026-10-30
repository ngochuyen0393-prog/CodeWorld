//! 造物文件层（D-023）：creations/ 独立文件 + @carry 引用清单 + 技能账本建账。
//!
//! 文件即持久化（与 players/ 同哲学）：文件存在=库存声明在，重启重扫重建。
//! 三层结构：
//! - 文件层：creations/<名>.wat = §头 + spawn 行（15 数）+ WAT 代码体
//! - 库存层：players/<名>.wat `;; @carry <造物名>` 行序列=该角色携带哪些造物
//! - 状态层：待命/在场 N 拍/已消亡——前端从 @carry+快照合成（协议零新帧）
//!
//! 严格哲学（与"出生重叠拒绝+日志"同款）：头缺失/坏行/超 64KB=整文件作废
//! （WARN 日志+跳过，不崩世界）。

use std::path::Path;

use crate::skills::SkillTemplate;

/// 单文件大小上限（任务书定值 64KB；metadata 先量后读，与 player-code 读路径对称）
pub(crate) const CREATION_FILE_MAX_BYTES: usize = 64 * 1024;

/// 玩家沙箱 fuel 配额（spawn_player 建账与技能补账同源——账本里的 fuel_limit
/// 只是重放兜底，实际重放用操控实体沙箱现配额）
pub(crate) const PLAYER_SANDBOX_FUEL: u64 = 1_000_000;

/// 造物文件解析失败（整文件作废，WARN+跳过；wire 码见 `wire`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CreationParseError {
    /// 首行不是 `;; §<名>`（缺 § 头）
    MissingHeader,
    /// 头区里没有 `;; spawn` 行（缺 spawn 行）
    MissingSpawnLine,
    /// spawn 行数字烂（个数≠15/非数字/非有限/寿命<1）
    BadSpawnLine,
    /// 空代码体（编不过的白纸）
    MissingBody,
}

impl CreationParseError {
    /// wire 码（HTTP 拒绝回执；前端 SAVE_REJECT_TEXT 同源补文案）
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::MissingHeader => "BAD_HEADER",
            Self::MissingSpawnLine | Self::BadSpawnLine => "BAD_SPAWN_LINE",
            Self::MissingBody => "EMPTY_BODY",
        }
    }
}

/// spawn 行 15 数（spawn 第 7..23 参原序）。全部经 codeworld_core 既有钳制律收编
/// ——单一法源：import 层怎么钳，文件解析就怎么钳（技能重放直构 PendingSpawn
/// 不走 import 层，钳制必须在解析层补齐，防超域档案绕过）
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreationSpawnLine {
    pub radius: f64,
    pub mass: f64,
    pub binding: i32,
    pub motion: i32,
    pub vx: f64,
    pub vy: f64,
    pub field_radius: f64,
    pub field_gravity_scale: f64,
    pub field_drag_delta: f64,
    pub liquid_density: f64,
    pub liquid_drag: f64,
    pub link: i32,
    pub restitution: f64,
    pub friction: f64,
    pub lifespan_ticks: u64,
}

/// 造物文件（解析成功产物）。名字以文件名为锚（§ 名只作展示对账，不一致 WARN）
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CreationFile {
    /// § 名（文件内容声明）
    pub name: String,
    /// spawn 行 15 数
    pub spawn: CreationSpawnLine,
    /// WAT 代码体（头区空行之后的全部行原样；wat 解析器本就忽略 ;; 注释）
    pub code: String,
}

/// 造物文件全文解析（纯函数，文件 IO 归 read_creation；max_speed=世界极速配置
/// 穿参——初速钳制律随世界配置走，不写死默认值）：
/// 1. 首行必须 `;; §<名>`（缺=MissingHeader）
/// 2. 头区（§ 行到首个非注释行/空行之间）必须出现 `;; spawn <15 数>`（缺/坏=作废）
/// 3. 头区空行后=代码体（空=MissingBody）
pub(crate) fn parse_creation_file(
    content: &str,
    max_speed: f64,
) -> Result<CreationFile, CreationParseError> {
    let lines: Vec<&str> = content.lines().collect();
    // 1. § 头：首行硬性要求
    let first = lines.first().copied().unwrap_or("");
    let name = first
        .strip_prefix(";; §")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(CreationParseError::MissingHeader)?
        .to_string();
    // 2. 头区找 spawn 行：;; 注释行之间容忍（原样保留语义）；
    //    代码行/空行出现即头区结束（spawn 还没出现=缺 spawn 行）
    let mut spawn: Option<CreationSpawnLine> = None;
    let mut body_start = lines.len(); // 默认=没有代码体
    for (idx, line) in lines.iter().enumerate().skip(1) {
        if let Some(rest) = line.strip_prefix(";; spawn") {
            spawn = Some(parse_spawn_line(rest, max_speed).ok_or(CreationParseError::BadSpawnLine)?);
            continue;
        }
        let is_comment = line.trim_start().starts_with(";;");
        if !is_comment || line.trim().is_empty() {
            body_start = idx; // 这行（含空行）属于代码体
            break;
        }
    }
    let spawn = spawn.ok_or(CreationParseError::MissingSpawnLine)?;
    // 3. 代码体=body_start 起全部行原样（头区空行=分隔符，连同多余空行一并跳过；
    //    尾换行保留）
    let mut start = body_start.min(lines.len());
    while start < lines.len() && lines[start].trim().is_empty() {
        start += 1;
    }
    let body_lines = &lines[start..];
    if body_lines.iter().all(|l| l.trim().is_empty()) {
        return Err(CreationParseError::MissingBody);
    }
    let mut code = body_lines.join("\n");
    code.push('\n');
    Ok(CreationFile { name, spawn, code })
}

/// spawn 行 15 数解析（域钳制复用 codeworld_core 单一法源；寿命 <1=D-019 强制
/// 声明拒绝；初速模长超世界极速按 import 层同款钳制）
fn parse_spawn_line(rest: &str, max_speed: f64) -> Option<CreationSpawnLine> {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.len() != 15 {
        return None;
    }
    let nums: Vec<f64> = tokens
        .iter()
        .map(|t| t.parse::<f64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if nums.iter().any(|v| !v.is_finite()) {
        return None;
    }
    // 寿命走 i32 转换（import 层同构：WASM 侧本就是 i32 域；<1 由 try 返 None 作废）
    let lifespan_ticks = codeworld_core::entity::try_lifespan_ticks(nums[14] as i32)?;
    let (vx, vy) = clamp_velocity(nums[4], nums[5], max_speed);
    Some(CreationSpawnLine {
        radius: codeworld_core::collision::clamp_radius(nums[0]),
        mass: codeworld_core::collision::clamp_mass(nums[1]),
        binding: codeworld_core::entity::clamp_binding(nums[2] as i32),
        motion: codeworld_core::entity::clamp_motion(nums[3] as i32),
        vx,
        vy,
        field_radius: codeworld_core::entity::clamp_field_radius(nums[6]),
        field_gravity_scale: codeworld_core::entity::clamp_field_gravity_scale(nums[7]),
        field_drag_delta: codeworld_core::entity::clamp_field_drag_delta(nums[8]),
        liquid_density: codeworld_core::entity::clamp_liquid_density(nums[9]),
        liquid_drag: codeworld_core::entity::clamp_liquid_drag(nums[10]),
        link: codeworld_core::entity::clamp_link(nums[11] as i32),
        restitution: codeworld_core::entity::clamp_restitution(nums[12]),
        friction: codeworld_core::entity::clamp_friction(nums[13]),
        lifespan_ticks,
    })
}

/// 初速钳制（import 层同口径：模长 ≤ 世界极速，恰等放行；非有限已在上面拦）
fn clamp_velocity(vx: f64, vy: f64, max_speed: f64) -> (f64, f64) {
    let len = (vx * vx + vy * vy).sqrt();
    if len <= max_speed || len == 0.0 {
        (vx, vy)
    } else {
        let k = max_speed / len;
        (vx * k, vy * k)
    }
}

/// @carry 清单解析（players/<名>.wat 全文 → 造物名序列）：`;; @carry <名>` 行
/// 位置无关（老文件零破坏）；空名行跳过；行序=清单序=格序锚
pub(crate) fn parse_carry_lines(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|l| l.strip_prefix(";; @carry").map(str::trim))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// @carry 行移除（返回 (新内容, 是否有改动)）：只动 `;; @carry <名>` 精确同名行，
/// 其余字节原样（"箭塔二号"不挨"箭塔"的刀——trim 后整行等值比较）
pub(crate) fn remove_carry_line(content: &str, creation: &str) -> (String, bool) {
    let mut changed = false;
    let mut out = Vec::new();
    for line in content.lines() {
        let is_target = line
            .strip_prefix(";; @carry")
            .map(str::trim)
            .is_some_and(|name| name == creation);
        if is_target {
            changed = true;
            continue;
        }
        out.push(line);
    }
    if !changed {
        return (content.to_string(), false);
    }
    (format!("{}\n", out.join("\n")), true)
}

/// @carry 行追加（返回 (新内容, 是否有改动)）：顶部插入引用清单段（新行在最上）；
/// 已携带（trim 后等值）不重复追加
pub(crate) fn append_carry_line(content: &str, creation: &str) -> (String, bool) {
    let already = parse_carry_lines(content).iter().any(|n| n == creation);
    if already {
        return (content.to_string(), false);
    }
    (format!(";; @carry {creation}\n{content}"), true)
}

/// 读档错误（HTTP 层映射回执码）
#[derive(Debug)]
pub(crate) enum CreationReadError {
    TooLarge,
    NotFound,
    Io(std::io::Error),
    Parse(CreationParseError),
}

impl From<CreationParseError> for CreationReadError {
    fn from(e: CreationParseError) -> Self {
        Self::Parse(e)
    }
}

/// 造物文件在盘读取（metadata 64KB 护栏 + 读文 + 解析，一步到档案）。
/// 名字闸门（sanitize_name）归调用方（HTTP 层已过闸）；max_speed 穿参（初速律）
pub(crate) fn read_creation(
    dir: &Path,
    name: &str,
    max_speed: f64,
) -> Result<CreationFile, CreationReadError> {
    let path = dir.join(format!("{name}.wat"));
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > CREATION_FILE_MAX_BYTES as u64 {
            tracing::warn!(name = %name, len = meta.len(), "[creations] 文件超 64KB 上限，作废跳过");
            return Err(CreationReadError::TooLarge);
        }
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_creation_file(&text, max_speed).map_err(CreationReadError::from),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(CreationReadError::NotFound),
        Err(e) => Err(CreationReadError::Io(e)),
    }
}

/// creations/ 目录位置（players/ 的同级兄弟；单一推导源——main/net/账本建账
/// 三处共用，防两个目录口径漂移）。parent 推导而非 with_file_name：后者会把
/// 整个末段组件换掉（测试临时目录的唯一后缀会被吞成共享 /tmp/creations）
pub(crate) fn dir_for(players_dir: &Path) -> std::path::PathBuf {
    match players_dir.parent() {
        Some(p) => p.join("creations"),
        None => std::path::PathBuf::from("creations"),
    }
}

/// 档案 → 技能模板（文件造物定案：glyph=名字首字符大写、color=gray——与
/// player_roster 离场灰「不可知=诚实」同款；在场后快照实体带真实外观）
pub(crate) fn to_template(
    cf: &CreationFile,
    name: &str,
    max_speed: f64,
    fuel_limit: u64,
) -> SkillTemplate {
    let _ = max_speed; // 初速已在 parse 层钳过（read_creation 穿参）；此处只透传档案
    SkillTemplate {
        name: name.to_string(),
        code: cf.code.clone(),
        glyph: crate::players::glyph_for(name),
        color: "gray".to_string(),
        radius: cf.spawn.radius,
        mass: cf.spawn.mass,
        binding: cf.spawn.binding,
        motion: cf.spawn.motion,
        vx: cf.spawn.vx,
        vy: cf.spawn.vy,
        blob: Vec::new(),
        field_radius: cf.spawn.field_radius,
        field_gravity_scale: cf.spawn.field_gravity_scale,
        field_drag_delta: cf.spawn.field_drag_delta,
        liquid_density: cf.spawn.liquid_density,
        liquid_drag: cf.spawn.liquid_drag,
        link: cf.spawn.link,
        restitution: cf.spawn.restitution,
        friction: cf.spawn.friction,
        lifespan_ticks: cf.spawn.lifespan_ticks,
        fuel_limit,
    }
}

/// 技能账本建账（库存层数据源切换的核心）：读 players/<名>.wat 的 @carry 清单
/// → 逐名读 creations/<名>.wat → SkillTemplate。格序=清单行序；引用不存在/
/// 坏文件=该行跳过 WARN 不炸清单；超 9 个由 SkillMemory::record 上限拦截（前 9 上栏）。
/// 返回模板清单（调用方 skills.rebuild 落账）。
pub(crate) fn build_player_ledger(
    players_dir: &Path,
    player: &str,
    max_speed: f64,
    fuel_limit: u64,
) -> Vec<SkillTemplate> {
    let creations_dir = dir_for(players_dir);
    let path = players_dir.join(format!("{player}.wat"));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(player = %player, error = %e, "[carry] 角色文件读取失败，清单按空");
            return Vec::new();
        }
    };
    let carry = parse_carry_lines(&text);
    let mut out = Vec::new();
    for name in &carry {
        match read_creation(&creations_dir, name, max_speed) {
            Ok(cf) => out.push(to_template(&cf, name, max_speed, fuel_limit)),
            Err(e) => {
                // 解构读字段（日志落地具体原因，排错先拉日志）
                let why = match &e {
                    CreationReadError::TooLarge => "文件超 64KB".to_string(),
                    CreationReadError::NotFound => "文件不存在".to_string(),
                    CreationReadError::Io(err) => format!("IO 错误：{err}"),
                    CreationReadError::Parse(p) => format!("坏档（{p:?}）"),
                };
                tracing::warn!(player = %player, creation = %name, why = %why, "[carry] 引用的造物文件读不到/坏档，该行跳过（清单其余照常）");
                continue;
            }
        }
    }
    tracing::info!(
        player = %player,
        carried = carry.len(),
        built = out.len(),
        "[carry] 进场扫 @carry 清单建账完成（格序=清单行序，上限 9 由账本落账时收口）"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标准好文件（箭塔：15 数默认律 + 代码体 + 头区附加注释）
    const GOOD: &str = ";; §箭塔\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 10000\n;; 皮肤资产：av_x1（装戴 API 属后续刀）\n\n(module (func (export \"life\") (result i32) i32.const 1))\n";

    #[test]
    fn good_header_parses_all_fields() {
        // 好头（§名+spawn 15 数+WAT 体）→ 15 字段逐位断言（PendingSpawn 等价档案）
        let cf = parse_creation_file(GOOD, 25.0).expect("好文件应解析通过");
        assert_eq!(cf.name, "箭塔");
        assert_eq!(cf.spawn.radius, 0.5);
        assert_eq!(cf.spawn.mass, 1.0);
        assert_eq!(cf.spawn.binding, 0);
        assert_eq!(cf.spawn.motion, 0);
        assert_eq!(cf.spawn.vx, 0.0);
        assert_eq!(cf.spawn.vy, 0.0);
        assert_eq!(cf.spawn.field_radius, 0.0);
        assert_eq!(cf.spawn.field_gravity_scale, 1.0);
        assert_eq!(cf.spawn.field_drag_delta, 0.0);
        assert_eq!(cf.spawn.liquid_density, 0.0);
        assert_eq!(cf.spawn.liquid_drag, 0.0);
        assert_eq!(cf.spawn.link, 0);
        assert_eq!(cf.spawn.restitution, 0.0);
        assert_eq!(cf.spawn.friction, 0.6);
        assert_eq!(cf.spawn.lifespan_ticks, 10000);
        assert_eq!(
            cf.code,
            "(module (func (export \"life\") (result i32) i32.const 1))\n",
            "代码体=头区空行之后的全部行（尾换行原样保留）"
        );
    }

    #[test]
    fn missing_header_rejected() {
        // 缺 § 头：首行不是 § 行（普通注释/直接代码开头）→ 整文件作废
        let a = ";; 普通注释\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 10000\n\n(module)\n";
        assert_eq!(parse_creation_file(a, 25.0), Err(CreationParseError::MissingHeader));
        let b = "(module)\n;; §箭塔\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 10000\n\n(module)\n";
        assert_eq!(parse_creation_file(b, 25.0), Err(CreationParseError::MissingHeader));
    }

    #[test]
    fn missing_spawn_line_rejected() {
        // 缺 spawn 行：§ 在但头区结束（代码出现）都没等到 spawn → 作废
        let a = ";; §箭塔\n;; 只是注释\n\n(module)\n";
        assert_eq!(parse_creation_file(a, 25.0), Err(CreationParseError::MissingSpawnLine));
        let b = ";; §箭塔\n(module)\n";
        assert_eq!(parse_creation_file(b, 25.0), Err(CreationParseError::MissingSpawnLine));
    }

    #[test]
    fn bad_spawn_line_rejected() {
        // spawn 行坏：14 个数（少一个）/非数字/寿命 0（D-019 强制声明 <1 拒绝）/NaN/16 个数（多一个）
        let cases = [
            ";; §箭塔\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6\n\n(module)\n",
            ";; §箭塔\n;; spawn 0.5 1.0 零 0 0 0 0 1.0 0 0 0 0 0 0.6 10000\n\n(module)\n",
            ";; §箭塔\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 0\n\n(module)\n",
            ";; §箭塔\n;; spawn NaN 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 10000\n\n(module)\n",
            ";; §箭塔\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 10000 99\n\n(module)\n",
        ];
        for src in cases {
            assert_eq!(
                parse_creation_file(src, 25.0),
                Err(CreationParseError::BadSpawnLine),
                "坏 spawn 行应作废"
            );
        }
    }

    #[test]
    fn cjk_name_ok_and_comments_between_header_lines() {
        // 中文名域照常；§ 与 spawn 之间、spawn 与空行之间的 ;; 注释行原样容忍
        let src = ";; §冰霜法师·壹\n;; 作者备注：remote 示例\n;; spawn 1.5 20.0 1 0 0 0 6.0 0.5 8.0 0 0 0 0.2 0.6 20000\n;; 皮肤资产：av_x9\n\n(module)\n";
        let cf = parse_creation_file(src, 25.0).expect("中文名+多注释头应解析通过");
        assert_eq!(cf.name, "冰霜法师·壹");
        assert_eq!(cf.spawn.radius, 1.5);
        assert_eq!(cf.spawn.binding, 1);
        assert_eq!(cf.spawn.field_radius, 6.0);
        assert_eq!(cf.spawn.field_gravity_scale, 0.5);
        assert_eq!(cf.spawn.field_drag_delta, 8.0);
        assert_eq!(cf.spawn.restitution, 0.2);
        assert_eq!(cf.spawn.friction, 0.6);
        assert_eq!(cf.spawn.lifespan_ticks, 20000);
    }

    #[test]
    fn body_may_contain_wat_comments() {
        // 代码体里的 ;; 是 WAT 行注释（wat 解析器语义），不是头区——空行分隔即定界
        let src = ";; §箭塔\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 10000\n\n;; 这行属于代码体的注释\n(module)\n";
        let cf = parse_creation_file(src, 25.0).expect("应解析通过");
        assert_eq!(cf.code, ";; 这行属于代码体的注释\n(module)\n");
    }

    #[test]
    fn missing_body_rejected() {
        // 空代码体（头区后什么都没有）→ 作废
        let src = ";; §箭塔\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 10000\n";
        assert_eq!(parse_creation_file(src, 25.0), Err(CreationParseError::MissingBody));
    }

    #[test]
    fn parse_carry_lines_zero_one_many_and_tolerant() {
        // @carry 解析：0/1/N 行 + 空名行跳过 + 行序保持
        let none = "(module)\n;; 普通注释\n";
        assert!(parse_carry_lines(none).is_empty(), "无 @carry 老文件=空清单");
        let one = ";; @carry 箭塔\n(module)\n";
        assert_eq!(parse_carry_lines(one), vec!["箭塔".to_string()]);
        let many = ";; @carry 箭塔\n;; @carry 炮台\n(module)\n;; @carry 冰霜\n";
        assert_eq!(
            parse_carry_lines(many),
            vec!["箭塔".to_string(), "炮台".to_string(), "冰霜".to_string()],
            "行序=清单序（格序锚）"
        );
        let junk = ";; @carry \n;; @carry 箭塔\n";
        assert_eq!(parse_carry_lines(junk), vec!["箭塔".to_string()], "空名行跳过");
    }

    #[test]
    fn remove_carry_line_only_named_and_exact() {
        // 移除只动同名行；其余内容字节不动；无匹配=原样返回 changed=false
        let src = ";; @carry 箭塔\n;; @carry 炮台\n(module)\n;; @carry 箭塔二号\n";
        let (out, changed) = remove_carry_line(src, "箭塔");
        assert!(changed);
        assert_eq!(
            out,
            ";; @carry 炮台\n(module)\n;; @carry 箭塔二号\n",
            "只移除同名行（箭塔二号不受牵连）"
        );
        let (same, changed2) = remove_carry_line(src, "冰霜");
        assert!(!changed2);
        assert_eq!(same, src, "无匹配原样返回");
    }

    #[test]
    fn append_carry_line_dedups_and_prepends() {
        // 追加=顶部插入引用清单段；已携带不重复加；新行在最上
        let src = "(module)\n";
        let (out, changed) = append_carry_line(src, "箭塔");
        assert!(changed);
        assert_eq!(out, ";; @carry 箭塔\n(module)\n");
        let (again, changed2) = append_carry_line(&out, "箭塔");
        assert!(!changed2, "已携带不再重复追加");
        assert_eq!(again, out);
    }

    /// 临时目录（每测试独立；不碰生产 players/creations）
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cw-creations-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 造物文件落盘助手（头 + 代码体）
    fn write_creation(dir: &Path, name: &str, body: &str) {
        let text = format!(
            ";; §{name}\n;; spawn 0.5 1.0 0 0 0 0 0 1.0 0 0 0 0 0 0.6 10000\n\n{body}"
        );
        std::fs::write(dir.join(format!("{name}.wat")), text).unwrap();
    }

    #[test]
    fn ledger_builds_templates_in_carry_order() {
        // 进场建账：@carry 清单行序=模板格序；字段从 spawn 行来；fuel 穿参
        //（嵌套目录：temp 根/players + temp 根/creations——dir_for 按 parent 推导，
        // 测试必须嵌套才隔离，不许拿 temp 根直接当 players 目录）
        let root = temp_dir("ledger-happy");
        let players = root.join("players");
        std::fs::create_dir_all(&players).unwrap();
        let cdir = dir_for(&players);
        std::fs::create_dir_all(&cdir).unwrap();
        write_creation(&cdir, "甲", "(module)\n");
        write_creation(&cdir, "乙", "(module)\n");
        std::fs::write(players.join("alice.wat"), ";; @carry 乙\n;; @carry 甲\n(module)\n").unwrap();
        let ledger = build_player_ledger(&players, "alice", 25.0, 777);
        assert_eq!(ledger.len(), 2, "两个引用都有效=两格");
        assert_eq!(ledger[0].name, "乙", "格序=清单行序（乙在前）");
        assert_eq!(ledger[1].name, "甲");
        assert_eq!(ledger[0].fuel_limit, 777, "fuel 穿参");
        assert_eq!(ledger[0].lifespan_ticks, 10000, "寿命从 spawn 行来");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ledger_skips_missing_refs_and_gone_player_file() {
        // 引用不存在的造物 → 该行跳过（其余照常）；角色文件本身没了 → 空清单
        let root = temp_dir("ledger-missing");
        let players = root.join("players");
        std::fs::create_dir_all(&players).unwrap();
        let cdir = dir_for(&players);
        std::fs::create_dir_all(&cdir).unwrap();
        write_creation(&cdir, "在册", "(module)\n");
        std::fs::write(
            players.join("alice.wat"),
            ";; @carry 幽灵\n;; @carry 在册\n(module)\n",
        )
        .unwrap();
        let ledger = build_player_ledger(&players, "alice", 25.0, 1000);
        assert_eq!(ledger.len(), 1, "幽灵引用跳过，在册照常建账");
        assert_eq!(ledger[0].name, "在册");
        // 角色文件缺失（读失败）→ 空清单（不 panic）
        let ledger2 = build_player_ledger(&players, "查无此人", 25.0, 1000);
        assert!(ledger2.is_empty(), "角色文件没了=空清单");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn to_template_file_law_glyph_first_char_color_gray() {
        // 文件造物外观定案：glyph=名字首字符大写、color=gray（离场灰同款诚实哲学）
        let cf = parse_creation_file(GOOD, 25.0).expect("好文件");
        let tpl = to_template(&cf, "箭塔", 25.0, 1_000_000);
        assert_eq!(tpl.glyph, "箭", "glyph=名字首字符");
        assert_eq!(tpl.color, "gray");
        assert_eq!(tpl.name, "箭塔", "模板名=文件名锚");
        assert_eq!(tpl.radius, 0.5);
        assert_eq!(tpl.friction, 0.6);
    }

    #[test]
    fn read_creation_rejects_oversize_with_gate() {
        // 超 64KB：metadata 先量后读，直接 TooLarge（不整读进内存）
        let root = temp_dir("read-oversize");
        let cdir = dir_for(&root.join("players"));
        std::fs::create_dir_all(&cdir).unwrap();
        let big = format!(
            ";; §大块头\n;; spawn {}\n\n(module)\n",
            "0.5 ".repeat(15).trim_end()
        );
        let filler = ";; ".to_string() + &"x".repeat(70 * 1024) + "\n";
        std::fs::write(cdir.join("大块头.wat"), format!("{big}{filler}")).unwrap();
        match read_creation(&cdir, "大块头", 25.0) {
            Err(CreationReadError::TooLarge) => {}
            other => panic!("超限应 TooLarge，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_creation_parses_file_from_disk() {
        // 在盘读档全链：临时目录真文件 → CreationFile
        let root = temp_dir("read-ok");
        let cdir = dir_for(&root.join("players"));
        std::fs::create_dir_all(&cdir).unwrap();
        write_creation(&cdir, "箭塔", "(module)\n");
        let cf = read_creation(&cdir, "箭塔", 25.0).expect("应读到并解析通过");
        assert_eq!(cf.name, "箭塔");
        let _ = std::fs::remove_dir_all(&root);
    }


    #[test]
    fn oversize_gate_constant_is_64k() {
        // 64KB 上限常数（HTTP 层与读档路径共用同一道门）
        assert_eq!(CREATION_FILE_MAX_BYTES, 64 * 1024);
    }
}
