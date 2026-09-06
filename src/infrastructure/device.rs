use std::net::UdpSocket;
use std::sync::Mutex;

use crate::domain::device::*;
use crate::error::{Error, Result};

pub struct NativeDeviceRepo {
    config: Mutex<ConnectionConfig>,
}

impl NativeDeviceRepo {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(ConnectionConfig::default()),
        }
    }

    /// Get local IP address by connecting a UDP socket (doesn't actually send data)
    fn local_ip() -> Result<String> {
        let socket =
            UdpSocket::bind("0.0.0.0:0").map_err(|e| Error::Device(format!("Bind socket: {e}")))?;
        socket
            .connect("8.8.8.8:80")
            .map_err(|e| Error::Device(format!("Connect: {e}")))?;
        let addr = socket
            .local_addr()
            .map_err(|e| Error::Device(format!("Local addr: {e}")))?;
        Ok(addr.ip().to_string())
    }
}

impl Default for NativeDeviceRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl DeviceRepository for NativeDeviceRepo {
    async fn local_info(&self) -> Result<LocalDeviceInfo> {
        let ip = Self::local_ip().unwrap_or_else(|_| "unknown".to_string());
        let name = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "Unknown".to_string());
        let config = self.config.lock().unwrap();
        Ok(LocalDeviceInfo {
            name,
            ip,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            port: config.port,
        })
    }

    async fn scan_devices(&self) -> Result<Vec<DeviceInfo>> {
        // Network scanning: send UDP broadcast and collect responses.
        // This is a placeholder; real implementation will use the network server module.
        Ok(vec![])
    }

    async fn get_config(&self) -> Result<ConnectionConfig> {
        let config = self.config.lock().unwrap();
        Ok(config.clone())
    }

    async fn set_config(&self, config: ConnectionConfig) -> Result<()> {
        let mut current = self.config.lock().unwrap();
        *current = config;
        Ok(())
    }
}
