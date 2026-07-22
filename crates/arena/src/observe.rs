//! 被动观测：ProbeMeta/ProbeResult + collect_meta
//!
//! 架构文档 4.3 节：框架收集被动观测数据（B 无法伪造）：gas 消耗、内存读写、API 调用。

use crate::sandbox::SandboxData;
use serde::{Deserialize, Serialize};

/// 被动观测数据（架构 4.3，B 无法伪造）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeMeta {
    /// gas（fuel）消耗
    pub gas_consumed: u64,
    /// 内存读次数
    pub mem_read_count: u64,
    /// 内存写次数
    pub mem_write_count: u64,
    /// API 调用列表
    pub api_calls: Vec<String>,
}

/// 探测结果（架构 4.3）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    /// 目标 on_probe 返回的数据（可造假）
    pub response: Vec<u8>,
    /// 被动观测数据（不可伪造）
    pub meta: ProbeMeta,
}

/// 从沙箱数据提取被动观测数据
pub fn collect_meta(sandbox_data: &SandboxData) -> ProbeMeta {
    let meta = ProbeMeta {
        gas_consumed: sandbox_data.fuel_consumed,
        mem_read_count: sandbox_data.mem_read_count,
        mem_write_count: sandbox_data.mem_write_count,
        api_calls: sandbox_data.api_calls.clone(),
    };
    tracing::debug!(
        gas = meta.gas_consumed,
        api_calls = meta.api_calls.len(),
        "收集被动观测"
    );
    meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxData;

    #[test]
    fn probe_meta_has_fields() {
        let meta = ProbeMeta {
            gas_consumed: 10,
            mem_read_count: 5,
            mem_write_count: 3,
            api_calls: vec!["log".to_string()],
        };
        assert_eq!(meta.gas_consumed, 10);
        assert_eq!(meta.mem_read_count, 5);
        assert_eq!(meta.mem_write_count, 3);
        assert_eq!(meta.api_calls, vec!["log".to_string()]);
    }

    #[test]
    fn probe_result_contains_response_and_meta() {
        let result = ProbeResult {
            response: vec![1, 2, 3],
            meta: ProbeMeta {
                gas_consumed: 0,
                mem_read_count: 0,
                mem_write_count: 0,
                api_calls: vec![],
            },
        };
        assert_eq!(result.response, vec![1, 2, 3]);
    }

    #[test]
    fn collect_meta_extracts_from_sandbox_data() {
        let mut data = SandboxData::new();
        data.fuel_consumed = 42;
        data.mem_read_count = 5;
        data.api_calls.push("log".to_string());
        let meta = collect_meta(&data);
        assert_eq!(meta.gas_consumed, 42);
        assert_eq!(meta.mem_read_count, 5);
        assert_eq!(meta.api_calls, vec!["log".to_string()]);
    }
}
