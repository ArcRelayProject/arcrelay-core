use arcrelay_core::debug;
use arcrelay_core::ArcRelayService;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    println!("ArcRelay Core Debug CLI");
    println!("==================\n");

    let service = match compose_native_service(None) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to initialize ArcRelayService: {e}");
            std::process::exit(1);
        }
    };

    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("all");

    match cmd {
        "system" => debug::debug_system_snapshot(&service).await,
        "process" | "proc" => {
            let limit: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);
            debug::debug_process_list(
                &service,
                arcrelay_core::domain::process::ProcessSortBy::Cpu,
                limit,
            )
            .await;
        }
        "media" => debug::debug_media_info(&service).await,
        "clipboard" | "clip" => debug::debug_clipboard(&service).await,
        "device" | "dev" => debug::debug_device_info(&service).await,
        "all" => debug::debug_all(&service).await,
        _ => {
            println!("Usage: debug-cli [command]");
            println!("Commands: all, system, process [limit], media, clipboard, device");
        }
    }
}

fn compose_native_service(
    database_path: Option<std::path::PathBuf>,
) -> arcrelay_core::Result<ArcRelayService> {
    use arcrelay_core::application::clipboard_service::ClipboardApplicationService;
    use arcrelay_core::domain::{
        clipboard::ClipboardRepository, input_control::InputControlRepository,
        window_manager::WindowManagerRepository,
    };
    use arcrelay_core::infrastructure::{
        clipboard::NativeClipboard, device::NativeDeviceRepo, input_control::NativeInputControl,
        media_control::NativeMediaControl, process::SysInfoProcessRepo,
        system_monitor::SysInfoMonitor, window_manager::NativeWindowManager,
    };
    use std::sync::Arc;
    let input_control: Arc<dyn InputControlRepository> = Arc::new(NativeInputControl::new());
    let window_manager: Arc<dyn WindowManagerRepository> = Arc::new(NativeWindowManager::new());
    let clipboard_repository: Arc<dyn ClipboardRepository> = Arc::new(NativeClipboard::new(
        database_path,
        window_manager.clone(),
        "debug-cli".into(),
        "ArcRelay Debug CLI".into(),
    )?);
    Ok(ArcRelayService::compose(
        Arc::new(SysInfoMonitor::new()),
        Arc::new(SysInfoProcessRepo::new()),
        Arc::new(NativeMediaControl::new()),
        Arc::new(ClipboardApplicationService::new(
            clipboard_repository,
            input_control.clone(),
        )),
        Arc::new(NativeDeviceRepo::new()),
        window_manager,
        input_control,
    ))
}
