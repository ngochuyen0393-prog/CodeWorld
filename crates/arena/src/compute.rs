//! 算力调度：ComputeTask/TaskType/ComputeResult/ComputeExecutor/LocalExecutor
//!
//! 架构文档 4.4 节：MVP 无外接算力，用基础算力执行（LocalExecutor 空实现）。

use serde::{Deserialize, Serialize};

/// 算力任务类型（架构 4.4）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskType {
    /// 哈希破解
    HashCrack,
    /// 暴力破解
    BruteForce,
    /// 模式分析
    PatternAnalysis,
    /// 玩家自定义
    Custom,
}

/// 算力任务（架构 4.4）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeTask {
    /// 任务类型
    pub task_type: TaskType,
    /// 输入数据
    pub input: Vec<u8>,
}

/// 算力结果（架构 4.4）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeResult {
    /// 输出数据
    pub output: Vec<u8>,
    /// 是否已验证
    pub verified: bool,
}

/// 算力执行器 trait（抽象外接算力接入）
///
/// MVP 提供 [`LocalExecutor`]（本地空实现）；外接算力接入后替换为真实执行器。
pub trait ComputeExecutor {
    /// 执行算力任务
    fn execute(&self, task: &ComputeTask) -> ComputeResult;
}

/// 本地执行器（MVP 默认）
///
/// 直接返回空结果，不真实计算（架构 4.4：不接外接算力时用基础算力慢跑）。
pub struct LocalExecutor;

impl ComputeExecutor for LocalExecutor {
    fn execute(&self, _task: &ComputeTask) -> ComputeResult {
        tracing::debug!(task_type = ?_task.task_type, "LocalExecutor 执行（MVP 空结果）");
        ComputeResult {
            output: Vec::new(),
            verified: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_task_stores_fields() {
        let task = ComputeTask {
            task_type: TaskType::HashCrack,
            input: vec![1, 2, 3],
        };
        assert_eq!(task.task_type, TaskType::HashCrack);
        assert_eq!(task.input, vec![1, 2, 3]);
    }

    #[test]
    fn task_type_variants_distinct() {
        assert_ne!(TaskType::HashCrack, TaskType::BruteForce);
        assert_ne!(TaskType::PatternAnalysis, TaskType::Custom);
    }

    #[test]
    fn local_executor_returns_empty_result() {
        let exec = LocalExecutor;
        let task = ComputeTask {
            task_type: TaskType::HashCrack,
            input: vec![],
        };
        let result = exec.execute(&task);
        assert!(result.output.is_empty());
        assert!(!result.verified);
    }
}
