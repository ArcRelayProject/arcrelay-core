pub mod bounded_command;
pub mod clipboard;
mod clipboard_ocr;
mod clipboard_store;
pub mod device;
pub mod input_control;
pub mod media_control;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod process;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod system_monitor;
pub mod window_manager;

#[cfg(feature = "test-support")]
pub mod clipboard_test_support;
