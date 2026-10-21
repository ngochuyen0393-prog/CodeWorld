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
use std::sync::atomic::{AtomicBool, Ordering};
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
    Stop,
    /// 登出处置三选一（#025）：take/stay_auto/stay_ai；仅操控连接生效，
    /// 处置由 tick 侧状态机落地（本层只转发意图+释放令牌）
    Logout {
        #[serde(rename = "disposition")]
        choice: crate::dispose::LogoutChoice,
    },
}

/// 上行文本 -> 客户端消息（纯函数；Err 带原因供 warn 丢弃，不 panic 不部分接受）
pub(crate) fn parse_client_message(text: &str) -> Result<ClientMessage, String> {
    let msg: ClientMessage =
        serde_json::from_str(text).map_err(|e| format!("JSON/协议不符: {e}"))?;
    let finite = |v: f64| v.is_finite();
    let coords_ok = match &msg {
        ClientMessage::Move { x, y } | ClientMessage::Attack { x, y } => finite(*x) && finite(*y),
        ClientMessage::Shoot { x, y } => finite(*x) && finite(*y),
        ClientMessage::Stop | ClientMessage::Logout { .. } => true,
    };
    if !coords_ok {
        return Err("坐标非有限值（NaN/无穷）".to_string());
    }
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
}

/// 单个力场投影（D-012：场源坐标+半径+成型态）
#[derive(Debug, Serialize)]
pub(crate) struct FieldSnapshot {
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
/// name 恒有（v1.1，= code_hash 代码身份）；npc 无可选字段（skip_serializing_if 保持 JSON 干净）。
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
}

/// kind 判定（纯映射，任务书定案）：开局四生物=npc；creation=造物（static 钉死）；
/// creation_dynamic=D-003 全活造物（motion=1，圆身体受完整物理）；
/// 弹丸=projectile（#034 弹道系统：前端画小圆点+尾迹）；players 目录在线名
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
        codeworld_core::collision::PROJECTILE_CODE => "projectile",
        name if players.is_online(name) => "player",
        _ if players.is_ghost(&entity_id) => "player",
        _ => "npc",
    }
}

/// 组装世界快照（每拍一帧全量投影；实体按 id 字典序稳定排列，前端逐帧 diff 依赖帧序稳定）
/// attack_events（v2.0，D-017）：本拍结算成功的攻击命中事件，随快照广播
pub(crate) fn build_snapshot(
    world: &WorldState,
    caption: &str,
    viewport: &Viewport,
    players: &PlayerDirectory,
    controlled: Option<EntityId>,
    attack_events: &[crate::AttackEventRecord],
    field_forming_ticks: u64,
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
            EntitySnapshot {
                id: e.id.0.to_string(),
                glyph: e.appearance.glyph.clone(),
                color: e.appearance.color.clone(),
                x,
                y,
                radius: e.radius(),
                kind,
                name: e.code_hash.clone(),
                controlled: (kind == "player").then(|| Some(e.id) == controlled),
                owner: (kind == "creation" || kind == "creation_dynamic")
                    .then(|| e.owner.0.to_string()),
                disposition: disposed.map(|s| s.wire_name()),
                wait_left: disposed.and_then(|s| s.wait_left(now)),
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
    // forming_ticks 判（虚线环=成型中给进攻方反应窗，实线环=已成型）
    let fields: Vec<FieldSnapshot> = world
        .entities
        .values()
        .filter(|e| e.is_alive() && e.field_radius() > 0.0)
        .map(|e| {
            let (x, y) = e.pos();
            FieldSnapshot {
                x,
                y,
                radius: e.field_radius(),
                formed: world.tick().saturating_sub(e.created_tick) >= field_forming_ticks,
            }
        })
        .collect();
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
    }
}

/// 静态验收页目录（相对工作目录；start.bat 已 cd 到项目根）
pub(crate) const STATIC_DIR: &str = "bin/codeworld/static";

/// WS 服务器共享状态：快照广播 + 上行意图队列 + 操控连接令牌（#027 单连接假设）
/// + 头像资产登记表（D-017：上传路由写入 / ws 连接建立时全量下发）
pub(crate) struct WsState {
    pub(crate) snapshots: broadcast::Sender<String>,
    /// 与终端键盘共享的意图队列生产端（单入口双来源，input.rs 路由不区分来源）
    pub(crate) intents: IntentSink,
    /// 操控连接令牌：false=无主（下一连接即操控端）/ true=已被占用。
    /// 第一连接认领、断开释放——"本机浏览器=我"的等价物（#024 me.txt 的连接版）。
    pub(crate) ctrl_taken: Arc<AtomicBool>,
    /// 头像资产登记表（D-017 资产层：上传落盘+内存索引，掩码连接建立时下发）
    pub(crate) avatars: Arc<AvatarRegistry>,
}

