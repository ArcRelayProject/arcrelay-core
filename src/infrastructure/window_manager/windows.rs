use crate::domain::window_manager::*;
use crate::error::{Error, Result};
#[cfg(target_os = "windows")]
pub struct NativeWindowManager {
    change_tx: tokio::sync::broadcast::Sender<()>,
    notifications_available: bool,
}

#[cfg(target_os = "windows")]
impl NativeWindowManager {
    pub fn new() -> Self {
        let (change_tx, _) = tokio::sync::broadcast::channel(64);
        let notifications_available = win_impl::start_window_event_watcher(change_tx.clone());
        Self {
            change_tx,
            notifications_available,
        }
    }
}

#[cfg(target_os = "windows")]
mod win_impl {
    use super::*;
    use crate::infrastructure::window_manager::encode_rgba_to_png;
    use crate::infrastructure::window_manager::windows_desktops::{self, DesktopSnapshot};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    #[allow(unused_imports)]
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    static WINDOW_EVENT_SENDERS: std::sync::OnceLock<
        std::sync::Mutex<Vec<tokio::sync::broadcast::Sender<()>>>,
    > = std::sync::OnceLock::new();
    static WINDOW_EVENT_WATCHER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    pub(super) fn start_window_event_watcher(tx: tokio::sync::broadcast::Sender<()>) -> bool {
        WINDOW_EVENT_SENDERS
            .get_or_init(|| std::sync::Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(tx);
        *WINDOW_EVENT_WATCHER.get_or_init(|| {
            std::thread::Builder::new()
                .name("arcrelay-window-events".into())
                .spawn(|| unsafe {
                    use windows::Win32::UI::Accessibility::SetWinEventHook;
                    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
                    let hooks = [
                        SetWinEventHook(
                            EVENT_SYSTEM_FOREGROUND,
                            EVENT_SYSTEM_FOREGROUND,
                            None,
                            Some(win_event_callback),
                            0,
                            0,
                            flags,
                        ),
                        SetWinEventHook(
                            EVENT_SYSTEM_MINIMIZESTART,
                            EVENT_SYSTEM_MINIMIZEEND,
                            None,
                            Some(win_event_callback),
                            0,
                            0,
                            flags,
                        ),
                        SetWinEventHook(
                            EVENT_SYSTEM_DESKTOPSWITCH,
                            EVENT_SYSTEM_DESKTOPSWITCH,
                            None,
                            Some(win_event_callback),
                            0,
                            0,
                            flags,
                        ),
                        SetWinEventHook(
                            EVENT_OBJECT_CREATE,
                            EVENT_OBJECT_NAMECHANGE,
                            None,
                            Some(win_event_callback),
                            0,
                            0,
                            flags,
                        ),
                    ];
                    if hooks.iter().all(|hook| hook.0.is_null()) {
                        tracing::warn!("failed to install Windows window event hooks");
                        return;
                    }
                    let mut message = MSG::default();
                    while GetMessageW(&mut message, None, 0, 0).as_bool() {}
                })
                .is_ok()
        })
    }

    unsafe extern "system" fn win_event_callback(
        _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
        event: u32,
        _window: HWND,
        object_id: i32,
        child_id: i32,
        _event_thread: u32,
        _event_time: u32,
    ) {
        if event >= EVENT_OBJECT_CREATE && (object_id != OBJID_WINDOW.0 || child_id != 0) {
            return;
        }
        if let Some(senders) = WINDOW_EVENT_SENDERS.get() {
            for sender in senders
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
            {
                let _ = sender.send(());
            }
        }
    }

    /// Information collected during EnumWindows callback.
    struct RawWindow {
        hwnd: HWND,
        title: String,
        is_focused: bool,
        space_id: u64,
        is_current: bool,
    }

    struct Enumeration {
        windows: Vec<RawWindow>,
        desktops: DesktopSnapshot,
    }

    unsafe fn enumerate() -> Enumeration {
        let mut sample = Enumeration {
            windows: Vec::new(),
            desktops: DesktopSnapshot::read(),
        };
        let _ = EnumWindows(
            Some(enum_windows_callback),
            LPARAM(&mut sample as *mut Enumeration as isize),
        );
        sample
    }

    unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let sample = &mut *(lparam.0 as *mut Enumeration);

        // Skip invisible windows
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }

