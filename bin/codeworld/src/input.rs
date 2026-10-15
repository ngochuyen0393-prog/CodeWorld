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
    /// 操控移动（#030 第六刀：矢量步 + 悬停独立）：
    /// - dx/dy = 本拍位移。远目标 = 矢量方向归一化 ×1.0（斜走一步到位，L 形废除）；
    ///   剩余 ≤1.0 = 一步精确踏上目标（防容差带振荡）
    /// - aerial=true = 本拍悬停（升力独立于移动被挡：main.rs 据此把实体放进
    ///   物理免重力集合——移动被挡/滑动时升力照出，被挡≠坠落，第五刀 bug 根治）
    Move {
        dx: f64,
        dy: f64,
        aerial: bool,
    },
    /// 操控攻击：目标坐标（最近可见敌人 / 点击坐标，与 scan 同源口径）；
    /// None = 视野内无目标（本拍挥空不出刀，但操控仍接管本拍）
    Attack(Option<(f64, f64)>),
}

/// 操控飞行相位（期四第六刀简化：爬升/平飞合一——矢量方向自然覆盖，保留悬停态）。
/// main.rs 按相位翻转沿打日志（状态切换才打，悬停维持不逐拍刷）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlightPhase {
    /// 飞行：矢量步逼近目标（爬升/平飞/受控降落合一）
    Fly,
    /// 悬停：到位/空中停止后原地维持（升力照出，目标保留到新意图）
    Hover,
}

impl FlightPhase {
    /// 日志中文名（相位沿日志用，GBK 安全性同既有中文 tracing 日志）
    pub fn name(self) -> &'static str {
        match self {
            Self::Fly => "飞行",
            Self::Hover => "悬停",
        }
    }
}

/// 意图路由（#022 双层模型核心；#027 期四第三刀：终端键盘与 WS 鼠标双来源汇入同一折叠）
///
/// - control_target = me 声明且在线的操控目标实体；None = 观战（无目标可路由）
/// - 同拍多意图按到达顺序折叠，后到覆盖先到（最后一键说了算）；Quit 不路由
///   （退出是主循环语义，输入线程已消费）
/// - `entity_pos`：操控目标当前坐标（算走位步向 + 空中判定用）
/// - `radius`：操控目标身体半径（#030：空中判定线 = y > radius，圆心即位置）
/// - `move_target`：点击走位持续目标（in-out 状态，存 EntitySandboxMap 跨拍）；
///   键盘 Move 直接接管脚下并清除它；Stop 清除站住；目标换实体不交接
/// - `nearest_enemy`：操控目标的最近可见敌人坐标（调用方用 main.rs find_nearest_enemy
///   预计算，与 scan 共用同一 viewport 口径；input.rs 不感知世界，保持纯路由）
pub fn route_intents(
    intents: &[Intent],
    control_target: Option<EntityId>,
    nearest_enemy: Option<(f64, f64)>,
    entity_pos: Option<(f64, f64)>,
    radius: f64,
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
    // 空中判定（#030：地面线 = 半径线，圆心即位置）：实体当前悬空 = 本拍意图
    // 自带悬停升力（键盘移动/攻击空中不坠落——升力独立于移动，第五刀 bug 根治）
    let aerial_now = entity_pos.map(|(_, py)| py > radius).unwrap_or(false);
    let mut action: Option<ControlledAction> = None;
    for intent in intents {
        match intent {
            Intent::Move { dx, dy } => {
                // 键盘直控 = 玩家接管脚下：清除点击走位目标（后到优先，同拍互斥）；
                // 空中按键 = 飞行操控（W 爬升 S 降落 A/D 平移，升力照出）
                *move_target = None;
                action = Some(ControlledAction::Move {
                    dx: *dx,
                    dy: *dy,
                    aerial: aerial_now,
                });
            }
            Intent::Attack => action = Some(ControlledAction::Attack(nearest_enemy)),
            Intent::WsMove { x, y } => {
                // 后到的点击走路覆盖本拍已有行动（后到优先）：行动交给持续目标步进
                action = None;
                *move_target = Some((target, (*x, *y)));
            }
            Intent::WsAttack { x, y } => action = Some(ControlledAction::Attack(Some((*x, *y)))),
            Intent::Stop => {
                if aerial_now {
                    // 空中停止 = 悬停不坠（stop 语义 = 停止移动不是坠落）：目标改写
                    // 为脚下坐标转悬停维持；要落地玩家点地面
                    if let Some(feet) = entity_pos {
                        *move_target = Some((target, feet));
                    }
                    action = Some(ControlledAction::Move {
                        dx: 0.0,
                        dy: 0.0,
                        aerial: true,
                    });
                } else {
                    *move_target = None;
                    action = None;
                }
            }
            Intent::Quit => {}
        }
    }
    // 持续目标执行：本拍无其他行动且点击目标在册 -> 向目标走一步；到达（格距≤0.5）清除站住
    if action.is_none() {
        if let Some((_tid, tpos)) = *move_target {
            match next_move_step(tpos, entity_pos, radius) {
                Some(step) => action = Some(step),
                None => *move_target = None,
            }
        }
    }
    action.map(|a| (target, a))
}

