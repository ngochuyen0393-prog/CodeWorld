//! 玩家事件流（D-020a，t_06b4efc6）：控制台面板的数据源。
//! 六类事件全部是既有 WARN/INFO 日志点的结构化投影（零新立法）；
//! 服务端按操控玩家过滤后再广播（可见性铁律 decisions #054 同族——
//! 你的内存里有什么你才看得到什么；拿不准归属的一律不发，宁缺勿泄）。

use crate::players::PlayerDirectory;
use codeworld_core::entity::EntityId;
use codeworld_core::world::WorldState;
use serde::Serialize;

/// spawn 拒绝原因码表（api.md 手册 + 事件流 reason 字段共用；SCREAMING_SNAKE）。
/// 集中一处定义：事件流与将来 import 返回值细化共用，禁止散落字面量。
/// allow(dead_code)：码表=api.md 契约全集（13 码），当前事件流只消费阶段二
/// 作废六码——其余成员（import 层当场拒绝码）是文档化常量，非死代码
#[allow(dead_code)]
pub(crate) mod reason {
    pub const RATE_LIMITED: &str = "RATE_LIMITED";
    pub const BAD_PARAMS: &str = "BAD_PARAMS";
    pub const BAD_GLYPH: &str = "BAD_GLYPH";
    pub const BAD_COLOR: &str = "BAD_COLOR";
    pub const BAD_BLOB: &str = "BAD_BLOB";
    pub const BIRTHPLACE_OCCUPIED: &str = "BIRTHPLACE_OCCUPIED";
    pub const OUT_OF_RANGE: &str = "OUT_OF_RANGE";
    pub const UNDERGROUND: &str = "UNDERGROUND";
    pub const NON_FINITE: &str = "NON_FINITE";
    pub const LIFESPAN_INVALID: &str = "LIFESPAN_INVALID";
    pub const CREATOR_DEAD: &str = "CREATOR_DEAD";
    pub const WORLD_FULL: &str = "WORLD_FULL";
    pub const COMPILE_FAILED: &str = "COMPILE_FAILED";
}

/// 错误/代码摘要上限（api.md 载明 ≤200 字符）
pub(crate) const ERROR_SNIPPET_MAX: usize = 200;

/// 截断摘要（按字符截不断 UTF-8；WAT 语法错/code_hint 共用）
pub(crate) fn snippet(s: &str) -> String {
    s.chars().take(ERROR_SNIPPET_MAX).collect()
}

/// 单条玩家事件。owner_name 只在服务端过滤用，`#[serde(skip)]` 不进协议；
/// body 用 internally-tagged enum flatten——线上形状
/// `{"seq":..,"tick":..,"entity_id":"..","kind":"born","x":..}`（kind 即协议字段）
#[derive(Debug, Serialize)]
pub(crate) struct PlayerEvent {
    pub seq: u64,
    pub tick: u64,
    pub entity_id: String,
    #[serde(skip)]
    pub owner_name: Option<String>,
    #[serde(flatten)]
    pub body: EventBody,
}

