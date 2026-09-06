pub use arcrelay_input::{
    SystemGestureEvent, SystemGestureSequence, SYSTEM_GESTURE_FORMAT_VERSION,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub enum InputPermissionState {
    Granted,
    Denied,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollGesturePhase {
    Began,
    Ended,
    Cancelled,
    MomentumBegan,
    MomentumEnded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    PointerMove {
        delta_x: f32,
        delta_y: f32,
    },
    PointerButton {
        button: MouseButton,
        down: bool,
        click_count: u8,
    },
    Scroll {
        delta_x: f32,
        delta_y: f32,
        precise: bool,
    },
    ScrollGesture {
        phase: ScrollGesturePhase,
    },
    SystemGesture(SystemGestureEvent),
    Key {
        hid_usage: u16,
        down: bool,
        repeat: bool,
    },
    TextCommit(String),
    ReleaseAll,
}

#[async_trait::async_trait]
pub trait InputControlRepository: Send + Sync {
    fn permission_state(&self) -> InputPermissionState;

    /// Advertised only when this receiver can inject bounded v1 horizontal and
    /// vertical swipes (DockSwipe on macOS, Precision Touchpad on Windows).
    fn supports_system_gestures(&self) -> bool {
        false
    }

    fn open_permission_settings(&self) -> crate::error::Result<()>;

    /// Validate an entire reliable-input batch before any native event is
    /// injected. This prevents deterministic failures, such as an unsupported
    /// platform key mapping, from applying only a prefix of the batch.
    fn validate_events(&self, events: &[InputEvent]) -> crate::error::Result<()>;

    async fn apply_events(&self, events: &[InputEvent]) -> crate::error::Result<()>;

    /// Emit the platform-native clipboard paste shortcut. Clipboard paste has
    /// stricter timing requirements than the general remote-input stream, so
    /// implementations keep the modifier/key sequence together and may choose
    /// a text-specific shortcut when the platform benefits from it.
    async fn paste_clipboard(&self, is_text: bool) -> crate::error::Result<()>;

    async fn release_all(&self) -> crate::error::Result<()>;
}
