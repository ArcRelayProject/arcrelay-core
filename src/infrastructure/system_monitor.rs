use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use sysinfo::{Components, Disks, Networks, System};

use crate::domain::system_monitor::*;
use crate::error::Result;

pub struct SysInfoMonitor {
    state: Arc<Mutex<MonitorState>>,
    /// GPU detection is lazy because `system_profiler`/PowerShell can be slow.
    gpu_names: Arc<OnceLock<Vec<String>>>,
}

struct MonitorState {
    sys: System,
    networks: Networks,
    components: Components,
    disks: Disks,
    last_network_refresh: Instant,
    slow_sample: Option<SlowSample>,
    last_slow_refresh: Option<Instant>,
}

#[derive(Clone)]
struct SlowSample {
    temperature_celsius: Option<f32>,
    gpus: Vec<GpuInfo>,
    disks: Vec<DiskInfo>,
}

const SLOW_SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

impl SysInfoMonitor {
    pub fn new() -> Self {
        let sys = System::new();
        Self {
            state: Arc::new(Mutex::new(MonitorState {
                sys,
                networks: Networks::new(),
                // Refresh lazily from `collect_snapshot_sync`, which runs on a
                // blocking worker. On Windows the initial component refresh
                // initializes COM as MTA, so doing it here would change the UI
                // thread's apartment before Tauri creates its WebView window.
                components: Components::new(),
                disks: Disks::new(),
                last_network_refresh: Instant::now(),
                slow_sample: None,
                last_slow_refresh: None,
            })),
            gpu_names: Arc::new(OnceLock::new()),
        }
    }

    async fn collect_snapshot(&self) -> Result<SystemSnapshot> {
        let state = Arc::clone(&self.state);
        let gpu_names = Arc::clone(&self.gpu_names);
        tokio::task::spawn_blocking(move || collect_snapshot_sync(&state, &gpu_names))
            .await
            .map_err(|error| {
                crate::error::Error::Other(format!("system sample task failed: {error}"))
            })?
    }
}

fn collect_snapshot_sync(
    state: &Mutex<MonitorState>,
    gpu_names: &OnceLock<Vec<String>>,
) -> Result<SystemSnapshot> {
    let gpu_names = gpu_names.get_or_init(detect_gpu_names);
    let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
    state.sys.refresh_cpu_all();
    state.sys.refresh_memory();
    state.networks.refresh(true);
    let network_elapsed = state
        .last_network_refresh
        .elapsed()
        .as_secs_f64()
        .max(0.001);
    state.last_network_refresh = Instant::now();
    let refresh_slow = state
        .last_slow_refresh
        .is_none_or(|last| last.elapsed() >= SLOW_SAMPLE_INTERVAL);
    if refresh_slow {
        state.components.refresh(true);
        state.disks.refresh(true);
    }

    let cpus = state.sys.cpus();
    let usage = if cpus.is_empty() {
        0.0
    } else {
        cpus.iter().map(|cpu| cpu.cpu_usage()).sum::<f32>() / cpus.len() as f32
    };
    let core_count = cpus.len() as u32;
    let model_name = cpus
        .first()
        .map(|cpu| cpu.brand().to_string())
        .unwrap_or_default();
    if refresh_slow {
        let cpu_temperature = state
            .components
            .iter()
            .find(|component| {
                let label = component.label().to_lowercase();
                label.contains("cpu") || label.contains("core")
            })
            .and_then(|component| component.temperature());
        let gpu_temps: Vec<(String, Option<f32>)> = state
            .components
            .iter()
            .filter(|component| component.label().to_lowercase().contains("gpu"))
            .map(|component| (component.label().to_string(), component.temperature()))
            .collect();
        let gpus = if gpu_names.is_empty() {
            gpu_temps
                .iter()
                .map(|(name, temperature)| GpuInfo {
                    name: name.clone(),
                    usage_percent: None,
                    temperature_celsius: *temperature,
                })
                .collect()
        } else {
            gpu_names
                .iter()
                .map(|name| {
                    let name_lower = name.to_lowercase();
                    let temperature = gpu_temps
                        .iter()
                        .find(|(label, _)| {
                            let label = label.to_lowercase();
                            label.contains(&name_lower) || name_lower.contains(&label)
                        })
                        .and_then(|(_, value)| *value)
                        .or_else(|| gpu_temps.first().and_then(|(_, value)| *value));
                    GpuInfo {
                        name: name.clone(),
                        usage_percent: None,
                        temperature_celsius: temperature,
                    }
                })
                .collect()
        };
        let disks = state
            .disks
            .iter()
            .map(|disk| {
                let total = disk.total_space();
                let used = total.saturating_sub(disk.available_space());
                DiskInfo {
                    name: disk.mount_point().to_string_lossy().to_string(),
                    used_bytes: used,
                    total_bytes: total,
                    usage_percent: if total > 0 {
                        used as f32 / total as f32 * 100.0
                    } else {
                        0.0
                    },
                }
            })
            .collect();
        state.slow_sample = Some(SlowSample {
            temperature_celsius: cpu_temperature,
            gpus,
            disks,
        });
        state.last_slow_refresh = Some(Instant::now());
    }
    let slow = state.slow_sample.clone().unwrap_or(SlowSample {
        temperature_celsius: None,
        gpus: Vec::new(),
        disks: Vec::new(),
    });
    let cpu = CpuInfo {
        usage_percent: usage,
        core_count,
        temperature_celsius: slow.temperature_celsius,
        model_name,
    };

    let total_memory = state.sys.total_memory();
    let used_memory = state.sys.used_memory();
    let memory = MemoryInfo {
        used_bytes: used_memory,
        total_bytes: total_memory,
        usage_percent: if total_memory > 0 {
            used_memory as f32 / total_memory as f32 * 100.0
        } else {
            0.0
        },
    };

    let mut total_rx = 0_u64;
    let mut total_tx = 0_u64;
    for network in state.networks.list().values() {
        total_rx = total_rx.saturating_add(network.received());
        total_tx = total_tx.saturating_add(network.transmitted());
    }
    let (download_bytes_per_sec, upload_bytes_per_sec) = if network_elapsed < 0.25 {
        (0, 0)
    } else {
        (
            (total_rx as f64 / network_elapsed) as u64,
            (total_tx as f64 / network_elapsed) as u64,
        )
    };

    Ok(SystemSnapshot {
        cpu,
        memory,
        gpus: slow.gpus,
        disks: slow.disks,
        network: NetworkStats {
            download_bytes_per_sec,
            upload_bytes_per_sec,
        },
    })
}

