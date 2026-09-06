use crate::domain::media_control::*;
use crate::error::{Error, Result};

// ── macOS implementation ──

#[cfg(target_os = "macos")]
pub struct NativeMediaControl {
    sample_cache: std::sync::Mutex<Option<(std::time::Instant, MacMediaSample)>>,
    sample_gate: tokio::sync::Mutex<()>,
    change_tx: tokio::sync::broadcast::Sender<()>,
    notifications_available: bool,
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
struct MacMediaSample {
    playback: Option<PlaybackInfo>,
    volume: VolumeInfo,
    microphone_active: bool,
    dnd_active: bool,
}

#[cfg(target_os = "macos")]
impl NativeMediaControl {
    pub fn new() -> Self {
        let (change_tx, _) = tokio::sync::broadcast::channel(64);
        let notifications_available = mac_media_events::start(change_tx.clone());
        Self {
            sample_cache: std::sync::Mutex::new(None),
            sample_gate: tokio::sync::Mutex::new(()),
            change_tx,
            notifications_available,
        }
    }

    fn run_osascript(script: &str) -> Result<String> {
        let mut command = std::process::Command::new("osascript");
        command.arg("-e").arg(script);
        let output = crate::infrastructure::bounded_command::output(
            command,
            std::time::Duration::from_secs(5),
        )
        .map_err(|e| Error::MediaControl(format!("osascript failed: {e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            Err(Error::MediaControl(format!("osascript error: {err}")))
        }
    }

    fn run_jxa(script: &str) -> Result<String> {
        let mut command = std::process::Command::new("osascript");
        command.arg("-l").arg("JavaScript").arg("-e").arg(script);
        let output = crate::infrastructure::bounded_command::output(
            command,
            std::time::Duration::from_secs(5),
        )
        .map_err(|e| Error::MediaControl(format!("JXA failed: {e}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            Err(Error::MediaControl(format!("JXA error: {err}")))
        }
    }

    async fn sample(&self) -> Result<MacMediaSample> {
        const CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(750);
        if let Some(sample) = self
            .sample_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|(captured_at, _)| captured_at.elapsed() < CACHE_TTL)
            .map(|(_, sample)| sample.clone())
        {
            return Ok(sample);
        }
        let _gate = self.sample_gate.lock().await;
        if let Some(sample) = self
            .sample_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|(captured_at, _)| captured_at.elapsed() < CACHE_TTL)
            .map(|(_, sample)| sample.clone())
        {
            return Ok(sample);
        }
        let sample = tokio::task::spawn_blocking(Self::sample_sync)
            .await
            .map_err(|error| Error::MediaControl(format!("media sample task failed: {error}")))??;
        *self
            .sample_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some((std::time::Instant::now(), sample.clone()));
        Ok(sample)
    }

    fn invalidate_sample(&self) {
        self.sample_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }

    fn sample_sync() -> Result<MacMediaSample> {
        let script = r#"
var host = Application.currentApplication();
host.includeStandardAdditions = true;
var settings = host.getVolumeSettings();
var names = Application('System Events').applicationProcesses.name();
var playback = null;
function generic(source) {
    return { title: '正在播放', artist: source, source: source,
             position: 0, duration: 0, isPlaying: true };
}
if (names.indexOf('Spotify') >= 0) {
    try {
        var spotify = Application('Spotify');
        playback = { title: spotify.currentTrack.name(),
                     artist: spotify.currentTrack.artist(), source: 'Spotify',
                     position: spotify.playerPosition(),
                     duration: spotify.currentTrack.duration() / 1000,
                     isPlaying: String(spotify.playerState()) === 'playing' };
    } catch (_) { playback = generic('Spotify'); }
} else if (names.indexOf('Music') >= 0) {
    try {
        var music = Application('Music');
        playback = { title: music.currentTrack.name(),
                     artist: music.currentTrack.artist(), source: 'Music',
                     position: music.playerPosition(),
                     duration: music.currentTrack.duration(),
                     isPlaying: String(music.playerState()) === 'playing' };
    } catch (_) { playback = generic('Music'); }
} else if (names.indexOf('QQMusic') >= 0) playback = generic('QQ音乐');
else if (names.indexOf('NeteaseMusic') >= 0) playback = generic('网易云音乐');
else if (names.indexOf('Kugou') >= 0) playback = generic('酷狗音乐');
else if (names.indexOf('KwMusic') >= 0) playback = generic('酷我音乐');
JSON.stringify({ outputVolume: settings.outputVolume,
                 outputMuted: settings.outputMuted,
                 inputVolume: settings.inputVolume,
                 playback: playback });
"#;
        let output = Self::run_jxa(script)?;
        let value: serde_json::Value = serde_json::from_str(&output)
            .map_err(|error| Error::MediaControl(format!("invalid media sample: {error}")))?;
        let playback = value
            .get("playback")
            .filter(|value| !value.is_null())
            .map(|playback| PlaybackInfo {
                title: playback["title"].as_str().unwrap_or_default().to_string(),
                artist: playback["artist"].as_str().unwrap_or_default().to_string(),
                source_app: playback["source"].as_str().unwrap_or_default().to_string(),
                position_secs: playback["position"].as_f64().unwrap_or(0.0),
                duration_secs: playback["duration"].as_f64().unwrap_or(0.0),
                is_playing: playback["isPlaying"].as_bool().unwrap_or(false),
            });
        let mut defaults = std::process::Command::new("defaults");
        defaults.args([
            "read",
            "com.apple.controlcenter",
            "NSStatusItem Visible FocusModes",
        ]);
        let dnd_active = crate::infrastructure::bounded_command::output(
            defaults,
            std::time::Duration::from_secs(2),
        )
        .ok()
        .is_some_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "1");
        Ok(MacMediaSample {
            playback,
            volume: VolumeInfo {
                system_volume: value["outputVolume"].as_u64().unwrap_or(50).min(100) as u8,
                is_muted: value["outputMuted"].as_bool().unwrap_or(false),
            },
            microphone_active: value["inputVolume"].as_u64().unwrap_or(0) > 0,
            dnd_active,
        })
    }

    /// Detect which known media app is currently running.
    /// Returns (process_name, display_name) or None.
    fn detect_running_media_app() -> Option<(&'static str, &'static str)> {
        let candidates: &[(&str, &str)] = &[
            ("Spotify", "Spotify"),
            ("QQMusic", "QQ音乐"),
            ("NeteaseMusic", "网易云音乐"),
            ("Kugou", "酷狗音乐"),
            ("KwMusic", "酷我音乐"),
            ("Music", "Music"),
        ];

        let script = r#"tell application "System Events" to set appList to name of every application process
return appList as text"#;
        let running = Self::run_osascript(script).unwrap_or_default();

        // Use exact match against comma-separated list to avoid substring
        // false-positives (e.g. "MusicHelper" matching "Music")
        for &(process_name, display_name) in candidates {
            for app in running.split(", ") {
                if app == process_name {
                    return Some((process_name, display_name));
                }
            }
        }
        None
    }

    /// Simulate a macOS media key press via JXA/CGEventPost.
    /// Requires Accessibility permissions to work.
    fn send_media_key(key_type: u32) -> Result<()> {
        let script = format!(
            r#"ObjC.import('Cocoa');
var k={key};
var down=(k<<16)|(0xA<<8);
var up=(k<<16)|(0xB<<8);
var e1=$.NSEvent.otherEventWithTypeLocationModifierFlagsTimestampWindowNumberContextSubtypeData1Data2(14,$.NSMakePoint(0,0),0xa00,0,0,null,8,down,-1);
$.CGEventPost(0,e1.CGEvent);
$.NSThread.sleepForTimeInterval(0.05);
var e2=$.NSEvent.otherEventWithTypeLocationModifierFlagsTimestampWindowNumberContextSubtypeData1Data2(14,$.NSMakePoint(0,0),0xb00,0,0,null,8,up,-1);
$.CGEventPost(0,e2.CGEvent);"#,
            key = key_type
        );
        Self::run_jxa(&script).map(|_| ())
    }

    /// Direct AppleScript control for apps that support it (no Accessibility
    /// permissions needed).  Returns `None` when the app/action combo is not
    /// covered, signalling the caller to fall back to media-key simulation.
    fn app_playback_action(process_name: &str, action: &PlaybackAction) -> Option<Result<()>> {
        let script = match (process_name, action) {
            ("Spotify", PlaybackAction::Play) => r#"tell application "Spotify" to play"#,
            ("Spotify", PlaybackAction::Pause) => r#"tell application "Spotify" to pause"#,
            ("Spotify", PlaybackAction::Next) => r#"tell application "Spotify" to next track"#,
            ("Spotify", PlaybackAction::Previous) => {
                r#"tell application "Spotify" to previous track"#
            }
            ("Music", PlaybackAction::Play) => r#"tell application "Music" to play"#,
            ("Music", PlaybackAction::Pause) => r#"tell application "Music" to pause"#,
            ("Music", PlaybackAction::Next) => r#"tell application "Music" to next track"#,
            ("Music", PlaybackAction::Previous) => r#"tell application "Music" to back track"#,
            _ => return None,
        };
        Some(Self::run_osascript(script).map(|_| ()))
    }
}

#[cfg(target_os = "macos")]
#[async_trait::async_trait]
impl MediaControlRepository for NativeMediaControl {
    fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
        self.notifications_available
            .then(|| self.change_tx.subscribe())
    }

