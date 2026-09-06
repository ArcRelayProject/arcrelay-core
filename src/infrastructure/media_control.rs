#[cfg(target_os = "macos")]
#[path = "media_control/macos.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "media_control/windows.rs"]
mod platform;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[path = "media_control/fallback.rs"]
mod platform;

pub use platform::NativeMediaControl;