/// 六类事件体（零新立法：全部投影自既有日志点）
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum EventBody {
    /// 阶段二 spawn 作废（import 层当场拒绝玩家已自知，不进事件流）
    SpawnRejected {
        reason: &'static str,
        x: f64,
        y: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        code_hint: Option<String>,
    },
    /// 玩家进场编译失败（整个玩家没进场；无主体，全员可见）
    CompileFailed { name: String, error: String },
    /// 代码运行错（on_tick / life 抛 Err）
    RuntimeError {
        which: &'static str,
        error: String,
        died: bool,
    },
    /// 造物诞生（D-020b 加 name：实体显示名——服务端 emit 时带上，前端零 join）
    Born {
        x: f64,
        y: f64,
        glyph: String,
        lifespan_ticks: u64,
        name: String,
    },
    /// 实体死亡（killed/life_false/lifespan_expired/runtime 四因；D-020b 加 name：
    /// 死者显示名——死者已出快照实体表，前端 join 不到，必须服务端带；
    /// killer 语义 v3.1 起=显示名（玩家名/造物实名），不再是 UUID）
    Died {
        cause: &'static str,
        killer: Option<String>,
        name: String,
    },
    /// 造物调 env.log(marker)
    PlayerLog { marker: i64 },
    /// 玩家进场成功（D-021b 进场仪式）：无主公共新闻（全员可见——面板据它收
    /// 起创建浮层 + 亮"你的生物出生了"横幅）
    PlayerJoined { name: String },
    /// create_player 被拒（D-021b）：无主公共新闻（结构化原因码回执面板红字）
    CreatePlayerRejected { reason: &'static str },
    /// 选角成功（D-021d 登录/选角页）：无主公共新闻——前端收到才断开重连
    /// （防早关丢消息；重连=第一连接重新认领操控令牌）
    PlayerSelected { name: String },
    /// select_player 被拒（D-021d）：无主公共新闻（结构化原因码回执登录页红字：
    /// NAME_UNKNOWN=players/ 无此角色 / ME_ENV_LOCKED=CODEWORLD_ME 已锁定 / IO_ERROR）
    SelectPlayerRejected { reason: &'static str },
}

/// 事件汇：世界单调 seq 计数器 + 本拍待发缓冲（挂在 EntitySandboxMap 上，
/// 随沙箱体系存续；每拍 push_snapshot 时 take_frame 排干——事件不跨拍滞留）
pub(crate) struct EventSink {
    next_seq: u64,
    pending: Vec<PlayerEvent>,
}

impl EventSink {
    pub(crate) fn new() -> Self {
        Self {
            next_seq: 1,
            pending: Vec::new(),
        }
    }

    /// 已发出的最大 seq（快照 events_cursor；从未发过=0）
    pub(crate) fn cursor(&self) -> u64 {
        self.next_seq - 1
    }

    /// 归属解析（纯函数，D-020a 双审 P0 修复后口径）：**只认实体 id 不认
    /// code_hash 字符串**——玩家可命名 creation.wat/attacker.wat 进场，字符串
    /// 判归属=身份碰撞绕穿可见性铁律。链路：主体 owner（玩家=自有实体 id/
    /// 造物=创造者实体 id，进场产权根修复后统一形态）→ owner 实体是在线玩家
    /// → 归属其名；owner 不可达时 bound_to 兜底一跳；全不通 = None（宁缺勿泄）。
    /// pub(crate)：D-020b 技能格模板入列/落名复用同一产权链解析（单一实现防两套口径）
    pub(crate) fn resolve_owner(
        world: &WorldState,
        players: &PlayerDirectory,
        subject: EntityId,
    ) -> Option<String> {
        let e = world.entities.get(&subject)?;
        let root = world.entities.get(&EntityId(e.owner.0))?;
        if players.is_online_entity(&root.id) {
            return Some(root.code_hash.clone());
        }
        // 兜底：owner 不可达（旧存档实体/测试直构随机 owner）走 bound_to 一跳
        let bound = world.entities.get(&e.bound_to()?)?;
        players.is_online_entity(&bound.id).then(|| bound.code_hash.clone())
    }

    /// 有主事件入汇（归属拿不准也照收——过滤在 take_frame 统一做，
    /// emit 侧只记账不裁剪；seq 对所有入汇事件单调递增）
    pub(crate) fn emit(
        &mut self,
        world: &WorldState,
        players: &PlayerDirectory,
        subject: EntityId,
        tick: u64,
        body: EventBody,
    ) {
        let owner_name = Self::resolve_owner(world, players, subject);
        // 归属决策 debug 级：服务端过滤链路可 grep，不刷 INFO 屏
        tracing::debug!(
            seq = self.next_seq,
            subject = %subject.0,
            owner = ?owner_name,
            "[events] 事件入汇（归属已解析，广播过滤在 take_frame）"
        );
        self.pending.push(PlayerEvent {
            seq: self.next_seq,
            tick,
            entity_id: subject.0.to_string(),
            owner_name,
            body,
        });
        self.next_seq += 1;
    }

    /// 无主事件入汇（compile_failed 专用：整个玩家没进场，无主体可归属——
    /// 全员可见；entity_id=空串）
    pub(crate) fn emit_unowned(&mut self, tick: u64, body: EventBody) {
        self.pending.push(PlayerEvent {
            seq: self.next_seq,
            tick,
            entity_id: String::new(),
            owner_name: None,
            body,
        });
        self.next_seq += 1;
    }

    /// 取本拍待发帧（服务端可见性过滤在此）：归属=操控玩家的事件放行；
    /// 无主事件只放行 compile_failed（宁缺勿泄）；其余丢弃不跨拍滞留。
    /// 返回（过滤后帧，游标）。controller=None（无操控/观战）时只发 compile_failed。
    pub(crate) fn take_frame(
        &mut self,
        controller: Option<&str>,
    ) -> (Vec<PlayerEvent>, u64) {
        let pending = std::mem::take(&mut self.pending);
        let frame: Vec<PlayerEvent> = pending
            .into_iter()
            .filter(|ev| match (&ev.owner_name, controller) {
                (Some(owner), Some(name)) => owner == name,
                // 无主事件：只放行公共新闻（compile_failed / player_joined /
                // create_player_rejected——D-021b 进场仪式回执走同一条公共信道；
                // player_selected / select_player_rejected——D-021d 登录/选角页
                // 回执同族）
                (None, _) => matches!(
                    ev.body,
                    EventBody::CompileFailed { .. }
                        | EventBody::PlayerJoined { .. }
                        | EventBody::CreatePlayerRejected { .. }
                        | EventBody::PlayerSelected { .. }
                        | EventBody::SelectPlayerRejected { .. }
                ),
                // 有主事件但无操控连接：不发（宁缺勿泄）
                (Some(_), None) => false,
            })
            .collect();
        (frame, self.cursor())
    }
}

/// 操控玩家名解析（push 路径用）：controlled 实体的 code_hash 若是在线玩家名
/// 即操控玩家名；否则 None（该帧只发无主 compile_failed）
pub(crate) fn controller_name(
    world: &WorldState,
    players: &PlayerDirectory,
    controlled: Option<EntityId>,
) -> Option<String> {
    let id = controlled?;
    let e = world.entities.get(&id)?;
    players.is_online(&e.code_hash).then(|| e.code_hash.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用：直推一条指定归属的事件（绕过 world/players 归属解析——
    /// 归属解析的真值由 main.rs e2e 覆盖，本模块只测汇与过滤的机械性质）
    #[cfg(test)]
    fn push_with_owner(sink: &mut EventSink, owner: Option<&str>, body: EventBody) {
        sink.pending.push(PlayerEvent {
            seq: sink.next_seq,
            tick: 1,
            entity_id: "e".to_string(),
            owner_name: owner.map(|s| s.to_string()),
            body,
        });
        sink.next_seq += 1;
    }

    #[test]
    fn t20_seq_monotonic_and_cursor() {
        // seq 世界单调递增；cursor=已发最大 seq；take后排干（事件不跨拍滞留）
        let mut sink = EventSink::new();
        assert_eq!(sink.cursor(), 0, "从未发过=0");
        push_with_owner(&mut sink, Some("alice"), EventBody::PlayerLog { marker: 1 });
        push_with_owner(&mut sink, Some("alice"), EventBody::PlayerLog { marker: 2 });
        push_with_owner(&mut sink, Some("alice"), EventBody::PlayerLog { marker: 3 });
        assert_eq!(sink.cursor(), 3, "三事件后游标=3");
        let (frame, cursor) = sink.take_frame(Some("alice"));
        assert_eq!(cursor, 3);
        let seqs: Vec<u64> = frame.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3], "seq 单调递增 1,2,3");
        assert_eq!(sink.cursor(), 3, "排干后游标不回退（单调）");
        let (frame2, _) = sink.take_frame(Some("alice"));
        assert!(frame2.is_empty(), "事件是新闻不是档案：排干后本拍无滞留");
    }

    #[test]
    fn t20_visibility_filter_owner_events() {
        // 可见性铁律：只发归属=操控玩家的事件；别人的事件不进我的帧（过滤即
        // 丢弃——快照是单帧广播，一拍只出一帧过滤结果）；无操控连接（None）
        // 时有主事件一律不发（宁缺勿泄）
        let mut sink = EventSink::new();
        push_with_owner(&mut sink, Some("alice"), EventBody::Born { x: 1.0, y: 0.0, glyph: "T".into(), lifespan_ticks: 10, name: "造物#1".into() });
        push_with_owner(&mut sink, Some("bob"), EventBody::Born { x: 2.0, y: 0.0, glyph: "T".into(), lifespan_ticks: 10, name: "造物#2".into() });
        let (alice_frame, _) = sink.take_frame(Some("alice"));
        assert_eq!(alice_frame.len(), 1, "alice 只见自己的 1 条（bob 的被过滤）");
        assert_eq!(alice_frame[0].owner_name.as_deref(), Some("alice"));
        // 单排干语义：bob 的事件已随 alice 帧过滤丢弃（快照一拍一帧，无二次投递）
        let (empty, _) = sink.take_frame(Some("bob"));
        assert!(empty.is_empty(), "排干后无滞留（bob 视角切换场景由下一拍事件补）");
        // 换新汇：bob 作为自己世界的操控者，看得到自己的事件
        let mut sink_bob = EventSink::new();
        push_with_owner(&mut sink_bob, Some("bob"), EventBody::Born { x: 2.0, y: 0.0, glyph: "T".into(), lifespan_ticks: 10, name: "造物#2".into() });
        let (bob_frame, _) = sink_bob.take_frame(Some("bob"));
        assert_eq!(bob_frame.len(), 1, "bob 操控视角见自己的 1 条");
        // 无操控：有主事件一律不发
        push_with_owner(&mut sink, Some("alice"), EventBody::PlayerLog { marker: 9 });
        let (spectator, _) = sink.take_frame(None);
        assert!(spectator.is_empty(), "无操控连接时有主事件不发");
    }

    #[test]
    fn t20_unowned_only_compile_failed_passes() {
        // 无主事件白名单=compile_failed（玩家进场失败的公共新闻）；无主
        // PlayerLog 等不放行（宁缺勿泄）；有操控时 compile_failed 照样可见
        let mut sink = EventSink::new();
        push_with_owner(&mut sink, None, EventBody::PlayerLog { marker: 1 });
        push_with_owner(
            &mut sink,
            None,
            EventBody::CompileFailed { name: "badwat".into(), error: "syntax".into() },
        );
        let (none_frame, _) = sink.take_frame(None);
        assert_eq!(none_frame.len(), 1, "无操控：只放行 compile_failed");
        assert!(matches!(none_frame[0].body, EventBody::CompileFailed { .. }));
        push_with_owner(
            &mut sink,
            None,
            EventBody::CompileFailed { name: "badwat".into(), error: "syntax".into() },
        );
        let (ctrl_frame, _) = sink.take_frame(Some("alice"));
        assert_eq!(ctrl_frame.len(), 1, "有操控：compile_failed（无主）照样可见");
    }

    #[test]
    fn t21d_unowned_select_events_are_public_news() {
        // D-021d：player_selected / select_player_rejected 是无主公共新闻
        //（登录/选角页的回执信道，与 player_joined/create_player_rejected 同族）
        // ——无操控连接也放行（宁缺勿泄只对"有主"事件生效）
        let mut sink = EventSink::new();
        push_with_owner(&mut sink, None, EventBody::PlayerSelected { name: "tudou".into() });
        push_with_owner(&mut sink, None, EventBody::SelectPlayerRejected { reason: "NAME_UNKNOWN" });
        let (frame, _) = sink.take_frame(None);
        assert_eq!(frame.len(), 2, "无操控：两个选角事件都放行");
        assert!(matches!(frame[0].body, EventBody::PlayerSelected { .. }));
        assert!(matches!(frame[1].body, EventBody::SelectPlayerRejected { .. }));
        // 协议形状锚：kind 标签进 JSON（前端按 kind 分流回执）
        let json = serde_json::to_string(&frame[0]).unwrap();
        assert!(json.contains("\"kind\":\"player_selected\""), "{json}");
        assert!(json.contains("\"name\":\"tudou\""), "{json}");
        let json = serde_json::to_string(&frame[1]).unwrap();
        assert!(json.contains("\"kind\":\"select_player_rejected\""), "{json}");
        assert!(json.contains("\"reason\":\"NAME_UNKNOWN\""), "{json}");
    }

    #[test]
    fn t20_snippet_caps_at_200_chars() {
        // 摘要按字符截断（UTF-8 安全），超 200 截 200，不足原样
        let long = "x".repeat(300);
        assert_eq!(snippet(&long).chars().count(), ERROR_SNIPPET_MAX);
        let cjk = "剑".repeat(300);
        assert_eq!(snippet(&cjk).chars().count(), ERROR_SNIPPET_MAX);
        assert_eq!(snippet("short"), "short");
    }

    #[test]
    fn t20_wire_shape_kind_tag_and_skip_owner() {
        // 协议形状锚：kind=serde tag 进 JSON；owner_name skip 不泄漏；
        // code_hint=None 缺席（省字段）。spawn_rejected 是有主事件
        // （主体=创造者），用 push_with_owner 走正常归属路径
        let mut sink = EventSink::new();
        push_with_owner(
            &mut sink,
            Some("alice"),
            EventBody::SpawnRejected {
                reason: reason::BIRTHPLACE_OCCUPIED,
                x: 40.0,
                y: 0.0,
                code_hint: None,
            },
        );
        let (frame, _) = sink.take_frame(Some("alice"));
        let json = serde_json::to_string(&frame[0]).unwrap();
        assert!(json.contains("\"kind\":\"spawn_rejected\""), "kind 标签进协议：{json}");
        assert!(json.contains("\"reason\":\"BIRTHPLACE_OCCUPIED\""));
        assert!(!json.contains("owner_name"), "归属字段不得进协议（服务端内部）：{json}");
        assert!(!json.contains("code_hint"), "code_hint=None 应缺席省字段：{json}");
        assert!(json.contains("\"seq\":1") && json.contains("\"tick\":1") && json.contains("\"entity_id\":\"e\""));
    }
}