impl Default for SysInfoMonitor {
    fn default() -> Self {
        Self::new()
    }
}

/// Detect GPU names using platform-specific commands.
#[cfg(target_os = "macos")]
fn detect_gpu_names() -> Vec<String> {
    use std::process::Command;
    let mut command = Command::new("system_profiler");
    command.args(["SPDisplaysDataType", "-json"]);
    let output = match crate::infrastructure::bounded_command::output(
        command,
        std::time::Duration::from_secs(10),
    ) {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };
    let json_str = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let mut names = Vec::new();
    if let Some(displays) = parsed.get("SPDisplaysDataType").and_then(|d| d.as_array()) {
        for display in displays {
            if let Some(name) = display.get("sppci_model").and_then(|n| n.as_str()) {
                names.push(name.to_string());
            }
        }
    }
    names
}

#[cfg(target_os = "windows")]
fn detect_gpu_names() -> Vec<String> {
    use std::process::Command;
    let mut command = Command::new("powershell");
    command.args([
        "-NoProfile",
        "-Command",
        "Get-CimInstance -ClassName Win32_VideoController | Select-Object -ExpandProperty Name",
    ]);
    let output = match crate::infrastructure::bounded_command::output(
        command,
        std::time::Duration::from_secs(10),
    ) {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn detect_gpu_names() -> Vec<String> {
    vec![]
}

#[async_trait::async_trait]
impl SystemMonitorRepository for SysInfoMonitor {
    async fn snapshot(&self) -> Result<SystemSnapshot> {
        self.collect_snapshot().await
    }

    async fn cpu_info(&self) -> Result<CpuInfo> {
        Ok(self.collect_snapshot().await?.cpu)
    }

    async fn memory_info(&self) -> Result<MemoryInfo> {
        Ok(self.collect_snapshot().await?.memory)
    }

    async fn gpu_info(&self) -> Result<Vec<GpuInfo>> {
        Ok(self.collect_snapshot().await?.gpus)
    }

    async fn disk_info(&self) -> Result<Vec<DiskInfo>> {
        Ok(self.collect_snapshot().await?.disks)
    }

    async fn network_stats(&self) -> Result<NetworkStats> {
        Ok(self.collect_snapshot().await?.network)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_system_monitor_does_not_scan_or_require_runtime() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let monitor = SysInfoMonitor::new();
        let state = monitor.state.lock().unwrap();
        assert!(state.sys.cpus().is_empty());
        assert!(state.disks.list().is_empty());
        assert!(state.networks.list().is_empty());
        assert!(monitor.gpu_names.get().is_none());
    }
}
