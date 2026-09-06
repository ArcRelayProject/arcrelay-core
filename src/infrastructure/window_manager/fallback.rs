use crate::domain::window_manager::*;
use crate::error::{Error, Result};
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct NativeWindowManager;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl NativeWindowManager {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[async_trait::async_trait]
impl WindowManagerRepository for NativeWindowManager {
    fn focused_app_name_now(&self) -> Result<String> {
        Ok(String::new())
    }

    async fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(vec![])
    }

    async fn list_windows_meta(&self) -> Result<Vec<WindowInfo>> {
        Ok(vec![])
    }

    async fn focused_app_name(&self) -> Result<String> {
        Ok(String::new())
    }

    async fn focus_window(&self, _window_id: u32) -> Result<()> {
        Err(Error::NotSupported(
            "Window focus not supported on this platform".into(),
        ))
    }

    async fn list_spaces(&self) -> Result<Vec<crate::domain::window_manager::SpaceInfo>> {
        Ok(vec![])
    }

    async fn switch_space(&self, _space_id: u64) -> Result<()> {
        Ok(())
    }

    async fn on_screen_window_ids(&self) -> Result<Vec<u32>> {
        Ok(vec![])
    }
}
