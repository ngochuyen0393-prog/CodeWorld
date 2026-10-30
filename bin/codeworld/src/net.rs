//! 期四第一刀：WebSocket 快照推送（authoritative server + snapshot broadcast）
//!
//! 世界是唯一权威（world-foundation.md），浏览器只是显示器：tick 侧每拍组装
//! 全量 JSON 快照推广播通道，WS 线程（tokio runtime 独立线程）广播给所有连接。
//! 本模块与终端 ANSI 渲染并行存在，不替代终端；WS 禁用（端口 0）零开销。
//!
//! 职责边界：快照是只读投影（渲染数据源 world.entities + caption + tick），
//! 不回写世界；客户端输入第一刀只 pong/忽略（意图流是第三刀，接 input.rs 入口）。

use crate::input::IntentSink;
use crate::players::PlayerDirectory;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Multipart, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codeworld_core::entity::EntityId;
use codeworld_core::world::WorldState;
use codeworld_render::Viewport;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

// D-017 收图通道子模块（资产登记表+上传落盘）。挂 net 下而非 main.rs——
// 共享工作树上 main.rs 有并行在途改动，本卡零触碰（模块归属纯组织问题，
// 对外路径 crate::net::avatars 不影响语义）
#[path = "avatars.rs"]
pub(crate) mod avatars;
use self::avatars::{AvatarRegistry, UploadInput};

/// 快照广播通道容量（未消费帧积压上限；慢客户端丢旧帧即可，快照有"后值覆盖"语义）
pub(crate) const SNAPSHOT_CHANNEL_CAP: usize = 16;

/// 角色列表条目（D-021d 登录/选角页下行 player_roster 帧的数组元素）：
/// name=players/<name>.wat 的角色名；online=true 条目 glyph/color 取世界里
/// 活实体实际值，online=false（离线/被带走/残影）glyph=名字首字符大写、
/// color=中性灰 #8b949e（颜色依赖出生序号，离线时不可知，灰=诚实）
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RosterEntry {
    pub name: String,
    pub glyph: String,
    pub color: String,
    pub online: bool,
}

/// 离线角色的中性灰（D-021d：出生序号不可知时的诚实外观；前端色块圆片用它）
pub(crate) const ROSTER_OFFLINE_COLOR: &str = "#8b949e";

/// 头像资产目录（相对工作目录，与 STATIC_DIR 同基准——start.bat 已 cd 到项目根）。
/// git 不入库（.gitignore），上传即写盘，启动扫目录重建登记表
pub(crate) const AVATARS_DIR: &str = "assets/avatars";

// ---- 上行意图协议（docs/net-protocol.md v1.2 客户端 -> 服务器，#027）----

/// 客户端上行消息（五型）：点击走位 / 点击出刀 / 远程射击（v1.9，#036）/
/// 停止 / 登出处置（v1.7，#025）。
/// 坐标一律世界坐标（快照 viewport 域内），非法值解析层拒绝。
/// （WS shutdown 关世界接口预留见 net-protocol.md v1.9——MVP 只做终端 Q，本层不收）
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum ClientMessage {
    Move { x: f64, y: f64 },
    Attack { x: f64, y: f64 },
    /// 远程射击（#036 期五第五刀）：点击远处敌人——前端只发目标坐标，
    /// 弹道参数服务器侧定装（驾驶档）
    Shoot { x: f64, y: f64 },
    /// 技能栏（v2.5，D-020a，t_06b4efc6）：slot=1..4，替操控玩家 spawn 预置
    /// 模板造物（UI 便利非新原语）；非法槽位解析层拒绝
    Skill { slot: u8 },
    Stop,
    /// 登出处置三选一（#025）：take/stay_auto/stay_ai；仅操控连接生效，
    /// 处置由 tick 侧状态机落地（本层只转发意图+释放令牌）
    Logout {
        #[serde(rename = "disposition")]
        choice: crate::dispose::LogoutChoice,
    },
    /// 创建角色（D-021b 进场仪式）：name+template（blank/grazer/hunter）→
    /// 服务器写 players/<name>.wat 当拍进场；回执走 player_events
    /// （player_joined / create_player_rejected，公共新闻）。
    /// 名字校验全在 tick 侧（R1 复审改法 B：解析层拒绝=静默丢弃无回执，
    /// 前端 busy 锁会挂死——任何拒绝都必须有事件回执）
    #[serde(rename = "create_player")]
    CreatePlayer { name: String, template: String },
    /// 选角（D-021d 登录/选角页 v2.7）：以 players/ 既有角色进场（等价 me.txt
    /// 声明该名字）。名字校验全在 tick 侧 process_select_player（拒绝走
    /// select_player_rejected 事件回执——R1 改法 B 同款）
    #[serde(rename = "select_player")]
    SelectPlayer { name: String },
}

/// 上行文本 -> 客户端消息（纯函数；Err 带原因供 warn 丢弃，不 panic 不部分接受）
pub(crate) fn parse_client_message(text: &str) -> Result<ClientMessage, String> {
    let msg: ClientMessage =
        serde_json::from_str(text).map_err(|e| format!("JSON/协议不符: {e}"))?;
    let finite = |v: f64| v.is_finite();
    let coords_ok = match &msg {
        ClientMessage::Move { x, y } | ClientMessage::Attack { x, y } => finite(*x) && finite(*y),
        ClientMessage::Shoot { x, y } => finite(*x) && finite(*y),
        ClientMessage::Stop | ClientMessage::Logout { .. } | ClientMessage::Skill { .. } => true,
        // D-021b：CreatePlayer 意图直通 tick 侧（名字校验在 tick 侧
        // process_create_player——R1 复审改法 B，拒绝全部走事件回执）
        // D-021d：SelectPlayer 同款（名字校验归 tick 侧 process_select_player）
        ClientMessage::CreatePlayer { .. } | ClientMessage::SelectPlayer { .. } => true,
    };
    if !coords_ok {
        return Err("坐标非有限值（NaN/无穷）".to_string());
    }
    // 技能槽位域校验（v2.5，D-020a；D-020b 语义重定义：slot=玩家技能格下标
    // 1..=9，格=该玩家 spawn 成功的造物模板清单）之外拒绝（0/10/99 等非法值丢弃）
    if let ClientMessage::Skill { slot } = &msg {
        if *slot < 1 || *slot as usize > crate::skills::SKILL_SLOTS_MAX {
            return Err(format!(
                "技能槽位非法（{slot}，合法域 1..={})",
                crate::skills::SKILL_SLOTS_MAX
            ));
        }
    }
    // create_player（D-021b）：解析层不做名字校验——R1 复审改法 B：名字校验
    // 若放解析层，拒绝走 Err 分支=静默丢弃（无事件回执），前端 busy 锁永久
    // 挂起（前后端口径漂移时即触发）。所有名字拒绝（含 NAME_EMPTY/
    // TOO_LONG/BAD_CHAR）统一走 tick 侧 process_create_player 的结构化
    // create_player_rejected 事件回执——任何拒绝都有回执，busy 锁永不悬空。
    // 解析层只保留协议形状（serde 必填字段）。
    Ok(msg)
}

// ---- 快照数据形状（docs/net-protocol.md v1 前端契约，字段名与顺序第二刀依赖）----

/// 每拍一帧的全量世界快照
#[derive(Debug, Serialize)]
pub(crate) struct WorldSnapshot {
    pub tick: u64,
    pub caption: String,
    pub viewport: ViewportInfo,
    pub entities: Vec<EntitySnapshot>,
    /// 本拍攻击命中事件（v2.0，D-017 协议小刀）：结算成功的刀的三件套——
    /// 攻方/目标ID/目标位置（结算时刻实况坐标）。空数组恒有（前端无需可选
    /// 分支）；落空/拦截/作废不广播。前端动画包方向变体（attack_high/mid/low）
    /// 的选槽依据
    pub attacks: Vec<AttackEventInfo>,
    /// 地形折线投影（D-016）：非平地才有（平地缺席——前端回落 y=0 横线；
    /// 旧前端忽略未知字段，向后兼容）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terrain: Option<TerrainInfo>,
    /// 力场环投影（D-012 可视铁律：场必须可视——温迪气流环范本，纯数值场配
    /// 贴地特效）。每拍全量（恒有字段，空数组=无场世界，前端无需可选分支）；
    /// 场源造物本体已照常渲染（fields 层只画"可见气流环"）。formed=false=
    /// 成型中（前端画虚线环，给进攻方反应窗），true=已成型（实线环）
    pub fields: Vec<FieldSnapshot>,
    /// 水域矩形投影（D-009 可视铁律：水域必须可视）。每拍全量（恒有字段，
    /// 空数组=无水世界，前端零可选分支）；颜色=密度映射是前端装饰非世界事实
    /// （玩家造 ρ=0.8 的"蓝油"世界不拦不判）
    pub liquids: Vec<LiquidSnapshot>,
    /// 本拍玩家事件（v2.5，D-020a）：增量事件流（新闻不是档案，只随当拍快照发
    /// 一次），已按操控玩家过滤（可见性铁律 decisions #054 同族：宁缺勿泄）；
    /// 空数组恒有（前端零可选分支）
    pub player_events: Vec<crate::events::PlayerEvent>,
    /// 事件游标（v2.5，D-020a）：世界已发出的最大 seq。断线对账用：前端以
    /// cursor − 本地已收 seq 估算断线期间丢失的事件数（MVP 断线即丢不回放）
    pub events_cursor: u64,
}

/// 单个水域矩形投影（D-009：液体造物格对齐 AABB 转矩形 + 密度）
#[derive(Debug, Serialize)]
pub(crate) struct LiquidSnapshot {
    /// 矩形左下角 x（格对齐 AABB min.x）
    pub x: f64,
    /// 矩形左下角 y
    pub y: f64,
    /// 矩形宽（max.x − min.x）
    pub w: f64,
    /// 矩形高（max.y − min.y）
    pub h: f64,
    /// 液体密度 ρ（无量纲，水=1.0 参照系；前端查表插值画色）
    pub density: f64,
}

/// 单个力场投影（D-012：场源坐标+半径+成型态）
#[derive(Debug, Serialize)]
pub(crate) struct FieldSnapshot {
    /// 场源实体 id（帧序稳定键，D-012 收尾刀 P2 排序用）
    pub id: u128,
    /// 场源 x（圆心）
    pub x: f64,
    /// 场源 y
    pub y: f64,
    /// 场域半径
    pub radius: f64,
    /// 是否已成型（current_tick - created_tick >= forming_ticks）
    pub formed: bool,
}

/// 地形投影（D-016：前端折线渲染数据源；非平地才发全量，平地发 None 省带宽）
#[derive(Debug, Serialize)]
pub(crate) struct TerrainInfo {
    /// 采样 x 起点（-50.0，codeworld_core TERRAIN_X_MIN）
    pub x0: f64,
    /// 采样间距（0.25，codeworld_core TERRAIN_SPACING）
    pub dx: f64,
    /// 高度数组（与 world.terrain.samples() 同源）
    pub heights: Vec<f64>,
}

/// 单条攻击命中事件（D-017 定案字段："攻方/目标ID/目标位置——本来在广播
/// 命中，多带字段，账本不变"）
#[derive(Debug, Serialize)]
pub(crate) struct AttackEventInfo {
    pub attacker: String,
    pub target: String,
    pub x: f64,
    pub y: f64,
}

/// 视口域（世界坐标 -> 画布坐标换算基准：x0/y0 左下角 + 宽高）
#[derive(Debug, Serialize)]
pub(crate) struct ViewportInfo {
    pub x0: f64,
    pub y0: f64,
    pub w: f64,
    pub h: f64,
}

/// 单实体快照。kind 决定可选字段：player 带 controlled；creation 带 owner；
/// name 恒有（v1.1=code_hash 代码身份；**v3.1 起造物实名优先**——creation/
/// creation_dynamic 的 name=creation_name 实名，缺省回退"造物#短id"，其他 kind
/// 照旧 code_hash）；npc 无可选字段（skip_serializing_if 保持 JSON 干净）。
/// radius（v1.5，#030）：活动实体圆半径（前端按它画身体圆）；creation 是格对齐
/// 方块，radius 无意义（前端按 kind=creation 画方格底衬）。
/// disposition/wait_left（v1.7，#025）：处置状态签与等待倒数（仅处置中的玩家有；
/// 字段缺席=online 在线态）。
#[derive(Debug, Serialize)]
pub(crate) struct EntitySnapshot {
    pub id: String,
    pub glyph: String,
    pub color: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub kind: &'static str,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controlled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disposition: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_left: Option<u64>,
    /// 装戴清单指纹（v2.4，#051 皮入沙箱）：asset_id 列表，恒有字段空数组
    /// （空盒省字段是优化，恒有=前端零可选分支，同 attacks/fields 先例）；
    /// 快照只带指纹不带货——前端首见按 id 走 /assets HTTP 取图（世界只搬运
    /// 不理解，修路不跑车）
    pub asset_ids: Vec<String>,
    /// 出生拍（v2.5，D-020a）：控制台造物列表算剩余寿命用（剩余=lifespan−(tick−born)）
    pub born_tick: u64,
    /// 声明寿命（v2.5，D-020a）：spawn 第 23 参落库值；0=永生（仅存量快照实体）
    pub lifespan_ticks: u64,
}

/// kind 判定（纯映射，任务书定案）：开局四生物=npc；creation=造物（static 钉死）；
/// creation_dynamic=D-003 全活造物（motion=1，圆身体受完整物理；【D-018】造物弹
/// 也走此 kind——前端按圆渲染）；弹丸=projectile（历史实体兼容判定：#045/D-018
/// 后世界不再产生 PROJECTILE_CODE 实体，恢复快照即按哑弹清场——正常快照永不含
/// 该 kind，分支纯协议历史兼容防御；新弹=creation_dynamic）；players 目录在线名
/// （或残影躯壳）=player；其余未知 code_hash 兜底 npc。
fn entity_kind(code_hash: &str, entity_id: EntityId, players: &PlayerDirectory, motion: i32) -> &'static str {
    match code_hash {
        crate::ATTACKER_CODE | crate::DEFENDER_CODE | crate::HUNTER_CODE | crate::SPY_CODE => "npc",
        "creation" => {
            if motion == codeworld_core::entity::MOTION_DYNAMIC {
                "creation_dynamic"
            } else {
                "creation"
            }
        }
        // 历史实体兼容分支（D-018 后无新增；前端 kind==='projectile' 分支同留过渡期）
        codeworld_core::collision::PROJECTILE_CODE => "projectile",
        name if players.is_online(name) => "player",
        _ if players.is_ghost(&entity_id) => "player",
        _ => "npc",
    }
}

/// 组装世界快照（每拍一帧全量投影；实体按 id 字典序稳定排列，前端逐帧 diff 依赖帧序稳定）
/// attack_events（v2.0，D-017）：本拍结算成功的攻击命中事件，随快照广播
/// player_events/events_cursor（v2.5，D-020a）：本拍玩家事件（调用方已按操控玩家过滤）
/// clippy too_many_arguments 豁免（9/7）：参数组与 push_snapshot 一一对应（8 参系
/// 先例同款——结构体化反而多一层拆装）
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_snapshot(
    world: &WorldState,
    caption: &str,
    viewport: &Viewport,
    players: &PlayerDirectory,
    controlled: Option<EntityId>,
    attack_events: &[crate::AttackEventRecord],
    field_forming_ticks: u64,
    player_events: Vec<crate::events::PlayerEvent>,
    events_cursor: u64,
) -> WorldSnapshot {
    let (min_x, max_x) = viewport.world_x_range();
    let (min_y, max_y) = viewport.world_y_range();
    let now = world.tick();
    let mut entities: Vec<EntitySnapshot> = world
        .entities
        .values()
        .map(|e| {
            let kind = entity_kind(&e.code_hash, e.id, players, e.motion());
            let (x, y) = e.pos();
            // 处置状态签（v1.7，#025）：账本命中的实体带 disposition/wait_left
            //（账本只收玩家名，npc/creation 天然缺席）；字段缺席=online 在线态
            let disposed = players.dispose.get(&e.code_hash);
            // 实名优先（v3.1，D-020b）：造物的 name=creation_name 实名，缺省回退
            // "造物#短id"（旧存档/驾驶层定装弹）；其他 kind 照旧 code_hash
            let name = if kind == "creation" || kind == "creation_dynamic" {
                e.creation_name()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("造物#{}", &e.id.0.to_string()[..8]))
            } else {
                e.code_hash.clone()
            };
            EntitySnapshot {
                id: e.id.0.to_string(),
                glyph: e.appearance.glyph.clone(),
                color: e.appearance.color.clone(),
                x,
                y,
                radius: e.radius(),
                kind,
                name,
                controlled: (kind == "player").then(|| Some(e.id) == controlled),
                owner: (kind == "creation" || kind == "creation_dynamic")
                    .then(|| e.owner.0.to_string()),
                disposition: disposed.map(|s| s.wire_name()),
                wait_left: disposed.and_then(|s| s.wait_left(now)),
                asset_ids: e.assets.clone(),
                born_tick: e.created_tick,
                lifespan_ticks: e.lifespan_ticks(),
            }
        })
        .collect();
    entities.sort_by(|a, b| a.id.cmp(&b.id));
    // 命中事件投影（D-017）：attacker/target 序列化为 UUID 字符串（与 entities.id
    // 同格式，前端可 join 找实体）；坐标原样 f64（结算时刻实况坐标）
    let attacks = attack_events
        .iter()
        .map(|ev| AttackEventInfo {
            attacker: ev.attacker.0.to_string(),
            target: ev.target.0.to_string(),
            x: ev.target_pos.0,
            y: ev.target_pos.1,
        })
        .collect();
    // 地形折线投影（D-016）：平地不发货（省 401 点×f64 的帧体积，前端回落旧
    // y=0 横线）；非平地全量 401 采样（≈6.4KB JSON/帧，与实体数据同量级可接受）
    let terrain = (world.terrain.preset != codeworld_core::terrain::TerrainPreset::Flat).then(|| {
        TerrainInfo {
            x0: codeworld_core::terrain::TERRAIN_X_MIN,
            dx: codeworld_core::terrain::TERRAIN_SPACING,
            heights: world.terrain.samples().to_vec(),
        }
    });
    // 力场环投影（D-012 可视铁律）：每拍全量（空数组恒有）；成型态按
    // forming_ticks 判（虚线环=成型中给进攻方反应窗，实线环=已成型）。
    // 定序纪律（D-012 收尾刀 P2）：按 id 升序，与 entities 帧序稳定要求同款
    // ——纯渲染层差异（半透明环叠序），但快照逐帧 diff 依赖帧序稳定
    let mut fields: Vec<FieldSnapshot> = world
        .entities
        .values()
        .filter(|e| e.is_alive() && e.field_radius() > 0.0)
        .map(|e| {
            let (x, y) = e.pos();
            FieldSnapshot {
                id: e.id.0.as_u128(),
                x,
                y,
                radius: e.field_radius(),
                formed: world.tick().saturating_sub(e.created_tick) >= field_forming_ticks,
            }
        })
        .collect();
    fields.sort_by_key(|f| f.id);
    // 水域矩形投影（D-009 可视铁律）：每拍全量（空数组恒有）；液体造物格对齐
    // AABB 转矩形+密度随投影（颜色=密度映射是前端装饰非世界事实）。定序纪律
    // 照 fields 同款：按 id 升序，快照逐帧 diff 依赖帧序稳定
    let mut liquids: Vec<LiquidSnapshot> = world
        .entities
        .values()
        .filter(|e| e.is_alive() && e.liquid_density() > 0.0)
        .map(|e| {
            let (min, max) = codeworld_core::collision::creation_aabb(e.pos());
            LiquidSnapshot {
                x: min.0,
                y: min.1,
                w: max.0 - min.0,
                h: max.1 - min.1,
                density: e.liquid_density(),
            }
        })
        .collect();
    // 定序纪律照 fields 同款（帧序稳定）：投影无 id 字段，用几何键（x,y,density,
    // w,h）全序——同格多水域也有确定帧序
    liquids.sort_by(|a, b| {
        a.x
            .total_cmp(&b.x)
            .then(a.y.total_cmp(&b.y))
            .then(a.density.total_cmp(&b.density))
            .then(a.w.total_cmp(&b.w))
            .then(a.h.total_cmp(&b.h))
    });
    WorldSnapshot {
        tick: world.tick(),
        caption: caption.to_string(),
        viewport: ViewportInfo {
            x0: min_x,
            y0: min_y,
            w: max_x - min_x,
            h: max_y - min_y,
        },
        entities,
        attacks,
        terrain,
        fields,
        liquids,
        player_events,
        events_cursor,
    }
}

