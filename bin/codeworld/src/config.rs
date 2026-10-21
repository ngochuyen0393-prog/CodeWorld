//! 运行配置（Config）：主程序启动参数
//!
//! 所有字段均可通过环境变量（`CODEWORLD_*`）覆盖，无环境变量时用默认值。
//! 主程序启动时调用 [`Config::load`] 读取配置。

use std::time::Duration;

/// 主程序运行配置
///
/// 字段含义与默认值见各字段文档；均可在运行时用环境变量覆盖。
#[derive(Debug, Clone)]
pub struct Config {
    /// tick 间隔（毫秒），默认 1000（1秒/tick）
    pub tick_interval_ms: u64,
    /// 数据库路径，默认 "codeworld.db"
    pub db_path: String,
    /// 视口中心，默认 (0.0, 0.0)
    pub viewport_center: (f64, f64),
    /// 视口半宽，默认 50.0
    pub viewport_half_width: f64,
    /// 视口半高，默认 25.0
    pub viewport_half_height: f64,
    /// 终端行数，默认 50
    pub term_rows: usize,
    /// 终端列数，默认 80
    pub term_cols: usize,
    /// 日志级别，默认 "info"
    pub log_level: String,
    /// 防御者 D 的盾量（拦截配额），默认 10。
    /// 决定默认剧本胜负时间线：10 = D 反制先杀 A；1 = A 磨穿薄盾反杀（翻盘剧本）。
    pub d_shield: i32,
    /// 是否登场第三只生物 H（黄"H" 搅局者），默认 true。
    /// CODEWORLD_H_ENABLED=0/false 关闭（经典双生物对局，供翻盘剧本与冒烟对照）。
    pub h_enabled: bool,
    /// 被探测方 on_probe 的独立小 fuel 预算（架构 3.2 技术保护
    /// probe_target_instruction_limit），默认 10_000。
    /// probe import 在 host 侧用 set_fuel 包裹目标沙箱：探测前存剩余量、
    /// 设小预算、调 on_probe、恢复原 fuel——恶意 on_probe 死循环最多烧
    /// 预算值即中断（不占目标 tick 配额）。CODEWORLD_PROBE_FUEL 覆盖。
    pub probe_fuel: u64,
    /// 是否登场第四只生物 S（紫"S" 伪装者/间谍），默认 true。
    /// CODEWORLD_S_ENABLED=0/false 关闭（三方混战对局，供剧本 1-3 冒烟对照）。
    pub s_enabled: bool,
    /// 玩家目录路径（门一进场：一个 .wat 文件 = 一名玩家），默认 "./players"。
    /// CODEWORLD_PLAYERS_DIR 覆盖；目录不存在自动创建。
    pub players_dir: String,
    /// 世界实体总数上限（门二 spawn 配套，decisions.md #020），默认 64。
    /// 达上限时 env.spawn 请求在阶段二拒绝。CODEWORLD_MAX_ENTITIES 覆盖。
    pub max_entities: usize,
    /// WebSocket 快照推送端口（期四第一刀：每拍快照广播给浏览器），默认 8080。
    /// 0 = 禁用 WS（不启动 WS 线程、tick 不组装快照，终端体验完全不变）。
    /// CODEWORLD_WS_PORT 覆盖；监听失败仅 warn 不影响主循环。
    pub ws_port: u16,
    /// 下线处置等待拍数（#025 期五第一刀：操控连接意外断开后角色留场等待处置
    /// 的窗口），默认 30。0 = 断开即默认带走（不等待）。CODEWORLD_DISPOSE_WAIT 覆盖。
    pub dispose_wait_ticks: u64,
    /// 阻力常量（#034 世界基石参数，与重力同级），默认 0.05。
    /// 阻力定律：v = v - drag*v/mass（线性阻力，方向不变模衰减）。
    /// 标定判据：默认参数下标准上抛弹（vx=4/vy=+9.8/mass=1.0）射程 3~6 格
    ///（#034 修订二 CCD 地面交点口径重标，drag 常数未变）。
    /// CODEWORLD_DRAG 覆盖。
    pub drag_coefficient: f64,
    /// 世界极速（#034 修订一：视觉可读性常数），默认 25.0（codeworld_core
    /// WORLD_MAX_SPEED）。任何离体之物一拍位移的硬上限：发射超限拒绝 + 飞行中
    /// 积分后归一化压回（重力累积封顶）。标定锚：横穿全图 100 格最短约 4 拍。
    /// CODEWORLD_MAX_SPEED 覆盖；非正数/非有限值回退默认。
    pub max_speed: f64,
    /// 地形预设（D-016 高度图地面），默认 "flat"（平地）。
    /// 取值 flat/hills/mountain，收英文中文别名（丘陵/山地）；此处存原文，
    /// 非法回退平地的口径在主程序装配时由 TerrainPreset::parse 执行（warn 提示）。
    /// CODEWORLD_TERRAIN 覆盖。仅新世界生效——快照恢复的世界地形随快照走。
    pub terrain_preset: String,
    /// 地形生成种子（D-016 可复现凭据：同预设+种子必产同一高度图），默认 0。
    /// CODEWORLD_SEED 覆盖；解析失败回退 0（容错语义同其他数值项）。
    pub terrain_seed: u64,
    /// 被接触方 on_contact 的独立小 fuel 预算（D-007 三兄弟，架构 3.3 同族
    /// contact_target_instruction_limit），默认 10_000（与 probe 同档——地雷/
    /// 哨戒圈是轻量应答）。host 侧 set_fuel 包裹同 probe 模式。
    /// CODEWORLD_CONTACT_FUEL 覆盖。
    pub contact_fuel: u64,
    /// 死亡谢幕 on_death 的独立 fuel 预算（D-007 遗嘱钩子，架构 3.3 同族
    /// death_instruction_limit），默认 200_000（与 on_attack 同档——遗嘱有
    /// 字数上限=预算即上限，烧尽即遗嘱写不完，世界不补偿不重试）。
    /// CODEWORLD_DEATH_FUEL 覆盖。
    pub death_fuel: u64,
    /// on_field 询问独立小 fuel 预算（D-012 协议 v3，架构 3.3 同族），默认
    /// 10_000（与 contact/probe 同档——轻量应答）。CODEWORLD_FIELD_FUEL 覆盖。
    pub field_fuel: u64,
    /// 力场成型期拍数（D-012 可视铁律：诞生 N 拍内不结算物理但已可见，给进攻
    /// 方反应窗），默认 FIELD_FORMING_TICKS=3。CODEWORLD_FIELD_FORMING 覆盖。
    pub field_forming_ticks: u64,
    /// 单沙箱资产配额（#051 皮入沙箱：引擎+魂+皮合计封顶，Screeps 5MB 思路），
    /// 默认 SANDBOX_ASSET_QUOTA_BYTES=1MB（与 64MB 内存上限≈1/64，资产=富余量
    /// 非主粮）。换血时新模块+随行资产超配额 → 拒绝换血（资产不丢）。
    /// ⚠️ 临时值待土豆拍板（同 MASK_FRAG_DROP_N 先例）。CODEWORLD_ASSET_QUOTA 覆盖；
    /// 0/非法值回退默认（0 会让空模块换血也被拒，无"禁皮"语义，坏值不进世界）。
    pub asset_quota: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tick_interval_ms: 1000,
            db_path: "codeworld.db".to_string(),
            viewport_center: (0.0, 0.0),
            viewport_half_width: 50.0,
            viewport_half_height: 25.0,
            term_rows: 50,
            term_cols: 80,
            log_level: "info".to_string(),
            d_shield: 10,
            h_enabled: true,
            probe_fuel: 10_000,
            s_enabled: true,
            players_dir: "./players".to_string(),
            max_entities: 64,
            ws_port: 8080,
            dispose_wait_ticks: 30,
            drag_coefficient: 0.05,
            max_speed: codeworld_core::physics::WORLD_MAX_SPEED,
            terrain_preset: "flat".to_string(),
            terrain_seed: 0,
            contact_fuel: 10_000,
            death_fuel: 200_000,
            field_fuel: 10_000,
            field_forming_ticks: codeworld_core::physics::FIELD_FORMING_TICKS,
            asset_quota: codeworld_arena::assets::SANDBOX_ASSET_QUOTA_BYTES,
        }
    }
}

