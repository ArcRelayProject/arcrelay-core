//! Per-domain state coordinators that cache the latest state, assign
//! monotonically increasing revisions, and notify subscribers when
//! state changes.
//!
//! Event-capable domains are refreshed from native change notifications so
//! multiple connected clients share one OS query instead of each triggering
//! their own. Continuous metrics and backends without notifications retain a
//! shared sampling loop; event streams also use a low-frequency reconciliation
//! watchdog to recover from dropped platform notifications.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch, Mutex, Notify};

use crate::application::service::ArcRelayService;

// ── Helpers ─────────────────────────────────────────────────────────

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

// ── Generic versioned snapshot ──────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Versioned<T: Clone> {
    pub data: T,
    pub revision: u64,
    pub captured_at_ms: i64,
}

// ── Channel-specific snapshot types ─────────────────────────────────

/// Aggregated media state pushed as a single unit.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaState {
    pub playback: Option<crate::domain::media_control::PlaybackInfo>,
    pub volume: crate::domain::media_control::VolumeInfo,
    pub app_volumes: Vec<crate::domain::media_control::AppVolume>,
    pub microphone_active: bool,
    pub dnd_active: bool,
}

/// The coordinator manages all domain channels from a single place.
pub struct StateCoordinator {
    service: Arc<ArcRelayService>,

    // Revision counters (one per domain)
    system_rev: AtomicU64,
    process_rev: AtomicU64,
    media_rev: AtomicU64,
    window_rev: AtomicU64,
    clipboard_rev: AtomicU64,
    clipboard_host_revision: AtomicU64,

    // Cached latest state
    system_cache: Mutex<Option<Arc<Versioned<crate::domain::system_monitor::SystemSnapshot>>>>,
    media_cache: Mutex<Option<Arc<Versioned<MediaState>>>>,
    window_cache: Mutex<Option<Arc<Versioned<Vec<crate::domain::window_manager::WindowInfo>>>>>,
    clipboard_cache: Mutex<Option<Arc<Versioned<ClipboardState>>>>,

    // Single-flight gates prevent concurrent cache misses from duplicating
    // expensive platform sampling work.
    system_refresh_gate: Mutex<()>,
    media_refresh_gate: Mutex<()>,
    window_refresh_gate: Mutex<()>,

    // Cached space list (updated with each coherent window-state refresh)
    space_cache: Mutex<Vec<crate::domain::window_manager::SpaceInfo>>,

    // Broadcast channels for subscribers
    system_tx: watch::Sender<Option<Arc<Versioned<crate::domain::system_monitor::SystemSnapshot>>>>,
    media_tx: watch::Sender<Option<Arc<Versioned<MediaState>>>>,
    window_tx:
        watch::Sender<Option<Arc<Versioned<Vec<crate::domain::window_manager::WindowInfo>>>>>,
    clipboard_tx: watch::Sender<Option<Arc<Versioned<ClipboardState>>>>,

    // Subscription notifications let idle loops sleep without periodic
    // receiver-count polling while still producing a first snapshot promptly.
    system_interest: Notify,
    media_interest: Notify,
    window_interest: Notify,
    clipboard_interest: Notify,
}

/// Aggregated clipboard state pushed as a single unit.
#[derive(Debug, Clone)]
pub struct ClipboardState {
    pub history: Vec<crate::domain::clipboard::ClipboardSummary>,
    pub policy: crate::domain::clipboard::ClipboardPolicy,
}

impl StateCoordinator {
    pub fn new(service: Arc<ArcRelayService>) -> Arc<Self> {
        let (system_tx, _) = watch::channel(None);
        let (media_tx, _) = watch::channel(None);
        let (window_tx, _) = watch::channel(None);
        let (clipboard_tx, _) = watch::channel(None);

        Arc::new(Self {
            service,
            system_rev: AtomicU64::new(0),
            process_rev: AtomicU64::new(0),
            media_rev: AtomicU64::new(0),
            window_rev: AtomicU64::new(0),
            clipboard_rev: AtomicU64::new(0),
            clipboard_host_revision: AtomicU64::new(0),
            system_cache: Mutex::new(None),
            media_cache: Mutex::new(None),
            window_cache: Mutex::new(None),
            clipboard_cache: Mutex::new(None),
            system_refresh_gate: Mutex::new(()),
            media_refresh_gate: Mutex::new(()),
            window_refresh_gate: Mutex::new(()),
            space_cache: Mutex::new(Vec::new()),
            system_tx,
            media_tx,
            window_tx,
            clipboard_tx,
            system_interest: Notify::new(),
            media_interest: Notify::new(),
            window_interest: Notify::new(),
            clipboard_interest: Notify::new(),
        })
    }

