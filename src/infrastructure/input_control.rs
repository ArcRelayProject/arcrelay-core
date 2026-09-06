use crate::domain::input_control::{
    InputControlRepository, InputEvent, InputPermissionState, MouseButton, ScrollGesturePhase,
};
use crate::error::{Error, Result};

#[cfg(any(target_os = "windows", test))]
pub mod windows_system_gesture;

#[cfg(target_os = "macos")]
pub mod macos_system_gesture;

#[cfg(target_os = "macos")]
#[path = "input_control/macos.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "input_control/windows.rs"]
mod platform;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[path = "input_control/fallback.rs"]
mod platform;

pub use platform::NativeInputControl;

impl Default for NativeInputControl {
    fn default() -> Self {
        Self::new()
    }
}
