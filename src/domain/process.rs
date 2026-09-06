use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    /// OS process id
    pub pid: u32,
    /// Process name
    pub name: String,
    /// CPU usage percentage
    pub cpu_percent: f32,
    /// Memory usage in bytes
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ProcessSortBy {
    Cpu,
    Memory,
    Name,
}

#[async_trait::async_trait]
pub trait ProcessRepository: Send + Sync {
    /// List processes sorted by the given criterion
    async fn list(&self, sort_by: ProcessSortBy) -> crate::error::Result<Vec<ProcessInfo>>;

    /// Kill a process by PID
    async fn kill(&self, pid: u32) -> crate::error::Result<()>;
}