/// 点击走位的一步分解（net-protocol.md v1.5 矢量步语义，#030 第六刀）：
/// - 步长恒 1.0 朝目标矢量方向（dx,dy 归一化 ×1.0——斜走一步到位，L 形先横后竖废除）
/// - 终进近：剩余距离 ≤1.0 时一步精确踏上目标（防 ±1 整步在到达容差带两侧永久振荡）
/// - 到达判定 = 欧氏格距平方 ≤ 0.25（格距 ≤0.5，含边界防振荡）：
///   空中目标转悬停维持（目标保留），地面目标清除站住
/// - aerial 标记：实体悬空（py > radius）或目标在空中（tpos.1 > radius）——
///   main.rs 据此注入物理免重力集合（升力独立，被挡≠坠落）
///
/// 坐标有限性由解析层（net.rs）保证，本函数信任来源。
fn next_move_step(
    tpos: (f64, f64),
    pos: Option<(f64, f64)>,
    radius: f64,
) -> Option<ControlledAction> {
    let (px, py) = pos?;
    let dx = tpos.0 - px;
    let dy = tpos.1 - py;
    let d2 = dx * dx + dy * dy;
    if d2 <= 0.25 {
        if tpos.1 > radius {
            // 空中目标到位：悬停维持（目标保留；这是"站着，只是站在空中"）
            return Some(ControlledAction::Move {
                dx: 0.0,
                dy: 0.0,
                aerial: true,
            });
        }
        return None; // 地面目标到位：清除目标站住
    }
    let (sx, sy) = if d2 <= 1.0 {
        // 终进近：剩余 ≤1.0 一步踏上目标（到达容差带不振荡）
        (dx, dy)
    } else {
        // 矢量方向 ×1.0（斜走一步到位）
        let inv = 1.0 / d2.sqrt();
        (dx * inv, dy * inv)
    };
    Some(ControlledAction::Move {
        dx: sx,
        dy: sy,
        aerial: py > radius || tpos.1 > radius,
    })
}

/// 点击走位停滞检测窗口（期四第七刀：贴障微抖修复）
///
/// 判据 = **2 拍滑动窗口净位移 < 0.01 格**（位移平方 < 0.0001）。单拍阈值会被
/// 蹭动骗过——实测贴障蹭动是"终进近步被挡滑出 0.08、下拍滑回"的乒乓（b8.log
/// 实锤 y ±0.08 连蹭 7 拍），单拍位移 0.08~0.16 而窗口净位移 ≈ 0；净位移才是
/// "贴着不走"的真签名。合法步最短也有 0.5（终进近剩余距离 ∈ (0.5,1.0]），
/// 对阈值 0.01 安全边际巨大，不误杀慢速接近。
///
/// 悬停维持拍位置恒定属主动悬停（aerial），调用方在 aerial 场景跳过判定
/// （悬停中位置不动是升力在出力，不是被挡）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveStallTrack {
    /// 两拍前坐标（窗口基准）
    pos_2ago: (f64, f64),
    /// 一拍前坐标
    pos_1ago: (f64, f64),
}

impl MoveStallTrack {
    /// 以开窗时刻坐标建档（两拍前基准 = 当前位置）
    pub fn start(pos: (f64, f64)) -> Self {
        Self {
            pos_2ago: pos,
            pos_1ago: pos,
        }
    }

