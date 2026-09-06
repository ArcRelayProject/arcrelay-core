use crate::domain::window_manager::*;
use crate::error::{Error, Result};

// ── macOS implementation ──
//
// Uses native CoreGraphics FFI (called in-process) so that the
// arcrelay-desktop binary's own Screen Recording permission applies.
// Previous approach spawned `osascript` which is a *separate* process
// and does NOT inherit Screen Recording authorisation on macOS 15+.

#[cfg(target_os = "macos")]
pub struct NativeWindowManager {
    thumbnail_cache:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<u32, CachedThumbnail>>>,
    change_tx: tokio::sync::broadcast::Sender<()>,
    notifications_available: bool,
}

#[cfg(target_os = "macos")]
struct CachedThumbnail {
    captured_at: std::time::Instant,
    png: Vec<u8>,
}

#[cfg(target_os = "macos")]
mod mac_impl {
    use crate::domain::window_manager::{SpaceInfo, WindowInfo, WindowStateSample};
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionaryRef;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_graphics::display::{
        kCGWindowListExcludeDesktopElements, kCGWindowListOptionAll,
        kCGWindowListOptionOnScreenOnly,
    };
    use std::ffi::c_void;

    // ── CoreGraphics FFI ────────────────────────────────────────────
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> *const c_void;
    }

    // ── Private CGSession APIs for Spaces ───────────────────────────
    // These are stable private SPI used by yabai, Amethyst, etc.
    type CGSConnectionID = i32;

    extern "C" {
        fn _CGSDefaultConnection() -> CGSConnectionID;
        fn CGSCopyManagedDisplaySpaces(conn: CGSConnectionID) -> *const c_void; // CFArray
        fn CGSGetActiveSpace(conn: CGSConnectionID) -> u64;
    }

    // CG dictionary key constants
    const K_WINDOW_NUMBER: &str = "kCGWindowNumber";
    const K_WINDOW_LAYER: &str = "kCGWindowLayer";
    const K_WINDOW_OWNER_NAME: &str = "kCGWindowOwnerName";
    const K_WINDOW_OWNER_PID: &str = "kCGWindowOwnerPID";
    const K_WINDOW_NAME: &str = "kCGWindowName";

    // ── Dictionary helpers ──────────────────────────────────────────

    unsafe fn dict_get_value(dict: CFDictionaryRef, key: &str) -> Option<CFType> {
        use core_foundation::dictionary::CFDictionaryGetValue;
        use core_foundation::string::CFStringRef;

        let cf_key = CFString::new(key);
        let raw = CFDictionaryGetValue(dict, cf_key.as_CFTypeRef());
        if raw.is_null() {
            None
        } else {
            Some(CFType::wrap_under_get_rule(
                raw as CFStringRef as *const c_void,
            ))
        }
    }

    fn dict_get_i64(dict: CFDictionaryRef, key: &str) -> Option<i64> {
        unsafe {
            dict_get_value(dict, key).and_then(|v| {
                let n: CFNumber = CFNumber::wrap_under_get_rule(v.as_CFTypeRef() as *const _);
                n.to_i64()
            })
        }
    }

    fn dict_get_string(dict: CFDictionaryRef, key: &str) -> Option<String> {
        unsafe {
            dict_get_value(dict, key).map(|v| {
                let s: CFString = CFString::wrap_under_get_rule(v.as_CFTypeRef() as *const _);
                s.to_string()
            })
        }
    }

    // ── CGWindowEntry ───────────────────────────────────────────────

    pub(super) struct CGWindowEntry {
        pub window_id: u32,
        pub app_name: String,
        pub title: String,
        pub pid: i64,
        pub space_id: u64,
    }

    fn assign_window_spaces(
        entries: &mut [CGWindowEntry],
        space_map: &std::collections::HashMap<u32, u64>,
        on_screen_window_ids: &std::collections::HashSet<u32>,
        active_space_id: Option<u64>,
    ) {
        for entry in entries {
            if let Some(&space_id) = space_map.get(&entry.window_id) {
                entry.space_id = space_id;
            } else if on_screen_window_ids.contains(&entry.window_id) {
                entry.space_id = active_space_id.unwrap_or_default();
            }
        }
    }

    /// Build a window_id → space_id map using CGSCopyManagedDisplaySpaces.
    ///
    /// This makes ONE CGS call instead of N per-window CGSCopySpacesForWindows
    /// calls, greatly reducing the chance of blocking on macOS internal locks
    /// during space transition animations.
    fn build_space_map(window_ids: &[u32]) -> std::collections::HashMap<u32, u64> {
        let mut map = std::collections::HashMap::new();
        if window_ids.is_empty() {
            return map;
        }

        let wanted: std::collections::HashSet<u32> = window_ids.iter().copied().collect();

        unsafe {
            let conn = _CGSDefaultConnection();

            // Fast path: read window→space mapping from CGSCopyManagedDisplaySpaces.
            // Each space dict has a "windows" key with its assigned window IDs.
            let raw = CGSCopyManagedDisplaySpaces(conn);
            if !raw.is_null() {
                let displays: CFArray = CFArray::wrap_under_create_rule(raw as *const _);
                for dptr in displays.get_all_values() {
                    let display_dict = dptr as CFDictionaryRef;
                    let Some(spaces_cf) = dict_get_value(display_dict, "Spaces") else {
                        continue;
                    };
                    let spaces_arr: CFArray =
                        CFArray::wrap_under_get_rule(spaces_cf.as_CFTypeRef() as *const _);
                    for sptr in spaces_arr.get_all_values() {
                        let space_dict = sptr as CFDictionaryRef;
                        let Some(sid64) = dict_get_i64(space_dict, "ManagedSpaceID") else {
                            continue;
                        };
                        let space_id = sid64 as u64;
                        let Some(windows_cf) = dict_get_value(space_dict, "windows") else {
                            continue;
                        };
                        let windows_arr: CFArray =
                            CFArray::wrap_under_get_rule(windows_cf.as_CFTypeRef() as *const _);
                        for wptr in windows_arr.get_all_values() {
                            let n: CFNumber = CFNumber::wrap_under_get_rule(wptr as *const _);
                            if let Some(wid64) = n.to_i64() {
                                let wid = wid64 as u32;
                                if wanted.contains(&wid) {
                                    map.insert(wid, space_id);
                                }
                            }
                        }
                    }
                }
            }
        }
        map
    }

    /// Enumerate ALL windows (all Spaces) via CoreGraphics.
    pub(super) fn cg_list_all_windows() -> Vec<CGWindowEntry> {
        let opts = kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements;

        let raw = unsafe { CGWindowListCopyWindowInfo(opts, 0) };
        if raw.is_null() {
            return vec![];
        }
        let array: CFArray = unsafe { CFArray::wrap_under_create_rule(raw as *const _) };

        let mut entries = Vec::new();
        let mut all_ids = Vec::new();

        let ptrs = array.get_all_values();
        for ptr in &ptrs {
            let dict: CFDictionaryRef = *ptr as CFDictionaryRef;

            let layer = dict_get_i64(dict, K_WINDOW_LAYER).unwrap_or(-1);
            if layer != 0 {
                continue;
            }
            let window_id = dict_get_i64(dict, K_WINDOW_NUMBER).unwrap_or(0) as u32;
            if window_id == 0 {
                continue;
            }
            let app_name = dict_get_string(dict, K_WINDOW_OWNER_NAME).unwrap_or_default();
            let title = dict_get_string(dict, K_WINDOW_NAME).unwrap_or_default();
            let pid = dict_get_i64(dict, K_WINDOW_OWNER_PID).unwrap_or(0);

            if title.is_empty() && app_name.is_empty() {
                continue;
            }

            all_ids.push(window_id);
            entries.push(CGWindowEntry {
                window_id,
                app_name,
                title,
                pid,
                space_id: 0, // filled in below
            });
        }

        // Map window_id → space_id. Some macOS releases omit the private
        // `windows` array from managed-space dictionaries. Do not let that
        // erase the entire window list: windows confirmed by CoreGraphics as
        // on-screen can still be assigned coherently to the active Space.
        let space_map = build_space_map(&all_ids);
        let has_unmapped_windows = entries
            .iter()
            .any(|entry| !space_map.contains_key(&entry.window_id));
        let (on_screen_window_ids, active_space_id) = if has_unmapped_windows {
            let on_screen_window_ids = cg_list_windows()
                .into_iter()
                .map(|window| window.window_id)
                .collect();
            let active_space_id = unsafe {
                let space_id = CGSGetActiveSpace(_CGSDefaultConnection());
                (space_id != 0).then_some(space_id)
            };
            (on_screen_window_ids, active_space_id)
        } else {
            (std::collections::HashSet::new(), None)
        };
        assign_window_spaces(
            &mut entries,
            &space_map,
            &on_screen_window_ids,
            active_space_id,
        );

        // Filter out windows that have no space (system/menubar extras)
        entries.retain(|e| e.space_id != 0);

        // ── Deduplicate fullscreen spaces ───────────────────────────
        // macOS often reports two windows for a single fullscreen app
        // (the real window + a backdrop / helper).  Keep only the one
        // with a non-empty title per (space_id, pid) pair.
        {
            let mut seen = std::collections::HashMap::<(u64, i64), usize>::new();
            let mut remove = std::collections::HashSet::new();
            for (idx, e) in entries.iter().enumerate() {
                let key = (e.space_id, e.pid);
                if let Some(&prev_idx) = seen.get(&key) {
                    // Prefer the entry with a non-empty title
                    let prev = &entries[prev_idx];
                    if prev.title.is_empty() && !e.title.is_empty() {
                        remove.insert(prev_idx);
                        seen.insert(key, idx);
                    } else {
                        remove.insert(idx);
                    }
                } else {
                    seen.insert(key, idx);
                }
            }
            if !remove.is_empty() {
                let mut idx = 0;
                entries.retain(|_| {
                    let keep = !remove.contains(&idx);
                    idx += 1;
                    keep
                });
            }
        }

        entries
    }

    /// Enumerate on-screen windows only (current Space).
    pub(super) fn cg_list_windows() -> Vec<CGWindowEntry> {
        let opts = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;

        let raw = unsafe { CGWindowListCopyWindowInfo(opts, 0) };
        if raw.is_null() {
            return vec![];
        }
        let array: CFArray = unsafe { CFArray::wrap_under_create_rule(raw as *const _) };

        let mut entries = Vec::new();
        let ptrs = array.get_all_values();
        for ptr in &ptrs {
            let dict: CFDictionaryRef = *ptr as CFDictionaryRef;
            let layer = dict_get_i64(dict, K_WINDOW_LAYER).unwrap_or(-1);
            if layer != 0 {
                continue;
            }
            let window_id = dict_get_i64(dict, K_WINDOW_NUMBER).unwrap_or(0) as u32;
            if window_id == 0 {
                continue;
            }
            let app_name = dict_get_string(dict, K_WINDOW_OWNER_NAME).unwrap_or_default();
            let title = dict_get_string(dict, K_WINDOW_NAME).unwrap_or_default();
            let pid = dict_get_i64(dict, K_WINDOW_OWNER_PID).unwrap_or(0);
            if title.is_empty() && app_name.is_empty() {
                continue;
            }
            entries.push(CGWindowEntry {
                window_id,
                app_name,
                title,
                pid,
                space_id: 0,
            });
        }
        entries
    }

    /// Collect window metadata, the active-space window set, focus, and Spaces
    /// in one blocking sampling job. CoreGraphics returns windows front-to-back,
    /// so the first ordinary on-screen window is the focused window without an
    /// `osascript` process.
    pub(super) fn sample_window_state() -> WindowStateSample {
        let on_screen = cg_list_windows();
        let focused_window_id = on_screen.first().map(|window| window.window_id);
        let focused_pid = on_screen.first().map(|window| window.pid);
        let focused_app_name = on_screen.first().map(|window| window.app_name.clone());
        let mut on_screen_window_ids = on_screen
            .iter()
            .map(|window| window.window_id)
            .collect::<Vec<_>>();
        on_screen_window_ids.sort_unstable();

        let mut seen_focused = false;
        let windows = cg_list_all_windows()
            .into_iter()
            .map(|window| {
                let is_focused = !seen_focused
                    && (focused_window_id == Some(window.window_id)
                        || focused_pid == Some(window.pid));
                if is_focused {
                    seen_focused = true;
                }
                WindowInfo {
                    window_id: window.window_id,
                    title: window.title,
                    app_name: window.app_name,
                    is_focused,
                    thumbnail_png: Vec::new(),
                    space_id: window.space_id,
                }
            })
            .collect();

        WindowStateSample {
            windows,
            spaces: list_spaces(),
            on_screen_window_ids,
            focused_app_name,
        }
    }

    /// Find PID + title for a given CGWindowID (searches all Spaces).
    pub(super) fn find_window_pid_title(window_id: u32) -> Option<(i64, String)> {
        cg_list_all_windows()
            .into_iter()
            .find(|e| e.window_id == window_id)
            .map(|e| (e.pid, e.title))
    }

    /// Find which space a window belongs to.
    pub(super) fn find_window_space(window_id: u32) -> Option<u64> {
        let map = build_space_map(&[window_id]);
        map.get(&window_id).copied()
    }

    // ── Spaces enumeration ──────────────────────────────────────────

    pub(super) fn list_spaces() -> Vec<SpaceInfo> {
        unsafe {
            let conn = _CGSDefaultConnection();
            let active_space = CGSGetActiveSpace(conn);

            let raw = CGSCopyManagedDisplaySpaces(conn);
            if raw.is_null() {
                return vec![];
            }
            let displays: CFArray = CFArray::wrap_under_create_rule(raw as *const _);
            let mut spaces = Vec::new();
            let mut index = 1u32;

            // Each element is a dictionary for a display (monitor)
            let display_ptrs = displays.get_all_values();
            for dptr in &display_ptrs {
                let display_dict: CFDictionaryRef = *dptr as CFDictionaryRef;
                // Get "Spaces" array from this display dict
                let spaces_val = dict_get_value(display_dict, "Spaces");
                let Some(spaces_cf) = spaces_val else {
                    continue;
                };

                let spaces_arr: CFArray =
                    CFArray::wrap_under_get_rule(spaces_cf.as_CFTypeRef() as *const _);
                let space_ptrs = spaces_arr.get_all_values();

                for sptr in &space_ptrs {
                    let space_dict: CFDictionaryRef = *sptr as CFDictionaryRef;
                    let Some(sid64) = dict_get_i64(space_dict, "ManagedSpaceID") else {
                        continue;
                    };
                    // Type 0 = user space, type 4 = fullscreen space
                    let space_type = dict_get_i64(space_dict, "type").unwrap_or(0);
                    let label = if space_type == 4 {
                        format!("Full Screen {index}")
                    } else {
                        format!("Desktop {index}")
                    };
                    let space_id = sid64 as u64;
                    spaces.push(SpaceInfo {
                        space_id,
                        label,
                        is_active: space_id == active_space,
                    });
                    index += 1;
                }
            }
            spaces
        }
    }

    /// Switch to a specific Space via System Events Control+Arrow.
    ///
    /// NOTE: We intentionally switch Spaces by simulating Control+Arrow via
    /// System Events. In-process CGS calls are sufficient for enumeration, but
    /// they are not what this implementation uses to perform the actual switch.
    pub(super) fn switch_space(target_space_id: u64) -> std::io::Result<()> {
        let spaces = list_spaces();
        let active_idx = spaces.iter().position(|s| s.is_active);
        let target_idx = spaces.iter().position(|s| s.space_id == target_space_id);

        let (Some(cur), Some(tgt)) = (active_idx, target_idx) else {
            tracing::warn!("switch_space: could not resolve space indices");
            return Ok(());
        };

        if cur == tgt {
            return Ok(());
        }

        let diff = tgt as i32 - cur as i32;
        // key code 124 = Right Arrow, 123 = Left Arrow
        let (arrow_code, steps) = if diff > 0 {
            (124, diff as usize)
        } else {
            (123, (-diff) as usize)
        };

        // Batch all key presses into a single osascript invocation.
        // The 0.4 s delay between presses is just enough for the space
        // switch animation to complete before the next one fires.
        let mut script = String::from("tell application \"System Events\"\n");
        for i in 0..steps {
            script.push_str(&format!("  key code {} using control down\n", arrow_code));
            if i + 1 < steps {
                script.push_str("  delay 0.4\n");
            }
        }
        script.push_str("end tell");

        let mut command = std::process::Command::new("osascript");
        command.arg("-e").arg(&script);
        let output = crate::infrastructure::bounded_command::output(
            command,
            std::time::Duration::from_secs(10),
        )?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn entry(window_id: u32) -> CGWindowEntry {
            CGWindowEntry {
                window_id,
                app_name: "App".into(),
                title: format!("Window {window_id}"),
                pid: window_id as i64,
                space_id: 0,
            }
        }

        #[test]
        fn visible_windows_fall_back_to_the_active_space_when_mapping_is_missing() {
            let mut entries = vec![entry(1), entry(2), entry(3)];
            let space_map = std::collections::HashMap::from([(1, 10)]);
            let on_screen_window_ids = std::collections::HashSet::from([2]);

            assign_window_spaces(&mut entries, &space_map, &on_screen_window_ids, Some(20));

            assert_eq!(entries[0].space_id, 10);
            assert_eq!(entries[1].space_id, 20);
            assert_eq!(entries[2].space_id, 0);
        }

        #[test]
        fn visible_window_fallback_requires_a_known_active_space() {
            let mut entries = vec![entry(1)];

            assign_window_spaces(
                &mut entries,
                &std::collections::HashMap::new(),
                &std::collections::HashSet::from([1]),
                None,
            );

            assert_eq!(entries[0].space_id, 0);
        }
    }
}

