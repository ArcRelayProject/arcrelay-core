use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackInfo {
    /// Track title
    pub title: String,
    /// Artist name
    pub artist: String,
    /// Application source (e.g. "Apple Music", "Spotify")
    pub source_app: String,
    /// Current position in seconds
    pub position_secs: f64,
    /// Total duration in seconds
    pub duration_secs: f64,
    /// Whether currently playing
    pub is_playing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeInfo {
    /// System master volume 0-100
    pub system_volume: u8,
    /// Whether system is muted
    pub is_muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppVolume {
    /// Application name
    pub app_name: String,
    /// Volume 0-100
    pub volume: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PlaybackAction {
    Play,
    Pause,
    Next,
    Previous,
    SeekForward,
    SeekBackward,
}

#[async_trait::async_trait]
pub trait MediaControlRepository: Send + Sync {
    /// Subscribe to platform media/audio change notifications. Backends that
    /// cannot provide reliable notifications return `None` and are sampled.
    fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
        None
    }

    /// Get current playback info (if any media is playing)
    async fn playback_info(&self) -> crate::error::Result<Option<PlaybackInfo>>;

    /// Execute a playback action
    async fn playback_action(&self, action: PlaybackAction) -> crate::error::Result<()>;

    /// Get system volume info
    async fn volume_info(&self) -> crate::error::Result<VolumeInfo>;

    /// Set system volume (0-100)
    async fn set_system_volume(&self, volume: u8) -> crate::error::Result<()>;

    /// Mute or unmute the system output without discarding the configured level.
    async fn set_system_muted(&self, muted: bool) -> crate::error::Result<()>;

    /// Get per-app volumes
    async fn app_volumes(&self) -> crate::error::Result<Vec<AppVolume>>;

    /// Set volume for a specific app
    async fn set_app_volume(&self, app_name: &str, volume: u8) -> crate::error::Result<()>;

    /// Get microphone enabled state
    async fn is_microphone_active(&self) -> crate::error::Result<bool>;

    /// Toggle microphone on/off
    async fn set_microphone_active(&self, active: bool) -> crate::error::Result<()>;

    /// Get Do Not Disturb state
    async fn is_dnd_active(&self) -> crate::error::Result<bool>;

    /// Toggle Do Not Disturb
    async fn set_dnd_active(&self, active: bool) -> crate::error::Result<()>;
}
