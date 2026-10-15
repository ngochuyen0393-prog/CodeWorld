//! 操控层 v0：终端键盘 -> 意图流 -> 意图路由（decisions.md #022 双层玩家模型）
//!
//! - 输入 = 意图流（#022：非坐标直写，防瞬移，物理照常裁决）
//! - 操控通道 = #014 外部通道首个应用（阵法接算力/NPC接大模型/玩家接键盘三位一体同构）；
//!   本期来源 = 终端键盘（crossterm raw mode），期四 WebSocket 复用同一路由只换来源
//! - 借鉴 roguelike 事实标准（crossterm 生态）：raw mode 裸键输入 + 独立输入线程
//!   + 意图队列（tick 每拍 drain）；按键 = 排队本拍意图，回合制传统
//! - 操控不豁免物理（宪法）：路由产物只进既有 pending/攻击队列，占位/两阶段结算照常

use codeworld_core::entity::EntityId;
use codeworld_render::{enter_terminal, exit_terminal};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, Command};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// 操控意图（#022：输入 = 意图流；#027 期四第三刀：终端键盘与 WS 鼠标双来源共享单入口）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Intent {
    /// 移动（方向键/WASD -> 四方向；步长 1.0 格，方向由 dx/dy 携带）
    Move { dx: f64, dy: f64 },
    /// 攻击（空格：对最近可见敌人坐标出刀）
    Attack,
    /// 退出（q/Enter/Ctrl+C）
    Quit,
    /// 点击走位（#027 WS 鼠标：点击世界坐标 = 持续目标，逐拍向其走一步，到达或 Stop 清除）
    WsMove { x: f64, y: f64 },
    /// 点击出刀（#027 WS 鼠标：点击敌人坐标 = 单次攻击，点一下砍一刀，不自动连击）
    WsAttack { x: f64, y: f64 },
    /// 停止（#027 右键/浏览器 S 键：清除点击走位目标，站住不动）
    Stop,
}

/// 按键 -> 意图映射全表（纯函数；crossterm KeyEvent 可在测试中直接构造）
///
/// - 方向键 + WASD 双支持（Vi 传统）；空格=攻击；q/Enter/Ctrl+C=退出
/// - 只认 Press/Repeat：Windows 松键产生 Release（丢弃）；按住键产生 Repeat
///   （保留，"按住 A 持续西行"靠它）；未知按键返回 None（忽略不产生意图）
pub fn key_to_intent(key: KeyEvent) -> Option<Intent> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    match key.code {
        KeyCode::Char('w') | KeyCode::Up => Some(Intent::Move { dx: 0.0, dy: 1.0 }),
        KeyCode::Char('s') | KeyCode::Down => Some(Intent::Move { dx: 0.0, dy: -1.0 }),
        KeyCode::Char('a') | KeyCode::Left => Some(Intent::Move { dx: -1.0, dy: 0.0 }),
        KeyCode::Char('d') | KeyCode::Right => Some(Intent::Move { dx: 1.0, dy: 0.0 }),
        KeyCode::Char(' ') => Some(Intent::Attack),
        KeyCode::Char('q') | KeyCode::Enter => Some(Intent::Quit),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Intent::Quit),
        _ => None,
    }
}

/// 意图队列消费端（tick 主线程持有；mpsc 通道线程安全）
pub struct IntentQueue {
    rx: Receiver<Intent>,
}

/// 意图队列生产端（输入线程持有；Clone 供 WS 线程共享同一队列——单入口双来源，#027）
#[derive(Clone)]
pub struct IntentSink {
    tx: Sender<Intent>,
}

/// 建立意图通道（输入线程生产，tick 主线程消费）
pub fn intent_channel() -> (IntentSink, IntentQueue) {
    let (tx, rx) = mpsc::channel();
    (IntentSink { tx }, IntentQueue { rx })
}

impl IntentSink {
    /// 入队一条意图（接收端已关闭时静默丢弃——输入是尽力而为的信号流）
    pub fn send(&self, intent: Intent) {
        let _ = self.tx.send(intent);
    }
}

