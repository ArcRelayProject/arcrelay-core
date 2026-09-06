#[cfg(any(target_os = "macos", target_os = "windows"))]
fn encode_rgba_to_png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let Ok(mut writer) = encoder.write_header() else {
            return Vec::new();
        };
        if writer.write_image_data(rgba).is_err() {
            return Vec::new();
        }
    }
    output
}

#[cfg(target_os = "macos")]
#[path = "window_manager/macos.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "window_manager/windows.rs"]
mod platform;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[path = "window_manager/fallback.rs"]
mod platform;

pub use platform::NativeWindowManager;
#[cfg(any(target_os = "windows", test))]
#[path = "window_manager/windows_desktops.rs"]
pub(crate) mod windows_desktops;
