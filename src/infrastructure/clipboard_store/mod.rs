mod entity;
mod migration;
mod ocr;
mod records;
mod replica;
mod search_index;
mod sync;
mod timeline;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;

use chrono::{TimeZone, Utc};
use sea_orm::sea_query::{Expr, ExprTrait, OnConflict, Order, SimpleExpr};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, Condition, ConnectOptions, ConnectionTrait,
    Database, DatabaseConnection, DbErr, EntityTrait, FromQueryResult, IntoActiveModel, Iterable,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait, Statement, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use sha2::{Digest, Sha256};
use xxhash_rust::xxh3::Xxh3;

use crate::domain::clipboard::{
    ClipboardReplicaCursor, ClipboardReplicaPage, ClipboardReplicaRecord,
};

use crate::domain::clipboard::{
    ClipboardContentKind, ClipboardCursor, ClipboardImageOcr, ClipboardLabel,
    ClipboardLabelMembership, ClipboardOcrBlock, ClipboardPage, ClipboardPayload, ClipboardPolicy,
    ClipboardQuery, ClipboardSortBy, ClipboardSummary, ClipboardSyncChangeKind, ClipboardSyncPage,
    ClipboardSyncRecord, ClipboardTextSyntax, ClipboardTimelinePage, ClipboardTimelinePosition,
    ClipboardTimelineQuery,
};
use crate::domain::clipboard_text::detect_text_syntax;

use self::entity::{
    clipboard_entry, clipboard_entry_label, clipboard_image_ocr, clipboard_label,
    clipboard_ocr_block, clipboard_payload, clipboard_state,
};
use self::migration::Migrator;

const STATE_ID: i32 = 1;
const OCR_STATUS_PENDING: i32 = 0;
const OCR_STATUS_COMPLETED: i32 = 1;
const OCR_STATUS_FAILED: i32 = 2;

#[derive(Debug)]
pub(super) enum ImageOcrState {
    Missing,
    Pending,
    Completed(ClipboardImageOcr),
    Failed(String),
}

pub(super) struct SqliteClipboardStore {
    db: DatabaseConnection,
    pub(in crate::infrastructure) maintenance_pending: std::sync::atomic::AtomicBool,
}

#[derive(FromQueryResult)]
struct PruneCandidate {
    id: i64,
    storage_bytes: i64,
}

struct StoredValues {
    text_payload: Option<String>,
    html_payload: Option<String>,
    plain_text_payload: Option<String>,
    rtf_payload: Option<String>,
    image_png: Option<Vec<u8>>,
    files_json: Option<String>,
    available: bool,
}

impl StoredValues {
    fn from_payload(payload: ClipboardPayload) -> Result<Self, DbErr> {
        Ok(match payload {
            ClipboardPayload::Text(text) => Self {
                text_payload: Some(text),
                html_payload: None,
                plain_text_payload: None,
                rtf_payload: None,
                image_png: None,
                files_json: None,
                available: true,
            },
            ClipboardPayload::RichText {
                html,
                plain_text,
                rtf,
            } => Self {
                text_payload: None,
                html_payload: Some(html),
                plain_text_payload: Some(plain_text),
                rtf_payload: rtf,
                image_png: None,
                files_json: None,
                available: true,
            },
            ClipboardPayload::Image { png, .. } => Self {
                text_payload: None,
                html_payload: None,
                plain_text_payload: None,
                rtf_payload: None,
                image_png: Some(png),
                files_json: None,
                available: true,
            },
            ClipboardPayload::Files(paths) => Self {
                text_payload: None,
                html_payload: None,
                plain_text_payload: None,
                rtf_payload: None,
                image_png: None,
                available: paths.iter().all(|path| Path::new(path).exists()),
                files_json: Some(
                    serde_json::to_string(&paths)
                        .map_err(|error| DbErr::Custom(format!("encode file paths: {error}")))?,
                ),
            },
        })
    }

    fn storage_bytes(&self) -> i64 {
        [
            self.text_payload.as_ref().map(String::len),
            self.html_payload.as_ref().map(String::len),
            self.plain_text_payload.as_ref().map(String::len),
            self.rtf_payload.as_ref().map(String::len),
            self.image_png.as_ref().map(Vec::len),
            self.files_json.as_ref().map(String::len),
        ]
        .into_iter()
        .flatten()
        .fold(0usize, usize::saturating_add)
        .min(i64::MAX as usize) as i64
    }