    /// Start the shared event/sampling coordinators. Call once after construction.
    pub fn start(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move { this.system_loop().await });
        let this = Arc::clone(self);
        tokio::spawn(async move { this.media_loop().await });
        let this = Arc::clone(self);
        tokio::spawn(async move { this.window_focus_loop().await });
        let this = Arc::clone(self);
        tokio::spawn(async move { this.clipboard_loop().await });
    }

    // ── System polling (every 3 s) ─────────────────────────────────

    async fn system_loop(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            wait_for_interest(&self.system_tx, &self.system_interest).await;
            interval.tick().await;
            match self.refresh_system().await {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(%e, "system snapshot error");
                }
            }
        }
    }

    // ── Media notifications, with sampling fallback ────────────────

    async fn media_loop(&self) {
        if let Some(mut changes) = self.service.media_control.subscribe_changes() {
            let mut reconciliation = tokio::time::interval(Duration::from_secs(60));
            reconciliation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            reconciliation.tick().await;
            loop {
                tokio::select! {
                    changed = wait_for_change(&mut changes, Duration::from_millis(80)) => {
                        if !changed {
                            break;
                        }
                        if self.media_tx.receiver_count() > 0 {
                            self.refresh_media(false).await;
                        }
                    }
                    _ = self.media_interest.notified() => {
                        if self.media_tx.receiver_count() > 0 {
                            self.refresh_media(false).await;
                        }
                    }
                    _ = reconciliation.tick() => {
                        if self.media_tx.receiver_count() > 0 {
                            self.refresh_media(false).await;
                        }
                    }
                }
            }
            tracing::warn!("media notification stream closed; falling back to sampling");
        }

        let mut delay = Duration::from_secs(1);
        let mut unchanged_samples = 0_u8;
        let mut last_revision = 0_u64;
        loop {
            if self.media_tx.receiver_count() == 0 {
                wait_for_interest(&self.media_tx, &self.media_interest).await;
                delay = Duration::from_secs(1);
                unchanged_samples = 0;
            }
            tokio::time::sleep(delay).await;
            let value = self.refresh_media(false).await;
            let changed = value.revision != last_revision;
            last_revision = value.revision;
            unchanged_samples = if changed {
                0
            } else {
                unchanged_samples.saturating_add(1)
            };
            let playing = value
                .data
                .playback
                .as_ref()
                .is_some_and(|playback| playback.is_playing);
            delay = if playing {
                Duration::from_secs(1)
            } else if unchanged_samples >= 8 {
                Duration::from_secs(10)
            } else if unchanged_samples >= 3 {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(2)
            };
        }
    }

    // ── Window notifications, with sampling fallback ───────────────

    async fn window_focus_loop(&self) {
        if let Some(mut changes) = self.service.window_manager.subscribe_changes() {
            // NSWorkspace covers application and Space lifecycle changes. A
            // lightweight CoreGraphics reconciliation catches window/title
            // changes inside an already-running app without subscribing to
            // every process through synchronous Accessibility IPC.
            let mut reconciliation = tokio::time::interval(Duration::from_secs(1));
            reconciliation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            reconciliation.tick().await;
            loop {
                tokio::select! {
                    changed = wait_for_change(&mut changes, Duration::from_millis(120)) => {
                        if !changed {
                            break;
                        }
                        if self.window_tx.receiver_count() > 0 {
                            if let Err(error) = self.refresh_window_state(false).await {
                                tracing::warn!(%error, "window notification refresh error");
                            }
                        }
                    }
                    _ = self.window_interest.notified() => {
                        if self.window_tx.receiver_count() > 0 {
                            if let Err(error) = self.refresh_window_state(true).await {
                                tracing::warn!(%error, "initial window snapshot error");
                            }
                        }
                    }
                    _ = reconciliation.tick() => {
                        if self.window_tx.receiver_count() > 0 {
                            if let Err(error) = self.refresh_window_state(false).await {
                                tracing::warn!(%error, "window reconciliation error");
                            }
                        }
                    }
                }
            }
            tracing::warn!("window notification stream closed; falling back to sampling");
        }

        let mut interval = tokio::time::interval(Duration::from_millis(750));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            wait_for_interest(&self.window_tx, &self.window_interest).await;
            interval.tick().await;

            if let Err(error) = self.refresh_window_state(false).await {
                tracing::warn!(%error, "window meta error");
            }
        }
    }

    // ── Clipboard notifications, with sampling fallback ───────────

    async fn clipboard_loop(&self) {
        if let Some(mut changes) = self.service.clipboard.subscribe_changes() {
            let mut reconciliation = tokio::time::interval(Duration::from_secs(60));
            reconciliation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            reconciliation.tick().await;
            loop {
                tokio::select! {
                    changed = wait_for_change(&mut changes, Duration::from_millis(35)) => {
                        if !changed {
                            break;
                        }
                        if self.clipboard_tx.receiver_count() > 0 {
                            if let Err(error) = self.refresh_clipboard(false).await {
                                tracing::warn!(%error, "clipboard notification refresh error");
                            }
                        }
                    }
                    _ = self.clipboard_interest.notified() => {
                        if self.clipboard_tx.receiver_count() > 0 {
                            if let Err(error) = self.refresh_clipboard(true).await {
                                tracing::warn!(%error, "initial clipboard snapshot error");
                            }
                        }
                    }
                    _ = reconciliation.tick() => {
                        if self.clipboard_tx.receiver_count() > 0 {
                            if let Err(error) = self.refresh_clipboard(false).await {
                                tracing::warn!(%error, "clipboard reconciliation error");
                            }
                        }
                    }
                }
            }
            tracing::warn!("clipboard notification stream closed; falling back to sampling");
        }

        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            wait_for_interest(&self.clipboard_tx, &self.clipboard_interest).await;
            interval.tick().await;
            if let Err(error) = self.refresh_clipboard(false).await {
                tracing::warn!(%error, "clipboard snapshot error");
            }
        }
    }

    // ── On-demand queries (not cached, used for process & clipboard) ──

    pub async fn query_process_list(
        &self,
        sort_by: crate::domain::process::ProcessSortBy,
    ) -> crate::error::Result<Versioned<Vec<crate::domain::process::ProcessInfo>>> {
        let data = self.service.process.list(sort_by).await?;
        let rev = self.process_rev.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(Versioned {
            data,
            revision: rev,
            captured_at_ms: now_ms(),
        })
    }

    pub async fn current_system_snapshot(
        &self,
    ) -> crate::error::Result<Arc<Versioned<crate::domain::system_monitor::SystemSnapshot>>> {
        if let Some(value) = self.system_cache.lock().await.clone() {
            if now_ms().saturating_sub(value.captured_at_ms) <= 3_000 {
                return Ok(value);
            }
        }
        self.refresh_system().await
    }

    pub async fn current_media_state(&self) -> Arc<Versioned<MediaState>> {
        if let Some(value) = self.media_cache.lock().await.clone() {
            if now_ms().saturating_sub(value.captured_at_ms) <= 1_000 {
                return value;
            }
        }
        self.refresh_media(false).await
    }

    pub async fn current_focused_app_name(&self) -> Option<String> {
        let cache = self.window_cache.lock().await;
        let cached = cache.as_ref().and_then(|value| {
            (now_ms().saturating_sub(value.captured_at_ms) <= 1_500)
                .then(|| value.data.iter().find(|window| window.is_focused))
                .flatten()
                .map(|window| window.app_name.clone())
        });
        drop(cache);
        match cached {
            Some(value) => Some(value),
            None => self.service.window_manager.focused_app_name().await.ok(),
        }
    }

    async fn refresh_system(
        &self,
    ) -> crate::error::Result<Arc<Versioned<crate::domain::system_monitor::SystemSnapshot>>> {
        let _refresh = self.system_refresh_gate.lock().await;
        if let Some(value) = self.system_cache.lock().await.clone() {
            if now_ms().saturating_sub(value.captured_at_ms) <= 250 {
                return Ok(value);
            }
        }
        let snap = self.service.system_monitor.snapshot().await?;
        let rev = self.system_rev.fetch_add(1, Ordering::Relaxed) + 1;
        let versioned = Arc::new(Versioned {
            data: snap,
            revision: rev,
            captured_at_ms: now_ms(),
        });
        *self.system_cache.lock().await = Some(versioned.clone());
        self.system_tx.send_replace(Some(versioned.clone()));
        Ok(versioned)
    }

    async fn collect_media(&self) -> MediaState {
        let mc = &self.service.media_control;
        let (playback, volume, app_volumes, mic, dnd) = tokio::join!(
            mc.playback_info(),
            mc.volume_info(),
            mc.app_volumes(),
            mc.is_microphone_active(),
            mc.is_dnd_active(),
        );
        MediaState {
            playback: playback.unwrap_or(None),
            volume: volume.unwrap_or(crate::domain::media_control::VolumeInfo {
                system_volume: 0,
                is_muted: false,
            }),
            app_volumes: app_volumes.unwrap_or_default(),
            microphone_active: mic.unwrap_or(false),
            dnd_active: dnd.unwrap_or(false),
        }
    }

    async fn refresh_media(&self, force_broadcast: bool) -> Arc<Versioned<MediaState>> {
        let _refresh = self.media_refresh_gate.lock().await;
        if !force_broadcast {
            if let Some(value) = self.media_cache.lock().await.clone() {
                if now_ms().saturating_sub(value.captured_at_ms) <= 250 {
                    return value;
                }
            }
        }
        let state = self.collect_media().await;
        let mut cache = self.media_cache.lock().await;
        if !force_broadcast {
            if let Some(current) = cache.as_ref() {
                if current.data == state {
                    let fresh = Arc::new(Versioned {
                        data: current.data.clone(),
                        revision: current.revision,
                        captured_at_ms: now_ms(),
                    });
                    *cache = Some(fresh.clone());
                    return fresh;
                }
            }
        }
        let rev = self.media_rev.fetch_add(1, Ordering::Relaxed) + 1;
        let versioned = Arc::new(Versioned {
            data: state,
            revision: rev,
            captured_at_ms: now_ms(),
        });
        *cache = Some(versioned.clone());
        drop(cache);
        self.media_tx.send_replace(Some(versioned.clone()));
        versioned
    }

    async fn refresh_window_state(&self, force_broadcast: bool) -> crate::error::Result<()> {
        let _refresh = self.window_refresh_gate.lock().await;
        let sample = self.service.window_manager.sample_window_state().await?;
        let spaces_changed = *self.space_cache.lock().await != sample.spaces;
        let windows_changed = self
            .window_cache
            .lock()
            .await
            .as_ref()
            .is_none_or(|current| current.data != sample.windows);
        *self.space_cache.lock().await = sample.spaces;
        if !force_broadcast && !spaces_changed && !windows_changed {
            return Ok(());
        }
        let rev = self.window_rev.fetch_add(1, Ordering::Relaxed) + 1;
        let versioned = Arc::new(Versioned {
            data: sample.windows,
            revision: rev,
            captured_at_ms: now_ms(),
        });
        *self.window_cache.lock().await = Some(versioned.clone());
        self.window_tx.send_replace(Some(versioned));
        Ok(())
    }

    async fn refresh_clipboard(&self, force_broadcast: bool) -> crate::error::Result<()> {
        let host_revision = self.service.clipboard.revision().await?;
        if !force_broadcast && self.clipboard_host_revision.load(Ordering::Relaxed) == host_revision
        {
            return Ok(());
        }
        let history = self
            .service
            .clipboard
            .history(crate::domain::clipboard::ClipboardQuery::recent(20))
            .await?
            .entries;
        let policy = self.service.clipboard.policy().await?;
        self.clipboard_host_revision
            .store(host_revision, Ordering::Relaxed);
        let rev = self.clipboard_rev.fetch_add(1, Ordering::Relaxed) + 1;
        let versioned = Arc::new(Versioned {
            data: ClipboardState { history, policy },
            revision: rev,
            captured_at_ms: now_ms(),
        });
        *self.clipboard_cache.lock().await = Some(versioned.clone());
        self.clipboard_tx.send_replace(Some(versioned));
        Ok(())
    }

    pub fn subscribe_system(
        &self,
    ) -> watch::Receiver<Option<Arc<Versioned<crate::domain::system_monitor::SystemSnapshot>>>>
    {
        let receiver = self.system_tx.subscribe();
        self.system_interest.notify_one();
        receiver
    }

    pub fn subscribe_media(&self) -> watch::Receiver<Option<Arc<Versioned<MediaState>>>> {
        let receiver = self.media_tx.subscribe();
        self.media_interest.notify_one();
        receiver
    }

    pub fn subscribe_windows(
        &self,
    ) -> watch::Receiver<Option<Arc<Versioned<Vec<crate::domain::window_manager::WindowInfo>>>>>
    {
        let receiver = self.window_tx.subscribe();
        self.window_interest.notify_one();
        receiver
    }

    pub fn subscribe_clipboard(&self) -> watch::Receiver<Option<Arc<Versioned<ClipboardState>>>> {
        let receiver = self.clipboard_tx.subscribe();
        self.clipboard_interest.notify_one();
        receiver
    }

    pub async fn latest_system(
        &self,
    ) -> Option<Arc<Versioned<crate::domain::system_monitor::SystemSnapshot>>> {
        self.system_cache.lock().await.clone()
    }

    pub async fn latest_media(&self) -> Option<Arc<Versioned<MediaState>>> {
        self.media_cache.lock().await.clone()
    }

    pub async fn latest_windows(
        &self,
    ) -> Option<Arc<Versioned<Vec<crate::domain::window_manager::WindowInfo>>>> {
        self.window_cache.lock().await.clone()
    }

    pub async fn latest_clipboard(&self) -> Option<Arc<Versioned<ClipboardState>>> {
        self.clipboard_cache.lock().await.clone()
    }

    pub async fn spaces(&self) -> Vec<crate::domain::window_manager::SpaceInfo> {
        self.space_cache.lock().await.clone()
    }

    /// Expose the inner service for command handlers that need direct access.
    pub fn service(&self) -> &ArcRelayService {
        &self.service
    }

    /// Force an immediate window-list push (e.g. after a space switch).
    /// Fetches current window metadata and broadcasts it to all subscribers.
    pub async fn force_push_windows(&self) {
        if let Err(error) = self.refresh_window_state(true).await {
            tracing::warn!(%error, "force_push_windows error");
        }
    }

    /// Force an immediate media state push (e.g. after a media command).
    /// Fetches current media state and broadcasts it to all subscribers.
    pub async fn force_push_media(&self) {
        self.refresh_media(true).await;
    }
}

