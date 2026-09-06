use crate::domain::media_control::*;
use crate::error::{Error, Result};
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct NativeMediaControl;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl NativeMediaControl {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[async_trait::async_trait]
impl MediaControlRepository for NativeMediaControl {
    async fn playback_info(&self) -> Result<Option<PlaybackInfo>> {
        Ok(None)
    }
    async fn playback_action(&self, _: PlaybackAction) -> Result<()> {
        Err(Error::NotSupported("Not supported".into()))
    }
    async fn volume_info(&self) -> Result<VolumeInfo> {
        Ok(VolumeInfo {
            system_volume: 50,
            is_muted: false,
        })
    }
    async fn set_system_volume(&self, _: u8) -> Result<()> {
        Err(Error::NotSupported("Not supported".into()))
    }
    async fn set_system_muted(&self, _: bool) -> Result<()> {
        Err(Error::NotSupported("Not supported".into()))
    }
    async fn app_volumes(&self) -> Result<Vec<AppVolume>> {
        Ok(vec![])
    }
    async fn set_app_volume(&self, _: &str, _: u8) -> Result<()> {
        Err(Error::NotSupported("Not supported".into()))
    }
    async fn is_microphone_active(&self) -> Result<bool> {
        Ok(false)
    }
    async fn set_microphone_active(&self, _: bool) -> Result<()> {
        Err(Error::NotSupported("Not supported".into()))
    }
    async fn is_dnd_active(&self) -> Result<bool> {
        Ok(false)
    }
    async fn set_dnd_active(&self, _: bool) -> Result<()> {
        Err(Error::NotSupported("Not supported".into()))
    }
}
