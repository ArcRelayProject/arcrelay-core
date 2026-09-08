use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
#[cfg(not(target_os = "macos"))]
use clipboard_rs::ClipboardContent;
use clipboard_rs::{
    common::RustImage, Clipboard, ClipboardContext, ClipboardHandler, ClipboardWatcher,
    ClipboardWatcherContext, ContentFormat,
};
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use tracing::{debug, warn};
use xxhash_rust::xxh3::Xxh3;

use crate::domain::clipboard::{
    ClipboardCaptureEvent, ClipboardCaptureOrigin, ClipboardContentKind, ClipboardImageOcr,
    ClipboardLabel, ClipboardPage, ClipboardPasteMode, ClipboardPayload, ClipboardPolicy,
    ClipboardQuery, ClipboardRepository, ClipboardSummary, ClipboardSyncChangeKind,
    ClipboardSyncPage, ClipboardSyncRecord, ClipboardTextSyntax, ClipboardTimelinePage,
    ClipboardTimelineQuery,
};
use crate::domain::clipboard_text::{convert_payload, detect_text_syntax};
use crate::domain::window_manager::WindowManagerRepository;
use crate::error::{Error, Result};
use crate::infrastructure::clipboard_ocr::{ClipboardOcr, OCR_MODEL_VERSION};
use crate::infrastructure::clipboard_store::{ImageOcrState, SqliteClipboardStore};

const MAX_PREVIEW_CHARS: usize = 140;
const MAX_TEXT_SYNTAX_DETECTION_BYTES: usize = 64 * 1024;
const MAX_SAFE_HTML_SOURCE_BYTES: usize = 256 * 1024;
const MAX_SAFE_HTML_INPUT_CHARS: usize = 12_000;
const MAX_CAPTURED_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const SUPPRESSION_TTL: Duration = Duration::from_secs(2);
const MAX_PENDING_SUPPRESSIONS: usize = 16;
const OCR_ENGINE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const OCR_RESULT_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct ClipboardFingerprints {
    content_hash: String,
    semantic_hash: String,
}

#[derive(Default)]
struct ClipboardCaptureState {
    // The currently observed content has no timeout: Handoff may arrive late.
    current: Option<ClipboardFingerprints>,
    // Platforms without native origin markers retain the short write fallback.
    writes: VecDeque<(ClipboardFingerprints, Instant)>,
}

type SharedCaptureState = Arc<Mutex<ClipboardCaptureState>>;

type DbResponse<T> = oneshot::Sender<Result<T>>;

#[derive(Debug, Clone, Copy)]
struct OcrJob {
    id: u64,
    urgent: bool,
}

enum DbCommand {
    Revision(DbResponse<u64>),
    CurrentSummary(DbResponse<Option<ClipboardSummary>>),
    History(ClipboardQuery, DbResponse<ClipboardPage>),
    Timeline(
        ClipboardTimelineQuery,
        DbResponse<Option<ClipboardTimelinePage>>,
    ),
    ImagePng(u64, DbResponse<Option<Vec<u8>>>),
    ImageOcr(u64, DbResponse<Option<ClipboardImageOcr>>),
    ImageOcrState(u64, DbResponse<ImageOcrState>),
    PendingImageOcrSource(u64, DbResponse<Option<Vec<u8>>>),
    StoreImageOcr(u64, ClipboardImageOcr, DbResponse<()>),
    StoreImageOcrFailure(u64, String, String, DbResponse<()>),
    FilePaths(u64, DbResponse<Vec<String>>),
    TextContent(u64, DbResponse<String>),
    HtmlPayload(u64, DbResponse<Option<String>>),
    Policy(DbResponse<ClipboardPolicy>),
    Store {
        payload: ClipboardPayload,
        content_hash: String,
        summary: ClipboardSummary,
        touch_existing: bool,
        source_device_id: String,
        source_device_name: String,
        live: bool,
        capture_origin: Option<ClipboardCaptureOrigin>,
        response: Option<DbResponse<Option<ClipboardSyncRecord>>>,
    },
    Payload(
        u64,
        DbResponse<(ClipboardPayload, String, ClipboardContentKind)>,
    ),
    UpdatePolicy(ClipboardPolicy, DbResponse<()>),
    Delete(u64, String, DbResponse<Option<ClipboardSyncRecord>>),
    SetFavorite(u64, bool, String, DbResponse<Option<ClipboardSyncRecord>>),
    Labels(DbResponse<Vec<ClipboardLabel>>),
    CreateLabel(String, String, String, DbResponse<ClipboardLabel>),
    UpdateLabel(
        String,
        String,
        String,
        String,
        DbResponse<Option<ClipboardSyncRecord>>,
    ),
    DeleteLabel(String, String, DbResponse<Vec<ClipboardSyncRecord>>),
    SetLabels(
        u64,
        Vec<String>,
        String,
        DbResponse<Option<ClipboardSyncRecord>>,
    ),
    SetLabelMembership(
        u64,
        String,
        bool,
        String,
        DbResponse<Option<ClipboardSyncRecord>>,
    ),
    Clear(DbResponse<()>),
    SyncRecords(u64, usize, DbResponse<ClipboardSyncPage>),
    SyncRecordRequiresPayload(String, u64, String, DbResponse<bool>),
    ApplySync(ClipboardSyncRecord, DbResponse<bool>),
    EditText(u64, String, String, DbResponse<Option<ClipboardSyncRecord>>),
}