#[cfg(target_os = "macos")]
mod mac_events {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification,
        NSWorkspaceDidActivateApplicationNotification,
        NSWorkspaceDidDeactivateApplicationNotification, NSWorkspaceDidHideApplicationNotification,
        NSWorkspaceDidLaunchApplicationNotification,
        NSWorkspaceDidTerminateApplicationNotification,
        NSWorkspaceDidUnhideApplicationNotification,
    };
    use objc2_foundation::{NSNotification, NSNotificationName, NSOperationQueue};

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopRun();
    }

    pub(super) fn start(change_tx: tokio::sync::broadcast::Sender<()>) -> bool {
        match std::thread::Builder::new()
            .name("arcrelay-window-events".into())
            .spawn(move || {
                autoreleasepool(|_| {
                    // NSWorkspace may otherwise execute a `queue=None` block on
                    // the posting thread, including AppKit's main thread. Keep
                    // callbacks on a dedicated serial operation queue and make
                    // them signal-only. Window metadata is sampled separately
                    // through CoreGraphics, so no cross-process AX IPC belongs
                    // in this notification path.
                    let callback_queue = NSOperationQueue::new();
                    callback_queue.setMaxConcurrentOperationCount(1);
                    let workspace = NSWorkspace::sharedWorkspace();
                    let center = workspace.notificationCenter();
                    let names: [&NSNotificationName; 7] = unsafe {
                        [
                            NSWorkspaceDidActivateApplicationNotification,
                            NSWorkspaceDidDeactivateApplicationNotification,
                            NSWorkspaceDidLaunchApplicationNotification,
                            NSWorkspaceDidTerminateApplicationNotification,
                            NSWorkspaceDidHideApplicationNotification,
                            NSWorkspaceDidUnhideApplicationNotification,
                            NSWorkspaceActiveSpaceDidChangeNotification,
                        ]
                    };
                    let mut notification_observers = Vec::new();
                    for name in names {
                        let change_tx = change_tx.clone();
                        let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                            let _ = change_tx.send(());
                        });
                        notification_observers.push(unsafe {
                            center.addObserverForName_object_queue_usingBlock(
                                Some(name),
                                None,
                                Some(&callback_queue),
                                &block,
                            )
                        });
                    }
                    let _ = change_tx.send(());
                    unsafe { CFRunLoopRun() };
                    drop(notification_observers);
                });
            }) {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(%error, "failed to start macOS window event watcher");
                false
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl NativeWindowManager {
    pub fn new() -> Self {
        let (change_tx, _) = tokio::sync::broadcast::channel(64);
        let notifications_available = mac_events::start(change_tx.clone());
        Self {
            thumbnail_cache: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            change_tx,
            notifications_available,
        }
    }
}

#[cfg(target_os = "macos")]
#[async_trait::async_trait]
impl WindowManagerRepository for NativeWindowManager {
    fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
        self.notifications_available
            .then(|| self.change_tx.subscribe())
    }

    fn focused_app_name_now(&self) -> Result<String> {
        Ok(mac_impl::sample_window_state()
            .focused_app_name
            .unwrap_or_default())
    }

    async fn list_windows(&self) -> Result<Vec<WindowInfo>> {
        let cache = std::sync::Arc::clone(&self.thumbnail_cache);
        tokio::task::spawn_blocking(move || {
            let mut sample = mac_impl::sample_window_state();
            for window in &mut sample.windows {
                window.thumbnail_png = Self::capture_window_thumbnail(&cache, window.window_id);
            }
            sample.windows
        })
        .await
        .map_err(|e| Error::Other(format!("thumbnail task failed: {e}")))
    }

    async fn list_windows_with_thumbnails(&self, window_ids: &[u32]) -> Result<Vec<WindowInfo>> {
        let requested = window_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let cache = std::sync::Arc::clone(&self.thumbnail_cache);
        tokio::task::spawn_blocking(move || {
            let mut sample = mac_impl::sample_window_state();
            for window in &mut sample.windows {
                if requested.contains(&window.window_id) {
                    window.thumbnail_png = Self::capture_window_thumbnail(&cache, window.window_id);
                }
            }
            sample.windows
        })
        .await
        .map_err(|e| Error::Other(format!("thumbnail task failed: {e}")))
    }

    async fn list_windows_meta(&self) -> Result<Vec<WindowInfo>> {
        tokio::task::spawn_blocking(|| mac_impl::sample_window_state().windows)
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking: {e}")))
    }

    async fn focused_app_name(&self) -> Result<String> {
        tokio::task::spawn_blocking(|| {
            Ok(mac_impl::sample_window_state()
                .focused_app_name
                .unwrap_or_default())
        })
        .await
        .map_err(|error| Error::Other(format!("focused app task failed: {error}")))?
    }

    async fn focus_window(&self, window_id: u32) -> Result<()> {
        // 1. Find PID + title for this CGWindowID (native FFI, all Spaces).
        let (pid, title, target_space, active_space) = tokio::task::spawn_blocking(move || {
            let (pid, title) = mac_impl::find_window_pid_title(window_id)
                .ok_or_else(|| Error::NotFound(format!("window {window_id} in CG list")))?;
            let target_space = mac_impl::find_window_space(window_id);
            let active_space = mac_impl::list_spaces()
                .into_iter()
                .find(|space| space.is_active)
                .map(|space| space.space_id);
            Ok::<_, Error>((pid, title, target_space, active_space))
        })
        .await
        .map_err(|error| Error::Other(format!("window lookup task failed: {error}")))??;

        // 2. If the window is on a different Space, switch there first
        //    using the System Events Control+Arrow path above.
        if let (Some(target_space), Some(active_space)) = (target_space, active_space) {
            if active_space != target_space {
                tokio::task::spawn_blocking(move || mac_impl::switch_space(target_space))
                    .await
                    .map_err(|error| Error::Other(format!("space switch task failed: {error}")))?
                    .map_err(|error| Error::Other(format!("space switch failed: {error}")))?;
                // Small buffer for the animation to fully settle
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }

        // 3. Now use AX API to activate the app and raise the specific window.
        //    At this point we should already be on the correct space,
        //    so activating won't pull the window across spaces.
        let escaped_title = title.replace('\\', "\\\\").replace('\'', "\\'");
        let jxa = format!(
            r#"ObjC.import('Cocoa');

var pid = {pid};
var targetTitle = '{escaped_title}';

// --- Activate via NSRunningApplication (we're already on the right space) ---
var app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(pid);
if (app && !app.isEqual($.nil)) {{
    app.activateWithOptions(2);
}}

delay(0.15);

// --- Use AX API to raise the specific window ---
var axApp = $.AXUIElementCreateApplication(pid);
var windowsRef = Ref();
var axErr = $.AXUIElementCopyAttributeValue(axApp, 'AXWindows', windowsRef);
if (axErr === 0 && windowsRef[0]) {{
    var axWindows = $.CFBridgingRelease(windowsRef[0]);
    if (axWindows) {{
        var wCount = axWindows.count;
        var raised = false;
        if (targetTitle) {{
            for (var j = 0; j < wCount; j++) {{
                var axWin = axWindows.objectAtIndex(j);
                var titleRef = Ref();
                $.AXUIElementCopyAttributeValue(axWin, 'AXTitle', titleRef);
                var axTitle = $.CFBridgingRelease(titleRef[0]);
                if (axTitle && axTitle.js === targetTitle) {{
                    $.AXUIElementPerformAction(axWin, 'AXRaise');
                    raised = true;
                    break;
                }}
            }}
        }}
        if (!raised && wCount > 0) {{
            $.AXUIElementPerformAction(axWindows.objectAtIndex(0), 'AXRaise');
        }}
    }}
}}
'ok';"#
        );

        let output = tokio::task::spawn_blocking(move || {
            let mut command = std::process::Command::new("osascript");
            command.arg("-l").arg("JavaScript").arg("-e").arg(jxa);
            crate::infrastructure::bounded_command::output(
                command,
                std::time::Duration::from_secs(5),
            )
        })
        .await
        .map_err(|error| Error::Other(format!("JXA focus task failed: {error}")))?
        .map_err(|error| Error::Other(format!("JXA focus failed: {error}")))?;

        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if result == "ok" {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            tracing::warn!(
                window_id,
                pid,
                stdout = ?result,
                stderr = ?stderr,
                "failed to focus window"
            );
            Err(Error::Other(format!(
                "failed to focus window {window_id}: {result} {stderr}"
            )))
        }
    }

    async fn list_spaces(&self) -> Result<Vec<crate::domain::window_manager::SpaceInfo>> {
        tokio::task::spawn_blocking(mac_impl::list_spaces)
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking: {e}")))
    }

    async fn switch_space(&self, space_id: u64) -> Result<()> {
        tokio::task::spawn_blocking(move || mac_impl::switch_space(space_id))
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking: {e}")))?
            .map_err(|e| Error::Other(format!("space switch failed: {e}")))
    }

    async fn on_screen_window_ids(&self) -> Result<Vec<u32>> {
        tokio::task::spawn_blocking(|| mac_impl::sample_window_state().on_screen_window_ids)
            .await
            .map_err(|e| Error::Other(format!("spawn_blocking: {e}")))
    }

    async fn sample_window_state(&self) -> Result<WindowStateSample> {
        tokio::task::spawn_blocking(mac_impl::sample_window_state)
            .await
            .map_err(|e| Error::Other(format!("window sample task failed: {e}")))
    }

    async fn sample_window_state_with_thumbnails(
        &self,
        window_ids: &[u32],
    ) -> Result<WindowStateSample> {
        let requested = window_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let cache = std::sync::Arc::clone(&self.thumbnail_cache);
        tokio::task::spawn_blocking(move || {
            let mut sample = mac_impl::sample_window_state();
            for window in &mut sample.windows {
                if requested.contains(&window.window_id) {
                    window.thumbnail_png = Self::capture_window_thumbnail(&cache, window.window_id);
                }
            }
            sample
        })
        .await
        .map_err(|e| Error::Other(format!("window sample task failed: {e}")))
    }
}

impl Default for NativeWindowManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "macos")]
impl NativeWindowManager {
    /// Capture a window thumbnail as PNG bytes, scaled to a reasonable size.
    fn capture_window_thumbnail(
        cache: &std::sync::Mutex<std::collections::HashMap<u32, CachedThumbnail>>,
        window_id: u32,
    ) -> Vec<u8> {
        const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(2);
        if let Some(cached) = cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&window_id)
            .filter(|cached| cached.captured_at.elapsed() < CACHE_TTL)
        {
            return cached.png.clone();
        }