    fn character_count(&self, kind: ClipboardContentKind) -> Option<i64> {
        match kind {
            ClipboardContentKind::Text => self.text_payload.as_deref(),
            ClipboardContentKind::Html => self.plain_text_payload.as_deref(),
            ClipboardContentKind::Image | ClipboardContentKind::Files => None,
        }
        .map(|text| text.chars().count().min(i64::MAX as usize) as i64)
    }

    fn into_active_model(self, entry_id: i64) -> clipboard_payload::ActiveModel {
        clipboard_payload::ActiveModel {
            entry_id: Set(entry_id),
            text_payload: Set(self.text_payload),
            html_payload: Set(self.html_payload),
            plain_text_payload: Set(self.plain_text_payload),
            rtf_payload: Set(self.rtf_payload),
            image_png: Set(self.image_png),
            files_json: Set(self.files_json),
        }
    }
}

fn byte_len(value: &str) -> i64 {
    value.len().min(i64::MAX as usize) as i64
}

fn ocr_block_into_active_model(
    entry_id: i64,
    index: usize,
    block: ClipboardOcrBlock,
) -> Result<clipboard_ocr_block::ActiveModel, DbErr> {
    Ok(clipboard_ocr_block::ActiveModel {
        entry_id: Set(entry_id),
        block_index: Set(index.min(i32::MAX as usize) as i32),
        text: Set(block.text),
        confidence: Set(f64::from(block.confidence)),
        left: Set(block.left),
        top: Set(block.top),
        width: Set(block.width.min(i32::MAX as u32) as i32),
        height: Set(block.height.min(i32::MAX as u32) as i32),
        points_json: Set(block
            .points
            .map(|points| serde_json::to_string(&points))
            .transpose()
            .map_err(|error| DbErr::Custom(format!("encode clipboard OCR points: {error}")))?),
        characters_json: Set((!block.characters.is_empty())
            .then(|| serde_json::to_string(&block.characters))
            .transpose()
            .map_err(|error| DbErr::Custom(format!("encode clipboard OCR characters: {error}")))?),
    })
}

fn ocr_block_from_model(model: clipboard_ocr_block::Model) -> Result<ClipboardOcrBlock, DbErr> {
    Ok(ClipboardOcrBlock {
        text: model.text,
        confidence: model.confidence as f32,
        left: model.left,
        top: model.top,
        width: model.width.max(0) as u32,
        height: model.height.max(0) as u32,
        points: model
            .points_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| DbErr::Custom(format!("decode clipboard OCR points: {error}")))?,
        characters: model
            .characters_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| DbErr::Custom(format!("decode clipboard OCR characters: {error}")))?
            .unwrap_or_default(),
    })
}

fn build_search_text(summary: &ClipboardSummary, stored: &StoredValues) -> String {
    if summary.sensitive {
        return String::new();
    }
    [
        Some(summary.preview.as_str()),
        summary.source_app.as_deref(),
        stored.text_payload.as_deref(),
        stored.plain_text_payload.as_deref(),
        stored.files_json.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
    .to_lowercase()
}

fn sync_id_from_hash(content_hash: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"arcrelay-clipboard-sync-v1\0");
    digest.update(content_hash.as_bytes());
    format!("{:x}", digest.finalize())
}

fn sync_record_payload(record: &ClipboardSyncRecord) -> Result<ClipboardPayload, DbErr> {
    match record.kind {
        ClipboardContentKind::Text => record
            .text
            .clone()
            .map(ClipboardPayload::Text)
            .ok_or_else(|| DbErr::Custom("synchronized text payload is missing".into())),
        ClipboardContentKind::Html => record
            .html
            .clone()
            .map(|html| ClipboardPayload::RichText {
                html,
                plain_text: record.text.clone().unwrap_or_default(),
                rtf: record.rtf.clone(),
            })
            .ok_or_else(|| DbErr::Custom("synchronized rich-text payload is missing".into())),
        ClipboardContentKind::Image => record
            .image_png
            .clone()
            .map(|png| ClipboardPayload::Image {
                png,
                width: record.width.unwrap_or_default(),
                height: record.height.unwrap_or_default(),
            })
            .ok_or_else(|| DbErr::Custom("synchronized image payload is missing".into())),
        ClipboardContentKind::Files => Err(DbErr::Custom(
            "file paths cannot be synchronized as clipboard content".into(),
        )),
    }
}