    /// 喂入本拍实际坐标；返回 true = 窗口净位移 < 0.01（停滞）。
    /// 窗口随后前移一格；首拍窗口历史不足 2 拍，按已有历史判定
    /// （全挡原地 0 位移首拍即真，滑出滑回第 2 拍判真——"被挡 2 拍清目标"）。
    pub fn check(&mut self, now: (f64, f64)) -> bool {
        let dx = now.0 - self.pos_2ago.0;
        let dy = now.1 - self.pos_2ago.1;
        let stagnant = dx * dx + dy * dy < 0.0001;
        self.pos_2ago = self.pos_1ago;
        self.pos_1ago = now;
        stagnant
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

    /// 路由测试便捷包装（迁移垫片：实体@pos 半径 0.5、无点击目标起步）
    fn route(
        intents: &[Intent],
        target: Option<EntityId>,
        nearest: Option<(f64, f64)>,
        pos: Option<(f64, f64)>,
    ) -> Option<(EntityId, ControlledAction)> {
        let mut mt: Option<(EntityId, (f64, f64))> = None;
        route_intents(intents, target, nearest, pos, 0.5, &mut mt)
    }

    /// 近似相等断言（矢量步产出归一化浮点，用容差比较）
    fn assert_step_close(
        action: Option<(EntityId, ControlledAction)>,
        want: (f64, f64, bool),
        ctx: &str,
    ) {
        match action {
            Some((_, ControlledAction::Move { dx, dy, aerial })) => {
                assert!((dx - want.0).abs() < 1e-9, "{ctx}: dx {} != {}", dx, want.0);
                assert!((dy - want.1).abs() < 1e-9, "{ctx}: dy {} != {}", dy, want.1);
                assert_eq!(aerial, want.2, "{ctx}: aerial 不符");
            }
            other => panic!("{ctx}: 期望 Move 步，实际 {:?}", other),
        }
    }

    #[test]
    fn no_player_online_routes_to_none() {
        // 无玩家进场 = 按键仅 Quit 生效（Quit 不进路由），路由无目标
        let intents = vec![Intent::Move { dx: 1.0, dy: 0.0 }, Intent::Attack];
        assert_eq!(route(&intents, None, None, None), None);
    }

    #[test]
    fn move_intent_routes_to_control_target() {
        // 地面实体（y=0.5=半径线）：键盘移动 = 地面步（无升力）
        let target = eid(1);
        let intents = vec![Intent::Move { dx: -1.0, dy: 0.0 }];
        assert_eq!(
            route(&intents, Some(target), None, Some((30.0, 0.5))),
            Some((
                target,
                ControlledAction::Move {
                    dx: -1.0,
                    dy: 0.0,
                    aerial: false
                }
            )),
            "Move 意图翻译为对操控目标的地面步"
        );
    }

    #[test]
    fn attack_intent_uses_nearest_visible_enemy() {
        let target = eid(1);
        let enemy_pos = (12.0, 0.5);
        let intents = vec![Intent::Attack];
        assert_eq!(
            route(&intents, Some(target), Some(enemy_pos), Some((30.0, 0.5))),
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
            route(&intents, Some(target), None, Some((30.0, 0.5))),
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
            route(&intents, Some(target), Some((5.0, 0.5)), Some((30.0, 0.5))),
            Some((target, ControlledAction::Attack(Some((5.0, 0.5))))),
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
            route(&intents, Some(target), Some((5.0, 0.5)), Some((30.0, 0.5))),
            Some((
                target,
                ControlledAction::Move {
                    dx: -1.0,
                    dy: 0.0,
                    aerial: false
                }
            )),
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
        assert_eq!(
            route(&[], Some(target), Some((5.0, 0.5)), Some((30.0, 0.5))),
            None
        );
    }

    // === route_intents：WS 鼠标意图（#027 点击走位/点击出刀/停止）===

    #[test]
    fn ws_move_sets_persistent_target_and_steps_toward_it() {
        // 点击世界坐标 = 持续目标：本拍立即向目标走一步，目标记入状态
        // （实体站在半径线 y=0.5，点击同高地面 (35,0.5)：矢量步 = 正东 (1,0)）
        let target = eid(1);
        let intents = vec![Intent::WsMove { x: 35.0, y: 0.5 }];
        let mut mt: Option<(EntityId, (f64, f64))> = None;
        let action = route_intents(&intents, Some(target), None, Some((30.0, 0.5)), 0.5, &mut mt);
        assert_step_close(
            action,
            (1.0, 0.0, false),
            "点击 (35,0.5) 本拍向东走一步（地面步）",
        );
        assert_eq!(mt, Some((target, (35.0, 0.5))), "持续目标入册（归属操控实体）");
    }

    #[test]
    fn persistent_target_steps_every_tick_without_intents() {
        // 持续目标的核心语义：无按键拍继续向目标走（逐拍一步），不用客户端重发
        let target = eid(1);
        let mut mt = Some((target, (35.0, 0.5)));
        let action = route_intents(&[], Some(target), None, Some((32.0, 0.5)), 0.5, &mut mt);
        assert_step_close(action, (1.0, 0.0, false), "无意图拍也向点击目标走一步");
        assert_eq!(mt, Some((target, (35.0, 0.5))), "未到达目标保持");
    }

    #[test]
    fn persistent_target_cleared_on_arrival() {
        // 到达判定 = 格距 ≤0.5（含边界防振荡）：走到目标邻域即清除站住
        let target = eid(1);
        let mut mt = Some((target, (35.0, 0.5)));
        let action = route_intents(&[], Some(target), None, Some((34.7, 0.5)), 0.5, &mut mt);
        assert_eq!(action, None, "格距 0.3 ≤ 0.5 = 已到达，无行动");
        assert_eq!(mt, None, "到达后目标清除（下拍站住不抖动）");
    }

    #[test]
    fn keyboard_move_clears_click_target() {
        // 键盘直控 = 玩家接管脚下：清除点击走位目标（松键后不会"复活"旧点击走路）
        let target = eid(1);
        let intents = vec![Intent::Move { dx: 0.0, dy: -1.0 }];
        let mut mt = Some((target, (35.0, 0.5)));
        let action = route_intents(&intents, Some(target), None, Some((32.0, 0.5)), 0.5, &mut mt);
        assert_eq!(
            action,
            Some((
                target,
                ControlledAction::Move {
                    dx: 0.0,
                    dy: -1.0,
                    aerial: false
                }
            ))
        );
        assert_eq!(mt, None, "键盘移动清除点击目标");
    }

    #[test]
    fn stop_clears_target_and_stands() {
        // Stop（右键/浏览器 S 键）= 清除点击目标站住；同拍已在走也不走
        let target = eid(1);
        let intents = vec![Intent::Stop];
        let mut mt = Some((target, (35.0, 0.5)));
        let action = route_intents(&intents, Some(target), None, Some((32.0, 0.5)), 0.5, &mut mt);
        assert_eq!(action, None, "Stop 拍站住");
        assert_eq!(mt, None, "Stop 清除持续目标");
    }

    #[test]
    fn ws_attack_uses_clicked_coords_not_nearest() {
        // 点击出刀 = 对点击坐标出刀（单次），不吃 nearest_enemy（与键盘空格同通路不同目标源）
        let target = eid(1);
        let intents = vec![Intent::WsAttack { x: 15.0, y: 0.5 }];
        let action = route(&intents, Some(target), Some((5.0, 0.5)), Some((30.0, 0.5)));
        assert_eq!(
            action,
            Some((target, ControlledAction::Attack(Some((15.0, 0.5))))),
            "点击坐标优先于最近敌人坐标"
        );
    }

    #[test]
    fn last_intent_wins_ws_attack_then_ws_move_same_batch() {
        // 同拍互斥后到优先对 WS 意图同样成立：先点敌人后点地 -> 本拍走路且目标更新
        let target = eid(1);
        let intents = vec![
            Intent::WsAttack { x: 15.0, y: 0.5 },
            Intent::WsMove { x: 35.0, y: 0.5 },
        ];
        let mut mt = Some((target, (40.0, 0.5)));
        let action = route_intents(&intents, Some(target), None, Some((30.0, 0.5)), 0.5, &mut mt);
        assert_step_close(
            action,
            (1.0, 0.0, false),
            "后到的 WsMove 生效（本拍向新目标走一步）",
        );
        assert_eq!(mt, Some((target, (35.0, 0.5))), "目标被后到点击覆盖");
    }

    #[test]
    fn move_target_not_handed_over_on_control_change() {
        // 点击目标不跨角色交接：目标属于 eid1，操控换 eid2 -> 旧目标清除，新角色不受旧点击支配
        let old = eid(1);
        let new = eid(2);
        let intents: Vec<Intent> = vec![];
        let mut mt = Some((old, (35.0, 0.5)));
        let action = route_intents(&intents, Some(new), None, Some((30.0, 0.5)), 0.5, &mut mt);
        assert_eq!(action, None, "换角后旧点击目标不生效");
        assert_eq!(mt, None, "旧目标清除");
    }

    // === 期四第六刀：矢量步 + 悬停独立（#030/#031；net-protocol.md v1.5）===

    #[test]
    fn diagonal_target_is_one_vector_step_not_l_shape() {
        // L 形废除（验收 a）：地面斜向目标 = 矢量方向归一化 ×1.0 一步斜走，
        // 不再先横后竖。（点击半径线地面点 (33,0)：dx=3 dy=-0.5，步长按
        // sqrt(9.25) 归一化——断言用同一表达式构造期望值免手算浮点）
        let target = eid(1);
        let mut mt = Some((target, (33.0, 0.0)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 0.5)), 0.5, &mut mt);
        let d = (9.25f64).sqrt();
        assert_step_close(action, (3.0 / d, -0.5 / d, false), "斜向目标一步矢量斜走（非先横后竖）");
    }

    #[test]
    fn aerial_target_marks_aerial_for_lift() {
        // 空中目标（ty > radius）→ 步带 aerial 标记（main.rs 注入物理免重力集合，
        // 升力独立于移动被挡——被挡≠坠落）
        let target = eid(1);
        let mut mt = Some((target, (30.0, 3.5)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 0.5)), 0.5, &mut mt);
        assert_step_close(action, (0.0, 1.0, true), "地面起步爬升：aerial 标记");
    }

