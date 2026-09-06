/// Debug interface for testing arcrelay-core functionality interactively.
/// These functions are meant for development / CLI testing, not for production FFI.
use crate::application::service::ArcRelayService;
use crate::domain::process::ProcessSortBy;

pub async fn debug_system_snapshot(service: &ArcRelayService) {
    match service.system_monitor.snapshot().await {
        Ok(snap) => {
            println!("=== System Snapshot ===");
            println!(
                "CPU: {:.1}% | {} cores | {} | temp: {}",
                snap.cpu.usage_percent,
                snap.cpu.core_count,
                snap.cpu.model_name,
                snap.cpu
                    .temperature_celsius
                    .map(|t| format!("{t:.0}°C"))
                    .unwrap_or_else(|| "N/A".into())
            );
            println!(
                "Memory: {:.1} GB / {:.1} GB ({:.0}%)",
                snap.memory.used_bytes as f64 / 1_073_741_824.0,
                snap.memory.total_bytes as f64 / 1_073_741_824.0,
                snap.memory.usage_percent,
            );
            for gpu in &snap.gpus {
                println!(
                    "GPU: {} | usage: {} | temp: {}",
                    gpu.name,
                    gpu.usage_percent
                        .map(|u| format!("{u:.0}%"))
                        .unwrap_or("N/A".into()),
                    gpu.temperature_celsius
                        .map(|t| format!("{t:.0}°C"))
                        .unwrap_or("N/A".into()),
                );
            }
            for disk in &snap.disks {
                println!(
                    "Disk [{}]: {:.1} GB / {:.1} GB ({:.0}%)",
                    disk.name,
                    disk.used_bytes as f64 / 1_073_741_824.0,
                    disk.total_bytes as f64 / 1_073_741_824.0,
                    disk.usage_percent,
                );
            }
            println!(
                "Network: ↓ {:.1} KB/s  ↑ {:.1} KB/s",
                snap.network.download_bytes_per_sec as f64 / 1024.0,
                snap.network.upload_bytes_per_sec as f64 / 1024.0,
            );
        }
        Err(e) => eprintln!("Error: {e}"),
    }
}

pub async fn debug_process_list(service: &ArcRelayService, sort_by: ProcessSortBy, limit: usize) {
    match service.process.list(sort_by).await {
        Ok(procs) => {
            println!("=== Processes (top {limit}, sort: {sort_by:?}) ===");
            for p in procs.iter().take(limit) {
                println!(
                    "  [{}] {} — CPU {:.1}% | MEM {:.1} MB",
                    p.pid,
                    p.name,
                    p.cpu_percent,
                    p.memory_bytes as f64 / 1_048_576.0,
                );
            }
        }
        Err(e) => eprintln!("Error: {e}"),
    }
}

pub async fn debug_media_info(service: &ArcRelayService) {
    println!("=== Media Control ===");
    match service.media_control.playback_info().await {
        Ok(Some(info)) => {
            println!(
                "Now {}: {} — {} [{}]",
                if info.is_playing { "Playing" } else { "Paused" },
                info.title,
                info.artist,
                info.source_app,
            );
            println!(
                "  Position: {:.0}s / {:.0}s",
                info.position_secs, info.duration_secs,
            );
        }
        Ok(None) => println!("  No media playing"),
        Err(e) => eprintln!("  Playback info error: {e}"),
    }
    match service.media_control.volume_info().await {
        Ok(vol) => {
            println!(
                "  Volume: {}% {}",
                vol.system_volume,
                if vol.is_muted { "(muted)" } else { "" }
            );
        }
        Err(e) => eprintln!("  Volume error: {e}"),
    }
    match service.media_control.is_microphone_active().await {
        Ok(active) => println!("  Microphone: {}", if active { "ON" } else { "OFF" }),
        Err(e) => eprintln!("  Mic error: {e}"),
    }
    match service.media_control.is_dnd_active().await {
        Ok(active) => println!("  DND: {}", if active { "ON" } else { "OFF" }),
        Err(e) => eprintln!("  DND error: {e}"),
    }
}

pub async fn debug_clipboard(service: &ArcRelayService) {
    println!("=== Clipboard ===");
    match service.clipboard.current_summary().await {
        Ok(summary) => println!("  Current summary: {summary:?}"),
        Err(e) => eprintln!("  Error reading clipboard: {e}"),
    }
    match service
        .clipboard
        .history(crate::domain::clipboard::ClipboardQuery::recent(5))
        .await
    {
        Ok(page) => {
            if page.entries.is_empty() {
                println!("  History: (empty)");
            } else {
                println!("  History ({} entries):", page.entries.len());
                for entry in &page.entries {
                    println!(
                        "    [{}] {} — {}",
                        entry.id,
                        entry.captured_at.format("%H:%M:%S"),
                        entry.preview
                    );
                }
            }
        }
        Err(e) => eprintln!("  History error: {e}"),
    }
}

pub async fn debug_device_info(service: &ArcRelayService) {
    println!("=== Device ===");
    match service.device.local_info().await {
        Ok(info) => {
            println!("  Local: {} ({})", info.name, info.ip);
            println!("  Version: {} | Port: {}", info.app_version, info.port);
        }
        Err(e) => eprintln!("  Error: {e}"),
    }
    match service.device.get_config().await {
        Ok(cfg) => {
            println!(
                "  Config: port={} encrypted={} auto_reconnect={}",
                cfg.port, cfg.encrypted, cfg.auto_reconnect
            );
        }
        Err(e) => eprintln!("  Config error: {e}"),
    }
}

/// Run all debug functions - a quick way to verify everything works
pub async fn debug_all(service: &ArcRelayService) {
    debug_system_snapshot(service).await;
    println!();
    debug_process_list(service, ProcessSortBy::Cpu, 10).await;
    println!();
    debug_media_info(service).await;
    println!();
    debug_clipboard(service).await;
    println!();
    debug_device_info(service).await;
}