impl Config {
    /// 从环境变量加载（`CODEWORLD_TICK_MS` / `CODEWORLD_DB_PATH` / `CODEWORLD_LOG_LEVEL` 等），
    /// 无环境变量或值非法时用默认值
    pub fn load() -> Self {
        let mut cfg = Self::default();
        // 数值类：解析失败则保留默认值（容错）
        if let Some(ms) = env_u64("CODEWORLD_TICK_MS") {
            cfg.tick_interval_ms = ms;
        }
        if let Some(rows) = env_usize("CODEWORLD_TERM_ROWS") {
            cfg.term_rows = rows;
        }
        if let Some(cols) = env_usize("CODEWORLD_TERM_COLS") {
            cfg.term_cols = cols;
        }
        if let Some(x) = env_f64("CODEWORLD_VIEWPORT_CENTER_X") {
            cfg.viewport_center.0 = x;
        }
        if let Some(y) = env_f64("CODEWORLD_VIEWPORT_CENTER_Y") {
            cfg.viewport_center.1 = y;
        }
        if let Some(w) = env_f64("CODEWORLD_VIEWPORT_HALF_WIDTH") {
            cfg.viewport_half_width = w;
        }
        if let Some(h) = env_f64("CODEWORLD_VIEWPORT_HALF_HEIGHT") {
            cfg.viewport_half_height = h;
        }
        if let Some(shield) = env_i32("CODEWORLD_D_SHIELD") {
            cfg.d_shield = shield;
        }
        if let Some(fuel) = env_u64("CODEWORLD_PROBE_FUEL") {
            cfg.probe_fuel = fuel;
        }
        if let Some(n) = env_usize("CODEWORLD_MAX_ENTITIES") {
            cfg.max_entities = n;
        }
        if let Some(p) = env_u16("CODEWORLD_WS_PORT") {
            cfg.ws_port = p;
        }
        if let Some(t) = env_u64("CODEWORLD_DISPOSE_WAIT") {
            cfg.dispose_wait_ticks = t;
        }
        if let Some(d) = env_f64("CODEWORLD_DRAG") {
            // 护栏（审查修正）：负阻力=弹丸加速器（factor 1-drag/mass 发散），钳 0；
            // 非有限值进除法公式污染世界，回退默认
            if !d.is_finite() {
                tracing::warn!(value = d, "[config] CODEWORLD_DRAG 非有限值，回退默认 0.05");
            } else if d < 0.0 {
                tracing::warn!(value = d, "[config] CODEWORLD_DRAG 负值（弹丸加速器），钳到 0");
                cfg.drag_coefficient = 0.0;
            } else {
                cfg.drag_coefficient = d;
            }
        }
        if let Some(s) = env_f64("CODEWORLD_MAX_SPEED") {
            // 护栏：世界极速是归一化压回的分母候选——0/负值除零，非有限值污染弹道，
            // 一律回退默认 25.0（视觉可读性常数，坏值不进世界）
            if !s.is_finite() || s <= 0.0 {
                tracing::warn!(value = s, "[config] CODEWORLD_MAX_SPEED 非正数或非有限值，回退默认 25.0");
            } else {
                cfg.max_speed = s;
            }
        }
        // 布尔类开关："0"/"false"（大小写不敏感）= 关闭，缺失或其他值 = 默认开启
        if let Ok(v) = std::env::var("CODEWORLD_H_ENABLED") {
            if v == "0" || v.eq_ignore_ascii_case("false") {
                cfg.h_enabled = false;
            }
        }
        if let Ok(v) = std::env::var("CODEWORLD_S_ENABLED") {
            if v == "0" || v.eq_ignore_ascii_case("false") {
                cfg.s_enabled = false;
            }
        }
        // 字符串类：有值即覆盖（空字符串也覆盖，符合"显式设置"语义）
        if let Ok(val) = std::env::var("CODEWORLD_DB_PATH") {
            cfg.db_path = val;
        }
        if let Ok(val) = std::env::var("CODEWORLD_LOG_LEVEL") {
            cfg.log_level = val;
        }
        if let Ok(val) = std::env::var("CODEWORLD_PLAYERS_DIR") {
            cfg.players_dir = val;
        }
        // D-016 地形装配参数：preset 存原文（parse 回退口径在装配层，坏值不进世界
        // 但也不无声消失——装配时 warn）；seed 解析失败回退 0，容错同其他数值项
        if let Ok(val) = std::env::var("CODEWORLD_TERRAIN") {
            cfg.terrain_preset = val;
        }
        if let Some(s) = env_u64("CODEWORLD_SEED") {
            cfg.terrain_seed = s;
        }
        // D-007 三兄弟预算（模式照抄 CODEWORLD_PROBE_FUEL：解析失败保留默认值）
        if let Some(f) = env_u64("CODEWORLD_CONTACT_FUEL") {
            cfg.contact_fuel = f;
        }
        if let Some(f) = env_u64("CODEWORLD_DEATH_FUEL") {
            cfg.death_fuel = f;
        }
        // D-012 力场常数（解析失败保留默认值；成型期默认 FIELD_FORMING_TICKS=3）
        if let Some(f) = env_u64("CODEWORLD_FIELD_FUEL") {
            cfg.field_fuel = f;
        }
        if let Some(n) = env_u64("CODEWORLD_FIELD_FORMING") {
            cfg.field_forming_ticks = n;
        }
        // #051 单沙箱资产配额（解析失败/0 保留默认值——0 无"禁皮"语义，坏值不进世界）
        if let Some(n) = env_usize("CODEWORLD_ASSET_QUOTA") {
            if n == 0 {
                tracing::warn!(
                    value = n,
                    "[config] CODEWORLD_ASSET_QUOTA=0 无效（会拒绝一切换血），回退默认 1MB"
                );
            } else {
                cfg.asset_quota = n;
            }
        }
        cfg
    }

