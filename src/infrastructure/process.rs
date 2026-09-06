use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, RefreshKind, System};

use crate::domain::process::*;
use crate::error::{Error, Result};

struct ProcessSample {
    captured_at: Instant,
    processes: Vec<ProcessInfo>,
}

type SharedProcessCache = Arc<Mutex<Option<ProcessSample>>>;

pub struct SysInfoProcessRepo {
    sys: Arc<Mutex<System>>,
    refresh_gate: tokio::sync::Mutex<()>,
    cache: SharedProcessCache,
}

impl SysInfoProcessRepo {
    pub fn new() -> Self {
        Self {
            sys: Arc::new(Mutex::new(System::new_with_specifics(
                RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
            ))),
            refresh_gate: tokio::sync::Mutex::new(()),
            cache: Arc::new(Mutex::new(None)),
        }
    }

    fn sorted(mut processes: Vec<ProcessInfo>, sort_by: ProcessSortBy) -> Vec<ProcessInfo> {
        match sort_by {
            ProcessSortBy::Cpu => processes.sort_by(|left, right| {
                right
                    .cpu_percent
                    .partial_cmp(&left.cpu_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            ProcessSortBy::Memory => {
                processes.sort_by_key(|process| std::cmp::Reverse(process.memory_bytes))
            }
            ProcessSortBy::Name => {
                processes.sort_by_cached_key(|process| process.name.to_lowercase())
            }
        }
        processes
    }
}

impl Default for SysInfoProcessRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ProcessRepository for SysInfoProcessRepo {
    async fn list(&self, sort_by: ProcessSortBy) -> Result<Vec<ProcessInfo>> {
        const CACHE_TTL: Duration = Duration::from_secs(3);
        if let Some(sample) = self
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            if sample.captured_at.elapsed() < CACHE_TTL {
                return Ok(Self::sorted(sample.processes.clone(), sort_by));
            }
        }

        let _refresh_guard = self.refresh_gate.lock().await;
        if let Some(sample) = self
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            if sample.captured_at.elapsed() < CACHE_TTL {
                return Ok(Self::sorted(sample.processes.clone(), sort_by));
            }
        }

        let sys = Arc::clone(&self.sys);
        let cache = Arc::clone(&self.cache);
        let processes = tokio::task::spawn_blocking(move || {
            let mut sys = sys.lock().unwrap_or_else(|error| error.into_inner());
            sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing().with_cpu().with_memory(),
            );
            let num_cpus = std::thread::available_parallelism()
                .map(|count| count.get() as f32)
                .unwrap_or(1.0);
            sys.processes()
                .values()
                .map(|process| ProcessInfo {
                    pid: process.pid().as_u32(),
                    name: process.name().to_string_lossy().to_string(),
                    cpu_percent: process.cpu_usage() / num_cpus,
                    memory_bytes: process.memory(),
                })
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|error| Error::Other(format!("process sample task failed: {error}")))?;

        *cache.lock().unwrap_or_else(|error| error.into_inner()) = Some(ProcessSample {
            captured_at: Instant::now(),
            processes: processes.clone(),
        });
        Ok(Self::sorted(processes, sort_by))
    }

    async fn kill(&self, pid: u32) -> Result<()> {
        let sys = self.sys.lock().unwrap_or_else(|error| error.into_inner());
        let sysinfo_pid = sysinfo::Pid::from_u32(pid);
        if let Some(process) = sys.process(sysinfo_pid) {
            if process.kill() {
                Ok(())
            } else {
                Err(Error::Other(format!("failed to kill process {pid}")))
            }
        } else {
            Err(Error::NotFound(format!("process {pid}")))
        }
    }
}
