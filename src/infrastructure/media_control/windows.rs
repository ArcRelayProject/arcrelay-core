use crate::domain::media_control::*;
use crate::error::{Error, Result};
#[cfg(target_os = "windows")]
pub struct NativeMediaControl {
    change_tx: tokio::sync::broadcast::Sender<()>,
    notifications_available: bool,
}

#[cfg(target_os = "windows")]
impl NativeMediaControl {
    pub fn new() -> Self {
        let (change_tx, _) = tokio::sync::broadcast::channel(64);
        let notifications_available = win_media_events::start(change_tx.clone());
        Self {
            change_tx,
            notifications_available,
        }
    }

    /// Get the default IAudioEndpointVolume for either output (render=true)
    /// or input/microphone (render=false) via COM.
    fn get_endpoint_volume(
        render: bool,
    ) -> Result<windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume> {
        use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
        use windows::Win32::Media::Audio::*;
        use windows::Win32::System::Com::*;

        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| Error::MediaControl(format!("COM create enumerator: {e}")))?;
            let data_flow = if render { eRender } else { eCapture };
            let device = enumerator
                .GetDefaultAudioEndpoint(data_flow, eConsole)
                .map_err(|e| Error::MediaControl(format!("GetDefaultAudioEndpoint: {e}")))?;
            let endpoint: IAudioEndpointVolume = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| Error::MediaControl(format!("Activate endpoint: {e}")))?;
            Ok(endpoint)
        }
    }
}