/// 静态验收页目录（相对工作目录；start.bat 已 cd 到项目根）
pub(crate) const STATIC_DIR: &str = "bin/codeworld/static";

/// WS 服务器共享状态：快照广播、上行意图队列、操控连接 id（D-021f 凭据制）、
/// 会话令牌表（D-021f）、头像资产登记表（D-017：上传路由写入 / ws 连接建立时
/// 全量下发）、角色列表共享槽（D-021d：主循环每拍重建 / ws 连接建立时全量下发）
pub(crate) struct WsState {
    pub(crate) snapshots: broadcast::Sender<String>,
    /// 与终端键盘共享的意图队列生产端（单入口双来源，input.rs 路由不区分来源）
    pub(crate) intents: IntentSink,
    /// 当前操控连接 id：None=无操控端 / Some(n)=连接 n 持有操控权。
    /// D-021f 凭据制取代旧 ctrl_taken AtomicBool：凭 token 认领时
    /// send(Some(新 id))，旧操控端在 select! 的 watch 分支比对自己的 id
    /// 降级观战（踢旧留新，连接不断）。#024 独占语义不变：任意时刻至多
    /// 一个操控连接，变的只是钥匙（先来者得 → 凭据者得）
    pub(crate) ctrl_conn: tokio::sync::watch::Sender<Option<u64>>,
    /// 会话令牌表：token → 玩家名（D-021f）。写入方=tick 侧（选角/创建成功
    /// 路径经 WsHandle 持有的 Arc），读取方=连接升级验证（?token= 认领）。
    /// 生命周期=进程（重启即清空=全部失效，重新登录页选角）
    pub(crate) sessions: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// 逃生门：CODEWORLD_ME 启动时非空 = true，保留旧"第一连接=操控端"
    ///（开发者 env 锁定场景 loginSkip 不重连也保有操控）；false=无 token
    /// 连接纯观战（普通玩家走凭据认领）
    pub(crate) legacy_first_conn: bool,
    /// 连接 id 发号器（每连接递增；操控权认领的凭证，watch 里流转的值）
    pub(crate) conn_ids: AtomicU64,
    /// 头像资产登记表（D-017 资产层：上传落盘+内存索引，掩码连接建立时下发）
    pub(crate) avatars: Arc<AvatarRegistry>,
    /// 角色列表共享槽（D-021d 登录/选角页）：主循环每拍 build_roster 重建，
    /// ws 连接建立时全量下发。std Mutex 足够（读写都是瞬时快照）
    pub(crate) roster: Arc<std::sync::Mutex<Vec<RosterEntry>>>,
    /// 玩家目录路径（D-022 工坊 HTTP 通道：/api/player-code 读写删 players/<名>.wat
    ///——工坊保存=写文件=世界目录轮询自动接手，零新进场协议）
    pub(crate) players_dir: std::path::PathBuf,
    /// 外桥保险库只读句柄（D-022 GET /api/bridge-config 端口列表只读展示；
    /// None=桥未启用/未装配——GET 回 enabled:false 空列表。与主循环共享同一把
    /// RwLock，bridges.json mtime 热重载两边看同一份账）
    pub(crate) bridge_vault:
        Option<Arc<std::sync::RwLock<crate::bridges::BridgeVault>>>,
    /// 外桥登记表路径（D-022b POST /api/bridge-config 写盘目标=config.bridge_config，
    /// 默认世界目录 bridges.json。vault 未装配也持有——写盘受理，桥下次启用即用）
    pub(crate) bridge_config_path: std::path::PathBuf,
}

/// 主循环持有的 WS 句柄：快照推送端、操控连接 id 观测（#025 期五第一刀；
/// D-021f 起改 watch Receiver）与会话令牌表/角色列表槽写入端。主循环每拍
/// poll watch 做翻转沿检测（Claimed/Released），作为处置状态机触发源；
/// 每拍重建角色列表写共享槽。
pub(crate) struct WsHandle {
    pub(crate) snapshots: broadcast::Sender<String>,
    /// 操控连接 id 观测端（None↔Some 翻转沿 = 处置状态机触发源）
    pub(crate) ctrl_conn: tokio::sync::watch::Receiver<Option<u64>>,
    /// 会话令牌表（D-021f）：tick 侧选角/创建成功时写入 token→name（与
    /// WsState.sessions 同一个 Arc）
    pub(crate) sessions: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// 角色列表共享槽（D-021d）：主循环每拍写入，ws 线程连接建立时读取下发
    pub(crate) roster: Arc<std::sync::Mutex<Vec<RosterEntry>>>,
}

/// 操控连接沿事件（主循环每拍沿检测；WS 禁用时恒 None）——
/// #025 处置状态机的两条触发源：Claimed=重连认领（取消处置/重新进场；
/// D-021f 起含凭 token 认领与踢旧易主），Released=意外断开（进入处置等待；
/// 主动登出沿由 tick 侧 logout_handled 抑制）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnEvent {
    Claimed,
    Released,
}

/// 生产装配：WS 启用（端口≠0）则起独立 ws-server 线程（内含 tokio runtime），
/// 返回句柄（快照推送端 + 操控令牌观测）供主循环使用；禁用（端口 0）返回 None。
/// bridge_vault=外桥保险库只读句柄（D-022 工坊 bridge-config 展示；主循环在
/// net::init 之前装配 BridgeOutbox，取出 Arc 传入——None=桥未启用）
pub(crate) fn init(
    config: &crate::config::Config,
    intents: IntentSink,
    bridge_vault: Option<Arc<std::sync::RwLock<crate::bridges::BridgeVault>>>,
) -> Option<WsHandle> {
    if config.ws_port == 0 {
        tracing::info!("[ws] CODEWORLD_WS_PORT=0，WS 快照推送禁用（纯终端模式）");
        return None;
    }
    let (tx, _) = broadcast::channel(SNAPSHOT_CHANNEL_CAP);
    let port = config.ws_port;
    // 头像资产登记表（D-017）：启动扫目录重建（坏文件 warn 跳过不拖垮世界）
    let avatars = Arc::new(AvatarRegistry::load(Path::new(AVATARS_DIR)));
    // 角色列表共享槽（D-021d）：初值空表，main 装配后立即种子一次
    //（load_players_initial 在 net::init 之前完成——见 main()），此后每拍刷新
    let roster: Arc<std::sync::Mutex<Vec<RosterEntry>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // 会话令牌表（D-021f）：初值空表，tick 侧选角/创建成功时写入 token→name。
    // 生命周期=进程：重启即清空=全部失效（重新登录页选角，单机阶段合理默认）
    let sessions: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>> =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    // 逃生门：CODEWORLD_ME 非空 = 开发者 env 锁定场景，保留旧"第一连接=操控端"
    //（loginSkip 不重连也保有操控）；普通玩家走凭据认领（无 token=纯观战）
    let legacy_first_conn = std::env::var("CODEWORLD_ME")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let state = Arc::new(WsState {
        snapshots: tx.clone(),
        intents,
        ctrl_conn: tokio::sync::watch::channel(None).0,
        sessions: sessions.clone(),
        legacy_first_conn,
        conn_ids: AtomicU64::new(1),
        avatars,
        roster: roster.clone(),
        players_dir: std::path::PathBuf::from(&config.players_dir),
        bridge_vault,
        // D-022b：登记表路径与 BridgeOutbox 装配同源（config.bridge_config，
        // 默认 bridges.json 与 codeworld.db 同级落世界工作目录）
        bridge_config_path: std::path::PathBuf::from(&config.bridge_config),
    });
    let handle = WsHandle {
        snapshots: tx,
        ctrl_conn: state.ctrl_conn.subscribe(),
        sessions,
        roster,
    };
    let spawned = std::thread::Builder::new()
        .name("ws-server".to_string())
        .spawn(move || run_ws_thread(port, state));
    match spawned {
        Ok(_) => {
            tracing::info!(
                port,
                "[ws] WS 快照推送已启动（浏览器打开 http://localhost:{port} 看世界；操控权凭会话令牌认领，选角后刷新不丢）"
            );
            Some(handle)
        }
        Err(e) => {
            tracing::warn!(error = %e, "[ws] WS 线程启动失败，降级纯终端模式（世界不受影响）");
            None
        }
    }
}

/// ws-server 线程体：自建 tokio current_thread runtime；监听失败/线程 panic
/// 都只 warn 收口——世界主循环（std 线程）完全不受影响。
fn run_ws_thread(port: u16, state: Arc<WsState>) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                tracing::warn!(error = %e, "[ws] tokio runtime 创建失败，WS 不可用");
                return;
            }
        };
        rt.block_on(async move {
            // 绑定回环地址：仅本机浏览器可访问（MVP 验收物；局域网观战留后续）
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => {
                    if let Err(e) = serve(
                        listener,
                        state,
                        std::path::PathBuf::from(STATIC_DIR),
                        std::future::pending::<()>(),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "[ws] 服务器异常退出（终端模式继续运行）");
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        port,
                        error = %e,
                        "[ws] 端口监听失败（占用/权限），WS 推送不可用，终端模式继续运行"
                    );
                }
            }
        });
    }));
    if let Err(panic) = result {
        let msg = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        tracing::warn!(panic = %msg, "[ws] WS 线程异常退出，世界主循环不受影响");
    }
}

/// 起服务器（axum：/ws 快照广播、/upload/avatar 收图（D-017）、/assets 原图静态
/// 与 ServeDir 静态验收页一把抓）。
/// shutdown future 完成即优雅停机（生产用永不完成的 pending，测试用 oneshot）。
pub(crate) async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<WsState>,
    static_dir: std::path::PathBuf,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/upload/avatar", post(upload_avatar))
        // D-022 造物工坊 HTTP 通道：保存=写 players/<名>.wat（热进场靠目录轮询）、
        // 读取=编辑器加载全文、删除=下拍残影下线；bridge-config=外桥端点表
        // 读+写（D-022b 可编辑：POST 整批校验+原子写盘+内存同步）
        .route(
            "/api/player-code",
            post(player_code_write)
                .get(player_code_read)
                .delete(player_code_delete),
        )
        .route("/api/bridge-config", get(bridge_config).post(bridge_config_save))
        .layer(axum::extract::DefaultBodyLimit::max(
            avatars::MAX_AVATAR_PNG_BYTES * 2, // 2MB PNG + multipart 边界开销余量；护栏仍由 store_upload 按 PNG 字节判
        ))
        .with_state(state.clone())
        // /assets：上传原图的 HTTP 直取口（前端按 URL 渲染原图；掩码走 WS 消息）。
        // nest_service 剥 /assets 前缀：GET /assets/{id}.png -> 资产目录内 {id}.png
        .nest_service("/assets", ServeDir::new(state.avatars.dir()))
        .fallback_service(ServeDir::new(static_dir));
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

/// WS 升级入口：连接建立后进入快照广播循环。URL ?token= 是会话令牌
///（D-021f 凭据认领：在 sessions 表=操控端，不在=观战+stale 通知；缺席=
/// legacy 逃生门或纯观战）。axum 0.7 Query 提取器内建，零新依赖
async fn ws_handler(
    ws: WebSocketUpgrade,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    State(state): State<Arc<WsState>>,
) -> Response {
    let token = params.get("token").cloned();
    ws.on_upgrade(move |socket| handle_socket(socket, state, token))
}

// ---- D-017 收图通道（POST /upload/avatar + avatar_assets 下行）----

/// avatar_assets 下行单资产条目（掩码=128x128 布尔表原样；约 32KB JSON/资产，
/// 资产数预期少可全量——资产多时改增量，注释锚）
#[derive(Debug, Serialize)]
struct AvatarAssetMsg<'a> {
    asset_id: &'a str,
    /// 原图 HTTP 直取路径（/assets 挂 ServeDir）
    url: String,
    /// 原图尺寸（前端按纵横比渲染原图）
    w: u32,
    h: u32,
    /// 128x128 布尔表（行主序 0/1，16384 项）
    mask: &'a [u8],
}

/// 组装 avatar_assets 消息 JSON（登记表全量快照；连接建立时一次性主动下发）
fn avatar_assets_json(registry: &AvatarRegistry) -> String {
    let snapshot = registry.list();
    let assets: Vec<AvatarAssetMsg> = snapshot
        .iter()
        .map(|a| AvatarAssetMsg {
            asset_id: &a.asset_id,
            url: format!("{}{}.png", avatars::AVATAR_URL_PREFIX, a.asset_id),
            w: a.orig_w,
            h: a.orig_h,
            mask: &a.mask.solid,
        })
        .collect();
    serde_json::to_string(&serde_json::json!({ "type": "avatar_assets", "assets": assets }))
        .unwrap_or_else(|_| r#"{"type":"avatar_assets","assets":[]}"#.to_string())
}

/// 组装 player_roster 消息 JSON（D-021d 登录/选角页：共享槽全量快照；连接建立
/// 时一次性主动下发，characters 数组恒有——零角色=空数组，前端不必判消息缺失）
fn roster_json(roster: &std::sync::Mutex<Vec<RosterEntry>>) -> String {
    let list = roster
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    serde_json::to_string(&serde_json::json!({ "type": "player_roster", "characters": list }))
        .unwrap_or_else(|_| r#"{"type":"player_roster","characters":[]}"#.to_string())
}

/// POST /upload/avatar（D-017）：multipart/form-data 收 PNG 角色图。
/// 字段：file=PNG 字节（必填）/ name=角色名（必填）/ height_world=世界高度格数
/// （选填默认 2.0，域钳制 0.05..=25.0）。HTTP 恒 200，成败看 JSON ok 字段；
/// 非 multipart 请求由 axum 内建 rejection 拒绝（415/400 文本体）。
/// 校验链：大小护栏->解码->降采样->连通校验，全过才落盘+登记（失败零副作用）
async fn upload_avatar(State(state): State<Arc<WsState>>, mut mp: Multipart) -> Response {
    let mut png: Option<Vec<u8>> = None;
    let mut name: Option<String> = None;
    let mut height_world: Option<f64> = None;
    loop {
        match mp.next_field().await {
            Ok(Some(field)) => match field.name().unwrap_or("") {
                "file" => match field.bytes().await {
                    Ok(b) => png = Some(b.to_vec()),
                    Err(e) => {
                        tracing::warn!(error = %e, "[upload] file 字段读字节失败（bad_json）");
                        return reject_json(avatars::RejectReason::BadJson);
                    }
                },
                "name" => name = Some(field.text().await.unwrap_or_default()),
                "height_world" => match field.text().await.unwrap_or_default().parse::<f64>() {
                    Ok(v) => height_world = Some(v),
                    Err(_) => {
                        tracing::warn!("[upload] height_world 非数值（bad_json）");
                        return reject_json(avatars::RejectReason::BadJson);
                    }
                },
                _ => {} // 未知字段忽略（协议向前兼容）
            },
            Ok(None) => break, // 字段遍历完毕
            Err(e) => {
                tracing::warn!(error = %e, "[upload] multipart 解析失败（bad_json）");
                return reject_json(avatars::RejectReason::BadJson);
            }
        }
    }
    let (Some(png_bytes), Some(name)) = (png, name) else {
        tracing::warn!("[upload] 缺 file/name 字段（bad_json）");
        return reject_json(avatars::RejectReason::BadJson);
    };
    match avatars::store_upload(
        &state.avatars,
        UploadInput {
            png: &png_bytes,
            name: &name,
            height_world,
        },
    ) {
        Ok(asset) => {
            tracing::info!(asset_id = %asset.asset_id, "[upload] 收图成功（HTTP 200 ok:true）");
            Json(serde_json::json!({ "ok": true, "asset_id": asset.asset_id })).into_response()
        }
        Err(reason) => {
            tracing::info!(reason = reason.wire(), "[upload] 收图拒绝（HTTP 200 ok:false）");
            reject_json(reason)
        }
    }
}

/// 拒绝响应体（{ok:false, reason}；HTTP 恒 200，成败看 ok 字段）
fn reject_json(reason: avatars::RejectReason) -> Response {
    Json(serde_json::json!({ "ok": false, "reason": reason.wire() })).into_response()
}

// ---- D-022 造物工坊 HTTP 通道（/api/player-code 三动作 + /api/bridge-config）----

/// 工坊保存代码长度上限（防滥用护栏：256KB；玩家代码都是小文件，卡面定值）
pub(crate) const PLAYER_CODE_MAX_BYTES: usize = 256 * 1024;

/// 工坊通道统一拒绝响应体（{ok:false, reason:string}；HTTP 恒 200，成败看 ok）
fn workshop_reject_json(reason: &str) -> Response {
    Json(serde_json::json!({ "ok": false, "reason": reason })).into_response()
}

/// POST /api/player-code 请求体（JSON：角色名 + WAT 全文）
#[derive(Debug, Deserialize)]
struct PlayerCodeBody {
    name: String,
    code: String,
}

/// POST /api/player-code（D-022 工坊保存=进场通道）：JSON {name, code} ->
/// sanitize_name（唯一权威闸门 crate::onboarding::sanitize_name）-> 写
/// players/<名>.wat（覆盖写）-> 世界下拍目录轮询自动拉进场。
/// **不写 me.txt**（造物/角色文件管理≠操控声明）；成功登记本机角色档案
///（与 create_player 成功路径同款——工坊角色列表数据源=player_roster，
/// 档案不登记则列表看不见它）。返回 {ok:true} / {ok:false,reason}。
async fn player_code_write(State(state): State<Arc<WsState>>, body: axum::body::Bytes) -> Response {
    let req: PlayerCodeBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "[workshop] 保存请求 JSON 解析失败（BAD_JSON）");
            return workshop_reject_json("BAD_JSON");
        }
    };
    let name = match crate::onboarding::sanitize_name(&req.name) {
        Ok(n) => n,
        Err(e) => {
            tracing::info!(reason = e.wire(), "[workshop] 保存拒绝：名字闸门");
            return workshop_reject_json(e.wire());
        }
    };
    if req.code.len() > PLAYER_CODE_MAX_BYTES {
        tracing::info!(len = req.code.len(), "[workshop] 保存拒绝：代码超上限");
        return workshop_reject_json("CODE_TOO_LONG");
    }
    let path = state.players_dir.join(format!("{name}.wat"));
    // 目录兜底创建（正常启动时 players/ 已在；测试/手删目录场景防 IO_ERROR）
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, req.code.as_bytes()) {
        Ok(()) => {
            tracing::info!(
                name = %name,
                len = req.code.len(),
                "[workshop] 代码已保存（写 players/<名>.wat；新文件=世界下拍进场）"
            );
            // 本机角色档案登记（create_player 成功路径同款；重复不重复追加）
            crate::onboarding::append_my_character(&state.players_dir, &name);
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "[workshop] 保存写盘失败（IO_ERROR）");
            workshop_reject_json("IO_ERROR")
        }
    }
}