impl IntentQueue {
    /// 取走当前积压的全部意图（tick 每拍开头 drain；非阻塞，不跨拍残留）
    pub fn drain(&self) -> Vec<Intent> {
        let mut out = Vec::new();
        while let Ok(intent) = self.rx.try_recv() {
            out.push(intent);
        }
        out
    }
}

/// 本拍操控行动（route_intents 产物；tick 据此接管操控目标）
#[derive(Debug, Clone, PartialEq)]
pub enum ControlledAction {
    /// 操控移动：相对操控目标当前坐标的方向向量（步长 MVP 恒 1.0 格）
    Move(f64, f64),
    /// 操控攻击：目标坐标（最近可见敌人，与 scan 同源口径）；
    /// None = 视野内无目标（本拍挥空不出刀，但操控仍接管本拍）
    Attack(Option<(f64, f64)>),
}

/// 意图路由（#022 双层模型核心；#027 期四第三刀：终端键盘与 WS 鼠标双来源汇入同一折叠）
///
/// - control_target = me 声明且在线的操控目标实体；None = 观战（无目标可路由）
/// - 同拍多意图按到达顺序折叠，后到覆盖先到（最后一键说了算）；Quit 不路由
///   （退出是主循环语义，输入线程已消费）
/// - `entity_pos`：操控目标当前坐标（算走位步向用）
/// - `move_target`：点击走位持续目标（in-out 状态，存 EntitySandboxMap 跨拍）；
///   键盘 Move 直接接管脚下并清除它；Stop 清除站住；目标换实体不交接
/// - `nearest_enemy`：操控目标的最近可见敌人坐标（调用方用 main.rs find_nearest_enemy
///   预计算，与 scan 共用同一 viewport 口径；input.rs 不感知世界，保持纯路由）
pub fn route_intents(
    intents: &[Intent],
    control_target: Option<EntityId>,
    nearest_enemy: Option<(f64, f64)>,
    entity_pos: Option<(f64, f64)>,
    move_target: &mut Option<(EntityId, (f64, f64))>,
) -> Option<(EntityId, ControlledAction)> {
    let target = control_target?;
    // 点击走位目标不跨角色交接：操控目标换了（换声明/重进场）就清掉旧目标
    if let Some((tid, _)) = *move_target {
        if tid != target {
            tracing::debug!("[input] 操控目标变更，清除旧点击走位目标");
            *move_target = None;
        }
    }
    let mut action: Option<ControlledAction> = None;
    for intent in intents {
        match intent {
            Intent::Move { dx, dy } => {
                // 键盘直控 = 玩家接管脚下：清除点击走位目标（后到优先，同拍互斥）
                *move_target = None;
                action = Some(ControlledAction::Move(*dx, *dy));
            }
            Intent::Attack => action = Some(ControlledAction::Attack(nearest_enemy)),
            Intent::WsMove { x, y } => {
                // 后到的点击走路覆盖本拍已有行动（后到优先）：行动交给持续目标步进
                action = None;
                *move_target = Some((target, (*x, *y)));
            }
            Intent::WsAttack { x, y } => action = Some(ControlledAction::Attack(Some((*x, *y)))),
            Intent::Stop => {
                *move_target = None;
                action = None;
            }
            Intent::Quit => {}
        }
    }
    // 持续目标执行：本拍无其他行动且点击目标在册 -> 向目标走一步；到达（格距<0.5）清除站住
    if action.is_none() {
        if let Some((_tid, tpos)) = *move_target {
            match next_move_step(tpos, entity_pos) {
                Some(step) => action = Some(step),
                None => *move_target = None,
            }
        }
    }
    action.map(|a| (target, a))
}

