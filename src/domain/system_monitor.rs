use serde::{Deserialize, Serialize};

// ── Value Objects ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuInfo {
    /// Usage percentage 0-100
    pub usage_percent: f32,
    /// Number of cores (logical)
    pub core_count: u32,
    /// Temperature in Celsius (if available)
    pub temperature_celsius: Option<f32>,
    /// CPU model name
    pub model_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryInfo {
    /// Used memory in bytes
    pub used_bytes: u64,
    /// Total memory in bytes
    pub total_bytes: u64,
    /// Usage percentage 0-100
    pub usage_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    /// GPU name
    pub name: String,
    /// Usage percentage 0-100 (if available)
    pub usage_percent: Option<f32>,
    /// Temperature in Celsius (if available)
    pub temperature_celsius: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskInfo {
    /// Disk name / mount point
    pub name: String,
    /// Used bytes
    pub used_bytes: u64,
    /// Total bytes
    pub total_bytes: u64,
    /// Usage percentage 0-100
    pub usage_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStats {
    /// Download speed in bytes/sec
    pub download_bytes_per_sec: u64,
    /// Upload speed in bytes/sec
    pub upload_bytes_per_sec: u64,
}

/// Aggregate root: full system snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub gpus: Vec<GpuInfo>,
    pub disks: Vec<DiskInfo>,
    pub network: NetworkStats,
}

// ── Repository trait ──

#[async_trait::async_trait]
pub trait SystemMonitorRepository: Send + Sync {
    /// Collect a full system snapshot
    async fn snapshot(&self) -> crate::error::Result<SystemSnapshot>;

    /// Collect CPU info only
    async fn cpu_info(&self) -> crate::error::Result<CpuInfo>;

    /// Collect memory info only
    async fn memory_info(&self) -> crate::error::Result<MemoryInfo>;

    /// Collect GPU info
    async fn gpu_info(&self) -> crate::error::Result<Vec<GpuInfo>>;

    /// Collect disk info
    async fn disk_info(&self) -> crate::error::Result<Vec<DiskInfo>>;

    /// Collect network stats
    async fn network_stats(&self) -> crate::error::Result<NetworkStats>;
}