/// GET /api/player-code?name=（D-022 工坊加载）：读 players/<名>.wat 全文 ->
/// {ok:true,code} / NAME_UNKNOWN（无此角色）/ NAME_*（名字闸门）/
/// CODE_TOO_LONG（落盘文件超上限，读前 metadata 护栏，与写路径对称）/ IO_ERROR。
async fn player_code_read(
    State(state): State<Arc<WsState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(raw) = params.get("name") else {
        return workshop_reject_json("NAME_EMPTY");
    };
    let name = match crate::onboarding::sanitize_name(raw) {
        Ok(n) => n,
        Err(e) => {
            tracing::info!(reason = e.wire(), "[workshop] 读取拒绝：名字闸门");
            return workshop_reject_json(e.wire());
        }
    };
    let path = state.players_dir.join(format!("{name}.wat"));
    // 读护栏（与写路径 CODE_TOO_LONG 对称）：落盘文件超上限先拒，不整读进内存/
    // 响应体；metadata 失败（无此文件等）不拦，交给下方读分支按语义回
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > PLAYER_CODE_MAX_BYTES as u64 {
            tracing::info!(name = %name, len = meta.len(), "[workshop] 读取拒绝：文件超上限（CODE_TOO_LONG）");
            return workshop_reject_json("CODE_TOO_LONG");
        }
    }
    match std::fs::read_to_string(&path) {
        Ok(code) => Json(serde_json::json!({ "ok": true, "code": code })).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(name = %name, "[workshop] 读取拒绝：无此角色（NAME_UNKNOWN）");
            workshop_reject_json("NAME_UNKNOWN")
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "[workshop] 读取失败（IO_ERROR）");
            workshop_reject_json("IO_ERROR")
        }
    }
}

/// DELETE /api/player-code?name=（D-022 工坊删除）：删 players/<名>.wat——
/// 世界下拍残影下线（既有语义，在线角色删除同理）。**不动 me.txt**。
/// 不存在=NAME_UNKNOWN。
async fn player_code_delete(
    State(state): State<Arc<WsState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(raw) = params.get("name") else {
        return workshop_reject_json("NAME_EMPTY");
    };
    let name = match crate::onboarding::sanitize_name(raw) {
        Ok(n) => n,
        Err(e) => {
            tracing::info!(reason = e.wire(), "[workshop] 删除拒绝：名字闸门");
            return workshop_reject_json(e.wire());
        }
    };
    let path = state.players_dir.join(format!("{name}.wat"));
    match std::fs::remove_file(&path) {
        Ok(()) => {
            tracing::info!(name = %name, "[workshop] 角色文件已删除（残影下线归世界轮询）");
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(name = %name, "[workshop] 删除拒绝：无此角色（NAME_UNKNOWN）");
            workshop_reject_json("NAME_UNKNOWN")
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "[workshop] 删除失败（IO_ERROR）");
            workshop_reject_json("IO_ERROR")
        }
    }
}

/// GET /api/bridge-config（D-022 工坊外桥展示；D-022b 升级可编辑回填）：
/// vault 装配 -> {ok,enabled:true,ports:[{port,url,note}]}（端口升序）；
/// 未装配 -> {ok,enabled:false,ports:[]}（前端黄字提示桥未启用，仍可编辑保存
/// ——写盘即用，桥下次带 CODEWORLD_BRIDGE=1 启动生效）。
async fn bridge_config(State(state): State<Arc<WsState>>) -> Response {
    let (enabled, views) = match &state.bridge_vault {
        Some(vault) => {
            let views = vault
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .views();
            (true, views)
        }
        None => (false, Vec::new()),
    };
    let ports: Vec<serde_json::Value> = views
        .into_iter()
        .map(|v| serde_json::json!({ "port": v.port, "url": v.url, "note": v.note }))
        .collect();
    Json(serde_json::json!({ "ok": true, "enabled": enabled, "ports": ports })).into_response()
}

/// POST /api/bridge-config 请求体（JSON：端点整批替换表。headers 凭证字段不进
/// 请求体——写盘按 port 合并保留旧文件已有值；未知字段 serde 默认忽略）
#[derive(Debug, Deserialize)]
struct BridgeConfigBody {
    ports: Vec<BridgePortEntryBody>,
}

/// POST /api/bridge-config 单端点条目（note 可缺省）
#[derive(Debug, Deserialize)]
struct BridgePortEntryBody {
    port: i32,
    url: String,
    #[serde(default)]
    note: String,
}

/// POST /api/bridge-config（D-022b 工坊外桥可编辑）：JSON {ports:[{port,url,note}]}
/// -> 整批校验（bridges::check_endpoint 唯一规则源，任一条非法=整单拒绝带 index
/// 定位）-> 原子写 bridges.json（headers 按 port 合并保留旧凭证）-> vault 装配时
/// 内存同步（写锁 reload_if_changed——下一拍 bridge_send 按新表解析）。vault 未
/// 装配也受理（写盘即用，桥下次启用生效）。返回 {ok:true} / {ok:false,reason,index?}。
async fn bridge_config_save(State(state): State<Arc<WsState>>, body: axum::body::Bytes) -> Response {
    let req: BridgeConfigBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "[bridge-config] 保存请求 JSON 解析失败（BAD_JSON）");
            return workshop_reject_json("BAD_JSON");
        }
    };
    let entries: Vec<crate::bridges::BridgeEntryInput> = req
        .ports
        .into_iter()
        .map(|p| crate::bridges::BridgeEntryInput {
            port: p.port,
            url: p.url,
            note: p.note,
        })
        .collect();
    tracing::info!(count = entries.len(), "[bridge-config] 收到保存请求");
    match crate::bridges::save_endpoint_entries(&state.bridge_config_path, &entries) {
        Ok(()) => {
            // 内存同步（vault 装配时）：盘上 mtime 已变，reload_if_changed 重载
            // 新表（主循环每拍热重载走同一把 RwLock，两边看同一份账）
            let synced = match &state.bridge_vault {
                Some(vault) => vault
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .reload_if_changed(),
                None => false,
            };
            tracing::info!(
                path = %state.bridge_config_path.display(),
                count = entries.len(),
                vault_synced = synced,
                "[bridge-config] 端点表已写盘（内存同步见 vault_synced：false=本拍未检出 mtime 变化，主循环每拍热重载会兜底重载，语义不变）"
            );
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(rej) => {
            tracing::info!(reason = rej.reason, index = ?rej.index, "[bridge-config] 保存整单拒绝");
            let mut body = serde_json::json!({ "ok": false, "reason": rej.reason });
            if let Some(i) = rej.index {
                body["index"] = serde_json::json!(i);
            }
            Json(body).into_response()
        }
    }
}