/// 点击走位的一步分解（net-protocol.md v1.2 写死语义）：先横后竖各整步；
/// 到达判定 = 与目标欧氏格距平方 <= 0.25（即格距 <= 0.5；含 0.5 防恰好半格外
/// 永远差半步的 ±0.5 振荡，浮点不用精确相等）。
/// 坐标有限性由解析层（net.rs）保证，本函数信任来源。
/// 注意：不能用 f64::signum——signum(+0.0) 返回 1.0 而非 0.0，横轴到位会被
/// 误判成"还差一横步"永远走不出锯齿。
fn next_move_step(tpos: (f64, f64), pos: Option<(f64, f64)>) -> Option<ControlledAction> {
    let (px, py) = pos?;
    let diff = (tpos.0 - px, tpos.1 - py);
    if diff.0 * diff.0 + diff.1 * diff.1 <= 0.25 {
        return None; // 已到达：格距 <= 0.5，清除目标站住
    }
    let sign = |v: f64| if v > 0.0 { 1.0 } else if v < 0.0 { -1.0 } else { 0.0 };
    if diff.0 != 0.0 {
        Some(ControlledAction::Move(sign(diff.0), 0.0)) // 先横：横轴还有差距
    } else {
        Some(ControlledAction::Move(0.0, sign(diff.1))) // 横已到位 -> 竖
    }
}

/// 独立输入线程：crossterm 事件轮询 -> 意图入队（生产者）
///
/// - event::poll(50ms) 非阻塞探测，线程不占主循环、可被 running 标志正常叫停
/// - 按键事件经 key_to_intent 转意图入队；Quit（q/Enter/Ctrl+C）直接置退出标志
///   并收工（退出是主循环语义，不进路由）
/// - poll/read 出错（EOF/终端消失）同样置退出标志（旧 stdin read_line 线程的
///   EOF 退出路由此替代）
pub fn spawn_input_thread(
    sink: IntentSink,
    running: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        // 50ms 轮询：输入线程不占主循环，退出响应 ≤50ms
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(k)) => match key_to_intent(k) {
                    Some(Intent::Quit) => {
                        tracing::info!("[input] 收到退出键（q/Enter/Ctrl+C），置退出标志");
                        running.store(false, Ordering::SeqCst);
                        break;
                    }
                    Some(intent) => sink.send(intent),
                    None => {}
                },
                Ok(_) => {} // 鼠标/窗口缩放等事件忽略（MVP 只认按键）
                Err(e) => {
                    tracing::info!(error = %e, "[input] 输入流结束（EOF），置退出标志");
                    running.store(false, Ordering::SeqCst);
                    break;
                }
            },
            Ok(false) => {} // 50ms 无按键，回 loop 查 running 标志
            Err(e) => {
                tracing::info!(error = %e, "[input] 事件轮询失败，置退出标志");
                running.store(false, Ordering::SeqCst);
                break;
            }
        }
    })
}

/// 进入终端操控模式（main 启动时调用一次）
///
/// 顺序：raw mode（裸键即达不回显）-> 备用屏（画面画在备用屏，退出还原用户
/// shell 现场）-> render 既有 enter_terminal（清屏+隐光标）。任一步失败返回
/// Err，调用方降级为普通输出运行（键控不可用但不崩）。
pub fn setup_terminal() -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    enter_terminal(&mut std::io::stdout().lock())
}

/// 恢复终端（正常退出/panic/EOF 三条路共用，尽力而为全静默——恢复失败不掩盖
/// 原始错误/panic 信息）。顺序：render 既有 exit_terminal（重置颜色+显光标）
/// -> 退备用屏（还原用户 shell 现场）-> 关 raw mode（否则崩溃后终端报废）。
pub fn restore_terminal() {
    restore_terminal_to(&mut std::io::stdout().lock());
    let _ = disable_raw_mode();
}

/// 恢复序列的输出部分（可注入 writer 供测试断言转义序列）
fn restore_terminal_to<W: Write>(out: &mut W) {
    let _ = exit_terminal(out); // \x1b[0m 重置颜色 + \x1b[?25h 显光标（render 既有）
    // LeaveAlternateScreen 的 ANSI 等价转义（write_ansi 与 execute! 输出同字节；
    // crossterm 的 write_ansi 收 fmt::Write，经 String 中转落到 io writer）
    let mut ansi = String::new();
    let _ = LeaveAlternateScreen.write_ansi(&mut ansi);
    let _ = out.write_all(ansi.as_bytes());
}