fn sync_record_summary(record: &ClipboardSyncRecord, stored: &StoredValues) -> ClipboardSummary {
    let captured_at = Utc
        .timestamp_millis_opt(record.captured_at_ms)
        .single()
        .unwrap_or_else(Utc::now);
    ClipboardSummary {
        id: 0,
        kind: record.kind,
        preview: record.preview.clone(),
        source_app: record.source_app.clone(),
        first_captured_at: captured_at,
        captured_at,
        last_used_at: None,
        size_bytes: stored.storage_bytes().max(0) as u64,
        character_count: match record.kind {
            ClipboardContentKind::Text => stored.text_payload.as_deref(),
            ClipboardContentKind::Html => stored.plain_text_payload.as_deref(),
            ClipboardContentKind::Image | ClipboardContentKind::Files => None,
        }
        .map(|text| text.chars().count().min(u64::MAX as usize) as u64),
        item_count: 1,
        width: record.width,
        height: record.height,
        sensitive: false,
        favorite: record.favorite,
        labels: record
            .labels
            .iter()
            .filter(|label| !label.deleted)
            .cloned()
            .collect(),
        copy_count: 1,
        available: true,
        sync_id: record.sync_id.clone(),
        source_device_id: Some(record.source_device_id.clone()),
        source_device_name: Some(record.source_device_name.clone()),
        text_syntax: record.text_syntax.clone(),
    }
}

fn content_hash_for_stored(
    record: &ClipboardSyncRecord,
    stored: &StoredValues,
) -> Result<String, DbErr> {
    match record.kind {
        ClipboardContentKind::Text => stored
            .text_payload
            .as_deref()
            .map(text_content_hash)
            .ok_or_else(|| DbErr::Custom("synchronized text payload is missing".into())),
        ClipboardContentKind::Html => rich_text_content_hash(stored),
        ClipboardContentKind::Image => stored
            .image_png
            .as_deref()
            .map(image_content_hash)
            .ok_or_else(|| DbErr::Custom("synchronized image payload is missing".into())),
        ClipboardContentKind::Files => Err(DbErr::Custom(
            "file paths cannot be synchronized as clipboard content".into(),
        )),
    }
}

fn text_content_hash(text: &str) -> String {
    let mut digest = Xxh3::new();
    digest.update(b"text\0");
    digest.update(text.as_bytes());
    format!("{:032x}", digest.digest128())
}

fn image_content_hash(png: &[u8]) -> String {
    let mut digest = Xxh3::new();
    digest.update(b"image\0");
    digest.update(png);
    format!("{:032x}", digest.digest128())
}

