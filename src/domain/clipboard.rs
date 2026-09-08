use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardContentKind {
    Text,
    Html,
    Image,
    Files,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardTextSyntax {
    #[default]
    Plain,
    Json,
    Yaml,
    Markdown,
    Code {
        language: Option<String>,
    },
    Xml,
    Svg,
    Mermaid,
    MxGraph,
    Url,
    Email,
    PhoneNumber,
    Color,
    IpAddress,
    JwtToken,
    FilePath,
    MagnetLink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardPasteMode {
    Source,
    PlainText,
    RichText,
    JsonCompact,
    JsonFormatted,
    Yaml,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardSummary {
    pub id: u64,
    pub kind: ClipboardContentKind,
    /// Host-generated, bounded preview. The original payload never crosses the
    /// protocol boundary when listing clipboard history.
    pub preview: String,
    pub source_app: Option<String>,
    pub first_captured_at: DateTime<Utc>,
    pub captured_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub size_bytes: u64,
    /// Exact Unicode scalar count of the original text/plain-text payload.
    /// This is intentionally independent from the bounded preview.
    pub character_count: Option<u64>,
    pub item_count: u32,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sensitive: bool,
    pub favorite: bool,
    pub labels: Vec<ClipboardLabel>,
    pub copy_count: u32,
    pub available: bool,
    /// Stable content-addressed identifier used by desktop-to-desktop sync.
    /// It is deliberately separate from the local numeric database ID used by
    /// the clipboard window and mobile Host APIs.
    pub sync_id: String,
    /// Internal provenance used by the host to distinguish local captures from
    /// records received from another desktop. UI-facing DTOs should not expose
    /// the local device as a remote source.
    pub source_device_id: Option<String>,
    pub source_device_name: Option<String>,
    pub text_syntax: ClipboardTextSyntax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardSyncPreferences {
    pub enabled: bool,
    pub update_system_clipboard: bool,
    pub sync_edits_and_deletes: bool,
    pub sync_favorites: bool,
}

impl Default for ClipboardSyncPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            update_system_clipboard: true,
            sync_edits_and_deletes: true,
            sync_favorites: true,
        }
    }
}

/// A convergent, content-addressed clipboard record exchanged by trusted
/// desktops. Protocol v5 preserves the complete rich-text representation.
/// Files remain local paths and are handed to Nearby Transfer explicitly.
/// A committed, live clipboard capture. Metadata-only edits and historical
/// synchronization never emit this local application event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardCaptureOrigin {
    Local,
    Remote,
}
#[derive(Debug, Clone, Copy)]
pub struct ClipboardCaptureEvent {
    pub origin: ClipboardCaptureOrigin,
    pub occurred_at: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardSyncRecord {
    pub sync_id: String,
    pub kind: ClipboardContentKind,
    pub text: Option<String>,
    pub html: Option<String>,
    pub rtf: Option<String>,
    pub image_png: Option<Vec<u8>>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub preview: String,
    pub source_app: Option<String>,
    pub source_device_id: String,
    pub source_device_name: String,
    pub captured_at_ms: i64,
    pub revision: u64,
    pub updated_by_device_id: String,
    pub favorite: bool,
    pub favorite_revision: u64,
    pub favorite_updated_by_device_id: String,
    pub labels: Vec<ClipboardLabel>,
    pub label_memberships: Vec<ClipboardLabelMembership>,
    pub deleted: bool,
    pub change_kind: ClipboardSyncChangeKind,
    /// True only for a fresh native copy. History pulls and metadata changes
    /// never replace another device's current system clipboard.
    pub live: bool,
    pub text_syntax: ClipboardTextSyntax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardSyncChangeKind {
    Copy,
    Edit,
    Favorite,
    Label,
    Delete,
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSyncPage {
    pub records: Vec<(u64, ClipboardSyncRecord)>,
    pub next_cursor: Option<u64>,
}

/// Desktop replication metadata. Manifests never contain payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardReplicaRecord {
    pub record: ClipboardSyncRecord,
    pub first_captured_at_ms: i64,
    pub copy_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardReplicaCursor {
    pub captured_at_ms: i64,
    pub sync_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardReplicaPage {
    pub records: Vec<ClipboardReplicaRecord>,
    pub next_cursor: Option<ClipboardReplicaCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardCursor {
    pub sort_at_ms: i64,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
pub struct ClipboardLabel {
    pub id: String,
    pub name: String,
    pub color: String,
    pub revision: u64,
    pub updated_by_device_id: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardLabelMembership {
    pub label_id: String,
    pub attached: bool,
    pub revision: u64,
    pub updated_by_device_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSortBy {
    CreatedAt,
    UpdatedAt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardQuery {
    pub include_total_count: bool,
    pub limit: usize,
    pub cursor: Option<ClipboardCursor>,
    pub search: Option<String>,
    pub kinds: Vec<ClipboardContentKind>,
    pub favorite_only: bool,
    pub label_ids: Vec<String>,
    pub sort_by: ClipboardSortBy,
}

impl ClipboardQuery {
    pub fn recent(limit: usize) -> Self {
        Self {
            include_total_count: true,
            limit,
            cursor: None,
            search: None,
            kinds: Vec::new(),
            favorite_only: false,
            label_ids: Vec::new(),
            sort_by: ClipboardSortBy::UpdatedAt,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardPage {
    pub entries: Vec<ClipboardSummary>,
    pub next_cursor: Option<ClipboardCursor>,
    /// Present only when explicitly requested; excludes the pagination cursor.
    pub total_count: Option<u64>,
}

/// Metadata-only navigation in the unfiltered history. Every page is returned
/// newest first, including pages fetched towards newer records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardTimelinePosition {
    AroundId(u64),
    NewerThan(ClipboardCursor),
    OlderThan(ClipboardCursor),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardTimelineQuery {
    pub position: ClipboardTimelinePosition,
    pub sort_by: ClipboardSortBy,
    /// Per-direction limit for AroundId; page size otherwise.
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardTimelinePage {
    pub revision: u64,
    pub entries: Vec<ClipboardSummary>,
    pub anchor: Option<ClipboardCursor>,
    pub newer_cursor: Option<ClipboardCursor>,
    pub older_cursor: Option<ClipboardCursor>,
    pub total_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardPolicy {
    pub history_enabled: bool,
    pub max_items: u32,
    pub max_bytes: u64,
    /// Zero means no age-based expiration.
    pub retention_days: u32,
    pub save_sensitive: bool,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            history_enabled: true,
            max_items: 500,
            max_bytes: 100 * 1024 * 1024,
            retention_days: 30,
            save_sensitive: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardPayload {
    Text(String),
    RichText {
        html: String,
        plain_text: String,
        rtf: Option<String>,
    },
    Image {
        png: Vec<u8>,
        width: u32,
        height: u32,
    },
    Files(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardOcrPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardOcrCharacter {
    pub text: String,
    pub confidence: f32,
    /// Four corners in source-image pixel coordinates, ordered from the
    /// recognition start edge around the character.
    pub points: [ClipboardOcrPoint; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardOcrBlock {
    pub text: String,
    pub confidence: f32,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// Four corners in source-image pixel coordinates when the detector
    /// provides a rotated quadrilateral.
    pub points: Option<[ClipboardOcrPoint; 4]>,
    /// CTC-aligned character geometry. Empty for results produced by OCR
    /// engines that predate character alignment.
    #[serde(default)]
    pub characters: Vec<ClipboardOcrCharacter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardImageOcr {
    pub text: String,
    pub blocks: Vec<ClipboardOcrBlock>,
    pub model_version: String,
    pub updated_at_ms: i64,
}

#[async_trait::async_trait]
pub trait ClipboardRepository: Send + Sync {
    async fn shutdown(&self) {}

    /// Subscribe to native clipboard/history changes when the platform backend
    /// provides notifications. `None` means the coordinator must reconcile by
    /// sampling as a compatibility fallback.
    fn subscribe_captures(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<ClipboardCaptureEvent>> {
        None
    }

    fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
        None
    }

    /// Subscribe to completed or failed background OCR jobs. OCR updates do
    /// not change history summaries and therefore use a separate event stream.
    fn subscribe_ocr_changes(&self) -> Option<tokio::sync::broadcast::Receiver<u64>> {
        None
    }

    fn subscribe_sync_changes(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<ClipboardSyncRecord>> {
        None
    }

    /// Monotonic host-side revision used to push summary-only snapshots.
    async fn revision(&self) -> crate::error::Result<u64>;

    async fn current_summary(&self) -> crate::error::Result<Option<ClipboardSummary>>;

    async fn history(&self, query: ClipboardQuery) -> crate::error::Result<ClipboardPage>;

    /// Returns None when an AroundId anchor has been deleted or pruned. Never
    /// restores a payload or touches the record's capture/usage timestamps.
    async fn timeline(
        &self,
        query: ClipboardTimelineQuery,
    ) -> crate::error::Result<Option<ClipboardTimelinePage>>;

    /// Returns the original PNG bytes for a locally stored image record so
    /// host UI surfaces can build bounded thumbnails without exposing payloads
    /// through the remote summary protocol.
    async fn image_png(&self, id: u64) -> crate::error::Result<Option<Vec<u8>>>;

    /// Returns persisted OCR text and source-image coordinates for an image.
    /// `None` means recognition is pending or has not completed successfully.
    async fn image_ocr(&self, id: u64) -> crate::error::Result<Option<ClipboardImageOcr>>;

    /// Returns the original local paths for a file-list record. Paths never
    /// participate in automatic clipboard synchronization; desktop clients
    /// may hand them to Nearby Transfer after an explicit user action.
    async fn file_paths(&self, id: u64) -> crate::error::Result<Vec<String>>;

    /// Returns complete text for local host UI. Text records return their
    /// original content; rich-text records return their stored plain-text
    /// representation. Summary previews are intentionally bounded and must
    /// never be used as a content source.
    async fn text_content(&self, id: u64) -> crate::error::Result<String>;

    /// Returns a bounded, strictly sanitized HTML fragment for local preview.
    /// Original HTML never crosses into the WebView.
    async fn safe_html_preview(&self, id: u64) -> crate::error::Result<Option<String>>;

    async fn policy(&self) -> crate::error::Result<ClipboardPolicy>;

    async fn update_policy(&self, policy: ClipboardPolicy) -> crate::error::Result<()>;

    /// Write arbitrary text to the host clipboard and retain it in host
    /// history. Kept for the existing write-clipboard capability.
    async fn set_text(&self, content: &str) -> crate::error::Result<()>;

    /// Restore a host-owned record to the system clipboard. The record payload
    /// stays local; callers address it only by id.
    async fn activate(
        &self,
        id: u64,
        mode: ClipboardPasteMode,
    ) -> crate::error::Result<ClipboardContentKind>;

    async fn delete(&self, id: u64) -> crate::error::Result<()>;

    async fn set_favorite(&self, id: u64, favorite: bool) -> crate::error::Result<()>;

    async fn labels(&self) -> crate::error::Result<Vec<ClipboardLabel>>;

    async fn create_label(&self, name: &str, color: &str) -> crate::error::Result<ClipboardLabel>;

    async fn update_label(
        &self,
        label_id: &str,
        name: &str,
        color: &str,
    ) -> crate::error::Result<()>;

    async fn delete_label(&self, label_id: &str) -> crate::error::Result<()>;

    async fn set_labels(&self, id: u64, label_ids: Vec<String>) -> crate::error::Result<()>;

    async fn set_label_membership(
        &self,
        id: u64,
        label_id: &str,
        attached: bool,
    ) -> crate::error::Result<()>;

    async fn clear_history(&self) -> crate::error::Result<()>;

    async fn sync_records(
        &self,
        after_local_id: u64,
        limit: usize,
    ) -> crate::error::Result<ClipboardSyncPage>;

    /// Returns whether an incoming content revision needs its payload before
    /// it is applied. Clipboard reconciliation can use this metadata-only
    /// preflight to avoid downloading rich-text and image blobs that are
    /// already present locally.
    async fn sync_record_requires_payload(
        &self,
        sync_id: &str,
        revision: u64,
        updated_by_device_id: &str,
    ) -> crate::error::Result<bool>;

    async fn apply_sync_record(
        &self,
        record: ClipboardSyncRecord,
        update_system_clipboard: bool,
        relay_change: bool,
    ) -> crate::error::Result<bool>;

    async fn replica_page(
        &self,
        _cursor: Option<ClipboardReplicaCursor>,
        _limit: usize,
    ) -> crate::error::Result<ClipboardReplicaPage> {
        Err(crate::error::Error::Clipboard(
            "desktop replication is unavailable".into(),
        ))
    }

    async fn replica_record(
        &self,
        _sync_id: &str,
    ) -> crate::error::Result<Option<ClipboardReplicaRecord>> {
        Err(crate::error::Error::Clipboard(
            "desktop replication is unavailable".into(),
        ))
    }

    /// Reject records outside the local retention window before downloading content.
    async fn check_replica_storage(
        &self,
        _replica: &ClipboardReplicaRecord,
    ) -> crate::error::Result<()> {
        Ok(())
    }

    /// Latest durable live copy, distinct from historical snapshot order.
    async fn replica_selection(&self) -> crate::error::Result<Option<ClipboardSyncRecord>> {
        Ok(None)
    }

    async fn replica_labels(&self) -> crate::error::Result<Vec<ClipboardLabel>> {
        self.labels().await
    }

    async fn apply_replica_labels(
        &self,
        _labels: Vec<ClipboardLabel>,
    ) -> crate::error::Result<usize> {
        Err(crate::error::Error::Clipboard(
            "desktop replication is unavailable".into(),
        ))
    }

    async fn apply_replica_record(
        &self,
        replica: ClipboardReplicaRecord,
        update_system_clipboard: bool,
    ) -> crate::error::Result<bool> {
        self.apply_sync_record(replica.record, update_system_clipboard, true)
            .await
    }

    async fn edit_text(&self, id: u64, content: &str) -> crate::error::Result<()>;
}