#[cfg(target_os = "windows")]
#[async_trait::async_trait]
impl MediaControlRepository for NativeMediaControl {
    fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
        self.notifications_available
            .then(|| self.change_tx.subscribe())
    }

    async fn playback_info(&self) -> Result<Option<PlaybackInfo>> {
        tokio::task::spawn_blocking(|| {
            use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;

            let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                .map_err(|e| Error::MediaControl(format!("GSMTC RequestAsync: {e}")))?
                .get()
                .map_err(|e| Error::MediaControl(format!("GSMTC get manager: {e}")))?;

            let session = match manager.GetCurrentSession() {
                Ok(s) => s,
                Err(_) => return Ok(None), // no active media session
            };

            let info = session
                .TryGetMediaPropertiesAsync()
                .map_err(|e| Error::MediaControl(format!("GetMediaProperties: {e}")))?
                .get()
                .map_err(|e| Error::MediaControl(format!("GetMediaProperties get: {e}")))?;

            let title = info.Title().unwrap_or_default().to_string_lossy();
            let artist = info.Artist().unwrap_or_default().to_string_lossy();

            if title.is_empty() && artist.is_empty() {
                return Ok(None);
            }

            let source_app = info.AlbumArtist().unwrap_or_default().to_string_lossy();

            // Get playback status
            let playback = session
                .GetPlaybackInfo()
                .map_err(|e| Error::MediaControl(format!("GetPlaybackInfo: {e}")))?;

            use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus;
            let is_playing = playback.PlaybackStatus()
                == Ok(GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);

            // Get timeline position/duration
            let timeline = session.GetTimelineProperties();
            let (position_secs, duration_secs) = match timeline {
                Ok(t) => {
                    let pos = t.Position().unwrap_or_default();
                    let dur = t.EndTime().unwrap_or_default();
                    (
                        pos.Duration as f64 / 10_000_000.0,
                        dur.Duration as f64 / 10_000_000.0,
                    )
                }
                Err(_) => (0.0, 0.0),
            };

            // Attempt to get source app name from session
            let source = session
                .SourceAppUserModelId()
                .map(|s| s.to_string_lossy())
                .unwrap_or_else(|_| source_app);

            Ok(Some(PlaybackInfo {
                title,
                artist,
                source_app: source,
                position_secs,
                duration_secs,
                is_playing,
            }))
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?
    }

    async fn playback_action(&self, action: PlaybackAction) -> Result<()> {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;

        let vk = match action {
            PlaybackAction::Play | PlaybackAction::Pause => VK_MEDIA_PLAY_PAUSE,
            PlaybackAction::Next => VK_MEDIA_NEXT_TRACK,
            PlaybackAction::Previous => VK_MEDIA_PREV_TRACK,
            PlaybackAction::SeekForward | PlaybackAction::SeekBackward => {
                return Err(Error::NotSupported(
                    "Seek not supported via media keys".into(),
                ));
            }
        };

        unsafe {
            let inputs = [
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: vk,
                            wScan: 0,
                            dwFlags: KEYBD_EVENT_FLAGS(0),
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                },
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: vk,
                            wScan: 0,
                            dwFlags: KEYEVENTF_KEYUP,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                },
            ];
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
        Ok(())
    }

    async fn volume_info(&self) -> Result<VolumeInfo> {
        tokio::task::spawn_blocking(|| {
            let endpoint = Self::get_endpoint_volume(true)?;
            unsafe {
                let level = endpoint
                    .GetMasterVolumeLevelScalar()
                    .map_err(|e| Error::MediaControl(format!("GetVolume: {e}")))?;
                let muted = endpoint
                    .GetMute()
                    .map_err(|e| Error::MediaControl(format!("GetMute: {e}")))?;
                Ok(VolumeInfo {
                    system_volume: (level * 100.0).round() as u8,
                    is_muted: muted.as_bool(),
                })
            }
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?
    }

    async fn set_system_volume(&self, volume: u8) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let endpoint = Self::get_endpoint_volume(true)?;
            let level = (volume.min(100) as f32) / 100.0;
            unsafe {
                endpoint
                    .SetMasterVolumeLevelScalar(level, std::ptr::null())
                    .map_err(|e| Error::MediaControl(format!("SetVolume: {e}")))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?
    }

    async fn set_system_muted(&self, muted: bool) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let endpoint = Self::get_endpoint_volume(true)?;
            unsafe {
                endpoint
                    .SetMute(muted, std::ptr::null())
                    .map_err(|e| Error::MediaControl(format!("SetMute: {e}")))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?
    }

    async fn app_volumes(&self) -> Result<Vec<AppVolume>> {
        Ok(vec![])
    }

    async fn set_app_volume(&self, _app_name: &str, _volume: u8) -> Result<()> {
        Err(Error::NotSupported(
            "Per-app volume not yet implemented on Windows".into(),
        ))
    }

    async fn is_microphone_active(&self) -> Result<bool> {
        tokio::task::spawn_blocking(|| {
            let endpoint = Self::get_endpoint_volume(false)?;
            unsafe {
                let muted = endpoint
                    .GetMute()
                    .map_err(|e| Error::MediaControl(format!("GetMicMute: {e}")))?;
                // Mic is "active" when it is NOT muted
                Ok(!muted.as_bool())
            }
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?
    }

    async fn set_microphone_active(&self, active: bool) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let endpoint = Self::get_endpoint_volume(false)?;
            unsafe {
                // active=true → unmute, active=false → mute
                endpoint
                    .SetMute(!active, std::ptr::null())
                    .map_err(|e| Error::MediaControl(format!("SetMicMute: {e}")))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::MediaControl(format!("task join error: {e}")))?
    }

    async fn is_dnd_active(&self) -> Result<bool> {
        Ok(false)
    }

    async fn set_dnd_active(&self, _active: bool) -> Result<()> {
        Err(Error::NotSupported(
            "DND toggle not supported on Windows".into(),
        ))
    }
}

#[cfg(target_os = "windows")]
mod win_media_events {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use windows::Foundation::TypedEventHandler;
    use windows::Media::Control::{
        CurrentSessionChangedEventArgs, GlobalSystemMediaTransportControlsSession,
        GlobalSystemMediaTransportControlsSessionManager, MediaPropertiesChangedEventArgs,
        PlaybackInfoChangedEventArgs, SessionsChangedEventArgs, TimelinePropertiesChangedEventArgs,
    };
    use windows::Win32::Media::Audio::Endpoints::{
        IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Vtbl,
    };
    use windows::Win32::Media::Audio::AUDIO_VOLUME_NOTIFICATION_DATA;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
    use windows_core::{IUnknown, IUnknown_Vtbl, Interface, GUID, HRESULT};

    struct SessionSubscription {
        session: GlobalSystemMediaTransportControlsSession,
        playback: windows::Foundation::EventRegistrationToken,
        media: windows::Foundation::EventRegistrationToken,
        timeline: windows::Foundation::EventRegistrationToken,
    }

    impl Drop for SessionSubscription {
        fn drop(&mut self) {
            let _ = self.session.RemovePlaybackInfoChanged(self.playback);
            let _ = self.session.RemoveMediaPropertiesChanged(self.media);
            let _ = self.session.RemoveTimelinePropertiesChanged(self.timeline);
        }
    }

    fn attach_current_session(
        manager: &GlobalSystemMediaTransportControlsSessionManager,
        change_tx: &tokio::sync::broadcast::Sender<()>,
    ) -> Option<SessionSubscription> {
        let session = manager.GetCurrentSession().ok()?;
        let playback_tx = change_tx.clone();
        let playback = session
            .PlaybackInfoChanged(&TypedEventHandler::<
                GlobalSystemMediaTransportControlsSession,
                PlaybackInfoChangedEventArgs,
            >::new(move |_, _| {
                let _ = playback_tx.send(());
                Ok(())
            }))
            .ok()?;
        let media_tx = change_tx.clone();
        let media = session
            .MediaPropertiesChanged(&TypedEventHandler::<
                GlobalSystemMediaTransportControlsSession,
                MediaPropertiesChangedEventArgs,
            >::new(move |_, _| {
                let _ = media_tx.send(());
                Ok(())
            }))
            .ok()?;
        let timeline_tx = change_tx.clone();
        let timeline = session
            .TimelinePropertiesChanged(&TypedEventHandler::<
                GlobalSystemMediaTransportControlsSession,
                TimelinePropertiesChangedEventArgs,
            >::new(move |_, _| {
                let _ = timeline_tx.send(());
                Ok(())
            }))
            .ok()?;
        Some(SessionSubscription {
            session,
            playback,
            media,
            timeline,
        })
    }

    fn start_session_events(change_tx: tokio::sync::broadcast::Sender<()>) -> bool {
        std::thread::Builder::new()
            .name("arcrelay-media-session-events".into())
            .spawn(move || unsafe {
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
                let Ok(manager) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                    .and_then(|operation| operation.get())
                else {
                    tracing::warn!("failed to initialize Windows media session notifications");
                    return;
                };
                let session = Arc::new(Mutex::new(attach_current_session(&manager, &change_tx)));

                let manager_for_current = manager.clone();
                let session_for_current = Arc::clone(&session);
                let current_tx = change_tx.clone();
                let current_handler = TypedEventHandler::<
                    GlobalSystemMediaTransportControlsSessionManager,
                    CurrentSessionChangedEventArgs,
                >::new(move |_, _| {
                    *session_for_current
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) =
                        attach_current_session(&manager_for_current, &current_tx);
                    let _ = current_tx.send(());
                    Ok(())
                });
                let Ok(current_token) = manager.CurrentSessionChanged(&current_handler) else {
                    tracing::warn!("failed to subscribe to Windows current media session changes");
                    return;
                };

                let manager_for_sessions = manager.clone();
                let session_for_sessions = Arc::clone(&session);
                let sessions_tx = change_tx.clone();
                let sessions_handler = TypedEventHandler::<
                    GlobalSystemMediaTransportControlsSessionManager,
                    SessionsChangedEventArgs,
                >::new(move |_, _| {
                    *session_for_sessions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) =
                        attach_current_session(&manager_for_sessions, &sessions_tx);
                    let _ = sessions_tx.send(());
                    Ok(())
                });
                let Ok(_sessions_token) = manager.SessionsChanged(&sessions_handler) else {
                    let _ = manager.RemoveCurrentSessionChanged(current_token);
                    tracing::warn!("failed to subscribe to Windows media session list changes");
                    return;
                };
                let _ = change_tx.send(());
                loop {
                    std::thread::park();
                }
            })
            .is_ok()
    }

    #[repr(C)]
    struct EndpointVolumeCallbackObject {
        vtable: *const IAudioEndpointVolumeCallback_Vtbl,
        references: AtomicU32,
        change_tx: tokio::sync::broadcast::Sender<()>,
    }

    unsafe extern "system" fn volume_query_interface(
        this: *mut core::ffi::c_void,
        interface_id: *const GUID,
        interface: *mut *mut core::ffi::c_void,
    ) -> HRESULT {
        if interface_id.is_null() || interface.is_null() {
            return HRESULT(-2147467261); // E_POINTER
        }
        *interface = if *interface_id == IAudioEndpointVolumeCallback::IID
            || *interface_id == IUnknown::IID
        {
            this
        } else {
            std::ptr::null_mut()
        };
        if (*interface).is_null() {
            HRESULT(-2147467262) // E_NOINTERFACE
        } else {
            volume_add_ref(this);
            HRESULT(0)
        }
    }

    unsafe extern "system" fn volume_add_ref(this: *mut core::ffi::c_void) -> u32 {
        let callback = &*(this as *const EndpointVolumeCallbackObject);
        callback.references.fetch_add(1, Ordering::Relaxed) + 1
    }

    unsafe extern "system" fn volume_release(this: *mut core::ffi::c_void) -> u32 {
        let callback = &*(this as *const EndpointVolumeCallbackObject);
        let remaining = callback.references.fetch_sub(1, Ordering::Release) - 1;
        if remaining == 0 {
            std::sync::atomic::fence(Ordering::Acquire);
            drop(Box::from_raw(this as *mut EndpointVolumeCallbackObject));
        }
        remaining
    }

    unsafe extern "system" fn volume_changed(
        this: *mut core::ffi::c_void,
        _notification: *mut AUDIO_VOLUME_NOTIFICATION_DATA,
    ) -> HRESULT {
        let callback = &*(this as *const EndpointVolumeCallbackObject);
        let _ = callback.change_tx.send(());
        HRESULT(0)
    }

    static VOLUME_CALLBACK_VTABLE: IAudioEndpointVolumeCallback_Vtbl =
        IAudioEndpointVolumeCallback_Vtbl {
            base__: IUnknown_Vtbl {
                QueryInterface: volume_query_interface,
                AddRef: volume_add_ref,
                Release: volume_release,
            },
            OnNotify: volume_changed,
        };

    fn endpoint_volume_callback(
        change_tx: tokio::sync::broadcast::Sender<()>,
    ) -> IAudioEndpointVolumeCallback {
        let callback = Box::new(EndpointVolumeCallbackObject {
            vtable: &VOLUME_CALLBACK_VTABLE,
            references: AtomicU32::new(1),
            change_tx,
        });
        unsafe {
            IAudioEndpointVolumeCallback::from_raw(Box::into_raw(callback) as *mut core::ffi::c_void)
        }
    }

    fn start_volume_events(change_tx: tokio::sync::broadcast::Sender<()>) -> bool {
        std::thread::Builder::new()
            .name("arcrelay-volume-events".into())
            .spawn(move || unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let callback = endpoint_volume_callback(change_tx.clone());
                let endpoints = [
                    super::NativeMediaControl::get_endpoint_volume(true),
                    super::NativeMediaControl::get_endpoint_volume(false),
                ]
                .into_iter()
                .filter_map(Result::ok)
                .collect::<Vec<IAudioEndpointVolume>>();
                if endpoints.is_empty() {
                    tracing::warn!("failed to initialize Windows audio endpoint notifications");
                    return;
                }
                let mut registered = false;
                for endpoint in &endpoints {
                    registered |= endpoint
                        .RegisterControlChangeNotify(Some(&callback))
                        .is_ok();
                }
                if !registered {
                    return;
                }
                let _ = change_tx.send(());
                loop {
                    std::thread::park();
                }
                #[allow(unreachable_code)]
                for endpoint in &endpoints {
                    let _ = endpoint.UnregisterControlChangeNotify(Some(&callback));
                }
            })
            .is_ok()
    }

    pub(super) fn start(change_tx: tokio::sync::broadcast::Sender<()>) -> bool {
        start_session_events(change_tx.clone()) | start_volume_events(change_tx)
    }
}

// ── Fallback for other platforms (development/CI) ──
