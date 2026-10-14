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

/// 操控意图（#022：输入 = 意图流）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Intent {
    /// 移动（方向键/WASD -> 四方向；步长 1.0 格，方向由 dx/dy 携带）
    Move { dx: f64, dy: f64 },
    /// 攻击（空格：对最近可见敌人坐标出刀）
    Attack,
    /// 退出（q/Enter/Ctrl+C）
    Quit,
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

/// 意图队列生产端（输入线程持有）
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

/// 意图路由（#022 双层模型核心；独立函数——期四 WebSocket 意图流复用同一入口，只换来源不换路由）
///
/// - control_target = players/ 目录进场的第一个玩家实体；None = 无玩家在线（按键仅
///   Quit 生效，路由返回 None，代码自主照常）
/// - 同拍多键按到达顺序折叠，后到覆盖先到（最后一键说了算）；Quit 不路由
///   （退出是主循环语义，输入线程已消费）
/// - `nearest_enemy`：操控目标的最近可见敌人坐标（调用方用 main.rs find_nearest_enemy
///   预计算，与 scan 共用同一 viewport 口径；input.rs 不感知世界，保持纯路由）
pub fn route_intents(
    intents: &[Intent],
    control_target: Option<EntityId>,
    nearest_enemy: Option<(f64, f64)>,
) -> Option<(EntityId, ControlledAction)> {
    let target = control_target?;
    let mut action: Option<ControlledAction> = None;
    for intent in intents {
        match intent {
            Intent::Move { dx, dy } => action = Some(ControlledAction::Move(*dx, *dy)),
            Intent::Attack => action = Some(ControlledAction::Attack(nearest_enemy)),
            Intent::Quit => {}
        }
    }
    action.map(|a| (target, a))
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

    // === route_intents：意图路由（任务 B，#022 双层模型核心）===

    #[test]
    fn no_player_online_routes_to_none() {
        // 无玩家进场 = 按键仅 Quit 生效（Quit 不进路由），路由无目标
        let intents = vec![Intent::Move { dx: 1.0, dy: 0.0 }, Intent::Attack];
        assert_eq!(route_intents(&intents, None, None), None);
    }

    #[test]
    fn move_intent_routes_to_control_target() {
        let target = eid(1);
        let intents = vec![Intent::Move { dx: -1.0, dy: 0.0 }];
        assert_eq!(
            route_intents(&intents, Some(target), None),
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
            route_intents(&intents, Some(target), Some(enemy_pos)),
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
            route_intents(&intents, Some(target), None),
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
            route_intents(&intents, Some(target), Some((5.0, 0.0))),
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
            route_intents(&intents, Some(target), Some((5.0, 0.0))),
            Some((target, ControlledAction::Move(-1.0, 0.0))),
            "先攻击后移动 -> 移动生效"
        );
    }

    #[test]
    fn quit_intent_is_not_routed() {
        // Quit 是主循环语义，不是角色行动：路由跳过
        let target = eid(1);
        let intents = vec![Intent::Quit];
        assert_eq!(route_intents(&intents, Some(target), None), None);
    }

    #[test]
    fn empty_intents_route_to_none() {
        // 无人按键 = 无操控行动（代码自主照常，#022 离线回落语义）
        let target = eid(1);
        assert_eq!(route_intents(&[], Some(target), Some((5.0, 0.0))), None);
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