/// 安装 panic hook：panic 时先恢复终端（raw mode + 备用屏 + 颜色/光标），
/// 再交回默认 hook 打印 panic 信息（否则崩溃后用户终端砖化）
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    fn release(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn eid(n: u128) -> EntityId {
        EntityId(Uuid::from_u128(n))
    }

    // === key_to_intent：键位映射全表（任务 A.3）===

    #[test]
    fn wasd_maps_to_four_direction_moves() {
        assert_eq!(
            key_to_intent(key(KeyCode::Char('w'))),
            Some(Intent::Move { dx: 0.0, dy: 1.0 }),
            "w = 北"
        );
        assert_eq!(
            key_to_intent(key(KeyCode::Char('s'))),
            Some(Intent::Move { dx: 0.0, dy: -1.0 }),
            "s = 南"
        );
        assert_eq!(
            key_to_intent(key(KeyCode::Char('a'))),
            Some(Intent::Move { dx: -1.0, dy: 0.0 }),
            "a = 西"
        );
        assert_eq!(
            key_to_intent(key(KeyCode::Char('d'))),
            Some(Intent::Move { dx: 1.0, dy: 0.0 }),
            "d = 东"
        );
    }

    #[test]
    fn arrow_keys_map_to_four_direction_moves() {
        // Vi 传统双支持：方向键与 WASD 同义
        assert_eq!(
            key_to_intent(key(KeyCode::Up)),
            Some(Intent::Move { dx: 0.0, dy: 1.0 })
        );
        assert_eq!(
            key_to_intent(key(KeyCode::Down)),
            Some(Intent::Move { dx: 0.0, dy: -1.0 })
        );
        assert_eq!(
            key_to_intent(key(KeyCode::Left)),
            Some(Intent::Move { dx: -1.0, dy: 0.0 })
        );
        assert_eq!(
            key_to_intent(key(KeyCode::Right)),
            Some(Intent::Move { dx: 1.0, dy: 0.0 })
        );
    }

    #[test]
    fn space_maps_to_attack() {
        assert_eq!(key_to_intent(key(KeyCode::Char(' '))), Some(Intent::Attack));
    }

    #[test]
    fn q_and_enter_and_ctrl_c_map_to_quit() {
        assert_eq!(key_to_intent(key(KeyCode::Char('q'))), Some(Intent::Quit));
        assert_eq!(key_to_intent(key(KeyCode::Enter)), Some(Intent::Quit));
        assert_eq!(key_to_intent(ctrl_c()), Some(Intent::Quit), "Ctrl+C 退出");
    }

    #[test]
    fn plain_c_is_not_quit() {
        // 边界：不带 CONTROL 的 'c' 不是退出（防误按）
        assert_ne!(key_to_intent(key(KeyCode::Char('c'))), Some(Intent::Quit));
        assert_eq!(key_to_intent(key(KeyCode::Char('c'))), None);
    }

    #[test]
    fn unknown_keys_yield_none() {
        assert_eq!(key_to_intent(key(KeyCode::Char('x'))), None, "无关字母忽略");
        assert_eq!(key_to_intent(key(KeyCode::F(1))), None, "功能键忽略");
        assert_eq!(key_to_intent(key(KeyCode::Esc)), None, "Esc 不绑定（防误退）");
    }

    #[test]
    fn release_events_yield_none() {
        // Windows 松键产生 Release 事件：丢弃（Press/Repeat 才算按键）
        assert_eq!(key_to_intent(release(KeyCode::Char('a'))), None);
    }

    // === IntentQueue：意图队列（输入线程生产，tick 每拍 drain）===

    #[test]
    fn drain_returns_all_pending_intents_in_order_and_empties() {
        let (sink, queue) = intent_channel();
        sink.send(Intent::Move { dx: 1.0, dy: 0.0 });
        sink.send(Intent::Attack);
        assert_eq!(
            queue.drain(),
            vec![
                Intent::Move { dx: 1.0, dy: 0.0 },
                Intent::Attack
            ],
            "按到达顺序取走全部积压"
        );
        assert!(queue.drain().is_empty(), "drain 后队列清空，不跨拍残留");
    }

    #[test]
    fn drain_on_empty_queue_returns_empty() {
        let (_sink, queue) = intent_channel();
        assert!(queue.drain().is_empty(), "空队列 drain 返回空");
    }

    // === route_intents：意图路由（#022 双层模型核心；#027 双来源折叠）===

    /// 路由测试便捷包装（旧签名迁移垫片：实体@pos、无点击目标起步）
    fn route(
        intents: &[Intent],
        target: Option<EntityId>,
        nearest: Option<(f64, f64)>,
        pos: Option<(f64, f64)>,
    ) -> Option<(EntityId, ControlledAction)> {
        let mut mt: Option<(EntityId, (f64, f64))> = None;
        route_intents(intents, target, nearest, pos, &mut mt)
    }

    #[test]
    fn no_player_online_routes_to_none() {
        // 无玩家进场 = 按键仅 Quit 生效（Quit 不进路由），路由无目标
        let intents = vec![Intent::Move { dx: 1.0, dy: 0.0 }, Intent::Attack];
        assert_eq!(route(&intents, None, None, None), None);
    }

    #[test]
    fn move_intent_routes_to_control_target() {
        let target = eid(1);
        let intents = vec![Intent::Move { dx: -1.0, dy: 0.0 }];
        assert_eq!(
            route(&intents, Some(target), None, Some((30.0, 0.0))),
            Some((target, ControlledAction::Move(-1.0, 0.0))),
            "Move 意图翻译为对操控目标的操控行动"
        );
    }

    #[test]
    fn attack_intent_uses_nearest_visible_enemy() {
        let target = eid(1);
        let enemy_pos = (12.0, 0.0);
        let intents = vec![Intent::Attack];
        assert_eq!(
            route(&intents, Some(target), Some(enemy_pos), Some((30.0, 0.0))),
            Some((target, ControlledAction::Attack(Some(enemy_pos)))),
            "Attack 意图锁定调用方给的最近可见敌人（与 scan 同源）"
        );
    }

    #[test]
    fn attack_intent_without_enemy_swings_at_nothing() {
        // 视野内无目标：挥空（不出刀）但操控仍接管本拍
        let target = eid(1);
        let intents = vec![Intent::Attack];
        assert_eq!(
            route(&intents, Some(target), None, Some((30.0, 0.0))),
            Some((target, ControlledAction::Attack(None))),
            "Attack(None) = 挥空"
        );
    }

    #[test]
    fn last_intent_wins_move_then_attack() {
        // 同拍多键：按到达顺序折叠，后到覆盖先到（最后一键说了算）
        let target = eid(1);
        let intents = vec![
            Intent::Move { dx: 1.0, dy: 0.0 },
            Intent::Attack,
        ];
        assert_eq!(
            route(&intents, Some(target), Some((5.0, 0.0)), Some((30.0, 0.0))),
            Some((target, ControlledAction::Attack(Some((5.0, 0.0))))),
            "先移动后攻击 -> 攻击生效"
        );
    }

    #[test]
    fn last_intent_wins_attack_then_move() {
        let target = eid(1);
        let intents = vec![
            Intent::Attack,
            Intent::Move { dx: -1.0, dy: 0.0 },
        ];
        assert_eq!(
            route(&intents, Some(target), Some((5.0, 0.0)), Some((30.0, 0.0))),
            Some((target, ControlledAction::Move(-1.0, 0.0))),
            "先攻击后移动 -> 移动生效"
        );
    }

    #[test]
    fn quit_intent_is_not_routed() {
        // Quit 是主循环语义，不是角色行动：路由跳过
        let target = eid(1);
        let intents = vec![Intent::Quit];
        assert_eq!(route(&intents, Some(target), None, None), None);
    }

    #[test]
    fn empty_intents_route_to_none() {
        // 无人按键且无点击目标 = 无操控行动
        let target = eid(1);
        assert_eq!(route(&[], Some(target), Some((5.0, 0.0)), Some((30.0, 0.0))), None);
    }

    // === route_intents：WS 鼠标意图（#027 点击走位/点击出刀/停止）===

    #[test]
    fn ws_move_sets_persistent_target_and_steps_toward_it() {
        // 点击世界坐标 = 持续目标：本拍立即向目标走一步（先横后竖），目标记入状态
        let target = eid(1);
        let intents = vec![Intent::WsMove { x: 35.0, y: 0.0 }];
        let mut mt: Option<(EntityId, (f64, f64))> = None;
        let action = route_intents(&intents, Some(target), None, Some((30.0, 0.0)), &mut mt);
        assert_eq!(
            action,
            Some((target, ControlledAction::Move(1.0, 0.0))),
            "点击 (35,0) 本拍向东走一步"
        );
        assert_eq!(mt, Some((target, (35.0, 0.0))), "持续目标入册（归属操控实体）");
    }

    #[test]
    fn persistent_target_steps_every_tick_without_intents() {
        // 持续目标的核心语义：无按键拍继续向目标走（逐拍一步），不用客户端重发
        let target = eid(1);
        let mut mt = Some((target, (35.0, 0.0)));
        let action = route_intents(&[], Some(target), None, Some((32.0, 0.0)), &mut mt);
        assert_eq!(
            action,
            Some((target, ControlledAction::Move(1.0, 0.0))),
            "无意图拍也向点击目标走一步"
        );
        assert_eq!(mt, Some((target, (35.0, 0.0))), "未到达目标保持");
    }

    #[test]
    fn persistent_target_cleared_on_arrival() {
        // 到达判定 = 格距 < 0.5（浮点不用精确相等）：走到目标邻域即清除站住
        let target = eid(1);
        let mut mt = Some((target, (35.0, 0.0)));
        let action = route_intents(&[], Some(target), None, Some((34.7, 0.0)), &mut mt);
        assert_eq!(action, None, "格距 0.3 < 0.5 = 已到达，无行动");
        assert_eq!(mt, None, "到达后目标清除（下拍站住不抖动）");
    }

    #[test]
    fn keyboard_move_clears_click_target() {
        // 键盘直控 = 玩家接管脚下：清除点击走位目标（松键后不会"复活"旧点击走路）
        let target = eid(1);
        let intents = vec![Intent::Move { dx: 0.0, dy: -1.0 }];
        let mut mt = Some((target, (35.0, 0.0)));
        let action = route_intents(&intents, Some(target), None, Some((32.0, 0.0)), &mut mt);
        assert_eq!(action, Some((target, ControlledAction::Move(0.0, -1.0))));
        assert_eq!(mt, None, "键盘移动清除点击目标");
    }

    #[test]
    fn stop_clears_target_and_stands() {
        // Stop（右键/浏览器 S 键）= 清除点击目标站住；同拍已在走也不走
        let target = eid(1);
        let intents = vec![Intent::Stop];
        let mut mt = Some((target, (35.0, 0.0)));
        let action = route_intents(&intents, Some(target), None, Some((32.0, 0.0)), &mut mt);
        assert_eq!(action, None, "Stop 拍站住");
        assert_eq!(mt, None, "Stop 清除持续目标");
    }

    #[test]
    fn ws_attack_uses_clicked_coords_not_nearest() {
        // 点击出刀 = 对点击坐标出刀（单次），不吃 nearest_enemy（与键盘空格同通路不同目标源）
        let target = eid(1);
        let intents = vec![Intent::WsAttack { x: 15.0, y: 0.0 }];
        let action = route(
            &intents,
            Some(target),
            Some((5.0, 0.0)),
            Some((30.0, 0.0)),
        );
        assert_eq!(
            action,
            Some((target, ControlledAction::Attack(Some((15.0, 0.0))))),
            "点击坐标优先于最近敌人坐标"
        );
    }

    #[test]
    fn last_intent_wins_ws_attack_then_ws_move_same_batch() {
        // 同拍互斥后到优先对 WS 意图同样成立：先点敌人后点地 -> 本拍走路且目标更新
        let target = eid(1);
        let intents = vec![
            Intent::WsAttack { x: 15.0, y: 0.0 },
            Intent::WsMove { x: 35.0, y: 0.0 },
        ];
        let mut mt = Some((target, (40.0, 0.0)));
        let action = route_intents(&intents, Some(target), None, Some((30.0, 0.0)), &mut mt);
        assert_eq!(
            action,
            Some((target, ControlledAction::Move(1.0, 0.0))),
            "后到的 WsMove 生效（本拍向新目标走一步）"
        );
        assert_eq!(mt, Some((target, (35.0, 0.0))), "目标被后到点击覆盖");
    }

    #[test]
    fn move_target_not_handed_over_on_control_change() {
        // 点击目标不跨角色交接：目标属于 eid1，操控换 eid2 -> 旧目标清除，新角色不受旧点击支配
        let old = eid(1);
        let new = eid(2);
        let intents: Vec<Intent> = vec![];
        let mut mt = Some((old, (35.0, 0.0)));
        let action = route_intents(&intents, Some(new), None, Some((30.0, 0.0)), &mut mt);
        assert_eq!(action, None, "换角后旧点击目标不生效");
        assert_eq!(mt, None, "旧目标清除");
    }

    #[test]
    fn diagonal_target_decomposes_horizontal_first() {
        // 斜向目标分解规则（net-protocol.md v1.2 写死）：先横后竖，每拍一格
        let target = eid(1);
        let mut mt = Some((target, (32.0, 3.0)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 0.0)), &mut mt);
        assert_eq!(
            action,
            Some((target, ControlledAction::Move(1.0, 0.0))),
            "横竖都差 -> 先横"
        );
        // 横到位后竖走
        let action2 = route_intents(&[], Some(target), None, Some((32.0, 0.0)), &mut mt);
        assert_eq!(
            action2,
            Some((target, ControlledAction::Move(0.0, 1.0))),
            "横已到位 -> 竖"
        );
    }

    #[test]
    fn target_behind_steps_negative_direction() {
        // 目标在起点西/北方向：分解出负向步（signum 语义，不是恒正向）
        let target = eid(1);
        let mut mt = Some((target, (28.0, 0.0)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 0.0)), &mut mt);
        assert_eq!(action, Some((target, ControlledAction::Move(-1.0, 0.0))));
    }

    // === 输入线程 / 终端模式（任务 A：raw mode + alternate screen + 全路径恢复）===

    #[test]
    fn input_thread_terminates_on_running_flag() {
        // 线程随 running 标志退出（正常退出路）：置 false 后 join 应返回。
        // poll 间隔 50ms，线程若不理会标志此测试会卡死。
        let (sink, _queue) = intent_channel();
        let running = Arc::new(AtomicBool::new(true));
        let handle = spawn_input_thread(sink, running.clone());
        running.store(false, Ordering::SeqCst);
        handle.join().expect("输入线程应正常退出");
    }

    #[test]
    fn restore_terminal_writes_alt_screen_leave_and_reset() {
        // 恢复序列（正常退出/panic/EOF 三条路共用）：重置颜色 + 显光标 + 退备用屏。
        // disable_raw_mode 对未启用的裸模式返回 Err（静默忽略），测试环境安全。
        let mut out = Vec::new();
        restore_terminal_to(&mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\x1b[0m"), "应重置颜色，实际 {:?}", s);
        assert!(s.contains("\x1b[?25h"), "应显示光标，实际 {:?}", s);
        assert!(
            s.contains("\x1b[?1049l"),
            "应退备用屏（LeaveAlternateScreen 等价转义），实际 {:?}",
            s
        );
    }

    #[test]
    fn panic_hook_restores_terminal_and_does_not_swallow_panic() {
        // panic 安全（验收#4）：hook 先恢复终端再交回默认 hook；恢复路径在
        // "raw mode 未启用"的测试环境下必须静默成功（disable_raw_mode Err 忽略），
        // 且 hook 不吞 panic（照常 unwind）。
        install_panic_hook();
        let result = std::panic::catch_unwind(|| panic!("模拟崩溃（测试 hook 恢复路径）"));
        assert!(result.is_err(), "panic 应照常发生（hook 不吞 panic）");
    }
}