        // Skip minimised windows (they have no meaningful thumbnail)
        // Actually, still include them—they are "visible" in Alt+Tab sense

        // Filter out tool windows, popups without captions, etc.
        let ex_style = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        if ex_style.contains(WS_EX_TOOLWINDOW) {
            return TRUE;
        }

        // Shell-cloaked windows on known desktops belong in the other pages.
        // Continue excluding app/inherited cloaking and unknown hidden windows.
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        let space_id = sample.desktops.window_space(hwnd);
        if cloaked != 0 && (cloaked & !2 != 0 || space_id.is_none()) {
            return TRUE;
        }

        // Get title
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return TRUE;
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        GetWindowTextW(hwnd, &mut buf);
        let title = OsString::from_wide(&buf[..len as usize])
            .to_string_lossy()
            .to_string();

        let foreground = GetForegroundWindow();
        let is_focused = hwnd == foreground;

        sample.windows.push(RawWindow {
            hwnd,
            title,
            is_focused,
            space_id: space_id.unwrap_or(0),
            is_current: if sample.desktops.spaces.is_empty() {
                cloaked == 0
            } else {
                sample.desktops.window_is_current(hwnd)
            },
        });

        TRUE
    }

    fn get_process_exe_name(hwnd: HWND) -> String {
        unsafe {
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return String::new();
            }
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return String::new();
            };
            let mut buf = [0u16; 512];
            let mut size = buf.len() as u32;
            if QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
            .is_ok()
            {
                let _ = CloseHandle(handle);
                let path = OsString::from_wide(&buf[..size as usize])
                    .to_string_lossy()
                    .to_string();
                // Return just the file name without extension
                if let Some(name) = std::path::Path::new(&path).file_stem() {
                    return name.to_string_lossy().to_string();
                }
                return path;
            }
            let _ = CloseHandle(handle);
            String::new()
        }
    }

    // Raw FFI for PrintWindow (user32.dll) — not exposed by the
    // windows crate feature set we use.
    extern "system" {
        fn PrintWindow(
            hwnd: *mut std::ffi::c_void,
            hdcblt: *mut std::ffi::c_void,
            nflags: u32,
        ) -> i32;
    }

    /// PW_RENDERFULLCONTENT (0x2): tells PrintWindow to use DWM
    /// composition to render the window content, which works correctly
    /// for hardware-accelerated / DWM-composed windows.
    const PW_RENDERFULLCONTENT: u32 = 0x00000002;

    fn capture_window_thumbnail(hwnd: HWND, is_current: bool) -> Vec<u8> {
        // Never use the screen-DC fallback for an inactive desktop: those
        // coordinates would capture an unrelated window on the active desktop.
        if !is_current {
            return vec![];
        }
        unsafe {
            use windows::Win32::Graphics::Gdi::*;

            // Minimized windows can't be meaningfully captured
            if IsIconic(hwnd).as_bool() {
                return vec![];
            }

            let mut rect = windows::Win32::Foundation::RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return vec![];
            }

            let w = (rect.right - rect.left).max(1) as i32;
            let h = (rect.bottom - rect.top).max(1) as i32;

            // Limit capture size to prevent excessive memory usage
            if w > 4096 || h > 4096 || w <= 0 || h <= 0 {
                return vec![];
            }

            let hdc_screen = GetDC(HWND::default());
            let hdc_mem = CreateCompatibleDC(hdc_screen);
            let hbm = CreateCompatibleBitmap(hdc_screen, w, h);
            let old = SelectObject(hdc_mem, hbm);

            // Use PrintWindow with PW_RENDERFULLCONTENT to correctly
            // capture DWM-composed / hardware-accelerated windows.
            let print_ok = PrintWindow(hwnd.0, hdc_mem.0, PW_RENDERFULLCONTENT);
            if print_ok == 0 {
                // Fallback: capture from screen DC at the window position.
                // This may include overlapping windows but is better than nothing.
                let _ = BitBlt(
                    hdc_mem, 0, 0, w, h, hdc_screen, rect.left, rect.top, SRCCOPY,
                );
            }

            // Scale down to thumbnail size
            let max_dim = 320i32;
            let scale = (max_dim as f64 / w.max(h) as f64).min(1.0);
            let tw = (w as f64 * scale) as i32;
            let th = (h as f64 * scale) as i32;

            // Read bitmap bits
            let mut bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: tw,
                    biHeight: -th, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0, // BI_RGB
                    ..Default::default()
                },
                ..Default::default()
            };

            // Create a scaled DC
            let hdc_thumb = CreateCompatibleDC(hdc_screen);
            let hbm_thumb = CreateCompatibleBitmap(hdc_screen, tw, th);
            let old_thumb = SelectObject(hdc_thumb, hbm_thumb);

            // Use StretchBlt to scale
            SetStretchBltMode(hdc_thumb, HALFTONE);
            let _ = SetBrushOrgEx(hdc_thumb, 0, 0, None);
            let _ = StretchBlt(hdc_thumb, 0, 0, tw, th, hdc_mem, 0, 0, w, h, SRCCOPY);

            let row_bytes = ((tw * 32 + 31) / 32) * 4;
            let img_size = (row_bytes * th) as usize;
            let mut pixels = vec![0u8; img_size];

            GetDIBits(
                hdc_thumb,
                hbm_thumb,
                0,
                th as u32,
                Some(pixels.as_mut_ptr() as *mut _),
                &mut bi,
                DIB_RGB_COLORS,
            );

            // Clean up GDI
            SelectObject(hdc_thumb, old_thumb);
            let _ = DeleteObject(hbm_thumb);
            let _ = DeleteDC(hdc_thumb);
            SelectObject(hdc_mem, old);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(hdc_mem);
            ReleaseDC(HWND::default(), hdc_screen);

            // If all pixels are zero the capture failed — return empty
            // so the client doesn't display a black rectangle.
            if pixels.iter().all(|&b| b == 0) {
                return vec![];
            }

            // GDI returns BGRA and often leaves alpha at zero. Convert once
            // into the shared compressed PNG encoder's RGBA representation.
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
            encode_rgba_to_png(&pixels, tw as u32, th as u32)
        }
    }

    #[async_trait::async_trait]
    impl WindowManagerRepository for NativeWindowManager {
        fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
            self.notifications_available
                .then(|| self.change_tx.subscribe())
        }

        fn focused_app_name_now(&self) -> Result<String> {
            unsafe {
                let hwnd = GetForegroundWindow();
                if hwnd.0.is_null() {
                    return Ok(String::new());
                }
                Ok(get_process_exe_name(hwnd))
            }
        }

        async fn list_windows(&self) -> Result<Vec<WindowInfo>> {
            tokio::task::spawn_blocking(|| unsafe {
                let raw_windows = enumerate().windows;

                let mut windows = Vec::new();
                for rw in raw_windows {
                    let app_name = get_process_exe_name(rw.hwnd);
                    let thumbnail_png = capture_window_thumbnail(rw.hwnd, rw.is_current);
                    tracing::debug!(
                        window_id = rw.hwnd.0 as usize,
                        app_name = ?app_name,
                        title = ?rw.title,
                        thumbnail_bytes = thumbnail_png.len(),
                        "sampled window"
                    );

                    windows.push(WindowInfo {
                        window_id: rw.hwnd.0 as usize as u32,
                        title: rw.title,
                        app_name,
                        is_focused: rw.is_focused,
                        thumbnail_png,
                        space_id: rw.space_id,
                    });
                }

                Ok(windows)
            })
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking failed: {e}")))?
        }

        async fn list_windows_with_thumbnails(
            &self,
            window_ids: &[u32],
        ) -> Result<Vec<WindowInfo>> {
            let requested = window_ids
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>();
            tokio::task::spawn_blocking(move || unsafe {
                let raw_windows = enumerate().windows;

                let windows = raw_windows
                    .into_iter()
                    .map(|raw| WindowInfo {
                        window_id: raw.hwnd.0 as usize as u32,
                        title: raw.title,
                        app_name: get_process_exe_name(raw.hwnd),
                        is_focused: raw.is_focused,
                        thumbnail_png: requested
                            .contains(&(raw.hwnd.0 as usize as u32))
                            .then(|| capture_window_thumbnail(raw.hwnd, raw.is_current))
                            .unwrap_or_default(),
                        space_id: raw.space_id,
                    })
                    .collect();
                Ok(windows)
            })
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking failed: {e}")))?
        }

        async fn list_windows_meta(&self) -> Result<Vec<WindowInfo>> {
            tokio::task::spawn_blocking(|| unsafe {
                let raw_windows = enumerate().windows;

                let mut windows = Vec::new();
                for rw in raw_windows {
                    let app_name = get_process_exe_name(rw.hwnd);
                    windows.push(WindowInfo {
                        window_id: rw.hwnd.0 as usize as u32,
                        title: rw.title,
                        app_name,
                        is_focused: rw.is_focused,
                        thumbnail_png: vec![],
                        space_id: rw.space_id,
                    });
                }

                Ok(windows)
            })
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking failed: {e}")))?
        }

        async fn focused_app_name(&self) -> Result<String> {
            tokio::task::spawn_blocking(|| unsafe {
                let hwnd = GetForegroundWindow();
                if hwnd.0.is_null() {
                    return Ok(String::new());
                }
                Ok(get_process_exe_name(hwnd))
            })
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking failed: {e}")))?
        }

        async fn focus_window(&self, window_id: u32) -> Result<()> {
            tokio::task::spawn_blocking(move || unsafe {
                let hwnd = HWND(window_id as usize as *mut std::ffi::c_void);
                let desktops = DesktopSnapshot::read();
                if !desktops.window_is_current(hwnd) {
                    if let Some(space) = desktops.window_space(hwnd) {
                        windows_desktops::switch_space(space)?;
                    }
                }

                // If the window is minimised, restore it first
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }

                // Bring window to foreground
                if SetForegroundWindow(hwnd).as_bool() {
                    Ok(())
                } else {
                    // Fallback: use Alt key trick to allow SetForegroundWindow
                    use windows::Win32::UI::Input::KeyboardAndMouse::*;
                    let inputs = [INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: VIRTUAL_KEY(0x12), // VK_MENU (Alt)
                                dwFlags: KEYEVENTF_EXTENDEDKEY,
                                ..Default::default()
                            },
                        },
                    }];
                    SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
                    let result = SetForegroundWindow(hwnd);
                    let inputs_up = [INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: VIRTUAL_KEY(0x12),
                                dwFlags: KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP,
                                ..Default::default()
                            },
                        },
                    }];
                    SendInput(&inputs_up, std::mem::size_of::<INPUT>() as i32);
                    if result.as_bool() {
                        Ok(())
                    } else {
                        Err(Error::Other(format!(
                            "SetForegroundWindow failed for hwnd {window_id}"
                        )))
                    }
                }
            })
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking failed: {e}")))?
        }

        async fn list_spaces(&self) -> Result<Vec<crate::domain::window_manager::SpaceInfo>> {
            tokio::task::spawn_blocking(|| DesktopSnapshot::read().spaces)
                .await
                .map_err(|error| Error::Other(format!("desktop enumeration failed: {error}")))
        }

        async fn switch_space(&self, space_id: u64) -> Result<()> {
            tokio::task::spawn_blocking(move || windows_desktops::switch_space(space_id))
                .await
                .map_err(|error| Error::Other(format!("desktop switch failed: {error}")))?
        }

        async fn on_screen_window_ids(&self) -> Result<Vec<u32>> {
            Ok(self.sample_window_state().await?.on_screen_window_ids)
        }

        async fn sample_window_state(&self) -> Result<WindowStateSample> {
            tokio::task::spawn_blocking(|| unsafe {
                let sample = enumerate();
                let mut on_screen_window_ids = Vec::new();
                let windows: Vec<_> = sample
                    .windows
                    .into_iter()
                    .map(|raw| {
                        if raw.is_current && !IsIconic(raw.hwnd).as_bool() {
                            on_screen_window_ids.push(raw.hwnd.0 as usize as u32);
                        }
                        WindowInfo {
                            window_id: raw.hwnd.0 as usize as u32,
                            title: raw.title,
                            app_name: get_process_exe_name(raw.hwnd),
                            is_focused: raw.is_focused,
                            thumbnail_png: vec![],
                            space_id: raw.space_id,
                        }
                    })
                    .collect();
                on_screen_window_ids.sort_unstable();
                let focused_app_name = windows
                    .iter()
                    .find(|window| window.is_focused)
                    .map(|window| window.app_name.clone());
                WindowStateSample {
                    windows,
                    spaces: sample.desktops.spaces,
                    on_screen_window_ids,
                    focused_app_name,
                }
            })
            .await
            .map_err(|error| Error::Other(format!("desktop sample failed: {error}")))
        }
    }
}

// ── Non-macOS non-Windows stub ──