        use core_graphics::base::kCGImageAlphaPremultipliedLast;
        use core_graphics::color_space::CGColorSpace;
        use core_graphics::context::{CGContext, CGInterpolationQuality};
        use core_graphics::display::{
            kCGWindowImageBoundsIgnoreFraming, kCGWindowImageNominalResolution,
            kCGWindowListOptionIncludingWindow, CGDisplay,
        };
        use core_graphics::geometry::{CGPoint, CGRect, CGSize};

        extern "C" {
            static CGRectNull: CGRect;
        }

        let Some(image) = CGDisplay::screenshot(
            unsafe { CGRectNull },
            kCGWindowListOptionIncludingWindow,
            window_id,
            kCGWindowImageBoundsIgnoreFraming | kCGWindowImageNominalResolution,
        ) else {
            return Vec::new();
        };
        let source_width = image.width();
        let source_height = image.height();
        if source_width == 0 || source_height == 0 {
            return Vec::new();
        }

        let scale = (320.0 / source_width.max(source_height) as f64).min(1.0);
        let width = (source_width as f64 * scale).round().max(1.0) as usize;
        let height = (source_height as f64 * scale).round().max(1.0) as usize;
        let color_space = CGColorSpace::create_device_rgb();
        let mut context = CGContext::create_bitmap_context(
            None,
            width,
            height,
            8,
            width * 4,
            &color_space,
            kCGImageAlphaPremultipliedLast,
        );
        context.set_interpolation_quality(CGInterpolationQuality::CGInterpolationQualityMedium);
        context.translate(0.0, height as f64);
        context.scale(1.0, -1.0);
        context.draw_image(
            CGRect::new(
                &CGPoint::new(0.0, 0.0),
                &CGSize::new(width as f64, height as f64),
            ),
            &image,
        );
        let png = super::encode_rgba_to_png(context.data(), width as u32, height as u32);
        let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
        cache.retain(|_, cached| cached.captured_at.elapsed() < CACHE_TTL);
        cache.insert(
            window_id,
            CachedThumbnail {
                captured_at: std::time::Instant::now(),
                png: png.clone(),
            },
        );
        png
    }
}

// ── Windows implementation ──