fn rich_text_content_hash(stored: &StoredValues) -> Result<String, DbErr> {
    let html = stored
        .html_payload
        .as_deref()
        .ok_or_else(|| DbErr::Custom("synchronized HTML payload is missing".into()))?;
    let mut digest = Xxh3::new();
    digest.update(b"rich-text\0");
    digest.update(html.as_bytes());
    digest.update(b"\0text\0");
    digest.update(
        stored
            .plain_text_payload
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    if let Some(rtf) = stored.rtf_payload.as_deref() {
        digest.update(b"\0rtf\0");
        digest.update(rtf.as_bytes());
    }
    Ok(format!("{:032x}", digest.digest128()))
}

fn deleted_content_hash(sync_id: &str) -> String {
    let mut digest = Xxh3::new();
    digest.update(b"deleted\0");
    digest.update(sync_id.as_bytes());
    format!("{:032x}", digest.digest128())
}

fn base_model_to_sync_record(
    model: &clipboard_entry::Model,
    payload: Option<&clipboard_payload::Model>,
    labels: Vec<ClipboardLabel>,
    label_memberships: Vec<ClipboardLabelMembership>,
) -> Result<Option<ClipboardSyncRecord>, DbErr> {
    let kind = kind_from_i32(model.kind)?;
    let (text, html, rtf, image_png) = if model.deleted || kind == ClipboardContentKind::Files {
        (None, None, None, None)
    } else {
        let payload = payload
            .ok_or_else(|| DbErr::Custom(format!("clipboard payload {} is missing", model.id)))?;
        match kind {
            ClipboardContentKind::Text => (payload.text_payload.clone(), None, None, None),
            ClipboardContentKind::Html => (
                payload.plain_text_payload.clone(),
                payload.html_payload.clone(),
                payload.rtf_payload.clone(),
                None,
            ),
            ClipboardContentKind::Image => (None, None, None, payload.image_png.clone()),
            ClipboardContentKind::Files => unreachable!(),
        }
    };
    if text.as_ref().is_some_and(|text| text.len() > 768 * 1024) {
        return Ok(None);
    }
    if !model.deleted && kind == ClipboardContentKind::Html && html.is_none() {
        return Ok(None);
    }
    Ok(Some(ClipboardSyncRecord {
        sync_id: model.sync_id.clone(),
        kind,
        text,
        html,
        rtf,
        image_png,
        width: model.width.map(|value| value.max(0) as u32),
        height: model.height.map(|value| value.max(0) as u32),
        preview: model.preview.clone(),
        source_app: model.source_app.clone(),
        source_device_id: model.source_device_id.clone(),
        source_device_name: model.source_device_name.clone(),
        captured_at_ms: model.captured_at_ms,
        revision: model.sync_revision.max(1) as u64,
        updated_by_device_id: model.updated_by_device_id.clone(),
        favorite: model.favorite,
        favorite_revision: model.favorite_revision.max(1) as u64,
        favorite_updated_by_device_id: model.favorite_updated_by_device_id.clone(),
        labels,
        label_memberships,
        deleted: model.deleted,
        change_kind: ClipboardSyncChangeKind::Snapshot,
        live: false,
        text_syntax: decode_text_syntax(&model.text_syntax)?,
    }))
}

fn cursor_condition(cursor: ClipboardCursor, sort_by: ClipboardSortBy) -> Condition {
    let id = i64::try_from(cursor.id).unwrap_or(i64::MAX);
    let sort_expression = sort_expression(sort_by);
    Condition::any()
        .add(sort_expression.clone().lt(cursor.sort_at_ms))
        .add(
            Condition::all()
                .add(sort_expression.eq(cursor.sort_at_ms))
                .add(Expr::col(clipboard_entry::Column::SyncId).lt(cursor_sync_id(id))),
        )
}

// Public legacy cursors address a local row; resolve its replicated identity
// for the tie-break so insertion order on another device cannot reorder history.
fn cursor_sync_id(id: i64) -> SimpleExpr {
    Expr::cust_with_values("(SELECT sync_id FROM clipboard_entries WHERE id = ?)", [id])
}

fn sort_expression(sort_by: ClipboardSortBy) -> SimpleExpr {
    match sort_by {
        ClipboardSortBy::CreatedAt => Expr::col(clipboard_entry::Column::FirstCapturedAtMs).into(),
        ClipboardSortBy::UpdatedAt => Expr::col(clipboard_entry::Column::UpdatedAtMs).into(),
    }
}

fn sort_timestamp(model: &clipboard_entry::Model, sort_by: ClipboardSortBy) -> i64 {
    match sort_by {
        ClipboardSortBy::CreatedAt => model.first_captured_at_ms,
        ClipboardSortBy::UpdatedAt => model.updated_at_ms,
    }
}

// List projections never pull multi-megabyte searchable payloads into UI pages.
fn summary_query() -> sea_orm::Select<clipboard_entry::Entity> {
    clipboard_entry::Entity::find()
        .select_only()
        .columns(
            clipboard_entry::Column::iter()
                .filter(|column| !matches!(column, clipboard_entry::Column::SearchText)),
        )
        .column_as(Expr::value(""), clipboard_entry::Column::SearchText)
}

async fn bump_revision<C>(db: &C) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    db.execute_unprepared("UPDATE clipboard_state SET revision = MAX(1, MIN(revision, 9223372036854775806) + 1) WHERE id = 1").await?;
    Ok(())
}

fn next_revision(current: &i64) -> i64 {
    current.saturating_add(1).max(1)
}

fn policy_from_state(state: &clipboard_state::Model) -> ClipboardPolicy {
    ClipboardPolicy {
        history_enabled: state.history_enabled,
        max_items: state.max_items.max(1) as u32,
        max_bytes: state.max_bytes.max(1) as u64,
        retention_days: state.retention_days.max(0) as u32,
        save_sensitive: state.save_sensitive,
    }
}

fn label_from_model(model: clipboard_label::Model) -> ClipboardLabel {
    ClipboardLabel {
        id: model.id,
        name: model.name,
        color: model.color,
        revision: model.revision.max(1) as u64,
        updated_by_device_id: model.updated_by_device_id,
        deleted: model.deleted,
    }
}

fn validate_label_name(value: &str) -> Result<String, DbErr> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 32 || value.chars().any(char::is_control) {
        return Err(DbErr::Custom(
            "clipboard label name must contain 1 to 32 visible characters".into(),
        ));
    }
    Ok(value.to_string())
}

fn normalize_label_name(value: &str) -> String {
    value.trim().to_lowercase()
}

