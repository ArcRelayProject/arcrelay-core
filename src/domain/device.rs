use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Device display name
    pub name: String,
    /// IP address
    pub ip: String,
    /// Device type
    pub device_type: DeviceType,
    /// Whether currently connected
    pub is_connected: bool,
    /// Latency in ms (if connected)
    pub latency_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeviceType {
    MacBook,
    WindowsPC,
    LinuxServer,
    IPad,
    IPhone,
    Android,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalDeviceInfo {
    /// Device name
    pub name: String,
    /// IP address
    pub ip: String,
    /// App version
    pub app_version: String,
    /// Connection port
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// Listening port
    pub port: u16,
    /// Whether to use encrypted transport
    pub encrypted: bool,
    /// Whether to auto-reconnect
    pub auto_reconnect: bool,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            port: 8765,
            encrypted: true,
            auto_reconnect: true,
        }
    }
}

#[async_trait::async_trait]
pub trait DeviceRepository: Send + Sync {
    /// Get local device info
    async fn local_info(&self) -> crate::error::Result<LocalDeviceInfo>;

    /// Scan for devices on the local network
    async fn scan_devices(&self) -> crate::error::Result<Vec<DeviceInfo>>;

    /// Get current connection config
    async fn get_config(&self) -> crate::error::Result<ConnectionConfig>;

    /// Update connection config
    async fn set_config(&self, config: ConnectionConfig) -> crate::error::Result<()>;
}
