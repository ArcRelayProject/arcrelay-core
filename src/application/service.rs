use std::sync::Arc;

use crate::application::clipboard_service::ClipboardApplicationService;
use crate::domain::device::DeviceRepository;
use crate::domain::input_control::InputControlRepository;
use crate::domain::media_control::MediaControlRepository;
use crate::domain::process::ProcessRepository;
use crate::domain::system_monitor::SystemMonitorRepository;
use crate::domain::window_manager::WindowManagerRepository;

/// The main application service that aggregates all domain capabilities.
/// This is the entry point for consumers of arcrelay-core.
pub struct ArcRelayService {
    pub system_monitor: Arc<dyn SystemMonitorRepository>,
    pub process: Arc<dyn ProcessRepository>,
    pub media_control: Arc<dyn MediaControlRepository>,
    pub clipboard: Arc<ClipboardApplicationService>,
    pub device: Arc<dyn DeviceRepository>,
    pub window_manager: Arc<dyn WindowManagerRepository>,
    pub input_control: Arc<dyn InputControlRepository>,
}

impl ArcRelayService {
    #[allow(clippy::too_many_arguments)]
    pub fn compose(
        system_monitor: Arc<dyn SystemMonitorRepository>,
        process: Arc<dyn ProcessRepository>,
        media_control: Arc<dyn MediaControlRepository>,
        clipboard: Arc<ClipboardApplicationService>,
        device: Arc<dyn DeviceRepository>,
        window_manager: Arc<dyn WindowManagerRepository>,
        input_control: Arc<dyn InputControlRepository>,
    ) -> Self {
        Self {
            system_monitor,
            process,
            media_control,
            clipboard,
            device,
            window_manager,
            input_control,
        }
    }
}