fn validate_label_color(value: &str) -> Result<String, DbErr> {
    let value = value.trim();
    if value.len() != 7
        || !value.starts_with('#')
        || !value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(DbErr::Custom(
            "clipboard label color must be a #RRGGBB value".into(),
        ));
    }
    Ok(value.to_ascii_uppercase())
}

fn model_to_summary(
    model: clipboard_entry::Model,
    labels: Vec<ClipboardLabel>,
) -> Result<ClipboardSummary, DbErr> {
    Ok(ClipboardSummary {
        id: model.id.max(0) as u64,
        kind: kind_from_i32(model.kind)?,
        preview: model.preview,
        source_app: model.source_app,
        first_captured_at: Utc
            .timestamp_millis_opt(model.first_captured_at_ms)
            .single()
            .unwrap_or_else(Utc::now),
        captured_at: Utc
            .timestamp_millis_opt(model.captured_at_ms)
            .single()
            .unwrap_or_else(Utc::now),
        last_used_at: model
            .last_used_at_ms
            .and_then(|value| Utc.timestamp_millis_opt(value).single()),
        size_bytes: model.size_bytes.max(0) as u64,
        character_count: model.character_count.map(|value| value.max(0) as u64),
        item_count: model.item_count.max(1) as u32,
        width: model.width.map(|value| value.max(0) as u32),
        height: model.height.map(|value| value.max(0) as u32),
        sensitive: model.sensitive,
        favorite: model.favorite,
        labels,
        copy_count: model.copy_count.max(1) as u32,
        available: model.available,
        sync_id: model.sync_id,
        source_device_id: (!model.source_device_id.is_empty()).then_some(model.source_device_id),
        source_device_name: (!model.source_device_name.is_empty())
            .then_some(model.source_device_name),
        text_syntax: decode_text_syntax(&model.text_syntax)?,
    })
}

fn model_to_payload(
    model: &clipboard_entry::Model,
    payload: &clipboard_payload::Model,
) -> Result<ClipboardPayload, DbErr> {
    match kind_from_i32(model.kind)? {
        ClipboardContentKind::Text => payload
            .text_payload
            .clone()
            .map(ClipboardPayload::Text)
            .ok_or_else(|| DbErr::Custom("stored text payload is missing".into())),
        ClipboardContentKind::Html => payload
            .html_payload
            .clone()
            .map(|html| ClipboardPayload::RichText {
                html,
                plain_text: payload.plain_text_payload.clone().unwrap_or_default(),
                rtf: payload.rtf_payload.clone(),
            })
            .ok_or_else(|| DbErr::Custom("stored HTML payload is missing".into())),
        ClipboardContentKind::Image => payload
            .image_png
            .clone()
            .map(|png| ClipboardPayload::Image {
                png,
                width: model.width.unwrap_or_default().max(0) as u32,
                height: model.height.unwrap_or_default().max(0) as u32,
            })
            .ok_or_else(|| DbErr::Custom("stored image payload is missing".into())),
        ClipboardContentKind::Files => Ok(ClipboardPayload::Files(decode_paths(
            payload.files_json.as_deref(),
        )?)),
    }
}

fn encode_text_syntax(value: &ClipboardTextSyntax) -> Result<String, DbErr> {
    serde_json::to_string(value)
        .map_err(|error| DbErr::Custom(format!("encode clipboard text syntax: {error}")))
}

fn decode_text_syntax(value: &str) -> Result<ClipboardTextSyntax, DbErr> {
    serde_json::from_str(value)
        .map_err(|error| DbErr::Custom(format!("decode clipboard text syntax: {error}")))
}

fn decode_paths(value: Option<&str>) -> Result<Vec<String>, DbErr> {
    serde_json::from_str(value.unwrap_or("[]"))
        .map_err(|error| DbErr::Custom(format!("decode file paths: {error}")))
}

const fn kind_to_i32(kind: ClipboardContentKind) -> i32 {
    match kind {
        ClipboardContentKind::Text => 1,
        ClipboardContentKind::Html => 2,
        ClipboardContentKind::Image => 3,
        ClipboardContentKind::Files => 4,
    }
}

fn kind_from_i32(kind: i32) -> Result<ClipboardContentKind, DbErr> {
    match kind {
        1 => Ok(ClipboardContentKind::Text),
        2 => Ok(ClipboardContentKind::Html),
        3 => Ok(ClipboardContentKind::Image),
        4 => Ok(ClipboardContentKind::Files),
        _ => Err(DbErr::Custom(format!("invalid clipboard kind {kind}"))),
    }
}