impl DbCommand {
    fn read_only(&self) -> bool {
        matches!(
            self,
            Self::Revision(_)
                | Self::CurrentSummary(_)
                | Self::History(..)
                | Self::Timeline(..)
                | Self::ImagePng(..)
                | Self::ImageOcr(..)
                | Self::ImageOcrState(..)
                | Self::PendingImageOcrSource(..)
                | Self::FilePaths(..)
                | Self::TextContent(..)
                | Self::HtmlPayload(..)
                | Self::Policy(_)
                | Self::Labels(_)
                | Self::SyncRecords(..)
                | Self::SyncRecordRequiresPayload(..)
        )
    }
}

#[derive(Clone)]
struct DbClient {
    tx: tokio_mpsc::Sender<DbCommand>,
    reads: tokio_mpsc::Sender<DbCommand>,
}

impl DbClient {
    async fn send(&self, command: DbCommand) -> Result<()> {
        (if command.read_only() {
            &self.reads
        } else {
            &self.tx
        })
        .send(command)
        .await
        .map_err(|_| Error::Clipboard("clipboard database worker stopped".into()))
    }

    fn blocking_send(&self, command: DbCommand) -> Result<()> {
        (if command.read_only() {
            &self.reads
        } else {
            &self.tx
        })
        .blocking_send(command)
        .map_err(|_| Error::Clipboard("clipboard database worker stopped".into()))
    }
}

pub struct NativeClipboard {
    resources: Arc<arcrelay_content::ContentResources>,
    workers: ClipboardWorkers,
    context: Mutex<ClipboardContext>,
    db: DbClient,
    capture_state: SharedCaptureState,
    change_tx: tokio::sync::broadcast::Sender<()>,
    capture_tx: tokio::sync::broadcast::Sender<ClipboardCaptureEvent>,
    sync_tx: tokio::sync::broadcast::Sender<ClipboardSyncRecord>,
    ocr_events: tokio::sync::broadcast::Sender<u64>,
    ocr_job_tx: mpsc::Sender<Option<OcrJob>>,
    local_device_id: String,
    local_device_name: String,
    notifications_available: bool,
}

impl NativeClipboard {
    pub fn new(
        database_path: Option<PathBuf>,
        source_provider: Arc<dyn WindowManagerRepository>,
        local_device_id: String,
        local_device_name: String,
    ) -> Result<Self> {
        Self::with_resources(
            database_path,
            source_provider,
            local_device_id,
            local_device_name,
            Arc::new(arcrelay_content::ContentResources::default()),
        )
    }

    pub fn with_resources(
        database_path: Option<PathBuf>,
        source_provider: Arc<dyn WindowManagerRepository>,
        local_device_id: String,
        local_device_name: String,
        resources: Arc<arcrelay_content::ContentResources>,
    ) -> Result<Self> {
        let context = ClipboardContext::new()
            .map_err(|error| Error::Clipboard(format!("initialize clipboard: {error}")))?;
        let (change_tx, _) = tokio::sync::broadcast::channel(32);
        let (capture_tx, _) = tokio::sync::broadcast::channel(16);
        let (sync_tx, _) = tokio::sync::broadcast::channel(128);
        let (ocr_event_tx, _) = tokio::sync::broadcast::channel(64);
        let (ocr_job_tx, ocr_job_rx) = mpsc::channel();
        let workers = ClipboardWorkers::new(ocr_job_tx.clone());
        let db = start_database_worker(
            database_path,
            change_tx.clone(),
            capture_tx.clone(),
            ocr_job_tx.clone(),
            &workers,
        )?;
        start_ocr_worker(
            Arc::new(ClipboardOcr::with_resources(resources.clone())),
            ocr_job_rx,
            db.clone(),
            ocr_event_tx.clone(),
            &workers,
        )?;
        let capture_state = Arc::new(Mutex::new(ClipboardCaptureState::default()));

        {
            let _work = capture_work(&context, &resources)?;
            if let Ok(Some(payload)) = capture_payload(&context) {
                let fingerprints = clipboard_fingerprints(&payload);
                let content_hash = fingerprints.content_hash.clone();
                capture_state
                    .lock()
                    .map_err(|_| Error::Clipboard("clipboard capture lock poisoned".into()))?
                    .current = Some(fingerprints);
                let source_app = detect_source_application(&context, source_provider.as_ref());
                let summary = summarize(&payload, source_app);
                db.blocking_send(DbCommand::Store {
                    payload,
                    content_hash,
                    summary,
                    touch_existing: false,
                    source_device_id: local_device_id.clone(),
                    source_device_name: local_device_name.clone(),
                    live: false,
                    capture_origin: None,
                    response: None,
                })?;
            }
        }
        let notifications_available = start_watcher(
            db.clone(),
            Arc::clone(&capture_state),
            source_provider.clone(),
            (local_device_id.clone(), local_device_name.clone()),
            sync_tx.clone(),
            resources.clone(),
            &workers,
        );

        Ok(Self {
            resources,
            workers,
            context: Mutex::new(context),
            db,
            capture_state,
            change_tx,
            capture_tx,
            sync_tx,
            ocr_events: ocr_event_tx,
            ocr_job_tx,
            notifications_available,
            local_device_id,
            local_device_name,
        })
    }

