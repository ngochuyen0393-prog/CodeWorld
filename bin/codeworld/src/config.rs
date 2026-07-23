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
        // 字符串类：有值即覆盖（空字符串也覆盖，符合"显式设置"语义）
        if let Ok(val) = std::env::var("CODEWORLD_DB_PATH") {
            cfg.db_path = val;
        }
        if let Ok(val) = std::env::var("CODEWORLD_LOG_LEVEL") {
            cfg.log_level = val;
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

/// 读取并解析 f64 环境变量，缺失或非法返回 None
fn env_f64(key: &str) -> Option<f64> {
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
}
