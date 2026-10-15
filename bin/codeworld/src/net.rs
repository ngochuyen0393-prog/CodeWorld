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
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use codeworld_core::entity::EntityId;
use codeworld_core::world::WorldState;
use codeworld_render::Viewport;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::services::ServeDir;

/// 快照广播通道容量（未消费帧积压上限；慢客户端丢旧帧即可，快照有"后值覆盖"语义）
pub(crate) const SNAPSHOT_CHANNEL_CAP: usize = 16;

// ---- 上行意图协议（docs/net-protocol.md v1.2 客户端 -> 服务器，#027）----

/// 客户端上行消息（三型）：点击走位 / 点击出刀 / 停止。
/// 坐标一律世界坐标（快照 viewport 域内），非法值解析层拒绝。
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum ClientMessage {
    Move { x: f64, y: f64 },
    Attack { x: f64, y: f64 },
    Stop,
}

/// 上行文本 -> 客户端消息（纯函数；Err 带原因供 warn 丢弃，不 panic 不部分接受）
pub(crate) fn parse_client_message(text: &str) -> Result<ClientMessage, String> {
    let msg: ClientMessage =
        serde_json::from_str(text).map_err(|e| format!("JSON/协议不符: {e}"))?;
    let finite = |v: f64| v.is_finite();
    let coords_ok = match &msg {
        ClientMessage::Move { x, y } | ClientMessage::Attack { x, y } => finite(*x) && finite(*y),
        ClientMessage::Stop => true,
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
}

/// kind 判定（纯映射，任务书定案）：开局四生物=npc；creation=造物；
/// players 目录在线名（或残影躯壳）=player；其余未知 code_hash 兜底 npc。
fn entity_kind(code_hash: &str, entity_id: EntityId, players: &PlayerDirectory) -> &'static str {
    match code_hash {
        crate::ATTACKER_CODE | crate::DEFENDER_CODE | crate::HUNTER_CODE | crate::SPY_CODE => "npc",
        "creation" => "creation",
        name if players.is_online(name) => "player",
        _ if players.is_ghost(&entity_id) => "player",
        _ => "npc",
    }
}

/// 组装世界快照（每拍一帧全量投影；实体按 id 字典序稳定排列，前端逐帧 diff 依赖帧序稳定）
pub(crate) fn build_snapshot(
    world: &WorldState,
    caption: &str,
    viewport: &Viewport,
    players: &PlayerDirectory,
    controlled: Option<EntityId>,
) -> WorldSnapshot {
    let (min_x, max_x) = viewport.world_x_range();
    let (min_y, max_y) = viewport.world_y_range();
    let mut entities: Vec<EntitySnapshot> = world
        .entities
        .values()
        .map(|e| {
            let kind = entity_kind(&e.code_hash, e.id, players);
            let (x, y) = e.pos();
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
                owner: (kind == "creation").then(|| e.owner.0.to_string()),
            }
        })
        .collect();
    entities.sort_by(|a, b| a.id.cmp(&b.id));
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
    }
}

/// 静态验收页目录（相对工作目录；start.bat 已 cd 到项目根）
pub(crate) const STATIC_DIR: &str = "bin/codeworld/static";

/// WS 服务器共享状态：快照广播 + 上行意图队列 + 操控连接令牌（#027 单连接假设）
pub(crate) struct WsState {
    pub(crate) snapshots: broadcast::Sender<String>,
    /// 与终端键盘共享的意图队列生产端（单入口双来源，input.rs 路由不区分来源）
    pub(crate) intents: IntentSink,
    /// 操控连接令牌：false=无主（下一连接即操控端）/ true=已被占用。
    /// 第一连接认领、断开释放——"本机浏览器=我"的等价物（#024 me.txt 的连接版）。
    pub(crate) ctrl_taken: Arc<AtomicBool>,
}

/// 生产装配：WS 启用（端口≠0）则起独立 ws-server 线程（内含 tokio runtime），
/// 返回快照发送端供主循环每拍推送；禁用（端口 0）返回 None = 主循环零开销分支。
pub(crate) fn init(config: &crate::config::Config, intents: IntentSink) -> Option<broadcast::Sender<String>> {
    if config.ws_port == 0 {
        tracing::info!("[ws] CODEWORLD_WS_PORT=0，WS 快照推送禁用（纯终端模式）");
        return None;
    }
    let (tx, _) = broadcast::channel(SNAPSHOT_CHANNEL_CAP);
    let port = config.ws_port;
    let state = Arc::new(WsState {
        snapshots: tx.clone(),
        intents,
        ctrl_taken: Arc::new(AtomicBool::new(false)),
    });
    let spawned = std::thread::Builder::new()
        .name("ws-server".to_string())
        .spawn(move || run_ws_thread(port, state));
    match spawned {
        Ok(_) => {
            tracing::info!(
                port,
                "[ws] WS 快照推送已启动（浏览器打开 http://localhost:{port} 看世界；第一连接=操控端）"
            );
            Some(tx)
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

/// 起服务器（axum：/ws 快照广播 + ServeDir 静态验收页一把抓）。
/// shutdown future 完成即优雅停机（生产用永不完成的 pending，测试用 oneshot）。
pub(crate) async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<WsState>,
    static_dir: std::path::PathBuf,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .with_state(state)
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

/// 单连接循环：下行推快照广播，上行解析意图入共享队列（仅操控连接）。
/// 循环退出即连接清理（socket/rx 随任务结束释放，无泄漏）。
async fn handle_socket(socket: WebSocket, state: Arc<WsState>) {
    // 操控权认领（#027 单连接假设）：第一连接=操控端，后来者=观战（上行丢弃）
    let controller = state
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
                        Ok(ClientMessage::Stop) => {
                            state.intents.send(crate::input::Intent::Stop);
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
pub(crate) fn push_snapshot(
    tx: &broadcast::Sender<String>,
    world: &WorldState,
    caption: &str,
    viewport: &Viewport,
    players: &PlayerDirectory,
    controlled: Option<EntityId>,
) {
    if tx.receiver_count() == 0 {
        return;
    }
    let snapshot = build_snapshot(world, caption, viewport, players, controlled);
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
        let snap = build_snapshot(world, "cap", &default_viewport(), players, controlled);
        serde_json::to_value(&snap).expect("快照应可序列化")
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
        push_snapshot(&tx, &world, "c1", &vp, &dir, None);
        world.advance_tick();
        push_snapshot(&tx, &world, "c2", &vp, &dir, None);
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
        push_snapshot(&tx, &world, "c", &vp, &dir, None);
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
        let (tx, _) = broadcast::channel::<String>(SNAPSHOT_CHANNEL_CAP);
        let (intents_tx, intents_rx) = crate::input::intent_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let state = Arc::new(WsState {
            snapshots: tx.clone(),
            intents: intents_tx,
            ctrl_taken: Arc::new(AtomicBool::new(false)),
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
        (addr, tx, intents_rx, shutdown_tx, handle)
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

        let mut world = WorldState::new();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        world.advance_tick();
        push_snapshot(&tx, &world, "f1", &vp, &dir, None);
        world.advance_tick();
        push_snapshot(&tx, &world, "f2", &vp, &dir, None);

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
        world.advance_tick();
        push_snapshot(&tx, &world, "f3", &vp, &dir, None);
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

        let mut world = WorldState::new();
        let dir = PlayerDirectory::disabled();
        let vp = default_viewport();
        world.advance_tick();
        push_snapshot(&tx, &world, "shared", &vp, &dir, None);

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
}