    #[test]
    fn aerial_arrival_hovers_and_keeps_target() {
        // 空中目标到位（格距 ≤0.5）：转悬停（零位移 + aerial），目标保留到新意图
        let target = eid(1);
        let mut mt = Some((target, (30.0, 3.5)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 3.0)), 0.5, &mut mt);
        assert_eq!(
            action,
            Some((
                target,
                ControlledAction::Move {
                    dx: 0.0,
                    dy: 0.0,
                    aerial: true
                }
            )),
            "到位转悬停（零位移 + 升力）"
        );
        assert_eq!(mt, Some((target, (30.0, 3.5))), "悬停维持：目标保留");
    }

    #[test]
    fn airborne_entity_descends_controlled_to_ground_target() {
        // 空中点地面：受控降落（矢量步向下，aerial=true 免重力——是操控不是自由落体）
        let target = eid(1);
        let mut mt = Some((target, (30.0, 0.5)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 5.0)), 0.5, &mut mt);
        assert_step_close(action, (0.0, -1.0, true), "空中降落地面向下 1.0/拍（受控）");
        // 落到半径线后到达（dy 残差 0），地面目标清除站住
        let action2 = route_intents(&[], Some(target), None, Some((30.0, 0.5)), 0.5, &mut mt);
        assert_eq!(action2, None, "落地到达清除目标");
        assert_eq!(mt, None, "落地目标清除");
    }

    #[test]
    fn keyboard_move_while_airborne_carries_lift() {
        // 空中键盘移动 = 飞行操控（aerial 标记随实体状态，不随按键方向）
        let target = eid(1);
        let intents = vec![Intent::Move { dx: 1.0, dy: 0.0 }];
        let action = route(&intents, Some(target), None, Some((30.0, 5.0)));
        assert_eq!(
            action,
            Some((
                target,
                ControlledAction::Move {
                    dx: 1.0,
                    dy: 0.0,
                    aerial: true
                }
            )),
            "空中按键平移：升力照出"
        );
    }

    #[test]
    fn ground_target_ground_start_regression_anchor() {
        // 回归锚：地面目标 + 地面实体（半径线坐标）直线走位，到达清除站住
        let target = eid(1);
        let mut mt = Some((target, (32.0, 0.5)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 0.5)), 0.5, &mut mt);
        assert_step_close(action, (1.0, 0.0, false), "地面直线走位");
        let action2 = route_intents(&[], Some(target), None, Some((32.0, 0.5)), 0.5, &mut mt);
        assert_eq!(action2, None, "到达清除站住");
        assert_eq!(mt, None, "地面到达清除目标");
    }

    #[test]
    fn stop_in_air_hovers_instead_of_falling() {
        // stop 空中 = 悬停不坠（stop 语义 = 停止移动不是坠落）：目标改写为脚下坐标，
        // 后续无意图拍继续悬停；要落地玩家点地面
        let target = eid(1);
        let mut mt = Some((target, (35.0, 0.5)));
        let action = route_intents(
            &[Intent::Stop],
            Some(target),
            None,
            Some((30.0, 5.0)),
            0.5,
            &mut mt,
        );
        assert_eq!(
            action,
            Some((
                target,
                ControlledAction::Move {
                    dx: 0.0,
                    dy: 0.0,
                    aerial: true
                }
            )),
            "空中 stop 本拍即转悬停"
        );
        assert_eq!(mt, Some((target, (30.0, 5.0))), "悬停锚点 = 脚下坐标（目标改写）");
        let action2 = route_intents(&[], Some(target), None, Some((30.0, 5.0)), 0.5, &mut mt);
        assert_eq!(
            action2,
            Some((
                target,
                ControlledAction::Move {
                    dx: 0.0,
                    dy: 0.0,
                    aerial: true
                }
            )),
            "后续无意图拍继续悬停不坠"
        );
    }

    #[test]
    fn stop_on_ground_keeps_old_stand_semantics() {
        // 回归锚：地面（半径线）stop 仍是清除站住
        let target = eid(1);
        let mut mt = Some((target, (35.0, 0.5)));
        let action = route_intents(
            &[Intent::Stop],
            Some(target),
            None,
            Some((30.0, 0.5)),
            0.5,
            &mut mt,
        );
        assert_eq!(action, None, "地面 stop 站住");
        assert_eq!(mt, None, "地面 stop 清除目标");
    }

    #[test]
    fn fractional_target_final_approach_no_oscillation() {
        // 终进近防振荡：剩余距离 ≤1.0 一步精确踏上目标（±1 整步会在到达容差带
        // 两侧永久振荡——点击是连续坐标）。(30.6, 0.8) 与 (30,0.5) 距离≈0.67 ∈ (0.5,1]
        let target = eid(1);
        let mut mt = Some((target, (30.6, 0.8)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 0.5)), 0.5, &mut mt);
        match action {
            Some((_, ControlledAction::Move { dx, dy, aerial })) => {
                assert!((dx - 0.6).abs() < 1e-9 && (dy - 0.3).abs() < 1e-9, "终进近踏上目标");
                assert!(aerial, "目标 y=0.8 > 半径 0.5 = 空中目标");
            }
            other => panic!("期望 Move 步，实际 {:?}", other),
        }
        // 踏上后到达（格距 0）转悬停/清除，不振荡
        let action2 = route_intents(&[], Some(target), None, Some((30.6, 0.8)), 0.5, &mut mt);
        assert_eq!(action2, Some((target, ControlledAction::Move { dx: 0.0, dy: 0.0, aerial: true })), "到位即悬停（收敛不振荡）");
    }

    #[test]
    fn target_behind_steps_negative_direction() {
        // 目标在起点西方向：矢量步为负向（不是恒正向）
        let target = eid(1);
        let mut mt = Some((target, (28.0, 0.5)));
        let action = route_intents(&[], Some(target), None, Some((30.0, 0.5)), 0.5, &mut mt);
        assert_step_close(action, (-1.0, 0.0, false), "西向目标负向步");
    }

    // === 期四第七刀：贴障停滞检测窗口（2 拍净位移）===

    #[test]
    fn move_stall_detects_two_tick_ping_pong_jitter() {
        // 乒乓蹭动签名（b8.log 实锤的 ±0.08 蹭动）：单拍位移 0.08 骗过单拍阈值，
        // 2 拍窗口净位移 ≈ 0（滑出去又滑回来）才是"贴着不走"的真签名
        let mut t = MoveStallTrack::start((30.0, 0.5));
        assert!(
            !t.check((30.08, 0.5)),
            "首拍滑动 0.08：窗口历史不足 2 拍不判停滞"
        );
        assert!(t.check((30.0, 0.5)), "滑回原位：2 拍窗口净位移 0 = 停滞");
    }

    #[test]
    fn move_stall_not_triggered_by_normal_walk() {
        // 正常走位每拍 1.0：窗口净位移 2.0，对阈值 0.01 安全边际巨大（不误杀慢速接近——
        // 合法步最短也有 0.5：终进近剩余距离 ∈ (0.5,1.0]）
        let mut t = MoveStallTrack::start((30.0, 0.5));
        assert!(!t.check((31.0, 0.5)));
        assert!(!t.check((32.0, 0.5)));
        assert!(!t.check((33.0, 0.5)));
    }

    #[test]
    fn move_stall_detects_fully_blocked_standstill() {
        // 全挡原地：窗口净位移 0，首个完整窗口即判停滞
        let mut t = MoveStallTrack::start((30.0, 0.5));
        assert!(t.check((30.0, 0.5)), "全挡原地 = 停滞");
        assert!(t.check((30.0, 0.5)), "持续全挡持续停滞");
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