    async fn request<T>(&self, build: impl FnOnce(DbResponse<T>) -> DbCommand) -> Result<T> {
        let (response_tx, response_rx) = oneshot::channel();
        self.db.send(build(response_tx)).await?;
        response_rx
            .await
            .map_err(|_| Error::Clipboard("clipboard database response was dropped".into()))?
    }

    async fn store_payload(
        &self,
        payload: ClipboardPayload,
        source_app: Option<String>,
        live: bool,
    ) -> Result<Option<ClipboardSyncRecord>> {
        let content_hash = content_hash(&payload);
        let summary = summarize(&payload, source_app);
        self.request(|response| DbCommand::Store {
            payload,
            content_hash,
            summary,
            touch_existing: true,
            source_device_id: self.local_device_id.clone(),
            source_device_name: self.local_device_name.clone(),
            live,
            capture_origin: None,
            response: Some(response),
        })
        .await
    }

    fn write_system_clipboard(&self, payload: ClipboardPayload, local_only: bool) -> Result<()> {
        let _work = if matches!(payload, ClipboardPayload::Image { .. }) {
            Some(
                self.resources
                    .blocking_work(256 * 1024 * 1024)
                    .map_err(|error| Error::Clipboard(error.to_string()))?,
            )
        } else {
            None
        };
        let context = self
            .context
            .lock()
            .map_err(|_| Error::Clipboard("clipboard context lock poisoned".into()))?;
        // Serialize writes with capture reads, including their successful DB commit.
        // The DB worker never takes this lock.
        let mut state = lock_capture_state(&self.capture_state)?;
        let fingerprints = clipboard_fingerprints_reusing(&payload, state.current.as_ref());
        write_payload(&context, payload, local_only)?;
        state.remember_write(fingerprints, Instant::now());
        Ok(())
    }

    async fn wait_for_image_ocr_text(&self, id: u64) -> Result<String> {
        let mut events = self.ocr_events.subscribe();
        self.ocr_job_tx
            .send(Some(OcrJob { id, urgent: true }))
            .map_err(|_| Error::Clipboard("clipboard OCR worker stopped".into()))?;
        loop {
            match self
                .request(|response| DbCommand::ImageOcrState(id, response))
                .await?
            {
                ImageOcrState::Completed(result) if result.text.trim().is_empty() => {
                    return Err(Error::Clipboard(
                        "no text was detected in the clipboard image".into(),
                    ));
                }
                ImageOcrState::Completed(result) => return Ok(result.text),
                ImageOcrState::Failed(error) => return Err(Error::Clipboard(error)),
                ImageOcrState::Missing => {
                    return Err(Error::Clipboard(
                        "clipboard image OCR has not been scheduled".into(),
                    ));
                }
                ImageOcrState::Pending => {}
            }
            let event = tokio::time::timeout(OCR_RESULT_WAIT_TIMEOUT, async {
                loop {
                    match events.recv().await {
                        Ok(completed_id) if completed_id == id => return Ok(()),
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return Ok(()),
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            return Err(Error::Clipboard("clipboard OCR worker stopped".into()));
                        }
                    }
                }
            })
            .await
            .map_err(|_| Error::Clipboard("clipboard OCR timed out".into()))?;
            event?;
        }
    }
}

struct HostClipboardHandler {
    changed: mpsc::SyncSender<()>,
}

impl ClipboardHandler for HostClipboardHandler {
    fn on_clipboard_change(&mut self) {
        // A capacity-one channel is an intentional latest-event queue. The
        // platform callback never waits for capture, parsing, image encoding,
        // or SQLite; bursts are coalesced while the worker is busy.
        let _ = self.changed.try_send(());
    }
}

struct ClipboardCaptureWorker {
    resources: Arc<arcrelay_content::ContentResources>,
    context: ClipboardContext,
    changed: mpsc::Receiver<()>,
    db: DbClient,
    capture_state: SharedCaptureState,
    source_provider: Arc<dyn WindowManagerRepository>,
    source_device_id: String,
    source_device_name: String,
    sync_tx: tokio::sync::broadcast::Sender<ClipboardSyncRecord>,
}

mod content;
#[cfg(target_os = "macos")]
mod macos;
mod repository;
mod worker;

use content::*;
use worker::*;

#[cfg(test)]
mod tests;

mod lifecycle;
use lifecycle::ClipboardWorkers;