    /// tick 间隔转为 [`Duration`]
    pub fn tick_interval(&self) -> Duration {
        Duration::from_millis(self.tick_interval_ms)
    }
}

/// 读取并解析 u64 环境变量，缺失或非法返回 None
fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.parse().ok()
}

/// 读取并解析 usize 环境变量，缺失或非法返回 None
fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.parse().ok()
}

/// 读取并解析 u16 环境变量，缺失或非法返回 None
fn env_u16(key: &str) -> Option<u16> {
    std::env::var(key).ok()?.parse().ok()
}

/// 读取并解析 f64 环境变量，缺失或非法返回 None
fn env_f64(key: &str) -> Option<f64> {
    std::env::var(key).ok()?.parse().ok()
}

/// 读取并解析 i32 环境变量，缺失或非法返回 None
fn env_i32(key: &str) -> Option<i32> {
    std::env::var(key).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // 环境变量读写非线程安全，串行化所有 env 测试避免并行竞态
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_returns_expected_values() {
        // 创建：default() 返回文档约定的默认值
        let c = Config::default();
        assert_eq!(c.tick_interval_ms, 1000);
        assert_eq!(c.db_path, "codeworld.db");
        assert_eq!(c.viewport_center, (0.0, 0.0));
        assert_eq!(c.viewport_half_width, 50.0);
        assert_eq!(c.viewport_half_height, 25.0);
        assert_eq!(c.term_rows, 50);
        assert_eq!(c.term_cols, 80);
        assert_eq!(c.log_level, "info");
    }

    #[test]
    fn load_without_env_returns_default() {
        // 创建：无环境变量时 load() 等价于 default()
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CODEWORLD_TICK_MS");
        std::env::remove_var("CODEWORLD_DB_PATH");
        std::env::remove_var("CODEWORLD_LOG_LEVEL");
        let c = Config::load();
        assert_eq!(c.tick_interval_ms, 1000);
        assert_eq!(c.db_path, "codeworld.db");
        assert_eq!(c.log_level, "info");
    }

    #[test]
    fn load_with_env_overrides_defaults() {
        // 修改：环境变量覆盖默认值
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_TICK_MS", "500");
        std::env::set_var("CODEWORLD_DB_PATH", "/tmp/test_cw.db");
        std::env::set_var("CODEWORLD_LOG_LEVEL", "debug");
        let c = Config::load();
        // 清理，避免污染其他测试
        std::env::remove_var("CODEWORLD_TICK_MS");
        std::env::remove_var("CODEWORLD_DB_PATH");
        std::env::remove_var("CODEWORLD_LOG_LEVEL");
        assert_eq!(c.tick_interval_ms, 500);
        assert_eq!(c.db_path, "/tmp/test_cw.db");
        assert_eq!(c.log_level, "debug");
    }

    #[test]
    fn tick_interval_converts_to_duration() {
        // 读写：tick_interval() 毫秒转 Duration
        let c = Config {
            tick_interval_ms: 2500,
            ..Config::default()
        };
        assert_eq!(c.tick_interval(), Duration::from_millis(2500));
    }

    #[test]
    fn default_d_shield_is_ten_and_h_enabled_true() {
        // 创建：D 盾量默认 10（默认剧本 D 胜的时间线由该值锁定）；H 搅局者默认登场
        let c = Config::default();
        assert_eq!(c.d_shield, 10, "D 盾量默认应为 10");
        assert!(c.h_enabled, "H 搅局者默认应登场");
    }

    #[test]
    fn default_probe_fuel_and_s_enabled_match_arch() {
        // 创建：probe 预算默认 10_000（架构 3.3 probe_target_instruction_limit）；
        // S 伪装者默认登场（剧本 4 需要它默认在局）
        let c = Config::default();
        assert_eq!(c.probe_fuel, 10_000, "probe 预算默认应为 10_000");
        assert!(c.s_enabled, "S 伪装者默认应登场");
    }

    #[test]
    fn load_probe_fuel_env_overrides_default() {
        // 修改：CODEWORLD_PROBE_FUEL 覆盖默认预算（技术保护可调）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_PROBE_FUEL", "500");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_PROBE_FUEL");
        assert_eq!(c.probe_fuel, 500, "环境变量应覆盖 probe 预算");
    }

    #[test]
    fn load_probe_fuel_invalid_falls_back_to_default() {
        // 边界：CODEWORLD_PROBE_FUEL 非法（非整数）-> 回退默认 10_000（容错语义）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_PROBE_FUEL", "not-a-number");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_PROBE_FUEL");
        assert_eq!(c.probe_fuel, 10_000, "非法值应回退默认 probe 预算");
    }

    #[test]
    fn load_s_enabled_env_can_disable() {
        // 修改：CODEWORLD_S_ENABLED=0/false 关闭 S（三方对局供剧本 1-3 冒烟对照）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_S_ENABLED", "0");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_S_ENABLED");
        assert!(!c.s_enabled, "CODEWORLD_S_ENABLED=0 应关闭 S");
        std::env::set_var("CODEWORLD_S_ENABLED", "false");
        let c2 = Config::load();
        std::env::remove_var("CODEWORLD_S_ENABLED");
        assert!(!c2.s_enabled, "CODEWORLD_S_ENABLED=false 应关闭 S");
    }

    #[test]
    fn load_s_enabled_absent_defaults_to_enabled() {
        // 边界：无环境变量 -> S 默认登场（剧本 4 伪装者默认在局）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CODEWORLD_S_ENABLED");
        let c = Config::load();
        assert!(c.s_enabled, "无环境变量时 S 应默认登场");
    }

    #[test]
    fn load_d_shield_env_overrides_default() {
        // 修改：CODEWORLD_D_SHIELD=1 覆盖默认（翻盘剧本：A 磨穿薄盾反杀）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_D_SHIELD", "1");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_D_SHIELD");
        assert_eq!(c.d_shield, 1, "环境变量应覆盖 D 盾量");
    }

    #[test]
    fn load_d_shield_invalid_value_falls_back_to_default() {
        // 边界：CODEWORLD_D_SHIELD 非法（非整数）-> 回退默认 10（容错语义与数值类一致）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_D_SHIELD", "not-a-number");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_D_SHIELD");
        assert_eq!(c.d_shield, 10, "非法值应回退默认盾量");
    }

    #[test]
    fn load_h_enabled_env_can_disable() {
        // 修改：CODEWORLD_H_ENABLED=0 关闭 H（经典双生物对局：默认/翻盘两剧本冒烟用）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_H_ENABLED", "0");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_H_ENABLED");
        assert!(!c.h_enabled, "CODEWORLD_H_ENABLED=0 应关闭 H");
        // "false" 同样关闭（语义别名）
        std::env::set_var("CODEWORLD_H_ENABLED", "false");
        let c2 = Config::load();
        std::env::remove_var("CODEWORLD_H_ENABLED");
        assert!(!c2.h_enabled, "CODEWORLD_H_ENABLED=false 应关闭 H");
    }

    #[test]
    fn load_h_enabled_absent_defaults_to_enabled() {
        // 边界：无环境变量 -> H 默认登场（start.bat 默认混战剧本）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CODEWORLD_H_ENABLED");
        let c = Config::load();
        assert!(c.h_enabled, "无环境变量时 H 应默认登场");
    }

    #[test]
    fn default_players_dir_is_local_players() {
        // 创建：玩家目录默认 "./players"（门一进场，world-foundation 二）
        let c = Config::default();
        assert_eq!(c.players_dir, "./players");
    }

    #[test]
    fn load_players_dir_env_overrides_default() {
        // 修改：CODEWORLD_PLAYERS_DIR 覆盖玩家目录路径
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_PLAYERS_DIR", "D:/tmp/my-players");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_PLAYERS_DIR");
        assert_eq!(c.players_dir, "D:/tmp/my-players");
    }

    #[test]
    fn default_max_entities_is_64() {
        // 创建：世界实体总数上限默认 64（门二 spawn 配套，decisions.md #020）
        let c = Config::default();
        assert_eq!(c.max_entities, 64, "实体上限默认应为 64");
    }

    #[test]
    fn load_max_entities_env_overrides_default() {
        // 修改：CODEWORLD_MAX_ENTITIES 覆盖默认上限
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_MAX_ENTITIES", "32");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_MAX_ENTITIES");
        assert_eq!(c.max_entities, 32, "环境变量应覆盖实体上限");
    }

    #[test]
    fn default_ws_port_is_8080() {
        // 创建：WS 端口默认 8080（期四第一刀快照推送；0=禁用）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CODEWORLD_WS_PORT");
        let c = Config::load();
        assert_eq!(c.ws_port, 8080, "WS 端口默认应为 8080");
    }

    #[test]
    fn load_ws_port_env_overrides_default() {
        // 修改：CODEWORLD_WS_PORT=9090 覆盖默认端口
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_WS_PORT", "9090");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_WS_PORT");
        assert_eq!(c.ws_port, 9090, "环境变量应覆盖 WS 端口");
    }

    #[test]
    fn load_ws_port_zero_means_disabled() {
        // 修改：CODEWORLD_WS_PORT=0 = 禁用 WS（终端体验不受影响的逃生门）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_WS_PORT", "0");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_WS_PORT");
        assert_eq!(c.ws_port, 0, "端口 0 应原样保留（禁用语义）");
    }

    #[test]
    fn load_ws_port_invalid_falls_back_to_default() {
        // 边界：CODEWORLD_WS_PORT 非法（非整数/超 u16 范围）-> 回退默认 8080（容错语义同数值类）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_WS_PORT", "not-a-port");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_WS_PORT");
        assert_eq!(c.ws_port, 8080, "非法值应回退默认 WS 端口");
        std::env::set_var("CODEWORLD_WS_PORT", "99999");
        let c2 = Config::load();
        std::env::remove_var("CODEWORLD_WS_PORT");
        assert_eq!(c2.ws_port, 8080, "超 u16 范围应回退默认 WS 端口");
    }

    // === 下线处置等待拍数（#025 期五第一刀）===

    #[test]
    fn default_dispose_wait_is_thirty() {
        // 创建：处置等待默认 30 拍（宪法 #025：世界不等人，窗口够玩家重开浏览器救场）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CODEWORLD_DISPOSE_WAIT");
        let c = Config::load();
        assert_eq!(c.dispose_wait_ticks, 30, "处置等待默认应为 30 拍");
    }

    #[test]
    fn load_dispose_wait_env_overrides_default() {
        // 修改：CODEWORLD_DISPOSE_WAIT 覆盖等待窗口（配置化）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_DISPOSE_WAIT", "10");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_DISPOSE_WAIT");
        assert_eq!(c.dispose_wait_ticks, 10, "环境变量应覆盖处置等待");
    }

    #[test]
    fn load_dispose_wait_zero_means_take_immediately() {
        // 修改：0 = 断开即默认带走（不等待，急性子/无人值守服务器场景）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_DISPOSE_WAIT", "0");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_DISPOSE_WAIT");
        assert_eq!(c.dispose_wait_ticks, 0, "0 应原样保留（立即 take 语义）");
    }

    #[test]
    fn load_dispose_wait_invalid_falls_back_to_default() {
        // 边界：非法值（非整数）-> 回退默认 30（容错语义同数值类）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_DISPOSE_WAIT", "soon");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_DISPOSE_WAIT");
        assert_eq!(c.dispose_wait_ticks, 30, "非法值应回退默认处置等待");
    }

    // === 阻力常量（#034 世界基石参数）===

    #[test]
    fn default_drag_is_point_zero_five() {
        // 创建：阻力默认 0.05（标定判据：默认参数标准上抛弹射程 5~15 格，
        // physics crate projectile_range_calibration_default_drag 测试锚定）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CODEWORLD_DRAG");
        let c = Config::load();
        assert_eq!(c.drag_coefficient, 0.05, "阻力默认应为 0.05");
    }

    #[test]
    fn load_drag_env_overrides_default() {
        // 修改：CODEWORLD_DRAG 覆盖默认阻力（世界基石参数可调）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_DRAG", "0.2");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_DRAG");
        assert_eq!(c.drag_coefficient, 0.2, "环境变量应覆盖阻力常量");
    }

    #[test]
    fn load_drag_non_finite_falls_back_to_default() {
        // 边界：非有限值（NaN/inf）-> 回退默认 0.05（阻力进除法公式，坏值不进世界）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_DRAG", "nan");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_DRAG");
        assert_eq!(c.drag_coefficient, 0.05, "非有限值应回退默认阻力");
    }

    #[test]
    fn load_drag_negative_clamps_to_zero() {
        // 边界（审查修正）：负阻力 = 弹丸加速器（factor 1-drag/mass 逐拍发散）-> 钳 0
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_DRAG", "-1.0");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_DRAG");
        assert_eq!(c.drag_coefficient, 0.0, "负阻力应钳到 0（无阻力，不放大力）");
    }

    // === 世界极速（#034 修订一：视觉可读性常数，CODEWORLD_MAX_SPEED）===

    // === 接触/遗嘱预算（D-007 三兄弟，模式照抄 probe_fuel）===

    #[test]
    fn default_contact_and_death_fuel_match_core_constraints() {
        // 创建：接触回调预算默认 10_000（core Constraints.contact_target_instruction_limit
        // 同值，与 probe 同档）；遗嘱预算默认 200_000（death_instruction_limit 同值，
        // 与 on_attack 同档——遗嘱有字数上限=预算即上限）
        let c = Config::default();
        assert_eq!(c.contact_fuel, 10_000, "接触回调预算默认应为 10_000");
        assert_eq!(c.death_fuel, 200_000, "遗嘱预算默认应为 200_000");
    }

    #[test]
    fn load_contact_fuel_env_overrides_default() {
        // 修改：CODEWORLD_CONTACT_FUEL 覆盖默认（技术保护可调）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_CONTACT_FUEL", "500");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_CONTACT_FUEL");
        assert_eq!(c.contact_fuel, 500, "环境变量应覆盖接触回调预算");
    }

    #[test]
    fn load_contact_fuel_invalid_falls_back_to_default() {
        // 边界：CODEWORLD_CONTACT_FUEL 非法（非整数）-> 回退默认 10_000（容错语义）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_CONTACT_FUEL", "soon");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_CONTACT_FUEL");
        assert_eq!(c.contact_fuel, 10_000, "非法值应回退默认接触回调预算");
    }

    #[test]
    fn load_death_fuel_env_overrides_default() {
        // 修改：CODEWORLD_DEATH_FUEL 覆盖默认（遗嘱字数上限世界可调）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_DEATH_FUEL", "777");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_DEATH_FUEL");
        assert_eq!(c.death_fuel, 777, "环境变量应覆盖遗嘱预算");
    }

    #[test]
    fn load_death_fuel_invalid_falls_back_to_default() {
        // 边界：CODEWORLD_DEATH_FUEL 非法（非整数）-> 回退默认 200_000（容错语义）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_DEATH_FUEL", "not-a-number");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_DEATH_FUEL");
        assert_eq!(c.death_fuel, 200_000, "非法值应回退默认遗嘱预算");
    }

    #[test]
    fn load_field_fuel_and_forming_env_overrides_default() {
        // 修改：CODEWORLD_FIELD_FUEL / CODEWORLD_FIELD_FORMING 覆盖默认（D-012
        // 力场常数世界可调；成型期默认 FIELD_FORMING_TICKS=3）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_FIELD_FUEL", "900");
        std::env::set_var("CODEWORLD_FIELD_FORMING", "5");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_FIELD_FUEL");
        std::env::remove_var("CODEWORLD_FIELD_FORMING");
        assert_eq!(c.field_fuel, 900, "环境变量应覆盖 on_field 预算");
        assert_eq!(c.field_forming_ticks, 5, "环境变量应覆盖成型期拍数");
    }

    #[test]
    fn load_field_defaults_match_world_constants() {
        // 创建：field 两常数默认值与世界常数总账同源（成型期=FIELD_FORMING_TICKS）
        let c = Config::default();
        assert_eq!(c.field_fuel, 10_000);
        assert_eq!(
            c.field_forming_ticks,
            codeworld_core::physics::FIELD_FORMING_TICKS,
            "成型期默认应取世界常数"
        );
    }

    #[test]
    fn default_max_speed_is_twenty_five() {
        // 创建：世界极速默认 25 格/拍（视觉可读性常数：一拍位移 ≤ 世界宽度 1/4，
        // 攻击永远看得见飞过来；横穿全图 100 格最短约 4 拍的标定锚）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CODEWORLD_MAX_SPEED");
        let c = Config::load();
        assert_eq!(c.max_speed, 25.0, "世界极速默认应为 25.0");
    }

    #[test]
    fn load_max_speed_env_overrides_default() {
        // 修改：CODEWORLD_MAX_SPEED 覆盖默认（世界常数可调）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_MAX_SPEED", "40");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_MAX_SPEED");
        assert_eq!(c.max_speed, 40.0, "环境变量应覆盖世界极速");
    }

    #[test]
    fn load_max_speed_invalid_falls_back_to_default() {
        // 边界：非正数/非有限值 -> 回退默认 25.0（极速是物理常数，坏值不进世界：
        // 0 或负值会让归一化压回除零/反向，NaN 污染弹道）
        let _g = ENV_LOCK.lock().unwrap();
        for bad in ["0", "-5", "nan", "abc"] {
            std::env::set_var("CODEWORLD_MAX_SPEED", bad);
            let c = Config::load();
            std::env::remove_var("CODEWORLD_MAX_SPEED");
            assert_eq!(c.max_speed, 25.0, "非法值 {bad} 应回退默认世界极速");
        }
    }

    // === D-016 地形预设与种子（CODEWORLD_TERRAIN / CODEWORLD_SEED）===

    #[test]
    fn default_terrain_preset_is_flat_and_seed_zero() {
        // 创建：地形默认平地 + 种子 0（D-016；快照恢复/旧部署零回归）
        let c = Config::default();
        assert_eq!(c.terrain_preset, "flat", "地形预设默认应为 flat");
        assert_eq!(c.terrain_seed, 0, "地形种子默认应为 0");
    }

    #[test]
    fn load_terrain_env_overrides_default() {
        // 修改：CODEWORLD_TERRAIN / CODEWORLD_SEED 覆盖默认
        //（preset 存原文，非法回退口径在装配层 TerrainPreset::parse；seed 数值容错）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_TERRAIN", "hills");
        std::env::set_var("CODEWORLD_SEED", "12345");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_TERRAIN");
        std::env::remove_var("CODEWORLD_SEED");
        assert_eq!(c.terrain_preset, "hills", "环境变量应覆盖地形预设");
        assert_eq!(c.terrain_seed, 12345, "环境变量应覆盖地形种子");
    }

    #[test]
    fn load_terrain_seed_invalid_falls_back_to_zero() {
        // 边界：CODEWORLD_SEED 非法（非整数）-> 回退默认 0（容错语义同其他数值项）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_SEED", "not-a-number");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_SEED");
        assert_eq!(c.terrain_seed, 0, "非法种子应回退默认 0");
    }

    #[test]
    fn load_terrain_preset_stores_raw_string_and_parse_falls_back_flat() {
        // 边界：CODEWORLD_TERRAIN 非法值 -> config 按字符串类语义存原文
        //（"有值即覆盖"），非法回退平地的口径在装配层 TerrainPreset::parse 执行
        //（terrain.rs preset_parse_aliases 已锚定）；此处锚定 config/装配两端分工
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_TERRAIN", "moon");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_TERRAIN");
        assert_eq!(c.terrain_preset, "moon", "config 应存原文（回退在装配层）");
        assert_eq!(
            codeworld_core::terrain::TerrainPreset::parse(&c.terrain_preset),
            codeworld_core::terrain::TerrainPreset::Flat,
            "非法值装配层 parse 应回退平地"
        );
    }

    // === 单沙箱资产配额（#051 皮入沙箱，CODEWORLD_ASSET_QUOTA）===

    #[test]
    fn default_asset_quota_matches_arena_constant() {
        // 创建：默认 1MB 与 arena 常数同源（game-menu 1.7 总账锚，⚠️ 待拍板）
        let c = Config::default();
        assert_eq!(
            c.asset_quota,
            codeworld_arena::assets::SANDBOX_ASSET_QUOTA_BYTES,
            "资产配额默认应与 arena 常数同源"
        );
    }

    #[test]
    fn load_asset_quota_env_overrides_default() {
        // 修改：CODEWORLD_ASSET_QUOTA 覆盖默认（临时值可调）
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CODEWORLD_ASSET_QUOTA", "4096");
        let c = Config::load();
        std::env::remove_var("CODEWORLD_ASSET_QUOTA");
        assert_eq!(c.asset_quota, 4096, "环境变量应覆盖资产配额");
    }

    #[test]
    fn load_asset_quota_invalid_or_zero_falls_back_to_default() {
        // 边界：非法值/0 -> 回退默认 1MB（0 会拒绝一切换血，无"禁皮"语义；
        // 容错语义同其他数值项）
        let _g = ENV_LOCK.lock().unwrap();
        for bad in ["0", "not-a-number"] {
            std::env::set_var("CODEWORLD_ASSET_QUOTA", bad);
            let c = Config::load();
            std::env::remove_var("CODEWORLD_ASSET_QUOTA");
            assert_eq!(
                c.asset_quota,
                codeworld_arena::assets::SANDBOX_ASSET_QUOTA_BYTES,
                "非法值 {bad} 应回退默认资产配额"
            );
        }
    }
}