async fn wait_for_interest<T>(sender: &watch::Sender<T>, interest: &Notify) {
    loop {
        // Register before checking the count so a subscription created between
        // the check and await cannot be missed.
        let notified = interest.notified();
        if sender.receiver_count() > 0 {
            return;
        }
        notified.await;
    }
}

async fn wait_for_change(receiver: &mut broadcast::Receiver<()>, debounce: Duration) -> bool {
    match receiver.recv().await {
        Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
        Err(broadcast::error::RecvError::Closed) => return false,
    }
    tokio::time::sleep(debounce).await;
    loop {
        match receiver.try_recv() {
            Ok(()) | Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Empty) => return true,
            Err(broadcast::error::TryRecvError::Closed) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_change_coalesces_a_notification_burst() {
        let (sender, mut receiver) = broadcast::channel(8);
        sender.send(()).unwrap();
        sender.send(()).unwrap();
        sender.send(()).unwrap();

        assert!(wait_for_change(&mut receiver, Duration::from_millis(1)).await);
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn wait_for_change_reports_a_closed_event_source() {
        let (sender, mut receiver) = broadcast::channel(1);
        drop(sender);

        assert!(!wait_for_change(&mut receiver, Duration::ZERO).await);
    }

    #[tokio::test]
    async fn wait_for_interest_wakes_only_after_a_subscriber_arrives() {
        let (sender, initial_receiver) = watch::channel(0_u8);
        drop(initial_receiver);
        let interest = Notify::new();
        let waiting = wait_for_interest(&sender, &interest);
        tokio::pin!(waiting);

        assert!(tokio::time::timeout(Duration::from_millis(5), &mut waiting)
            .await
            .is_err());

        let _receiver = sender.subscribe();
        interest.notify_one();

        tokio::time::timeout(Duration::from_millis(50), &mut waiting)
            .await
            .expect("subscriber notification should wake the coordinator");
    }
}