    async fn playback_info(&self) -> Result<Option<PlaybackInfo>> {
        Ok(self.sample().await?.playback)
    }

    async fn playback_action(&self, action: PlaybackAction) -> Result<()> {
        let result = tokio::task::spawn_blocking(move || {
            // For Spotify / Music, prefer direct AppleScript (no Accessibility
            // permission required and more reliable than media-key simulation).
            if let Some((process_name, _)) = Self::detect_running_media_app() {
                if let Some(result) = Self::app_playback_action(process_name, &action) {
                    return result;
                }
            }

            // Fallback: system-wide media-key simulation
            const KEY_PLAY: u32 = 16;
            const KEY_NEXT: u32 = 17;
            const KEY_PREVIOUS: u32 = 18;
            const KEY_FAST: u32 = 19;
            const KEY_REWIND: u32 = 20;

            let key = match action {
                PlaybackAction::Play | PlaybackAction::Pause => KEY_PLAY,
                PlaybackAction::Next => KEY_NEXT,
                PlaybackAction::Previous => KEY_PREVIOUS,
                PlaybackAction::SeekForward => KEY_FAST,
                PlaybackAction::SeekBackward => KEY_REWIND,
            };

            if let Err(e) = Self::send_media_key(key) {
                tracing::warn!(%e, "media key simulation failed");
                return Err(e);
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?;
        if result.is_ok() {
            self.invalidate_sample();
        }
        result
    }

    async fn volume_info(&self) -> Result<VolumeInfo> {
        Ok(self.sample().await?.volume)
    }

    async fn set_system_volume(&self, volume: u8) -> Result<()> {
        let result = tokio::task::spawn_blocking(move || {
            let vol = volume.min(100);
            Self::run_osascript(&format!("set volume output volume {vol}"))?;
            Ok(())
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?;
        if result.is_ok() {
            self.invalidate_sample();
        }
        result
    }

    async fn set_system_muted(&self, muted: bool) -> Result<()> {
        let result = tokio::task::spawn_blocking(move || {
            let clause = if muted {
                "with output muted"
            } else {
                "without output muted"
            };
            Self::run_osascript(&format!("set volume {clause}"))?;
            Ok(())
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?;
        if result.is_ok() {
            self.invalidate_sample();
        }
        result
    }

    async fn app_volumes(&self) -> Result<Vec<AppVolume>> {
        Ok(vec![])
    }

    async fn set_app_volume(&self, _app_name: &str, _volume: u8) -> Result<()> {
        Err(Error::NotSupported(
            "Per-app volume not supported natively on macOS".into(),
        ))
    }

    async fn is_microphone_active(&self) -> Result<bool> {
        Ok(self.sample().await?.microphone_active)
    }

    async fn set_microphone_active(&self, active: bool) -> Result<()> {
        let result = tokio::task::spawn_blocking(move || {
            let vol = if active { 75 } else { 0 };
            Self::run_osascript(&format!("set volume input volume {vol}"))?;
            Ok(())
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?;
        if result.is_ok() {
            self.invalidate_sample();
        }
        result
    }

    async fn is_dnd_active(&self) -> Result<bool> {
        Ok(self.sample().await?.dnd_active)
    }

    async fn set_dnd_active(&self, _active: bool) -> Result<()> {
        Err(Error::NotSupported(
            "Programmatic DND toggle requires Shortcuts app on macOS".into(),
        ))
    }
}

#[cfg(target_os = "macos")]
mod mac_media_events {
    use std::collections::HashSet;
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{
        NSWorkspace, NSWorkspaceDidActivateApplicationNotification,
        NSWorkspaceDidLaunchApplicationNotification,
        NSWorkspaceDidTerminateApplicationNotification,
    };
    use objc2_foundation::{
        NSDistributedNotificationCenter, NSNotification, NSNotificationName, NSOperationQueue,
        NSString,
    };

    const SYSTEM_AUDIO_OBJECT: u32 = 1;
    const GLOBAL_SCOPE: u32 = fourcc(*b"glob");
    const MASTER_ELEMENT: u32 = 0;
    const WILDCARD_PROPERTY: u32 = fourcc(*b"****");
    const WILDCARD_ELEMENT: u32 = u32::MAX;
    const DEFAULT_OUTPUT_DEVICE: u32 = fourcc(*b"dOut");
    const DEFAULT_INPUT_DEVICE: u32 = fourcc(*b"dIn ");

    const fn fourcc(bytes: [u8; 4]) -> u32 {
        u32::from_be_bytes(bytes)
    }

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    type AudioObjectPropertyListener =
        unsafe extern "C" fn(u32, u32, *const AudioObjectPropertyAddress, *mut c_void) -> i32;

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectAddPropertyListener(
            object_id: u32,
            address: *const AudioObjectPropertyAddress,
            listener: AudioObjectPropertyListener,
            client_data: *mut c_void,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object_id: u32,
            address: *const AudioObjectPropertyAddress,
            qualifier_data_size: u32,
            qualifier_data: *const c_void,
            data_size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopRun();
    }

    struct CoreAudioEvents {
        change_tx: tokio::sync::broadcast::Sender<()>,
        registered_devices: Mutex<HashSet<u32>>,
    }

    impl CoreAudioEvents {
        fn signal(&self) {
            let _ = self.change_tx.send(());
        }

        fn install(self: &Arc<Self>) -> bool {
            let mut installed = false;
            for selector in [DEFAULT_OUTPUT_DEVICE, DEFAULT_INPUT_DEVICE] {
                let address = AudioObjectPropertyAddress {
                    selector,
                    scope: GLOBAL_SCOPE,
                    element: MASTER_ELEMENT,
                };
                installed |= unsafe {
                    AudioObjectAddPropertyListener(
                        SYSTEM_AUDIO_OBJECT,
                        &address,
                        audio_property_changed,
                        Arc::as_ptr(self) as *mut c_void,
                    ) == 0
                };
            }
            self.refresh_default_devices();
            installed
        }

        fn default_device(selector: u32) -> Option<u32> {
            let address = AudioObjectPropertyAddress {
                selector,
                scope: GLOBAL_SCOPE,
                element: MASTER_ELEMENT,
            };
            let mut device = 0_u32;
            let mut size = std::mem::size_of::<u32>() as u32;
            let status = unsafe {
                AudioObjectGetPropertyData(
                    SYSTEM_AUDIO_OBJECT,
                    &address,
                    0,
                    std::ptr::null(),
                    &mut size,
                    &mut device as *mut u32 as *mut c_void,
                )
            };
            (status == 0 && device != 0).then_some(device)
        }

        fn refresh_default_devices(&self) {
            let devices = [
                Self::default_device(DEFAULT_OUTPUT_DEVICE),
                Self::default_device(DEFAULT_INPUT_DEVICE),
            ]
            .into_iter()
            .flatten()
            .collect::<HashSet<_>>();
            for device in devices {
                if self
                    .registered_devices
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .contains(&device)
                {
                    continue;
                }
                let address = AudioObjectPropertyAddress {
                    selector: WILDCARD_PROPERTY,
                    scope: WILDCARD_PROPERTY,
                    element: WILDCARD_ELEMENT,
                };
                if unsafe {
                    AudioObjectAddPropertyListener(
                        device,
                        &address,
                        audio_property_changed,
                        self as *const Self as *mut c_void,
                    ) == 0
                } {
                    self.registered_devices
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .insert(device);
                }
            }
        }
    }

    unsafe extern "C" fn audio_property_changed(
        object_id: u32,
        _address_count: u32,
        _addresses: *const AudioObjectPropertyAddress,
        client_data: *mut c_void,
    ) -> i32 {
        let Some(events) = (client_data as *const CoreAudioEvents).as_ref() else {
            return 0;
        };
        events.signal();
        if object_id == SYSTEM_AUDIO_OBJECT {
            events.refresh_default_devices();
        }
        0
    }

    fn start_core_audio(change_tx: tokio::sync::broadcast::Sender<()>) -> bool {
        let events = Arc::new(CoreAudioEvents {
            change_tx,
            registered_devices: Mutex::new(HashSet::new()),
        });
        if !events.install() {
            return false;
        }
        // CoreAudio owns callbacks for the process lifetime. Keep the callback
        // context alive for the same lifetime.
        let _ = Arc::into_raw(events);
        true
    }

    fn start_now_playing_notifications(change_tx: tokio::sync::broadcast::Sender<()>) -> bool {
        std::thread::Builder::new()
            .name("arcrelay-media-events".into())
            .spawn(move || {
                autoreleasepool(|_| {
                    let callback_queue = NSOperationQueue::new();
                    callback_queue.setMaxConcurrentOperationCount(1);
                    let distributed = NSDistributedNotificationCenter::defaultCenter();
                    let names = [
                        NSString::from_str("com.apple.Music.playerInfo"),
                        NSString::from_str("com.apple.iTunes.playerInfo"),
                        NSString::from_str("com.spotify.client.PlaybackStateChanged"),
                    ];
                    let mut observers = Vec::new();
                    for name in &names {
                        let change_tx = change_tx.clone();
                        let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                            let _ = change_tx.send(());
                        });
                        observers.push(unsafe {
                            distributed.addObserverForName_object_queue_usingBlock(
                                Some(name),
                                None,
                                Some(&callback_queue),
                                &block,
                            )
                        });
                    }

                    let workspace = NSWorkspace::sharedWorkspace();
                    let center = workspace.notificationCenter();
                    let workspace_names: [&NSNotificationName; 3] = unsafe {
                        [
                            NSWorkspaceDidActivateApplicationNotification,
                            NSWorkspaceDidLaunchApplicationNotification,
                            NSWorkspaceDidTerminateApplicationNotification,
                        ]
                    };
                    for name in workspace_names {
                        let change_tx = change_tx.clone();
                        let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                            let _ = change_tx.send(());
                        });
                        observers.push(unsafe {
                            center.addObserverForName_object_queue_usingBlock(
                                Some(name),
                                None,
                                Some(&callback_queue),
                                &block,
                            )
                        });
                    }
                    let _run_loop = unsafe { CFRunLoopGetCurrent() };
                    let _ = change_tx.send(());
                    unsafe { CFRunLoopRun() };
                    drop(observers);
                });
            })
            .is_ok()
    }

    pub(super) fn start(change_tx: tokio::sync::broadcast::Sender<()>) -> bool {
        let audio = start_core_audio(change_tx.clone());
        let now_playing = start_now_playing_notifications(change_tx);
        audio || now_playing
    }
}

impl Default for NativeMediaControl {
    fn default() -> Self {
        Self::new()
    }
}

// ── Windows implementation ──