/// 主循环持有的 WS 句柄：快照推送端 + 操控令牌观测（#025 期五第一刀）。
/// 主循环每拍读 ctrl_taken 做翻转沿检测（Claimed/Released），作为处置状态机触发源。
pub(crate) struct WsHandle {
    pub(crate) snapshots: broadcast::Sender<String>,
    pub(crate) ctrl_taken: Arc<AtomicBool>,
}

/// 操控连接令牌翻转事件（主循环每拍沿检测；WS 禁用时恒 None）——
/// #025 处置状态机的两条触发源：Claimed=重连认领（取消处置/重新进场），
/// Released=意外断开（进入处置等待；主动登出沿由 tick 侧 logout_handled 抑制）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnEvent {
    Claimed,
    Released,
}

/// 生产装配：WS 启用（端口≠0）则起独立 ws-server 线程（内含 tokio runtime），
/// 返回句柄（快照推送端 + 操控令牌观测）供主循环使用；禁用（端口 0）返回 None。
pub(crate) fn init(config: &crate::config::Config, intents: IntentSink) -> Option<WsHandle> {
    if config.ws_port == 0 {
        tracing::info!("[ws] CODEWORLD_WS_PORT=0，WS 快照推送禁用（纯终端模式）");
        return None;
    }
    let (tx, _) = broadcast::channel(SNAPSHOT_CHANNEL_CAP);
    let port = config.ws_port;
    // 头像资产登记表（D-017）：启动扫目录重建（坏文件 warn 跳过不拖垮世界）
    let avatars = Arc::new(AvatarRegistry::load(Path::new(AVATARS_DIR)));
    let state = Arc::new(WsState {
        snapshots: tx.clone(),
        intents,
        ctrl_taken: Arc::new(AtomicBool::new(false)),
        avatars,
    });
    let handle = WsHandle {
        snapshots: tx,
        ctrl_taken: state.ctrl_taken.clone(),
    };
    let spawned = std::thread::Builder::new()
        .name("ws-server".to_string())
        .spawn(move || run_ws_thread(port, state));
    match spawned {
        Ok(_) => {
            tracing::info!(
                port,
                "[ws] WS 快照推送已启动（浏览器打开 http://localhost:{port} 看世界；第一连接=操控端）"
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

/// WS 升级入口：连接建立后进入快照广播循环
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WsState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
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

/// 单连接循环：下行推快照广播，上行解析意图入共享队列（仅操控连接）。
/// 循环退出即连接清理（socket/rx 随任务结束释放，无泄漏）。
async fn handle_socket(socket: WebSocket, state: Arc<WsState>) {
    // 操控权认领（#027 单连接假设）：第一连接=操控端，后来者=观战（上行丢弃）
    let mut controller = state
        .ctrl_taken
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok();
    if controller {
        tracing::info!("[ws] 客户端连接（操控连接：上行意图生效）");
    } else {
        tracing::info!("[ws] 客户端连接（观战连接：操控权已被占用）");
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
    loop {
        tokio::select! {
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
                    if !controller {
                        tracing::warn!("[ws] 观战连接的上行消息已丢弃（单连接假设：非操控端）");
                        continue;
                    }
                    match parse_client_message(&text) {
                        Ok(ClientMessage::Move { x, y }) => {
                            state.intents.send(crate::input::Intent::WsMove { x, y });
                        }
                        Ok(ClientMessage::Attack { x, y }) => {
                            state.intents.send(crate::input::Intent::WsAttack { x, y });
                        }
                        Ok(ClientMessage::Shoot { x, y }) => {
                            state.intents.send(crate::input::Intent::WsShoot { x, y });
                        }
                        Ok(ClientMessage::Stop) => {
                            state.intents.send(crate::input::Intent::Stop);
                        }
                        Ok(ClientMessage::Logout { choice }) => {
                            // #025 登出处置：意图入共享队列（与键盘/鼠标单入口），
                            // 处置三选一由 tick 侧状态机落地；本层只做令牌语义——
                            // 主动登出=立即释放操控令牌（重连的第一个连接重新认领），
                            // 本连接降级观战：下行快照继续（登出后世界还能看），上行丢弃
                            tracing::info!(
                                choice = choice.wire_name(),
                                "[dispose] WS 收到登出消息（令牌释放，连接降级观战）"
                            );
                            state.intents.send(crate::input::Intent::Logout { choice });
                            state.ctrl_taken.store(false, Ordering::SeqCst);
                            controller = false;
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
    if controller {
        state.ctrl_taken.store(false, Ordering::SeqCst);
        tracing::info!("[ws] 操控连接断开（操控权已释放，下一连接接管）");
    } else {
        tracing::info!("[ws] 客户端断开（连接资源已清理）");
    }
}

/// tick 侧推送（std 主循环每拍调用）：无观众不组装（零客户端零开销），
/// 序列化成功推广播通道；无接收者 send 报错属正常（忽略）。
/// 8 参系快照契约全量投影（tick/caption/viewport/players/controlled/attacks/
/// forming_ticks）——clippy too_many_arguments 豁免：参数组与 build_snapshot
/// 一一对应，结构体化反而多一层拆装
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
) {
    if tx.receiver_count() == 0 {
        return;
    }
    let snapshot = build_snapshot(world, caption, viewport, players, controlled, attack_events, field_forming_ticks);
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
        let snap = build_snapshot(world, "cap", &default_viewport(), players, controlled, &[], 3);
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
        let snap = build_snapshot(&world, "cap", &default_viewport(), &dir, None, &events, 3);
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
        let snap = build_snapshot(&world, "cap", &default_viewport(), &dir, None, &[], 3);
        let json = serde_json::to_value(&snap).expect("快照应可序列化");
        assert_eq!(
            json["attacks"].as_array().map(Vec::len),
            Some(0),
            "attacks 空数组恒有"
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
        // 规则：code_hash="creation" = 造物，owner = 创造者 PlayerId 的 UUID 字符串
        let mut world = WorldState::new();
        let (_, owner) = put_entity(&mut world, "creation", "T", "cyan", (40.0, 0.0));
        let dir = PlayerDirectory::disabled();
        let json = build(&world, &dir, None);
        let e = &json["entities"][0];
        assert_eq!(e["kind"], "creation");
        assert_eq!(e["name"], "creation", "creation 的 name = code_hash 原文");
        assert_eq!(
            e["owner"],
            owner.0.to_string(),
            "owner = 创造者 PlayerId UUID 字符串"
        );
        assert!(e.get("controlled").is_none(), "creation 不带 controlled");
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
        push_snapshot(&tx, &world, "c1", &vp, &dir, None, &[], 3);
        world.advance_tick();
        push_snapshot(&tx, &world, "c2", &vp, &dir, None, &[], 3);
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
        push_snapshot(&tx, &world, "c", &vp, &dir, None, &[], 3);
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
        let (addr, tx, queue, shutdown_tx, server, _avatars) =
            start_test_server_with_avatars(static_dir, &temp_assets_dir("shared")).await;
        (addr, tx, queue, shutdown_tx, server)
    }

    /// 带资产目录的测试服务器（D-017 上传通道测试用：注册表目录可指定可检查）
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
    ) {
        let (tx, _) = broadcast::channel::<String>(SNAPSHOT_CHANNEL_CAP);
        let (intents_tx, intents_rx) = crate::input::intent_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let avatars = Arc::new(avatars::AvatarRegistry::load(avatars_dir));
        let state = Arc::new(WsState {
            snapshots: tx.clone(),
            intents: intents_tx,
            ctrl_taken: Arc::new(AtomicBool::new(false)),
            avatars: avatars.clone(),
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
        (addr, tx, intents_rx, shutdown_tx, handle, avatars)
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
        let (addr, _tx, _queue, shutdown_tx, server, reg) =
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
        let (addr, _tx, _queue, shutdown_tx, server, reg) =
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
        let (addr, _tx, _queue, shutdown_tx, server, _reg) =
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
        let (addr, tx, _queue, shutdown_tx, server, reg) =
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
        // 资产消息是首帧：之后照常收快照（推送一帧验证次序）
        let mut world = WorldState::new();
        world.advance_tick();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        push_snapshot(&tx, &world, "after-assets", &vp, &dir, None, &[], 3);
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
        let (addr, _tx, _queue, shutdown_tx, server, _reg) =
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
        // 场景：第一连接 = 操控连接；发 move JSON -> 解析入共享意图队列（#027 单入口）
        let static_dir = temp_static_dir("up-move");
        let (addr, _tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut client = connect_client(addr).await;
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
        let (addr, _tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut client = connect_client(addr).await;
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
        let (addr, _tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut client = connect_client(addr).await;
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
        let (addr, _tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut client = connect_client(addr).await;
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
    async fn second_connection_uplink_dropped_controller_keeps_queue() {
        // 单连接假设：第一连接拿操控权；第二连接（观战端）上行消息 warn 丢弃不入队；
        // 操控连接的消息照常入队
        let static_dir = temp_static_dir("up-second");
        let (addr, _tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut c1 = connect_client(addr).await;
        let mut c2 = connect_client(addr).await;
        c2.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":99.0,"y":99.0}"#.to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            queue.drain().is_empty(),
            "观战连接的意图不入队（操控权在第一连接）"
        );
        c1.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":12.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsMove { x: 12.0, y: 0.0 },
            "操控连接意图照常入队"
        );
        shutdown_tx.send(()).ok();
        let _ = server.await;
    }

    #[tokio::test]
    async fn controller_released_on_disconnect_next_connection_takes_over() {
        // 操控权随连接生命周期：操控连接断开 -> 释放；下一个连接成为新操控端
        let static_dir = temp_static_dir("up-release");
        let (addr, _tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut c1 = connect_client(addr).await;
        c1.send(tokio_tungstenite::tungstenite::Message::Close(None))
            .await
            .ok();
        drop(c1);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut c2 = connect_client(addr).await;
        c2.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":7.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog).await,
            crate::input::Intent::WsMove { x: 7.0, y: 0.0 },
            "断开后新连接获得操控权（消息入队）"
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

    /// 读掉连接首帧 avatar_assets（v2.2 起连接首帧恒为资产消息；快照类测试先消费它）
    async fn consume_avatar_assets(ws: &mut ClientWs) -> serde_json::Value {
        let v: serde_json::Value =
            serde_json::from_str(&next_text(ws).await).expect("首帧应为合法 JSON");
        assert_eq!(v["type"], "avatar_assets", "首帧恒为 avatar_assets：{v}");
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
        push_snapshot(&tx, &world, "f1", &vp, &dir, None, &[], 3);
        world.advance_tick();
        push_snapshot(&tx, &world, "f2", &vp, &dir, None, &[], 3);

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
        push_snapshot(&tx, &world, "f3", &vp, &dir, None, &[], 3);
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
        push_snapshot(&tx, &world, "shared", &vp, &dir, None, &[], 3);

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
            init(&cfg, intent_channel().0).is_none(),
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

    #[tokio::test]
    async fn logout_message_releases_token_and_demotes_to_spectator() {
        // #025 令牌语义：主动登出=立即释放操控令牌+本连接降级观战——
        // ① Logout 意图入共享队列（处置由 tick 侧落地）；
        // ② 令牌释放后新连接成为操控端（重连的第一个连接重新认领）；
        // ③ 登出后的原连接降级观战：上行丢弃、下行快照照收（登出后世界还能看）
        let static_dir = temp_static_dir("logout");
        let (addr, tx, queue, shutdown_tx, server) = start_test_server(static_dir).await;
        let mut c1 = connect_client(addr).await;

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

        // ② 新连接拿到操控权（c1 已释放令牌）
        let mut c2 = connect_client(addr).await;
        c2.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":1.0,"y":0.0}"#.to_string(),
        ))
        .await
        .unwrap();
        let mut backlog2 = Vec::new();
        assert_eq!(
            next_intent(&queue, &mut backlog2).await,
            crate::input::Intent::WsMove { x: 1.0, y: 0.0 },
            "登出后新连接成为操控端（重连认领）"
        );

        // ③ c1 降级观战：再发 move 被丢弃（不入队）；下行快照照收
        c1.send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"move","x":99.0,"y":99.0}"#.to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(queue.drain().is_empty(), "登出连接的后续上行已丢弃");
        consume_avatar_assets(&mut c1).await; // v2.2 首帧=资产消息（连接时排队未消费）
        let mut world = WorldState::new();
        world.advance_tick();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        push_snapshot(&tx, &world, "after-logout", &vp, &dir, None, &[], 3);
        let frame: serde_json::Value = serde_json::from_str(&next_text(&mut c1).await).unwrap();
        assert_eq!(frame["caption"], "after-logout", "登出连接下行快照照收（观战）");

        shutdown_tx.send(()).ok();
        let _ = server.await;
    }
}