/// 单连接循环：下行推快照广播，上行解析意图入共享队列（仅操控连接）。
/// 循环退出即连接清理（socket/rx 随任务结束释放，无泄漏）。
/// 操控权认领（D-021f 凭据制）：token 命中会话表=操控端（踢旧留新）；
/// token 不命中=观战+stale 通知；无 token 走 legacy 逃生门（第一连接认领）
/// 或纯观战。#024 独占不变：任意时刻仍只有一个操控连接。
async fn handle_socket(socket: WebSocket, state: Arc<WsState>, url_token: Option<String>) {
    let conn_id = state.conn_ids.fetch_add(1, Ordering::SeqCst);
    let mut ctrl_rx = state.ctrl_conn.subscribe();
    // 认领判定：钥匙=URL token（凭据者得）；my_token=本连接验证过的令牌
    //（登出时从表移除用）
    let mut my_token: Option<String> = None;
    let mut controller = false;
    if let Some(tok) = &url_token {
        let hit = state
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(tok)
            .cloned();
        if let Some(player) = hit {
            my_token = Some(tok.clone());
            controller = true;
            // 踢旧沿：send(Some(本连接 id))——旧操控端 select! 的 watch 分支
            // 监听到变化、比对自己已不是持有者，单播 control_revoked{taken} 降观战
            let _ = state.ctrl_conn.send(Some(conn_id));
            tracing::info!(conn_id, player = %player, "[ws] 客户端连接（操控连接：凭 token 认领）");
        } else {
            tracing::info!(conn_id, "[ws] 客户端连接（观战连接：token 已失效）");
        }
    } else if state.legacy_first_conn {
        // 逃生门（CODEWORLD_ME env 锁定的开发者场景）：保留旧"第一连接=操控端"
        //——watch 当前无操控者时认领（ws 线程是 current_thread 单线程 runtime，
        // 判定与 send 之间无 await，无竞态窗）
        if state.ctrl_conn.borrow().is_none() {
            controller = true;
            let _ = state.ctrl_conn.send(Some(conn_id));
            tracing::info!(conn_id, "[ws] 客户端连接（操控连接：legacy 第一连接认领）");
        } else {
            tracing::info!(conn_id, "[ws] 客户端连接（观战连接：操控权已被占用）");
        }
    } else {
        tracing::info!(conn_id, "[ws] 客户端连接（观战连接：无会话令牌）");
    }
    let mut rx = state.snapshots.subscribe();
    let (mut sender, mut receiver) = socket.split();
    // D-017：连接建立即主动推一条 avatar_assets（一次性全量，先于任何快照帧；
    // 前端缓存 asset_id->{img,mask} 做调试面板）。发送失败=socket 已死，循环首拍
    // 自然 break 走清理
    let assets_json = avatar_assets_json(&state.avatars);
    if sender.send(Message::Text(assets_json)).await.is_err() {
        tracing::debug!("[ws] avatar_assets 首帧发送失败（连接已断，走常规清理）");
    }
    // D-021d：资产后紧接着发 player_roster（角色列表，登录页数据源；一次性
    // 全量主动下发，与 avatar_assets 同款时机）。内容=下发时点状态，允许陈旧
    //（前端不做实时刷新）；发送失败=连接已断，循环首拍自然 break 走清理
    let roster_json = roster_json(&state.roster);
    if sender.send(Message::Text(roster_json)).await.is_err() {
        tracing::debug!("[ws] player_roster 发送失败（连接已断，走常规清理）");
    }
    // D-021f：stale 通知——带了 token 但不在会话表（过期/登出后/伪造），
    // 立即单播告知观战身份（前端清本地凭据并提示重新登录页选角）
    if url_token.is_some() && !controller {
        let stale = r#"{"type":"control_revoked","reason":"stale"}"#.to_string();
        if sender.send(Message::Text(stale)).await.is_err() {
            tracing::debug!("[ws] control_revoked(stale) 发送失败（连接已断，走常规清理）");
        }
    }
    loop {
        tokio::select! {
            // 踢旧沿（D-021f）：操控权易主且自己不再是持有者 -> 降观战（连接不断，
            // 下行快照照收）。reason=taken；自己认领引发的沿（新值=自己）不触发
            _ = ctrl_rx.changed() => {
                let now = *ctrl_rx.borrow_and_update();
                if controller && now != Some(conn_id) {
                    tracing::info!(conn_id, "[ws] 操控已被新凭据接管（本连接降观战，下行照收）");
                    let taken = r#"{"type":"control_revoked","reason":"taken"}"#.to_string();
                    if sender.send(Message::Text(taken)).await.is_err() {
                        break; // 发送失败=连接已死，走常规清理（controller 已降，不误释操控）
                    }
                    controller = false;
                }
            }
            recv = rx.recv() => match recv {
                Ok(json) => {
                    if sender.send(Message::Text(json)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "[ws] 客户端消费不及，跳过旧快照帧（后值覆盖语义）");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = receiver.next() => match msg {
                Some(Ok(Message::Ping(payload))) => {
                    let _ = sender.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Text(text))) => {
                    match parse_client_message(&text) {
                        // D-021d（v2.7）：会话动作任意连接生效——登出后的浏览器
                        // 是观战连接，登录/选角/新建必须能用（登录页=观战连接的
                        // 进场通道）。落地全在 tick 侧（名字拒绝走事件回执，
                        // R1 改法 B 同款），本层只转发意图。
                        // D-021f：本层颁发会话令牌（uuid）——Intent 带上（tick 侧
                        // 成功后绑定 token→name）+ 当场单播 session_token 帧
                        //（不等 tick 结果：拒绝路径该 token 永不绑定自然无效，
                        // 前端只在成功回执后才落盘）
                        Ok(ClientMessage::CreatePlayer { name, template }) => {
                            let token = uuid::Uuid::new_v4().simple().to_string();
                            tracing::info!(
                                name = %name,
                                template = %template,
                                "[onboarding] WS 收到创建角色消息（任意连接生效，颁发会话令牌）"
                            );
                            let announce = serde_json::to_string(&serde_json::json!({
                                "type": "session_token", "token": token, "name": name
                            }))
                            .unwrap_or_else(|_| r#"{"type":"session_token"}"#.to_string());
                            state.intents.send(crate::input::Intent::CreatePlayer {
                                name,
                                template,
                                token: token.clone(),
                            });
                            if sender.send(Message::Text(announce)).await.is_err() {
                                break;
                            }
                        }
                        Ok(ClientMessage::SelectPlayer { name }) => {
                            tracing::info!(
                                name = %name,
                                "[onboarding] WS 收到选角消息（任意连接生效，颁发会话令牌）"
                            );
                            let token = uuid::Uuid::new_v4().simple().to_string();
                            let announce = serde_json::to_string(&serde_json::json!({
                                "type": "session_token", "token": token, "name": name
                            }))
                            .unwrap_or_else(|_| r#"{"type":"session_token"}"#.to_string());
                            state.intents.send(crate::input::Intent::SelectPlayer {
                                name,
                                token: token.clone(),
                            });
                            if sender.send(Message::Text(announce)).await.is_err() {
                                break;
                            }
                        }
                        Ok(other) => {
                            // 其余上行仅操控连接生效（#024 独占：非操控端丢弃）
                            if !controller {
                                tracing::warn!("[ws] 观战连接的上行消息已丢弃（非操控端）");
                                continue;
                            }
                            match other {
                                ClientMessage::Move { x, y } => {
                                    state.intents.send(crate::input::Intent::WsMove { x, y });
                                }
                                ClientMessage::Attack { x, y } => {
                                    state.intents.send(crate::input::Intent::WsAttack { x, y });
                                }
                                ClientMessage::Shoot { x, y } => {
                                    state.intents.send(crate::input::Intent::WsShoot { x, y });
                                }
                                ClientMessage::Skill { slot } => {
                                    // 技能栏（v2.5，D-020a）：槽位已在解析层校验（1..=4），
                                    // 这里只转发意图——spawn 语义在 tick 侧旁路落地
                                    state.intents.send(crate::input::Intent::Skill { slot });
                                }
                                ClientMessage::Stop => {
                                    state.intents.send(crate::input::Intent::Stop);
                                }
                                ClientMessage::Logout { choice } => {
                                    // #025 登出处置：意图入共享队列（与键盘/鼠标单入口），
                                    // 处置三选一由 tick 侧状态机落地；本层做两件令牌语义——
                                    // ①D-021f：登出=会话终结，本连接验证过的 token 从表
                                    // 移除（不得复用）；②释放操控（Released 沿），
                                    // 本连接降级观战：下行快照继续，上行丢弃
                                    tracing::info!(
                                        choice = choice.wire_name(),
                                        "[dispose] WS 收到登出消息（会话终结+令牌释放，连接降级观战）"
                                    );
                                    state.intents.send(crate::input::Intent::Logout { choice });
                                    if let Some(tok) = my_token.take() {
                                        state
                                            .sessions
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .remove(&tok);
                                        tracing::info!("[ws] 登出：会话令牌已失效（token 不得复用）");
                                    }
                                    let _ = state.ctrl_conn.send(None);
                                    controller = false;
                                }
                                // 会话动作已在上面分流（任意连接生效），此处不可达
                                ClientMessage::CreatePlayer { .. } | ClientMessage::SelectPlayer { .. } => {}
                            }
                        }
                        Err(reason) => {
                            tracing::warn!(raw = %text, reason, "[ws] 非法上行消息丢弃");
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // Binary 等：忽略
                Some(Err(e)) => {
                    tracing::debug!(error = %e, "[ws] 连接收发异常，断开清理");
                    break;
                }
            },
        }
    }
    if controller && *state.ctrl_conn.borrow() == Some(conn_id) {
        // 自己仍是操控端时断开 = 操控权释放（Released 沿）。双门守卫：①本地
        // controller 标记（踢旧沿已消费的常规路径）②watch 当前值=自己（防
        // "踢旧沿待处理 + socket 同拍退出"竞态——select! 多分支就绪随机选，
        // 若只查标记，清理段会以过期身份发 None 错拆新操控端。borrow 与 send
        // 间无 await，current_thread 单线程下原子）
        let _ = state.ctrl_conn.send(None);
        tracing::info!("[ws] 操控连接断开（操控权已释放）");
    } else {
        tracing::info!("[ws] 客户端断开（连接资源已清理）");
    }
}

/// tick 侧推送（std 主循环每拍调用）：无观众不组装（零客户端零开销），
/// 序列化成功推广播通道；无接收者 send 报错属正常（忽略）。
/// player_events/events_cursor（v2.5，D-020a）由调用点先排干传入（与
/// attack_events 同款"不管有没有观众都取走"——事件不跨拍滞留防无头世界
/// 缓冲无界增长；无观众=取走即丢，事件是新闻不是档案）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_snapshot(
    tx: &broadcast::Sender<String>,
    world: &WorldState,
    caption: &str,
    viewport: &Viewport,
    players: &PlayerDirectory,
    controlled: Option<EntityId>,
    attack_events: &[crate::AttackEventRecord],
    field_forming_ticks: u64,
    player_events: Vec<crate::events::PlayerEvent>,
    events_cursor: u64,
) {
    if tx.receiver_count() == 0 {
        return;
    }
    let snapshot = build_snapshot(
        world,
        caption,
        viewport,
        players,
        controlled,
        attack_events,
        field_forming_ticks,
        player_events,
        events_cursor,
    );
    match serde_json::to_string(&snapshot) {
        Ok(json) => {
            if tx.send(json).is_err() {
                tracing::debug!("[ws] 本拍无快照接收者（浏览器未连接，正常）");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "[ws] 快照序列化失败，本帧丢弃（世界不受影响）");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::intent_channel;
    use crate::AttackEventRecord;
    use codeworld_core::entity::{Appearance, Entity, PlayerId};

    /// 造一个测试实体（快照测试助手）
    fn put_entity(
        world: &mut WorldState,
        code_hash: &str,
        glyph: &str,
        color: &str,
        pos: (f64, f64),
    ) -> (EntityId, PlayerId) {
        let id = EntityId::new();
        let owner = PlayerId::new();
        world.entities.insert(
            id,
            Entity::new(
                id,
                owner,
                code_hash.to_string(),
                pos,
                Appearance::new(glyph.to_string(), color.to_string(), 1.0),
                0,
                1000,
            ),
        );
        (id, owner)
    }

    fn default_viewport() -> Viewport {
        Viewport::new((0.0, 0.0), 50.0, 25.0, 50, 80)
    }

    fn build(
        world: &WorldState,
        players: &PlayerDirectory,
        controlled: Option<EntityId>,
    ) -> serde_json::Value {
        let snap = build_snapshot(world, "cap", &default_viewport(), players, controlled, &[], 3, Vec::new(), 0);
        serde_json::to_value(&snap).expect("快照应可序列化")
    }

    // === v2.0 攻击命中事件（D-017 协议小刀）===

    #[test]
    fn snapshot_carries_attack_events_with_target_position() {
        // 规则（v2.0）：本拍结算成功的命中事件随快照广播——attacker/target
        // 转 UUID 字符串（与 entities.id 同格式可 join），target 位置=结算时刻
        // 实况坐标；无事件的拍 attacks 为空数组（字段恒有，前端零可选分支）
        let mut world = WorldState::new();
        let (a, _) = put_entity(&mut world, "attacker", "A", "red", (0.0, 0.0));
        let (t, _) = put_entity(&mut world, "defender", "D", "cyan", (1.0, 2.0));
        let dir = PlayerDirectory::disabled();
        let events = vec![AttackEventRecord {
            attacker: a,
            target: t,
            target_pos: (1.0, 2.0),
        }];
        let snap = build_snapshot(&world, "cap", &default_viewport(), &dir, None, &events, 3, Vec::new(), 0);
        let json = serde_json::to_value(&snap).expect("快照应可序列化");
        let atk = &json["attacks"];
        assert_eq!(atk.as_array().map(Vec::len), Some(1), "恰一条命中事件");
        assert_eq!(atk[0]["attacker"], a.0.to_string(), "攻方 id=UUID 字符串");
        assert_eq!(atk[0]["target"], t.0.to_string(), "目标 id=UUID 字符串");
        assert_eq!(atk[0]["x"], 1.0, "目标位置 x=结算时刻实况坐标");
        assert_eq!(atk[0]["y"], 2.0, "目标位置 y=结算时刻实况坐标");
    }

    #[test]
    fn snapshot_attacks_empty_array_when_no_hits() {
        // 规则（v2.0）：无命中的拍 attacks=[] 恒有字段——前端不必判空键；
        // 空快照默认可序列化（既有 build 助手已带 &[]，此测锚定字段恒在）
        let mut world = WorldState::new();
        put_entity(&mut world, "attacker", "A", "red", (0.0, 0.0));
        let dir = PlayerDirectory::disabled();
        let snap = build_snapshot(&world, "cap", &default_viewport(), &dir, None, &[], 3, Vec::new(), 0);
        let json = serde_json::to_value(&snap).expect("快照应可序列化");
        assert_eq!(
            json["attacks"].as_array().map(Vec::len),
            Some(0),
            "attacks 空数组恒有"
        );
    }

    // === v2.5 玩家事件流 + 技能栏（D-020a，t_06b4efc6）===

    #[test]
    fn t20_skill_message_parses_valid_slots_and_rejects_bad() {
        // 上行解析锚（D-020b 域扩到 1..=9）：skill 消息 slot=1..9 合法；
        // 0/10/99/缺参拒绝（解析层丢弃，Err 带原因）。serde tag lowercase
        //（wire 形如 {"type":"skill","slot":2}）
        for slot in 1u8..=crate::skills::SKILL_SLOTS_MAX as u8 {
            let msg = parse_client_message(&format!(r#"{{"type":"skill","slot":{slot}}}"#))
                .unwrap_or_else(|e| panic!("slot {slot} 应合法：{e}"));
            assert_eq!(msg, ClientMessage::Skill { slot }, "slot {slot} 解析保真");
        }
        for bad in [0u8, 10, 99] {
            let err = parse_client_message(&format!(r#"{{"type":"skill","slot":{bad}}}"#))
                .expect_err("非法槽位应拒绝");
            assert!(err.contains("槽位非法"), "拒绝原因应明示槽位域：{err}");
        }
        assert!(
            parse_client_message(r#"{"type":"skill"}"#).is_err(),
            "缺 slot 字段应拒绝"
        );
    }

    // === D-021b create_player（进场仪式上行）===

    #[test]
    fn t21_create_player_message_parses_name_and_template() {
        // 契约（v2.6）：{"type":"create_player","name":..,"template":..} 三字段
        // 齐全且名字过 sanitize 才放行；模板 id 只查非空（业务拒绝归 tick 侧）
        let msg = parse_client_message(
            r#"{"type":"create_player","name":"土豆","template":"grazer"}"#,
        )
        .expect("合法创建应放行");
        assert_eq!(
            msg,
            ClientMessage::CreatePlayer {
                name: "土豆".to_string(),
                template: "grazer".to_string(),
            }
        );
    }

    #[test]
    fn t21_create_player_rejects_bad_names_at_parse_layer() {
        // R1 复审改法 B 后的解析层语义：名字格式**不再**在解析层拒绝——
        // 解析层 Err 分支=静默丢弃（无事件回执），前端 busy 锁会挂死。
        // 全部名字拒绝（含格式类）统一走 tick 侧 create_player_rejected
        // 事件（见 main.rs create_player_* 测试与 onboarding::sanitize_name）。
        // 本测锚定新契约：危险名字在解析层**放行**（回执归 tick 侧）。
        for tricky in [
            r#"{"type":"create_player","name":"","template":"blank"}"#,
            r#"{"type":"create_player","name":"   ","template":"blank"}"#,
            r#"{"type":"create_player","name":"a/b","template":"blank"}"#,
            r#"{"type":"create_player","name":"a.wat","template":"blank"}"#,
            r#"{"type":"create_player","name":"..","template":"blank"}"#,
            r#"{"type":"create_player","name":"乔丹·科比","template":"blank"}"#,
            r#"{"type":"create_player","name":"１２３","template":"blank"}"#,
        ] {
            assert!(
                parse_client_message(tricky).is_ok(),
                "解析层应放行（拒绝归 tick 侧事件回执）：{tricky}"
            );
        }
        // 缺字段拒绝（serde 必填）——协议形状错误仍归解析层
        assert!(parse_client_message(r#"{"type":"create_player","name":"ok"}"#).is_err());
        assert!(parse_client_message(r#"{"type":"create_player"}"#).is_err());
    }

    #[test]
    fn t20_snapshot_carries_player_events_cursor_and_lifespan_fields() {
        // 快照锚（v2.5）：player_events/events_cursor 顶层恒有；实体带
        // born_tick/lifespan_ticks（控制台造物列表算剩余寿命用）；事件 JSON
        // 形状 kind 标签在（前端按 kind 分色）
        let mut world = WorldState::new();
        let (eid, _) = put_entity(&mut world, "creation", "T", "cyan", (5.0, 0.0));
        world
            .entities
            .get_mut(&eid)
            .unwrap()
            .set_lifespan_ticks(30);
        let dir = PlayerDirectory::disabled();
        let mut sink = crate::events::EventSink::new();
        sink.emit_unowned(
            world.tick(),
            crate::events::EventBody::CompileFailed {
                name: "badwat".into(),
                error: "unexpected token".into(),
            },
        );
        let (events, cursor) = sink.take_frame(None);
        assert_eq!(cursor, 1, "一事件后游标=1");
        let snap = build_snapshot(&world, "cap", &default_viewport(), &dir, None, &[], 3, events, cursor);
        let json = serde_json::to_value(&snap).expect("快照应可序列化");
        let ev = json["player_events"].as_array().expect("player_events 恒有");
        assert_eq!(ev.len(), 1, "一帧一事件");
        assert_eq!(ev[0]["kind"], "compile_failed", "kind 标签=协议字段");
        assert_eq!(ev[0]["name"], "badwat");
        assert_eq!(json["events_cursor"], 1, "游标随帧走");
        let ent = &json["entities"][0];
        assert_eq!(ent["born_tick"], 0, "born_tick 恒有（new 世界出生拍 0）");
        assert_eq!(ent["lifespan_ticks"], 30, "lifespan_ticks 恒有（声明落库值）");
    }

    // === v2.4 资产指纹（#051 皮入沙箱：快照只带 asset_id 不带货）===

    #[test]
    fn snapshot_entity_carries_asset_ids() {
        // 规则（v2.4）：带皮实体的快照条目带 asset_ids（装戴清单指纹序列，
        // 与 entity.assets 镜像一致；前端首见按 id 走 /assets HTTP 取图）
        let mut world = WorldState::new();
        let (id, _) = put_entity(&mut world, "attacker", "A", "red", (0.0, 0.0));
        world
            .entities
            .get_mut(&id)
            .unwrap()
            .assets = vec!["av_0123456789abcdef".to_string()];
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let e = &json["entities"][0];
        assert_eq!(
            e["asset_ids"],
            serde_json::json!(["av_0123456789abcdef"]),
            "快照应携带装戴清单指纹（不带货）"
        );
    }

    #[test]
    fn snapshot_asset_ids_empty_array_always_present() {
        // 规则（v2.4）：无皮实体 asset_ids=[] 恒有字段（空盒省字段是优化，恒有=
        // 前端零可选分支，同 attacks/fields 先例）
        let mut world = WorldState::new();
        put_entity(&mut world, "attacker", "A", "red", (0.0, 0.0));
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        assert_eq!(
            json["entities"][0]["asset_ids"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "asset_ids 空数组恒有"
        );
    }

    #[test]
    fn test_entity_maps_to_npc_with_code_identity_name() {
        // 规则（v1.1）：npc 带 name=code_hash（代码身份，悬浮提示用），不带 controlled/owner
        let mut world = WorldState::new();
        put_entity(&mut world, "attacker", "A", "red", (12.5, 0.0));
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let e = &json["entities"][0];
        assert_eq!(e["kind"], "npc", "attacker 是 NPC");
        assert_eq!(e["name"], "attacker", "npc 的 name = code_hash 代码身份");
        assert_eq!(e["glyph"], "A");
        assert_eq!(e["color"], "red");
        assert_eq!(e["x"], 12.5);
        assert_eq!(e["y"], 0.0);
        assert!(e.get("controlled").is_none(), "npc 不带 controlled 字段");
        assert!(e.get("owner").is_none(), "npc 不带 owner 字段");
        assert!(e["id"].is_string(), "id 是 UUID 字符串");
    }

    #[test]
    fn online_player_maps_to_player_kind_with_name_and_controlled() {
        // 规则：players 在线名 = player（name=code_hash 玩家名）；me 操控目标 controlled=true，
        // 非操控玩家 controlled=false（player 字段恒在，前端契约稳定）
        let mut world = WorldState::new();
        let (p1, _) = put_entity(&mut world, "pioneer", "P", "green", (35.0, 0.0));
        let (p2, _) = put_entity(&mut world, "ranger", "R", "blue", (40.0, 0.0));
        let mut dir = PlayerDirectory::new("unused");
        dir.register_online("pioneer", p1);
        dir.register_online("ranger", p2);
        let json = build(&world, &dir, Some(p1));
        let find_by_name = |json: &serde_json::Value, name: &str| {
            json["entities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["name"] == name)
                .cloned()
        };
        let me = find_by_name(&json, "pioneer").expect("pioneer 应在快照中");
        assert_eq!(me["kind"], "player");
        assert_eq!(me["controlled"], true, "me 操控目标应有 controlled=true");
        let other = find_by_name(&json, "ranger").expect("ranger 应在快照中");
        assert_eq!(other["kind"], "player");
        assert_eq!(
            other["controlled"], false,
            "非操控玩家 controlled=false"
        );
    }

    #[test]
    fn creation_maps_to_creation_kind_with_owner_uuid() {
        // 规则：code_hash="creation" = 造物，owner = 创造者 PlayerId 的 UUID 字符串。
        // v3.1（D-020b）：name=实名优先——测试桩无 creation_name → 回退"造物#短id"
        //（8 位十六进制短 id，旧存档/无名造物的显示兜底）
        let mut world = WorldState::new();
        let (cid, owner) = put_entity(&mut world, "creation", "T", "cyan", (40.0, 0.0));
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let e = &json["entities"][0];
        assert_eq!(e["kind"], "creation");
        let fallback = format!("造物#{}", cid.0.to_string()[..8].to_string());
        assert_eq!(e["name"], fallback, "无名造物 name 回退 造物#短id（v3.1）");
        assert_ne!(e["name"], "creation", "造物 name 不再裸奔 code_hash");
        assert_eq!(
            e["owner"],
            owner.0.to_string(),
            "owner = 创造者 PlayerId UUID 字符串"
        );
        assert!(e.get("controlled").is_none(), "creation 不带 controlled");
    }

    #[test]
    fn creation_snapshot_name_prefers_creation_name() {
        // v3.1 实名优先（D-020b）：带 creation_name 的造物快照 name=实名（前端
        // 播报/技能栏/造物列表的显示名数据源）；非造物 kind 不受影响照旧 code_hash
        let mut world = WorldState::new();
        let (cid, _) = put_entity(&mut world, "creation", "T", "cyan", (40.0, 0.0));
        world
            .entities
            .get_mut(&cid)
            .unwrap()
            .set_creation_name(Some("箭塔".to_string()));
        let (pid, _) = put_entity(&mut world, "pioneer", "P", "blue", (0.0, 0.0));
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let tower = json["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == cid.0.to_string().as_str())
            .unwrap();
        assert_eq!(tower["name"], "箭塔", "造物 name=creation_name 实名");
        let other = json["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == pid.0.to_string().as_str())
            .unwrap();
        assert_eq!(other["name"], "pioneer", "非造物 name 照旧 code_hash 不受影响");
    }

    #[test]
    fn dynamic_creation_maps_to_creation_dynamic_kind() {
        // D-003：motion=1 的造物 kind=creation_dynamic（前端按圆渲染+圆点击判定），
        // owner 产权签照带（产权链与运动身份正交）
        let mut world = WorldState::new();
        let (cid, owner) = put_entity(&mut world, "creation", "B", "cyan", (40.0, 0.0));
        world
            .entities
            .get_mut(&cid)
            .unwrap()
            .set_motion(codeworld_core::entity::MOTION_DYNAMIC);
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let e = &json["entities"][0];
        assert_eq!(e["kind"], "creation_dynamic", "motion=1 造物应映射 creation_dynamic");
        assert_eq!(e["radius"], 0.5, "radius 字段照常携带（圆身体半径）");
        assert_eq!(e["owner"], owner.0.to_string(), "dynamic 造物照带 owner 产权签");
    }

    #[test]
    fn offline_ghost_entity_still_maps_to_player_kind() {
        // 规则：残影（#013）实体 code_hash 仍是玩家名但已不在线 -> kind=player
        //（is_ghost 识别；残影是玩家躯壳不是 NPC）
        let mut world = WorldState::new();
        let (gid, _) = put_entity(&mut world, "leaver", "L", "green", (30.0, 0.0));
        let mut dir = PlayerDirectory::new("unused");
        dir.register_online("leaver", gid);
        dir.note_offline("leaver", 0);
        let json = build(&world, &dir, None);
        let e = &json["entities"][0];
        assert_eq!(e["kind"], "player", "残影实体仍是玩家躯壳");
        assert_eq!(e["name"], "leaver");
        assert_eq!(e["controlled"], false, "残影不可被操控");
    }

    #[test]
    fn snapshot_top_level_fields_tick_caption_viewport() {
        // 契约：顶层 tick/caption/viewport/entities；viewport = 世界坐标域（x0/y0 左下角+宽高）
        let mut world = WorldState::new();
        for _ in 0..3 {
            world.advance_tick();
        }
        let (tid, _) = put_entity(&mut world, "attacker", "A", "red", (1.0, 2.0));
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, Some(tid));
        assert_eq!(json["tick"], 3);
        assert_eq!(json["caption"], "cap");
        let vp = &json["viewport"];
        assert_eq!(vp["x0"], -50.0);
        assert_eq!(vp["y0"], -25.0);
        assert_eq!(vp["w"], 100.0);
        assert_eq!(vp["h"], 50.0);
        assert_eq!(json["entities"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn entities_sorted_by_id_for_stable_frame_order() {
        // 契约：实体按 id 字典序（HashMap 遍历序不稳定，前端逐帧 diff 需要稳定帧序）
        let mut world = WorldState::new();
        let mut ids = Vec::new();
        for i in 0..5 {
            let (id, _) = put_entity(&mut world, "attacker", "A", "red", (i as f64, 0.0));
            ids.push(id.0.to_string());
        }
        ids.sort();
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let got: Vec<String> = json["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, ids, "帧内实体应按 id 字典序稳定排列");
    }

    // ---- channel 桥（tick 侧 std 线程 -> WS 侧 tokio 线程）----

    #[tokio::test]
    async fn push_snapshot_delivers_json_frames_in_tick_order() {
        // 读写：tick 侧 push -> 订阅者收到合法 JSON 帧，tick 递增（std 线程可直接 send，
        // tokio broadcast Sender::send 非 async，跨线程桥接零 async 开销）
        let (tx, mut rx) = broadcast::channel::<String>(SNAPSHOT_CHANNEL_CAP);
        let mut world = WorldState::new();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        world.advance_tick();
        push_snapshot(&tx, &world, "c1", &vp, &dir, None, &[], 3, Vec::new(), 0);
        world.advance_tick();
        push_snapshot(&tx, &world, "c2", &vp, &dir, None, &[], 3, Vec::new(), 0);
        let f1: serde_json::Value =
            serde_json::from_str(&tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.expect("第一帧应在 2s 内到达").unwrap()).unwrap();
        let f2: serde_json::Value =
            serde_json::from_str(&tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await.expect("第二帧应在 2s 内到达").unwrap()).unwrap();
        assert_eq!(f1["tick"], 1, "第一帧 tick=1");
        assert_eq!(f1["caption"], "c1");
        assert_eq!(f2["tick"], 2, "第二帧 tick=2（帧序即 tick 序）");
    }

    #[test]
    fn push_snapshot_without_receivers_is_silent_noop() {
        // 边界：无订阅者（浏览器全关）-> 组装跳过/发送失败静默，不 panic 不刷日志
        let (tx, _rx) = broadcast::channel::<String>(SNAPSHOT_CHANNEL_CAP);
        drop(_rx); // 零接收者
        let world = WorldState::new();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        push_snapshot(&tx, &world, "c", &vp, &dir, None, &[], 3, Vec::new(), 0);
        assert_eq!(tx.receiver_count(), 0, "无观众场景保持零接收者");
    }

    // ---- WS 服务器集成测试（axum serve + tungstenite 测试客户端）----

    type ClientWs = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// 起测试服务器（127.0.0.1 临时端口；返回连接地址与快照发送端/意图接收端/停机开关）
    #[allow(clippy::type_complexity)]
    async fn start_test_server(
        static_dir: std::path::PathBuf,
    ) -> (
        std::net::SocketAddr,
        broadcast::Sender<String>,
        crate::input::IntentQueue,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        let (addr, tx, queue, shutdown_tx, server, _avatars, _roster) =
            start_test_server_with_avatars(static_dir, &temp_assets_dir("shared")).await;
        (addr, tx, queue, shutdown_tx, server)
    }

    /// 带资产目录的测试服务器（D-017 上传通道测试用：注册表目录可指定可检查；
    /// D-021d 起第 7 个返回值=角色列表共享槽，连接建立前可预置条目）
    #[allow(clippy::type_complexity)]
    async fn start_test_server_with_avatars(
        static_dir: std::path::PathBuf,
        avatars_dir: &std::path::Path,
    ) -> (
        std::net::SocketAddr,
        broadcast::Sender<String>,
        crate::input::IntentQueue,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<std::io::Result<()>>,
        Arc<avatars::AvatarRegistry>,
        Arc<std::sync::Mutex<Vec<RosterEntry>>>,
    ) {
        let (tx, _) = broadcast::channel::<String>(SNAPSHOT_CHANNEL_CAP);
        let (intents_tx, intents_rx) = crate::input::intent_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let avatars = Arc::new(avatars::AvatarRegistry::load(avatars_dir));
        let roster = Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = Arc::new(WsState {
            snapshots: tx.clone(),
            intents: intents_tx,
            ctrl_conn: tokio::sync::watch::channel(None).0,
            sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            legacy_first_conn: false,
            conn_ids: AtomicU64::new(1),
            avatars: avatars.clone(),
            roster: roster.clone(),
            players_dir: temp_assets_dir("shared-players"),
            bridge_vault: None,
            bridge_config_path: temp_bridge_vault_path("shared-bridge"),
        });
        let handle = tokio::spawn(async move {
            serve(
                listener,
                state,
                static_dir,
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        });
        (addr, tx, intents_rx, shutdown_tx, handle, avatars, roster)
    }

    /// 临时资产目录（D-017 测试用；路径带进程号防并行撞）
    fn temp_assets_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codeworld-net-test-{tag}-{}-assets",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    // === D-017 收图通道（上传路由 + /assets 静态服务 + avatar_assets 下发）===

    /// 测试助手：RGBA8 像素 -> PNG 字节（png crate 编码，测试自产 fixtures）
    fn solid_test_png(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        let mut px = Vec::new();
        for _ in 0..w * h {
            px.extend_from_slice(&[255, 0, 0, alpha]);
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&px).unwrap();
        }
        out
    }

    /// 测试助手：手拼 multipart/form-data 请求体（字段按序编码）
    fn multipart_body(boundary: &str, fields: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, data) in fields {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n")
                    .as_bytes(),
            );
            body.extend_from_slice(data.as_slice());
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }

    /// 测试助手：手写 HTTP 请求（POST/GET 通用，返回原始响应字节）
    async fn http_request(
        addr: &std::net::SocketAddr,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> Vec<u8> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut req = format!("{method} {path} HTTP/1.0\r\nHost: {addr}\r\n");
        if let Some(ct) = content_type {
            req.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        if let Some(b) = body {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        req.push_str("\r\n");
        let mut raw = req.into_bytes();
        if let Some(b) = body {
            raw.extend_from_slice(b);
        }
        stream.write_all(&raw).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        buf
    }

    /// 测试助手：响应头之后的 body 字节（\r\n\r\n 分隔；手写分割——slice split_once 未稳定）
    fn http_body(resp: &[u8]) -> &[u8] {
        const SEP: &[u8] = b"\r\n\r\n";
        for i in 0..resp.len().saturating_sub(SEP.len()) {
            if &resp[i..i + SEP.len()] == SEP {
                return &resp[i + SEP.len()..];
            }
        }
        &[]
    }

    #[tokio::test]
    async fn upload_avatar_multipart_happy_path_serves_asset() {
        // 场景：POST /upload/avatar 收合法 PNG -> JSON ok:true + asset_id ->
        // PNG+元数据落盘 + 登记表在册 + GET /assets/avatars/{id}.png 原图可取
        let static_dir = temp_static_dir("av-up");
        let avatars_dir = temp_assets_dir("av-up");
        let (addr, _tx, _queue, shutdown_tx, server, reg, _roster) =
            start_test_server_with_avatars(static_dir, &avatars_dir).await;
        let png = solid_test_png(64, 64, 255);
        let body = multipart_body(
            "XBOUND",
            &[
                ("file", png.clone()),
                ("name", b"pioneer".to_vec()),
                ("height_world", b"3.5".to_vec()),
            ],
        );
        let resp = http_request(
            &addr,
            "POST",
            "/upload/avatar",
            Some("multipart/form-data; boundary=XBOUND"),
            Some(&body),
        )
        .await;
        let json: serde_json::Value =
            serde_json::from_slice(http_body(&resp)).expect("响应应为 JSON");
        assert_eq!(json["ok"], true, "合法上传应成功：{json}");
        let id = json["asset_id"].as_str().expect("asset_id 应为字符串");
        assert!(id.starts_with("av_"), "asset_id 前缀 av_：{id}");
        // 登记表在册 + 参数入档
        let list = reg.list();
        assert_eq!(list.len(), 1, "恰一条登记");
        assert_eq!(list[0].name, "pioneer");
        assert_eq!(list[0].height_world, 3.5, "身高参数入档");
        // 文件落盘（注册表目录）
        assert!(avatars_dir.join(format!("{id}.png")).exists(), "PNG 落盘");
        assert!(avatars_dir.join(format!("{id}.json")).exists(), "元数据落盘");
        // 原图 HTTP 可取（/assets 静态挂载，nest 剥前缀后=资产目录内 {id}.png），
        // 字节与上传一致
        let raw = http_request(&addr, "GET", &format!("/assets/{id}.png"), None, None).await;
        assert!(raw.starts_with(b"HTTP/1.0 200"), "原图应 200：{:?}", &raw[..20.min(raw.len())]);
        assert_eq!(http_body(&raw), png.as_slice(), "原图字节应原样返回");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn upload_avatar_rejects_disconnected_png_with_isolate_fail() {
        // 规则：断件图（两个大连通域）-> {ok:false, reason:"isolate_fail"}
        let static_dir = temp_static_dir("av-iso");
        let avatars_dir = temp_assets_dir("av-iso");
        let (addr, _tx, _queue, shutdown_tx, server, reg, _roster) =
            start_test_server_with_avatars(static_dir, &avatars_dir).await;
        // 上下两半实心 = 两个巨型连通域
        let mut px = vec![0u8; 128 * 128 * 4];
        for y in 0..128u32 {
            let a = if y < 60 || y >= 68 { 255 } else { 0 };
            for x in 0..128u32 {
                let i = ((y * 128 + x) * 4) as usize;
                px[i] = 255;
                px[i + 3] = a;
            }
        }
        let mut disconnected = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut disconnected, 128, 128);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&px).unwrap();
        }
        let body = multipart_body(
            "XB",
            &[("file", disconnected), ("name", b"broken".to_vec())],
        );
        let resp = http_request(
            &addr,
            "POST",
            "/upload/avatar",
            Some("multipart/form-data; boundary=XB"),
            Some(&body),
        )
        .await;
        let json: serde_json::Value =
            serde_json::from_slice(http_body(&resp)).expect("响应应为 JSON");
        assert_eq!(json["ok"], false, "断件图应拒绝");
        assert_eq!(json["reason"], "isolate_fail", "拒绝原因=断件：{json}");
        assert!(reg.list().is_empty(), "拒绝的上传不登记");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn upload_avatar_rejects_oversize_and_corrupt() {
        // 规则：> 2MB -> oversize；坏字节 -> decode_failed（护栏逐级生效）
        let static_dir = temp_static_dir("av-guard");
        let avatars_dir = temp_assets_dir("av-guard");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster) =
            start_test_server_with_avatars(static_dir, &avatars_dir).await;
        let big = vec![0xABu8; avatars::MAX_AVATAR_PNG_BYTES + 1];
        for (payload, expect) in [
            (big, "oversize"),
            (b"definitely not png".to_vec(), "decode_failed"),
        ] {
            let body = multipart_body("XB", &[("file", payload), ("name", b"x".to_vec())]);
            let resp = http_request(
                &addr,
                "POST",
                "/upload/avatar",
                Some("multipart/form-data; boundary=XB"),
                Some(&body),
            )
            .await;
            let json: serde_json::Value =
                serde_json::from_slice(http_body(&resp)).expect("响应应为 JSON");
            assert_eq!(json["ok"], false);
            assert_eq!(json["reason"], expect, "护栏 reason：{json}");
        }
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn ws_connection_receives_avatar_assets_first_frame() {
        // 规则：连接建立后服务器主动发一条 avatar_assets（一次性全量）——
        // 先于任何快照帧；mask=128x128=16384 项 0/1；url 可直取原图
        let static_dir = temp_static_dir("av-ws");
        let avatars_dir = temp_assets_dir("av-ws");
        let (addr, tx, _queue, shutdown_tx, server, reg, _roster) =
            start_test_server_with_avatars(static_dir, &avatars_dir).await;
        // 先上传一张（直接走登记表，上传路由已有独立测试）
        let png = solid_test_png(64, 64, 255);
        let asset = avatars::store_upload(
            &reg,
            avatars::UploadInput {
                png: &png,
                name: "pioneer",
                height_world: None,
            },
        )
        .expect("预置上传应成功");
        let mut client = connect_client(addr).await;
        let frame: serde_json::Value = serde_json::from_str(&next_text(&mut client).await).unwrap();
        assert_eq!(frame["type"], "avatar_assets", "首帧=资产消息：{frame}");
        let assets = frame["assets"].as_array().expect("assets 数组恒有");
        assert_eq!(assets.len(), 1, "全量=恰一张");
        assert_eq!(assets[0]["asset_id"], asset.asset_id.as_str());
        assert_eq!(
            assets[0]["url"],
            format!("/assets/{}.png", asset.asset_id),
            "url 可直取原图"
        );
        assert_eq!(assets[0]["w"], 64, "原图尺寸入消息");
        assert_eq!(assets[0]["h"], 64);
        let mask = assets[0]["mask"].as_array().expect("mask 数组");
        assert_eq!(mask.len(), 128 * 128, "掩码 16384 项");
        assert!(
            mask.iter().all(|v| v == 0 || v == 1),
            "掩码值恒 0/1"
        );
        // D-021d：资产后紧跟 player_roster（消费掉），之后照常收快照（推送一帧验证次序）
        let roster: serde_json::Value = serde_json::from_str(&next_text(&mut client).await).unwrap();
        assert_eq!(roster["type"], "player_roster", "资产后恒为角色列表：{roster}");
        let mut world = WorldState::new();
        world.advance_tick();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        push_snapshot(&tx, &world, "after-assets", &vp, &dir, None, &[], 3, Vec::new(), 0);
        let snap: serde_json::Value = serde_json::from_str(&next_text(&mut client).await).unwrap();
        assert_eq!(snap["caption"], "after-assets", "资产消息先于快照帧");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn ws_connection_with_empty_registry_still_receives_avatar_assets() {
        // 规则：零资产也发（assets:[] 恒有）——前端不必判消息缺失
        let static_dir = temp_static_dir("av-empty");
        let avatars_dir = temp_assets_dir("av-empty");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster) =
            start_test_server_with_avatars(static_dir, &avatars_dir).await;
        let mut client = connect_client(addr).await;
        let frame: serde_json::Value = serde_json::from_str(&next_text(&mut client).await).unwrap();
        assert_eq!(frame["type"], "avatar_assets");
        assert_eq!(
            frame["assets"].as_array().map(Vec::len),
            Some(0),
            "零资产=空数组恒有"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    /// 轮询意图队列直到出现一条意图（backlog 保留 drain 多取的后续意图防丢；
    /// 服务器异步处理消息，测试端轮询 std mpsc）
    async fn next_intent(
        queue: &crate::input::IntentQueue,
        backlog: &mut Vec<crate::input::Intent>,
    ) -> crate::input::Intent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if backlog.is_empty() {
                *backlog = queue.drain();
            }
            if !backlog.is_empty() {
                return backlog.remove(0);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "5s 内应收到服务器转发的意图"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn ws_move_message_lands_in_intent_queue() {
        // 场景：凭 token 认领的操控连接（D-021f）；发 move JSON -> 解析入共享意图队列（#027 单入口）
        let static_dir = temp_static_dir("up-move");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        let mut client = connect_client_with_token(addr, "t1").await;
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"type":"move","x":12.0,"y":0.0}"#.to_string(),
            ))
            .await
            .unwrap();
        let mut backlog = Vec::new();
        let intent = next_intent(&queue, &mut backlog).await;
        assert_eq!(
            intent,
            crate::input::Intent::WsMove { x: 12.0, y: 0.0 },
            "move 消息转 WsMove 意图入共享队列"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn ws_attack_and_stop_messages_forward_to_queue() {
        // attack/stop 同通路：WS 客户端消息 -> 共享意图队列（与键盘同一条 mpsc）
        let static_dir = temp_static_dir("up-multi");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        let mut client = connect_client_with_token(addr, "t1").await;
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"type":"attack","x":15.0,"y":1.0}"#.to_string(),
            ))
            .await
            .unwrap();
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"type":"stop"}"#.to_string(),
            ))
            .await
            .unwrap();
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsAttack { x: 15.0, y: 1.0 }
        );
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::Stop
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn ws_shoot_message_lands_in_intent_queue() {
        // #036 远程射击同通路：WS shoot 消息 -> 共享意图队列（与键盘/鼠标单入口）
        let static_dir = temp_static_dir("up-shoot");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        let mut client = connect_client_with_token(addr, "t1").await;
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"type":"shoot","x":15.0,"y":2.5}"#.to_string(),
            ))
            .await
            .unwrap();
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsShoot { x: 15.0, y: 2.5 },
            "shoot 消息转 WsShoot 意图入共享队列"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn invalid_uplink_messages_dropped_connection_alive() {
        // 非法上行（未知 type/坏 JSON）：丢弃不入队，连接不断（后续合法消息照常转发）
        let static_dir = temp_static_dir("up-bad");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        let mut client = connect_client_with_token(addr, "t1").await;
        for bad in [r#"{"type":"dance"}"#.to_string(), "not json".to_string()] {
            client
                .send(tokio_tungstenite::tungstenite::Message::Text(bad))
                .await
                .unwrap();
        }
        client
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"type":"stop"}"#.to_string(),
            ))
            .await
            .unwrap();
        // 前两条被丢弃：队列里只有最后的 stop
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::Stop
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            queue.drain().is_empty(),
            "丢弃消息不入队（只有 stop 一条）"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn no_token_connection_is_spectator_uplink_dropped() {
        // T3（D-021f 语义翻正）：默认（legacy=false）下无 token 连接=纯观战——
        // 旧"第一连接=操控端"退役，操控权凭会话令牌认领；两个无 token 连接的
        // 上行一律丢弃不入队
        let static_dir = temp_static_dir("up-second");
        let (addr, _tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut c1 = connect_client(addr).await;
        let mut c2 = connect_client(addr).await;
        c1.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":12.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        c2.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":99.0,"y":99.0}"#.to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            queue.drain().is_empty(),
            "无 token 连接（含第一个）都是观战端，上行不入队"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    async fn connect_client(addr: std::net::SocketAddr) -> ClientWs {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("测试客户端应能连上 /ws");
        ws
    }

    /// 读下一帧文本消息（跳过 Pong 等控制帧；5s 超时防挂死）
    async fn next_text(ws: &mut ClientWs) -> String {
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
                .await
                .expect("5s 内应收到帧")
                .expect("连接不应中断")
                .expect("帧不应带协议错误");
            if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
                return t;
            }
        }
    }

    /// 读掉连接首帧 avatar_assets（v2.2 起连接首帧恒为资产消息；快照类测试先消费它）。
    /// D-021d 起资产后紧跟 player_roster（角色列表）——一并消费（快照类测试不关心它；
    /// roster 本身的断言见 ws_connection_receives_player_roster_after_avatar_assets）
    async fn consume_avatar_assets(ws: &mut ClientWs) -> serde_json::Value {
        let v: serde_json::Value =
            serde_json::from_str(&next_text(ws).await).expect("首帧应为合法 JSON");
        assert_eq!(v["type"], "avatar_assets", "首帧恒为 avatar_assets：{v}");
        let r: serde_json::Value = serde_json::from_str(&next_text(ws).await).expect("次帧应为合法 JSON");
        assert_eq!(r["type"], "player_roster", "资产后恒为 player_roster：{r}");
        v
    }

    /// 造临时静态目录（含 index.html），供 ServeDir 伺服测试
    fn temp_static_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codeworld-net-test-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html>cw-test</html>").unwrap();
        dir
    }

    // === D-021f 会话令牌：操控权随凭据走（踢旧留新）===

    /// 带会话表预填与 legacy 标志的测试服务器（token 认领/踢旧/stale 测试用）。
    /// 返回会话表 Arc（断言/再预填用）与操控连接 id watch 观测端（沿检测断言用）
    #[allow(clippy::type_complexity)]
    async fn start_test_server_with_sessions(
        static_dir: std::path::PathBuf,
        sessions_pre: &[(&str, &str)],
        legacy_first_conn: bool,
    ) -> (
        std::net::SocketAddr,
        broadcast::Sender<String>,
        crate::input::IntentQueue,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<std::io::Result<()>>,
        Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
        tokio::sync::watch::Receiver<Option<u64>>,
    ) {
        let (tx, _) = broadcast::channel::<String>(SNAPSHOT_CHANNEL_CAP);
        let (intents_tx, intents_rx) = crate::input::intent_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let sessions = Arc::new(std::sync::Mutex::new(
            sessions_pre
                .iter()
                .map(|(t, n)| (t.to_string(), n.to_string()))
                .collect::<std::collections::HashMap<_, _>>(),
        ));
        let (ctrl_tx, ctrl_rx) = tokio::sync::watch::channel(None);
        let state = Arc::new(WsState {
            snapshots: tx.clone(),
            intents: intents_tx,
            ctrl_conn: ctrl_tx,
            sessions: sessions.clone(),
            legacy_first_conn,
            conn_ids: std::sync::atomic::AtomicU64::new(1),
            avatars: Arc::new(avatars::AvatarRegistry::load(&temp_assets_dir("d021f"))),
            roster: Arc::new(std::sync::Mutex::new(Vec::new())),
            players_dir: temp_assets_dir("d021f-players"),
            bridge_vault: None,
            bridge_config_path: temp_bridge_vault_path("d021f-bridge"),
        });
        let handle = tokio::spawn(async move {
            serve(
                listener,
                state,
                static_dir,
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        });
        (addr, tx, intents_rx, shutdown_tx, handle, sessions, ctrl_rx)
    }

    /// 带 token 的测试客户端（连接 URL ?token=，与生产前端 connect() 同形）
    async fn connect_client_with_token(addr: std::net::SocketAddr, token: &str) -> ClientWs {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={token}"))
            .await
            .expect("测试客户端应能连上 /ws?token=");
        ws
    }

    #[tokio::test]
    async fn token_claim_via_query_becomes_controller() {
        // T1：sessions 表预填 t1->p1；带 ?token=t1 的连接=操控端（move 意图入队）
        let static_dir = temp_static_dir("tok-claim");
        let (addr, _tx, queue, shutdown_tx, server, sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        assert_eq!(
            sessions.lock().unwrap().get("t1").map(String::as_str),
            Some("p1"),
            "预填会话表可查"
        );
        let mut c = connect_client_with_token(addr, "t1").await;
        consume_avatar_assets(&mut c).await;
        c.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":3.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsMove { x: 3.0, y: 0.0 },
            "凭 token 认领的连接=操控端（上行入队）"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn new_token_claim_kicks_old_controller() {
        // T2 踢旧留新：A 带 t1 操控中，B 带同 t1 连上 -> A 收 control_revoked{taken}
        //（降观战但连接不断），B=新操控端（move 入队）；A 被踢后上行丢弃
        let static_dir = temp_static_dir("tok-kick");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        let mut a = connect_client_with_token(addr, "t1").await;
        consume_avatar_assets(&mut a).await;
        let mut b = connect_client_with_token(addr, "t1").await;
        consume_avatar_assets(&mut b).await;
        b.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":5.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsMove { x: 5.0, y: 0.0 },
            "新凭据连接接管操控（B 的意图入队）"
        );
        let frame: serde_json::Value =
            serde_json::from_str(&next_text(&mut a).await).unwrap();
        assert_eq!(frame["type"], "control_revoked", "被接管者收踢旧通知：{frame}");
        assert_eq!(frame["reason"], "taken", "reason=taken（被新凭据接管）：{frame}");
        a.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":99.0,"y":99.0}"#.to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(queue.drain().is_empty(), "被踢连接上行已丢弃（降观战）");
        // 被踢者断开不误拆新操控端：A 断开后 B 的操控权仍在（move 照常入队）
        a.send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await
            .ok();
        drop(a);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        b.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":6.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsMove { x: 6.0, y: 0.0 },
            "被踢者断开后新操控端不受影响"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn stale_token_gets_revoked_and_uplink_dropped() {
        // T4：未注册 token（过期/登出后/伪造）-> 观战 + 立即单播
        // control_revoked{stale}；上行丢弃
        let static_dir = temp_static_dir("tok-stale");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        let mut c = connect_client_with_token(addr, "ghost-token").await;
        consume_avatar_assets(&mut c).await;
        let frame: serde_json::Value =
            serde_json::from_str(&next_text(&mut c).await).unwrap();
        assert_eq!(frame["type"], "control_revoked", "失效 token 连接收通知：{frame}");
        assert_eq!(frame["reason"], "stale", "reason=stale（token 不在表）：{frame}");
        c.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":1.0,"y":1.0}"#.to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(queue.drain().is_empty(), "stale 连接=观战（上行丢弃）");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn logout_invalidates_session_token() {
        // T5：登出=会话终结。A 带 t1 操控 -> logout -> sessions 表 t1 移除；
        // 新连接带同 token -> stale（token 不得复用）
        let static_dir = temp_static_dir("tok-logout");
        let (addr, _tx, _queue, shutdown_tx, server, sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        let mut a = connect_client_with_token(addr, "t1").await;
        consume_avatar_assets(&mut a).await;
        a.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"logout","disposition":"take"}"#.to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !sessions.lock().unwrap().contains_key("t1"),
            "登出后 token 已从会话表移除"
        );
        let mut b = connect_client_with_token(addr, "t1").await;
        consume_avatar_assets(&mut b).await;
        let frame: serde_json::Value =
            serde_json::from_str(&next_text(&mut b).await).unwrap();
        assert_eq!(frame["type"], "control_revoked");
        assert_eq!(frame["reason"], "stale", "已登出的 token 不可复用：{frame}");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn select_player_intent_carries_token_and_unicasts_session_token() {
        // T6：select_player 上行 -> 服务器生成会话 token：Intent 带上（tick 侧
        // 成功后绑定）+ 当场单播 session_token 帧（两处 token 同值非空）
        let static_dir = temp_static_dir("tok-sel");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[], false).await;
        let mut c = connect_client(addr).await; // 无 token（登录页观战连接）
        consume_avatar_assets(&mut c).await;
        c.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"select_player","name":"p1"}"#.to_string(),
        ))
        .await
        .unwrap();
        let frame: serde_json::Value =
            serde_json::from_str(&next_text(&mut c).await).unwrap();
        assert_eq!(frame["type"], "session_token", "选角后当场单播会话令牌：{frame}");
        assert_eq!(frame["name"], "p1", "单播帧带角色名：{frame}");
        let tok = frame["token"].as_str().expect("token 字段应存在").to_string();
        assert!(!tok.is_empty(), "token 非空");
        let mut backlog = Vec::new();
        match next_intent(&queue, &mut backlog).await {
            crate::input::Intent::SelectPlayer { name, token } => {
                assert_eq!(name, "p1");
                assert_eq!(token, tok, "意图携带的 token 与单播帧同值");
            }
            other => panic!("应转 SelectPlayer 意图，实为 {other:?}"),
        }
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn legacy_first_conn_keeps_first_connection_claim() {
        // 逃生门：legacy_first_conn=true（CODEWORLD_ME env 锁定的开发者场景）
        // 保留旧"第一连接=操控端"语义（无 token 也认领）；后来无 token 连接=观战
        let static_dir = temp_static_dir("tok-legacy");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[], true).await;
        let mut c1 = connect_client(addr).await;
        consume_avatar_assets(&mut c1).await;
        c1.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":1.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsMove { x: 1.0, y: 0.0 },
            "legacy 模式第一连接无 token 也认领操控"
        );
        let mut c2 = connect_client(addr).await;
        consume_avatar_assets(&mut c2).await;
        c2.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":99.0,"y":99.0}"#.to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(queue.drain().is_empty(), "legacy 模式后来无 token 连接=观战");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn reconnect_with_same_token_reclaims_control() {
        // 刷新页面场景（D-021f 的 bug 修复锚）：带 t1 操控的连接断开 ->
        // 同 token 新连接（刷新后的浏览器）重新认领操控——"出生点不出去"根除
        let static_dir = temp_static_dir("tok-reclaim");
        let (addr, _tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1")], false).await;
        let mut a = connect_client_with_token(addr, "t1").await;
        consume_avatar_assets(&mut a).await;
        a.send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await
            .ok();
        drop(a);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut b = connect_client_with_token(addr, "t1").await;
        consume_avatar_assets(&mut b).await;
        b.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":7.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsMove { x: 7.0, y: 0.0 },
            "断开后同 token 新连接重新认领（刷新页面不丢操控）"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn ws_server_delivers_frames_then_survives_client_disconnect() {
        // 场景：起服务器 -> 客户端连上收 2 帧（合法 JSON 且 tick 递增）-> 断开
        // -> 服务器仍可用（新客户端可再连：连接清理不伤服务）-> 优雅停机
        let static_dir = temp_static_dir("frames");
        let (addr, tx, _queue, shutdown_tx, server) = start_test_server(static_dir.clone()).await;
        let mut client = connect_client(addr).await;
        consume_avatar_assets(&mut client).await; // v2.2 首帧=资产消息，消费后才是快照帧

        let mut world = WorldState::new();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        world.advance_tick();
        push_snapshot(&tx, &world, "f1", &vp, &dir, None, &[], 3, Vec::new(), 0);
        world.advance_tick();
        push_snapshot(&tx, &world, "f2", &vp, &dir, None, &[], 3, Vec::new(), 0);

        let frame1: serde_json::Value = serde_json::from_str(&next_text(&mut client).await).unwrap();
        let frame2: serde_json::Value = serde_json::from_str(&next_text(&mut client).await).unwrap();
        assert_eq!(frame1["tick"], 1, "第一帧 tick=1");
        assert_eq!(frame2["tick"], 2, "第二帧 tick=2（合法 JSON 且递增）");

        // 客户端断开（发 Close 帧后丢弃）
        client
            .send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await
            .ok();
        drop(client);
        // 给服务器一点时间走断开清理路径（handle_socket 返回、日志打出）
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // 服务器仍可用：新客户端再连并收到新帧
        let mut client2 = connect_client(addr).await;
        consume_avatar_assets(&mut client2).await;
        world.advance_tick();
        push_snapshot(&tx, &world, "f3", &vp, &dir, None, &[], 3, Vec::new(), 0);
        let frame3: serde_json::Value = serde_json::from_str(&next_text(&mut client2).await).unwrap();
        assert_eq!(frame3["tick"], 3, "断开后新客户端照常收帧");

        // 优雅停机
        shutdown_tx.send(()).expect("停机信号应送达");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("服务器应优雅退出")
            .expect("服务器任务不应 panic");
        result.expect("serve 应正常返回");
    }

    #[tokio::test]
    async fn ws_server_replies_pong_to_ping() {
        // 场景：客户端 Ping -> 服务器 Pong（连接保活；第一刀对客户端消息只回 pong/忽略）
        let static_dir = temp_static_dir("ping");
        let (addr, _tx, _queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut client = connect_client(addr).await;
        client
            .send(tokio_tungstenite::tungstenite::Message::Ping(b"ka".to_vec()))
            .await
            .unwrap();
        let pong = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let msg = client_next_raw(&mut client).await;
                if let tokio_tungstenite::tungstenite::Message::Pong(p) = msg {
                    return p;
                }
            }
        })
        .await
        .expect("5s 内应收到 Pong");
        assert_eq!(pong, b"ka", "Pong 应携带 Ping 的载荷");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    async fn client_next_raw(ws: &mut ClientWs) -> tokio_tungstenite::tungstenite::Message {
        tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("5s 内应收到消息")
            .expect("连接不应中断")
            .expect("消息不应带协议错误")
    }

    #[tokio::test]
    async fn ws_server_broadcasts_to_multiple_clients() {
        // 场景：两个客户端同时在线 -> 一拍推送双方各收到同一帧（多客户端并发观战）
        let static_dir = temp_static_dir("multi");
        let (addr, tx, _queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut c1 = connect_client(addr).await;
        let mut c2 = connect_client(addr).await;
        consume_avatar_assets(&mut c1).await; // v2.2 首帧=资产消息
        consume_avatar_assets(&mut c2).await;

        let mut world = WorldState::new();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        world.advance_tick();
        push_snapshot(&tx, &world, "shared", &vp, &dir, None, &[], 3, Vec::new(), 0);

        let f1: serde_json::Value = serde_json::from_str(&next_text(&mut c1).await).unwrap();
        let f2: serde_json::Value = serde_json::from_str(&next_text(&mut c2).await).unwrap();
        assert_eq!(f1["tick"], f2["tick"], "两客户端应收到同一拍快照");
        assert_eq!(f1["caption"], "shared");

        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn static_root_serves_index_html() {
        // 场景：GET / 走 ServeDir 返回 index.html（验收页伺服，第二刀 Canvas 同入口）
        let static_dir = temp_static_dir("static");
        let (addr, _tx, _queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let body = http_get(&addr).await;
        assert!(
            body.contains("cw-test"),
            "GET / 应返回 index.html 内容，实际：{body}"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    /// 极简 HTTP GET（测试用，不引 reqwest 依赖：手写 TCP + HTTP/1.0 请求）
    async fn http_get(addr: &std::net::SocketAddr) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream
            .write_all(format!("GET / HTTP/1.0\r\nHost: {addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    }

    // ---- 生产装配（init：配置分流；线程隔离：挂了不影响主循环）----

    #[test]
    fn init_disabled_when_port_zero_returns_no_channel() {
        // 规则：CODEWORLD_WS_PORT=0 = 禁用 -> 不起线程、不返回通道（tick 侧零开销分支）
        let cfg = crate::config::Config {
            ws_port: 0,
            ..crate::config::Config::default()
        };
        assert!(
            init(&cfg, intent_channel().0, None).is_none(),
            "端口 0 应禁用 WS（None = 主循环不组装快照）"
        );
    }

    // ---- 上行意图解析（#027 期四第三刀：客户端 JSON -> ClientMessage）----

    #[test]
    fn parse_move_message_with_world_coords() {
        let msg = parse_client_message(r#"{"type":"move","x":12.0,"y":0.0}"#).expect("move 应可解析");
        assert_eq!(
            msg,
            ClientMessage::Move { x: 12.0, y: 0.0 },
            "move 携带点击的世界坐标"
        );
    }

    #[test]
    fn parse_attack_message_with_world_coords() {
        let msg =
            parse_client_message(r#"{"type":"attack","x":15.5,"y":-2.0}"#).expect("attack 应可解析");
        assert_eq!(msg, ClientMessage::Attack { x: 15.5, y: -2.0 });
    }

    #[test]
    fn parse_shoot_message_with_world_coords() {
        // #036 远程射击上行：前端只发目标坐标（弹道参数服务器算）
        let msg =
            parse_client_message(r#"{"type":"shoot","x":15.0,"y":2.5}"#).expect("shoot 应可解析");
        assert_eq!(
            msg,
            ClientMessage::Shoot { x: 15.0, y: 2.5 },
            "shoot 携带点击的世界坐标"
        );
    }

    #[test]
    fn parse_shoot_non_finite_coords_rejected() {
        // 有限性校验与 move/attack 同口径（溢出/非有限拒绝）
        assert!(parse_client_message(r#"{"type":"shoot","x":1e400,"y":0}"#).is_err());
    }

    #[test]
    fn parse_stop_message() {
        let msg = parse_client_message(r#"{"type":"stop"}"#).expect("stop 应可解析");
        assert_eq!(msg, ClientMessage::Stop);
    }

    #[test]
    fn parse_unknown_type_rejected() {
        // 未知 type：warn 丢弃的解析层依据（Err 带原因，不 panic）
        assert!(parse_client_message(r#"{"type":"dance"}"#).is_err());
    }

    #[test]
    fn parse_missing_or_bad_coords_rejected() {
        // 非法坐标：缺字段 / 类型不对 / 非有限值 -> 一律 Err
        assert!(parse_client_message(r#"{"type":"move","x":12.0}"#).is_err(), "缺 y");
        assert!(
            parse_client_message(r#"{"type":"move","x":"abc","y":0}"#).is_err(),
            "坐标类型不对"
        );
        assert!(
            parse_client_message(r#"{"type":"move","x":1e400,"y":0}"#).is_err(),
            "溢出/非有限坐标拒绝"
        );
    }

    #[test]
    fn parse_malformed_json_rejected() {
        assert!(parse_client_message("not json").is_err());
        assert!(parse_client_message(r#"[]"#).is_err(), "非对象 JSON 拒绝");
    }

    // ---- 登出处置（#025 期五第一刀，net-protocol.md v1.7）----

    #[test]
    fn parse_logout_message_three_dispositions() {
        // 契约：{"type":"logout","disposition":"take"|"stay_auto"|"stay_ai"} 三选一
        assert_eq!(
            parse_client_message(r#"{"type":"logout","disposition":"take"}"#).unwrap(),
            ClientMessage::Logout {
                choice: crate::dispose::LogoutChoice::Take
            }
        );
        assert_eq!(
            parse_client_message(r#"{"type":"logout","disposition":"stay_auto"}"#).unwrap(),
            ClientMessage::Logout {
                choice: crate::dispose::LogoutChoice::StayAuto
            }
        );
        assert_eq!(
            parse_client_message(r#"{"type":"logout","disposition":"stay_ai"}"#).unwrap(),
            ClientMessage::Logout {
                choice: crate::dispose::LogoutChoice::StayAi
            }
        );
    }

    #[test]
    fn parse_logout_invalid_disposition_rejected() {
        // 非法 disposition 值 / 缺字段：解析层拒绝（warn 丢弃不入队）
        assert!(parse_client_message(r#"{"type":"logout","disposition":"dance"}"#).is_err());
        assert!(parse_client_message(r#"{"type":"logout"}"#).is_err(), "缺 disposition 拒绝");
    }

    #[test]
    fn snapshot_player_disposition_fields_match_book() {
        // 契约 v1.7：player 实体新增可选 disposition/wait_left——
        // 处置等待带剩余拍数；auto/ai-await 只带状态签；在线（无处置）字段缺席
        let mut world = WorldState::new();
        let (pid, _) = put_entity(&mut world, "pioneer", "P", "green", (30.0, 0.5));
        let mut dir = PlayerDirectory::new("unused");
        dir.register_online("pioneer", pid);
        world.advance_tick();
        let now = world.tick();

        // 无处置：字段缺席
        let json = build(&world, &dir, Some(pid));
        let p = &json["entities"][0];
        assert!(p.get("disposition").is_none(), "在线玩家无 disposition 字段");
        assert!(p.get("wait_left").is_none());

        // 处置等待：disposition=dispose_wait + wait_left 倒数
        dir.dispose.apply(
            "pioneer",
            crate::dispose::DisposeState::Wait {
                deadline_tick: now + 30,
            },
        );
        let json = build(&world, &dir, None);
        let p = &json["entities"][0];
        assert_eq!(p["disposition"], "dispose_wait");
        assert_eq!(p["wait_left"], 30, "等待开始剩 30 拍");

        // 自主接管：只带状态签，无剩余拍数
        dir.dispose.apply("pioneer", crate::dispose::DisposeState::Auto);
        let json = build(&world, &dir, None);
        let p = &json["entities"][0];
        assert_eq!(p["disposition"], "auto");
        assert!(p.get("wait_left").is_none(), "auto 无 wait_left");

        // AI 待接入
        dir.dispose.apply("pioneer", crate::dispose::DisposeState::AiAwait);
        let json = build(&world, &dir, None);
        let p = &json["entities"][0];
        assert_eq!(p["disposition"], "ai-await");
    }

    // === D-016 地形折线投影 ===

    #[test]
    fn snapshot_flat_world_has_no_terrain_field() {
        // 契约（D-016）：平地世界快照不带 terrain 字段（省 401 点/帧带宽；
        // 前端回落 y=0 横线，旧前端兼容）
        let world = WorldState::new(); // 默认平地
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        assert!(json.get("terrain").is_none(), "平地快照不应携带 terrain 字段");
    }

    #[test]
    fn snapshot_mountain_world_carries_terrain_polyline() {
        // 契约（D-016）：非平地快照带 terrain 折线（x0/dx/heights 与 world.terrain
        // 同源，首尾采样精确一致；401 点全量 ≈6.4KB/帧，与实体数据同量级可接受）
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Mountain,
            42,
        ));
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let t = json.get("terrain").expect("山地快照应携带 terrain 折线");
        assert_eq!(t["x0"], -50.0, "采样 x 起点=域左端");
        assert_eq!(t["dx"], 0.25, "采样间距=0.25 格");
        let hs = t["heights"].as_array().expect("heights 应为数组");
        assert_eq!(hs.len(), world.terrain.sample_count(), "全量采样与 world 同源");
        let last = hs.len() - 1;
        assert_eq!(
            hs[0].as_f64().unwrap(),
            world.terrain.samples()[0],
            "首采样与 world.terrain 同源"
        );
        assert_eq!(
            hs[last].as_f64().unwrap(),
            world.terrain.samples()[last],
            "末采样与 world.terrain 同源"
        );
    }

    #[test]
    fn snapshot_npc_and_creation_never_carry_disposition() {
        // 边界：npc/creation 无处置概念，字段恒缺席（账本只收玩家名）
        let mut world = WorldState::new();
        put_entity(&mut world, "attacker", "A", "red", (12.0, 0.0));
        put_entity(&mut world, "creation", "T", "cyan", (40.0, 0.0));
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        for e in json["entities"].as_array().unwrap() {
            assert!(e.get("disposition").is_none(), "非 player 实体无 disposition");
        }
    }

    // === D-009 水域投影（前端密度映射画色的数据源）===

    #[test]
    fn snapshot_carries_liquid_rectangles_with_density() {
        // 契约（D-009）：液体造物的格对齐 AABB 投影为矩形 {x,y,w,h,density}——
        // 颜色=密度映射是前端装饰非世界事实；死水/非液体实体不入投影
        let mut world = WorldState::new();
        let (id, _) = put_entity(&mut world, "creation", "W", "blue", (10.5, 0.0));
        world
            .entities
            .get_mut(&id)
            .unwrap()
            .set_liquid_density(1.0);
        world.entities.get_mut(&id).unwrap().set_liquid_drag(0.35);
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let liqs = json["liquids"].as_array().expect("liquids 数组恒有");
        assert_eq!(liqs.len(), 1, "一个液体造物=一个矩形投影");
        assert_eq!(liqs[0]["x"], 10.0, "矩形 x=格对齐左下角（floor）");
        assert_eq!(liqs[0]["y"], 0.0);
        assert_eq!(liqs[0]["w"], 1.0, "1×1 AABB 宽");
        assert_eq!(liqs[0]["h"], 1.0);
        assert_eq!(liqs[0]["density"], 1.0, "密度随投影（前端查表画色）");
    }

    #[test]
    fn snapshot_liquids_empty_array_always_present() {
        // 契约：无水世界 liquids=[] 恒有字段（前端零可选分支，attacks/fields 先例）
        let mut world = WorldState::new();
        put_entity(&mut world, "creation", "T", "cyan", (10.0, 0.0)); // 非液体
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        assert_eq!(
            json["liquids"].as_array().map(Vec::len),
            Some(0),
            "liquids 空数组恒有"
        );
    }

    #[tokio::test]
    async fn logout_message_releases_token_and_demotes_to_spectator() {
        // #025 令牌语义（D-021f 凭据制）：主动登出=token 失效+释放操控+本连接降级观战——
        // ① Logout 意图入共享队列（处置由 tick 侧落地）；
        // ② 操控权释放后凭另一有效 token 的连接成为操控端；
        // ③ 登出后的原连接降级观战：上行丢弃、下行快照照收（登出后世界还能看）
        let static_dir = temp_static_dir("logout");
        let (addr, tx, queue, shutdown_tx, server, _sessions, _ctrl_rx) =
            start_test_server_with_sessions(static_dir, &[("t1", "p1"), ("t2", "p2")], false).await;
        let mut c1 = connect_client_with_token(addr, "t1").await;
        consume_avatar_assets(&mut c1).await;

        c1.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"logout","disposition":"stay_auto"}"#.to_string(),
        ))
        .await
        .unwrap();
        // ① Logout 意图入队（不走 move/attack 路由）
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::Logout {
                choice: crate::dispose::LogoutChoice::StayAuto
            },
            "登出意图经单入口入共享队列"
        );

        // ② c2 凭另一有效 token 拿到操控权（c1 已释放）
        let mut c2 = connect_client_with_token(addr, "t2").await;
        consume_avatar_assets(&mut c2).await;
        c2.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":1.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog2 = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog2).await,
            crate::input::Intent::WsMove { x: 1.0, y: 0.0 },
            "登出后凭有效 token 的新连接成为操控端"
        );

        // ③ c1 降级观战：再发 move 被丢弃（不入队）；下行快照照收
        c1.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":99.0,"y":99.0}"#.to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(queue.drain().is_empty(), "登出连接的后续上行已丢弃");
        let mut world = WorldState::new();
        world.advance_tick();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        push_snapshot(&tx, &world, "after-logout", &vp, &dir, None, &[], 3, Vec::new(), 0);
        let frame: serde_json::Value = serde_json::from_str(&next_text(&mut c1).await).unwrap();
        assert_eq!(frame["caption"], "after-logout", "登出连接下行快照照收（观战）");

        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn create_player_message_queues_intent_from_controller_socket() {
        // D-021b 进场仪式 WS 端到端 + D-021d v2.7 放开：会话动作
        //（create_player/select_player）**任意连接都入队**——登出后的浏览器是
        // 观战连接，登录/选角必须能用（旧断言"观战连接发 create_player 被丢弃"
        // 已随 v2.7 反向：观战连接的会话动作照常转发，落地拒绝归 tick 侧事件）
        let static_dir = temp_static_dir("onboard");
        let (addr, _tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut c1 = connect_client(addr).await;
        c1.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"create_player","name":"土豆","template":"hunter"}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog = Vec::new();
        match next_intent(&queue, &mut backlog).await {
            crate::input::Intent::CreatePlayer { name, template, .. } => {
                // token 是 WS 层 uuid 随机颁发，只断言非空（同值断言见 T6）
                assert_eq!(name, "土豆", "创建意图经单入口入共享队列（中文名原样）");
                assert_eq!(template, "hunter");
            }
            other => panic!("应转 CreatePlayer 意图，实为 {other:?}"),
        }
        // 观战连接（c1 占着操控权，c2=观战）：create_player 与 select_player
        // 都入队（v2.7 会话动作任意连接生效）
        let mut c2 = connect_client(addr).await;
        c2.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"create_player","name":"ghost","template":"blank"}"#.to_string(),
        ))
        .await
        .unwrap();
        match next_intent(&queue, &mut backlog).await {
            crate::input::Intent::CreatePlayer { name, template, .. } => {
                assert_eq!(name, "ghost", "观战连接的创建请求也入队（v2.7）");
                assert_eq!(template, "blank");
            }
            other => panic!("应转 CreatePlayer 意图，实为 {other:?}"),
        }
        c2.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"select_player","name":"土豆"}"#.to_string(),
        ))
        .await
        .unwrap();
        match next_intent(&queue, &mut backlog).await {
            crate::input::Intent::SelectPlayer { name, token } => {
                assert_eq!(name, "土豆", "观战连接的选角请求也入队（v2.7：登出后选角通道）");
                assert!(!token.is_empty(), "选角意图携带 WS 层颁发的会话令牌");
            }
            other => panic!("应转 SelectPlayer 意图，实为 {other:?}"),
        }
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[test]
    fn t21d_select_player_message_parses_name() {
        // 契约（v2.7）：{"type":"select_player","name":..}；名字校验全在 tick 侧
        // process_select_player（R1 改法 B 同款——解析层拒绝=静默丢弃无回执）
        let msg = parse_client_message(r#"{"type":"select_player","name":"土豆"}"#)
            .expect("合法选角应放行");
        assert_eq!(
            msg,
            ClientMessage::SelectPlayer {
                name: "土豆".to_string(),
            },
            "名字原样携带（含中文）"
        );
        // 缺字段拒绝（serde 必填——协议形状归解析层）
        assert!(parse_client_message(r#"{"type":"select_player"}"#).is_err());
        // 危险名字解析层放行（拒绝归 tick 侧事件回执）
        assert!(parse_client_message(r#"{"type":"select_player","name":""}"#).is_ok());
    }

    #[tokio::test]
    async fn ws_connection_receives_player_roster_after_avatar_assets() {
        // 契约（v2.7）：连接建立后先 avatar_assets 再 player_roster（角色列表，
        // 登录页数据源）；characters 数组恒有，条目四字段 name/glyph/color/online
        let static_dir = temp_static_dir("roster");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, roster_slot) =
            start_test_server_with_avatars(static_dir, &temp_assets_dir("roster")).await;
        // 连接建立前预置一条角色（读槽发生在连接时，预置时机无竞态）
        *roster_slot.lock().unwrap() = vec![RosterEntry {
            name: "土豆".to_string(),
            glyph: "土".to_string(),
            color: "green".to_string(),
            online: true,
        }];
        let mut client = connect_client(addr).await;
        let assets: serde_json::Value =
            serde_json::from_str(&next_text(&mut client).await).unwrap();
        assert_eq!(assets["type"], "avatar_assets", "首帧=资产消息");
        let roster: serde_json::Value = serde_json::from_str(&next_text(&mut client).await).unwrap();
        assert_eq!(roster["type"], "player_roster", "次帧=角色列表");
        let chars = roster["characters"].as_array().expect("characters 数组恒有");
        assert_eq!(chars.len(), 1, "共享槽预置了一条角色");
        assert_eq!(chars[0]["name"], "土豆");
        assert_eq!(chars[0]["glyph"], "土");
        assert_eq!(chars[0]["color"], "green");
        assert_eq!(chars[0]["online"], true);
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    // === D-022 造物工坊 HTTP 通道（/api/player-code 三动作 + /api/bridge-config）===

    /// 临时玩家目录（工坊通道测试用；路径带进程号+纳秒防并行撞）
    fn temp_net_players_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codeworld-net-players-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 临时桥登记表路径（D-022b bridge-config 测试用；独立路径，绝不碰世界目录
    /// 真 bridges.json——POST 会写盘，必须落在系统临时目录）
    fn temp_bridge_vault_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "codeworld-net-bridges-{tag}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ))
    }

    /// 带玩家目录与外桥保险库的测试服务器（D-022 工坊通道测试用；其余同
    /// start_test_server_with_avatars。vault=None=桥未装配，路由应回空列表）
    #[allow(clippy::type_complexity)]
    async fn start_test_server_full(
        static_dir: std::path::PathBuf,
        avatars_dir: &std::path::Path,
        players_dir: &std::path::Path,
        vault: Option<Arc<std::sync::RwLock<crate::bridges::BridgeVault>>>,
        bridge_config_path: &std::path::Path,
    ) -> (
        std::net::SocketAddr,
        broadcast::Sender<String>,
        crate::input::IntentQueue,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<std::io::Result<()>>,
        Arc<avatars::AvatarRegistry>,
        Arc<std::sync::Mutex<Vec<RosterEntry>>>,
        std::path::PathBuf,
    ) {
        let (tx, _) = broadcast::channel::<String>(SNAPSHOT_CHANNEL_CAP);
        let (intents_tx, intents_rx) = crate::input::intent_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let avatars = Arc::new(avatars::AvatarRegistry::load(avatars_dir));
        let roster = Arc::new(std::sync::Mutex::new(Vec::new()));
        let state = Arc::new(WsState {
            snapshots: tx.clone(),
            intents: intents_tx,
            ctrl_conn: tokio::sync::watch::channel(None).0,
            sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            legacy_first_conn: false,
            conn_ids: AtomicU64::new(1),
            avatars: avatars.clone(),
            roster: roster.clone(),
            players_dir: players_dir.to_path_buf(),
            bridge_vault: vault,
            bridge_config_path: bridge_config_path.to_path_buf(),
        });
        let handle = tokio::spawn(async move {
            serve(
                listener,
                state,
                static_dir,
                async move {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        });
        (
            addr,
            tx,
            intents_rx,
            shutdown_tx,
            handle,
            avatars,
            roster,
            players_dir.to_path_buf(),
        )
    }

    /// 工坊保存的 POST 请求助手（JSON body）
    async fn post_json(
        addr: &std::net::SocketAddr,
        path: &str,
        body: &serde_json::Value,
    ) -> serde_json::Value {
        let raw = serde_json::to_vec(body).unwrap();
        let resp = http_request(addr, "POST", path, Some("application/json"), Some(&raw)).await;
        serde_json::from_slice(http_body(&resp)).expect("响应应为 JSON")
    }

    #[tokio::test]
    async fn workshop_save_writes_player_wat_and_registers_archive() {
        // 场景：POST /api/player-code {name, code} -> ok:true -> players/<名>.wat
        // 落盘（覆盖写）+ 本机角色档案登记（角色文件列表数据源=player_roster，
        // 档案不登记则列表看不见本卡保存的角色——create_player 成功路径同款）
        let static_dir = temp_static_dir("ws-save");
        let players_dir = temp_net_players_dir("ws-save");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("ws-save"),
                &players_dir,
                None,
                &temp_bridge_vault_path("ws-save"),
            )
            .await;
        let body = serde_json::json!({
            "name": "工坊造物",
            "code": "(module (func (export \"life\") (result i32) i32.const 1))",
        });
        let json = post_json(&addr, "/api/player-code", &body).await;
        assert_eq!(json["ok"], true, "保存成功应回 ok:true：{json}");
        let written = std::fs::read_to_string(players.join("工坊造物.wat")).expect("文件应落盘");
        assert!(written.contains("life"), "内容=提交代码原文");
        let archive = std::fs::read_to_string(players_dir.join(crate::onboarding::MY_CHARACTERS_FILE))
            .expect("档案应存在");
        assert!(
            archive.contains("工坊造物"),
            "保存成功应登记本机角色档案（player_roster 同源）"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn workshop_save_rejects_bad_name_oversize_and_missing_field() {
        // 三条拒绝路：名字闸门同源 sanitize_name（a/b=NAME_BAD_CHAR）/ 代码超
        // 256KB=CODE_TOO_LONG / 缺字段=BAD_JSON；全部拒绝零落盘
        let static_dir = temp_static_dir("ws-rej");
        let players_dir = temp_net_players_dir("ws-rej");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("ws-rej"),
                &players_dir,
                None,
                &temp_bridge_vault_path("ws-rej"),
            )
            .await;
        // 危险名字（路径注入）拒绝
        let json = post_json(
            &addr,
            "/api/player-code",
            &serde_json::json!({ "name": "a/b", "code": "(module)" }),
        )
        .await;
        assert_eq!(json["ok"], false);
        assert_eq!(json["reason"], "NAME_BAD_CHAR");
        // 代码超上限（256KB+1）拒绝
        let big = format!("(module){}", " ".repeat(256 * 1024));
        let json = post_json(
            &addr,
            "/api/player-code",
            &serde_json::json!({ "name": "bigguy", "code": big }),
        )
        .await;
        assert_eq!(json["ok"], false);
        assert_eq!(json["reason"], "CODE_TOO_LONG");
        // 缺 name 字段拒绝
        let raw = br#"{"code":"(module)"}"#;
        let resp = http_request(&addr, "POST", "/api/player-code", Some("application/json"), Some(raw)).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["reason"], "BAD_JSON");
        // 全部拒绝零落盘
        assert!(
            !players.join("a").exists() && !players.join("bigguy.wat").exists(),
            "拒绝路径不得写任何文件"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn workshop_read_returns_saved_code_and_unknown_rejected() {
        // GET /api/player-code?name= -> 读回全文；不存在=NAME_UNKNOWN；坏名=NAME_BAD_CHAR
        let static_dir = temp_static_dir("ws-read");
        let players_dir = temp_net_players_dir("ws-read");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, _players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("ws-read"),
                &players_dir,
                None,
                &temp_bridge_vault_path("ws-read"),
            )
            .await;
        let body = serde_json::json!({ "name": "wstower", "code": "(module)\n;; 塔骨架" });
        assert_eq!(post_json(&addr, "/api/player-code", &body).await["ok"], true);
        let resp = http_request(&addr, "GET", "/api/player-code?name=wstower", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["code"], "(module)\n;; 塔骨架", "读回=保存原文");
        // 不存在的角色
        let resp = http_request(&addr, "GET", "/api/player-code?name=nobody", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["reason"], "NAME_UNKNOWN");
        // 危险名字（前端万一没拦）
        let resp = http_request(&addr, "GET", "/api/player-code?name=a%2Fb", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["reason"], "NAME_BAD_CHAR");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn workshop_read_rejects_oversized_file_on_disk() {
        // 读护栏（与写路径对称）：>256KB 文件直接落盘构造（绕过 POST 上限门槛），
        // GET 拒 CODE_TOO_LONG，不把大文件整读进响应体
        let static_dir = temp_static_dir("ws-read-big");
        let players_dir = temp_net_players_dir("ws-read-big");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("ws-read-big"),
                &players_dir,
                None,
                &temp_bridge_vault_path("ws-read-big"),
            )
            .await;
        std::fs::write(players.join("bigguy.wat"), vec![b'a'; 256 * 1024 + 1]).unwrap();
        let resp = http_request(&addr, "GET", "/api/player-code?name=bigguy", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], false, "超上限文件应拒绝：{json}");
        assert_eq!(json["reason"], "CODE_TOO_LONG");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn workshop_delete_removes_wat_then_unknown() {
        // DELETE /api/player-code?name= -> 文件消失；再删=NAME_UNKNOWN；坏名=NAME_BAD_CHAR
        //（不碰 me.txt——造物文件管理≠操控声明）
        let static_dir = temp_static_dir("ws-del");
        let players_dir = temp_net_players_dir("ws-del");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("ws-del"),
                &players_dir,
                None,
                &temp_bridge_vault_path("ws-del"),
            )
            .await;
        std::fs::write(players.join("me.txt"), "wstower\n").unwrap();
        let body = serde_json::json!({ "name": "wstower", "code": "(module)" });
        assert_eq!(post_json(&addr, "/api/player-code", &body).await["ok"], true);
        let resp = http_request(&addr, "DELETE", "/api/player-code?name=wstower", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], true, "删除成功：{json}");
        assert!(!players.join("wstower.wat").exists(), "文件应消失（残影下线归世界轮询）");
        assert!(players.join("me.txt").exists(), "me 声明一字不动");
        // 再删=NAME_UNKNOWN
        let resp = http_request(&addr, "DELETE", "/api/player-code?name=wstower", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["reason"], "NAME_UNKNOWN");
        // 坏名拒绝
        let resp = http_request(&addr, "DELETE", "/api/player-code?name=..", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["reason"], "NAME_BAD_CHAR");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn bridge_config_lists_registered_ports_with_notes() {
        // GET /api/bridge-config（D-022b 扩展）：保险库读锁 -> ports 升序
        // [{port,url,note}] + enabled:true；桥未装配（vault=None）->
        // {ok:true,enabled:false,ports:[]}（前端黄字提示桥未启用，仍可编辑保存）
        let static_dir = temp_static_dir("ws-bridge");
        let players_dir = temp_net_players_dir("ws-bridge");
        let vault_path = temp_bridge_vault_path("ws-bridge");
        std::fs::write(
            &vault_path,
            serde_json::to_string(&serde_json::json!([
                { "port": 2, "url": "http://127.0.0.1:9002", "note": "后装端点" },
                { "port": 1, "url": "http://localhost:9001", "note": "外脑服务器" },
            ]))
            .unwrap(),
        )
        .unwrap();
        let vault = Arc::new(std::sync::RwLock::new(crate::bridges::BridgeVault::load(
            &vault_path,
        )));
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, _players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("ws-bridge"),
                &players_dir,
                Some(vault),
                &vault_path,
            )
            .await;
        let resp = http_request(&addr, "GET", "/api/bridge-config", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["enabled"], true, "vault 装配=enabled:true");
        let ports = json["ports"].as_array().expect("ports 数组恒有");
        assert_eq!(ports.len(), 2, "登记的两条端点都该列出");
        assert_eq!(ports[0]["port"], 1, "端口升序");
        assert_eq!(ports[0]["url"], "http://localhost:9001", "url 回显（编辑框回填）");
        assert_eq!(ports[0]["note"], "外脑服务器", "note 原样透传");
        assert_eq!(ports[1]["port"], 2);
        assert_eq!(ports[1]["url"], "http://127.0.0.1:9002");
        assert_eq!(ports[1]["note"], "后装端点");
        shutdown_tx.send(()).ok();
        let _ = server.await;
        let _ = std::fs::remove_file(&vault_path);
        // 桥未装配：enabled:false 空列表（ok 仍 true）
        let static_dir = temp_static_dir("ws-bridge2");
        let players_dir = temp_net_players_dir("ws-bridge2");
        let vault_path2 = temp_bridge_vault_path("ws-bridge2");
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, _players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("ws-bridge2"),
                &players_dir,
                None,
                &vault_path2,
            )
            .await;
        let resp = http_request(&addr, "GET", "/api/bridge-config", None, None).await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["enabled"], false, "无保险库=enabled:false");
        assert_eq!(json["ports"].as_array().unwrap().len(), 0, "无保险库=空列表");
        shutdown_tx.send(()).ok();
        let _ = server.await;
        let _ = std::fs::remove_file(&vault_path2);
    }

    // === D-022b 外桥可编辑（POST /api/bridge-config 写通道）===

    #[tokio::test]
    async fn bridge_config_post_saves_valid_batch_get_echoes_url() {
        // T1：POST 合法单端点 -> ok:true -> 文件落盘内容正确（读回断言）->
        // GET 立即回显含 url（内存同步，不等 mtime 拍）+ enabled:true
        let static_dir = temp_static_dir("bc-post");
        let players_dir = temp_net_players_dir("bc-post");
        let vault_path = temp_bridge_vault_path("bc-post");
        let _ = std::fs::remove_file(&vault_path);
        let vault = Arc::new(std::sync::RwLock::new(crate::bridges::BridgeVault::load(
            &vault_path,
        )));
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, _players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("bc-post"),
                &players_dir,
                Some(vault),
                &vault_path,
            )
            .await;
        let body = serde_json::json!({
            "ports": [ { "port": 1, "url": "http://127.0.0.1:9001", "note": "外脑服务器" } ]
        });
        let json = post_json(&addr, "/api/bridge-config", &body).await;
        assert_eq!(json["ok"], true, "合法整批应保存成功：{json}");
        // 文件落盘（读回断言：port/url/note 齐；headers 字段不凭空出现）
        let text = std::fs::read_to_string(&vault_path).expect("bridges.json 应落盘");
        let list: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(list[0]["port"], 1);
        assert_eq!(list[0]["url"], "http://127.0.0.1:9001");
        assert_eq!(list[0]["note"], "外脑服务器");
        assert!(list[0].get("headers").is_none(), "无凭证可并=不写 headers 字段");
        // GET 立即回显（内存同步生效）
        let resp = http_request(&addr, "GET", "/api/bridge-config", None, None).await;
        let get: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(get["enabled"], true, "vault 装配=enabled:true");
        let ports = get["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 1, "保存后 GET 应见到新端点");
        assert_eq!(ports[0]["port"], 1);
        assert_eq!(ports[0]["url"], "http://127.0.0.1:9001", "GET 回显 url（编辑框回填）");
        assert_eq!(ports[0]["note"], "外脑服务器");
        shutdown_tx.send(()).ok();
        let _ = server.await;
        let _ = std::fs::remove_file(&vault_path);
    }

    #[tokio::test]
    async fn bridge_config_post_rejects_invalid_batches_and_keeps_file() {
        // T2：非法批（port=0 / url=https / 非回环 host / 非回环域名）-> ok:false
        // 对应 reason（带 index 定位），文件不被改写，GET 仍回旧表
        let static_dir = temp_static_dir("bc-rej");
        let players_dir = temp_net_players_dir("bc-rej");
        let vault_path = temp_bridge_vault_path("bc-rej");
        std::fs::write(
            &vault_path,
            r#"[{"port": 5, "url": "http://127.0.0.1:9005", "note": "保持原样"}]"#,
        )
        .unwrap();
        let vault = Arc::new(std::sync::RwLock::new(crate::bridges::BridgeVault::load(
            &vault_path,
        )));
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, _players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("bc-rej"),
                &players_dir,
                Some(vault),
                &vault_path,
            )
            .await;
        // 四种非法形态：port=0 / https / 非回环 IP / 非回环域名（各自一条 POST）
        let cases = [
            (
                serde_json::json!({ "ports": [
                    { "port": 1, "url": "http://127.0.0.1:9001" },
                    { "port": 0, "url": "http://127.0.0.1:9002" }
                ] }),
                "PORT_INVALID",
                1,
            ),
            (
                serde_json::json!({ "ports": [
                    { "port": 2, "url": "https://127.0.0.1:9003" }
                ] }),
                "URL_INVALID",
                0,
            ),
            (
                serde_json::json!({ "ports": [
                    { "port": 3, "url": "http://10.0.0.5:80" }
                ] }),
                "URL_INVALID",
                0,
            ),
            (
                serde_json::json!({ "ports": [
                    { "port": 4, "url": "http://Example.COM" }
                ] }),
                "URL_INVALID",
                0,
            ),
            (
                serde_json::json!({ "ports": [
                    { "port": 6, "url": "http://127.0.0.1:0" }
                ] }),
                "URL_INVALID",
                0,
            ),
        ];
        for (body, reason, index) in cases {
            let json = post_json(&addr, "/api/bridge-config", &body).await;
            assert_eq!(json["ok"], false, "非法批应拒绝：{body}");
            assert_eq!(json["reason"], reason, "原因码：{json}");
            assert_eq!(json["index"], index, "定位下标：{json}");
            // 文件不被改写（原表仍在）
            let text = std::fs::read_to_string(&vault_path).unwrap();
            assert!(
                text.contains("保持原样"),
                "拒绝后文件不被改写（body={body}）"
            );
        }
        // GET 仍回旧表（内存未被污染）
        let resp = http_request(&addr, "GET", "/api/bridge-config", None, None).await;
        let get: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        let ports = get["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 1, "拒绝后内存仍旧表");
        assert_eq!(ports[0]["port"], 5);
        assert_eq!(ports[0]["note"], "保持原样");
        shutdown_tx.send(()).ok();
        let _ = server.await;
        let _ = std::fs::remove_file(&vault_path);
    }

    #[tokio::test]
    async fn bridge_config_post_empty_array_clears_registry() {
        // T3：POST 空数组 -> 清空登记（文件空表）-> GET ports 空
        let static_dir = temp_static_dir("bc-empty");
        let players_dir = temp_net_players_dir("bc-empty");
        let vault_path = temp_bridge_vault_path("bc-empty");
        std::fs::write(
            &vault_path,
            r#"[{"port": 1, "url": "http://127.0.0.1:9001", "note": "将被清空"}]"#,
        )
        .unwrap();
        let vault = Arc::new(std::sync::RwLock::new(crate::bridges::BridgeVault::load(
            &vault_path,
        )));
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, _players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("bc-empty"),
                &players_dir,
                Some(vault),
                &vault_path,
            )
            .await;
        let json = post_json(&addr, "/api/bridge-config", &serde_json::json!({ "ports": [] })).await;
        assert_eq!(json["ok"], true, "空数组=清空登记，应受理：{json}");
        let list: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&vault_path).unwrap()).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 0, "文件应为空表");
        let resp = http_request(&addr, "GET", "/api/bridge-config", None, None).await;
        let get: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(get["ports"].as_array().unwrap().len(), 0, "GET 应回空列表");
        shutdown_tx.send(()).ok();
        let _ = server.await;
        let _ = std::fs::remove_file(&vault_path);
    }

    #[tokio::test]
    async fn bridge_config_post_works_without_vault_enabled_flag_false() {
        // T4：vault=None（桥未启用 CODEWORLD_BRIDGE）-> POST 也受理（文件落盘，
        // 桥下次启用即用）；GET enabled:false ports:[]（仍可编辑保存的语义）
        let static_dir = temp_static_dir("bc-novault");
        let players_dir = temp_net_players_dir("bc-novault");
        let vault_path = temp_bridge_vault_path("bc-novault");
        let _ = std::fs::remove_file(&vault_path);
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, _players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("bc-novault"),
                &players_dir,
                None,
                &vault_path,
            )
            .await;
        let body = serde_json::json!({
            "ports": [ { "port": 1, "url": "http://localhost:9001", "note": "桥未启用也保存" } ]
        });
        let json = post_json(&addr, "/api/bridge-config", &body).await;
        assert_eq!(json["ok"], true, "vault 未装配也应受理写盘：{json}");
        let text = std::fs::read_to_string(&vault_path).expect("文件应落盘（桥下次启用即用）");
        let list: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(list[0]["port"], 1);
        assert_eq!(list[0]["url"], "http://localhost:9001");
        // GET：enabled:false + ports 空（无 vault 内存可回显，文件内容不进 GET）
        let resp = http_request(&addr, "GET", "/api/bridge-config", None, None).await;
        let get: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(get["ok"], true);
        assert_eq!(get["enabled"], false);
        assert_eq!(get["ports"].as_array().unwrap().len(), 0);
        shutdown_tx.send(()).ok();
        let _ = server.await;
        let _ = std::fs::remove_file(&vault_path);
    }

    #[tokio::test]
    async fn bridge_config_post_bad_json_rejected() {
        // T5：请求体非 JSON -> ok:false reason:BAD_JSON（文件不动）
        let static_dir = temp_static_dir("bc-badjson");
        let players_dir = temp_net_players_dir("bc-badjson");
        let vault_path = temp_bridge_vault_path("bc-badjson");
        let _ = std::fs::remove_file(&vault_path);
        let (addr, _tx, _queue, shutdown_tx, server, _reg, _roster, _players) =
            start_test_server_full(
                static_dir,
                &temp_assets_dir("bc-badjson"),
                &players_dir,
                None,
                &vault_path,
            )
            .await;
        let resp = http_request(
            &addr,
            "POST",
            "/api/bridge-config",
            Some("application/json"),
            Some(b"this is not json".as_slice()),
        )
        .await;
        let json: serde_json::Value = serde_json::from_slice(http_body(&resp)).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["reason"], "BAD_JSON");
        assert!(!vault_path.exists(), "BAD_JSON 不得写盘");
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }
}
