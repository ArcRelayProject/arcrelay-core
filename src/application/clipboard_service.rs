use std::sync::{Arc, RwLock};

use crate::domain::clipboard::{
    ClipboardContentKind, ClipboardImageOcr, ClipboardLabel, ClipboardPage, ClipboardPasteMode,
    ClipboardPolicy, ClipboardQuery, ClipboardRepository, ClipboardSummary, ClipboardSyncPage,
    ClipboardSyncPreferences, ClipboardSyncRecord, ClipboardTimelinePage, ClipboardTimelineQuery,
};
use crate::domain::input_control::InputControlRepository;
use crate::error::Result;

/// Application service for clipboard use cases. It coordinates the clipboard
/// aggregate with native input, while both capabilities remain behind domain
/// ports.
pub struct ClipboardApplicationService {
    repository: Arc<dyn ClipboardRepository>,
    input: Arc<dyn InputControlRepository>,
    sync_preferences: Arc<RwLock<ClipboardSyncPreferences>>,
}

impl ClipboardApplicationService {
    pub async fn shutdown(&self) {
        self.repository.shutdown().await;
    }

    pub fn new(
        repository: Arc<dyn ClipboardRepository>,
        input: Arc<dyn InputControlRepository>,
    ) -> Self {
        Self::with_sync_preferences(
            repository,
            input,
            Arc::new(RwLock::new(ClipboardSyncPreferences::default())),
        )
    }

    pub fn with_sync_preferences(
        repository: Arc<dyn ClipboardRepository>,
        input: Arc<dyn InputControlRepository>,
        sync_preferences: Arc<RwLock<ClipboardSyncPreferences>>,
    ) -> Self {
        Self {
            repository,
            input,
            sync_preferences,
        }
    }

    pub async fn revision(&self) -> Result<u64> {
        self.repository.revision().await
    }

    pub fn subscribe_captures(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<crate::domain::clipboard::ClipboardCaptureEvent>>
    {
        self.repository.subscribe_captures()
    }

    pub fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
        self.repository.subscribe_changes()
    }

    pub fn subscribe_ocr_changes(&self) -> Option<tokio::sync::broadcast::Receiver<u64>> {
        self.repository.subscribe_ocr_changes()
    }

    pub fn subscribe_sync_changes(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<ClipboardSyncRecord>> {
        self.repository.subscribe_sync_changes()
    }

    pub fn sync_preferences(&self) -> ClipboardSyncPreferences {
        *self
            .sync_preferences
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn update_sync_preferences(&self, preferences: ClipboardSyncPreferences) {
        *self
            .sync_preferences
            .write()
            .unwrap_or_else(|error| error.into_inner()) = preferences;
    }

    pub fn should_send_sync_record(&self, record: &ClipboardSyncRecord) -> bool {
        let preferences = self.sync_preferences();
        if !preferences.enabled || !is_device_syncable_kind(record.kind) {
            return false;
        }
        match record.change_kind {
            crate::domain::clipboard::ClipboardSyncChangeKind::Edit
            | crate::domain::clipboard::ClipboardSyncChangeKind::Delete => {
                preferences.sync_edits_and_deletes
            }
            crate::domain::clipboard::ClipboardSyncChangeKind::Favorite => {
                preferences.sync_favorites
            }
            crate::domain::clipboard::ClipboardSyncChangeKind::Label => true,
            crate::domain::clipboard::ClipboardSyncChangeKind::Copy
            | crate::domain::clipboard::ClipboardSyncChangeKind::Snapshot => true,
        }
    }

    pub async fn current_summary(&self) -> Result<Option<ClipboardSummary>> {
        self.repository.current_summary().await
    }

    pub async fn history(&self, query: ClipboardQuery) -> Result<ClipboardPage> {
        self.repository.history(query).await
    }

    pub async fn timeline(
        &self,
        query: ClipboardTimelineQuery,
    ) -> Result<Option<ClipboardTimelinePage>> {
        self.repository.timeline(query).await
    }

    pub async fn image_png(&self, id: u64) -> Result<Option<Vec<u8>>> {
        self.repository.image_png(id).await
    }

    pub async fn image_ocr(&self, id: u64) -> Result<Option<ClipboardImageOcr>> {
        self.repository.image_ocr(id).await
    }

    pub async fn file_paths(&self, id: u64) -> Result<Vec<String>> {
        self.repository.file_paths(id).await
    }

    pub async fn text_content(&self, id: u64) -> Result<String> {
        self.repository.text_content(id).await
    }

    pub async fn safe_html_preview(&self, id: u64) -> Result<Option<String>> {
        self.repository.safe_html_preview(id).await
    }

    pub async fn policy(&self) -> Result<ClipboardPolicy> {
        self.repository.policy().await
    }

    pub async fn update_policy(&self, policy: ClipboardPolicy) -> Result<()> {
        self.repository.update_policy(policy).await
    }

    pub async fn set_text(&self, content: &str) -> Result<()> {
        self.repository.set_text(content).await
    }

    /// Restores a record to the local system clipboard without emitting a
    /// paste shortcut. Desktop clipboard managers use this for explicit copy
    /// actions while double-click insertion uses `paste_record` below.
    pub async fn copy_record(&self, id: u64) -> Result<()> {
        self.repository
            .activate(id, ClipboardPasteMode::Source)
            .await
            .map(|_| ())
    }

    pub async fn clear_history(&self) -> Result<()> {
        self.repository.clear_history().await
    }

    pub async fn delete(&self, id: u64) -> Result<()> {
        self.repository.delete(id).await
    }

    pub async fn set_favorite(&self, id: u64, favorite: bool) -> Result<()> {
        self.repository.set_favorite(id, favorite).await
    }

    pub async fn labels(&self) -> Result<Vec<ClipboardLabel>> {
        self.repository.labels().await
    }

    pub async fn create_label(&self, name: &str, color: &str) -> Result<ClipboardLabel> {
        self.repository.create_label(name, color).await
    }

    pub async fn update_label(&self, label_id: &str, name: &str, color: &str) -> Result<()> {
        self.repository.update_label(label_id, name, color).await
    }

    pub async fn delete_label(&self, label_id: &str) -> Result<()> {
        self.repository.delete_label(label_id).await
    }

    pub async fn set_labels(&self, id: u64, label_ids: Vec<String>) -> Result<()> {
        self.repository.set_labels(id, label_ids).await
    }

    pub async fn set_label_membership(
        &self,
        id: u64,
        label_id: &str,
        attached: bool,
    ) -> Result<()> {
        self.repository
            .set_label_membership(id, label_id, attached)
            .await
    }

    pub async fn edit_text(&self, id: u64, content: &str) -> Result<()> {
        self.repository.edit_text(id, content).await
    }

    pub async fn sync_records(
        &self,
        after_local_id: u64,
        limit: usize,
    ) -> Result<ClipboardSyncPage> {
        let mut page = self.repository.sync_records(after_local_id, limit).await?;
        page.records
            .retain(|(_, record)| is_device_syncable_kind(record.kind));
        Ok(page)
    }

    pub async fn sync_record_requires_payload(
        &self,
        sync_id: &str,
        revision: u64,
        updated_by_device_id: &str,
    ) -> Result<bool> {
        self.repository
            .sync_record_requires_payload(sync_id, revision, updated_by_device_id)
            .await
    }

    pub async fn apply_sync_record(
        &self,
        record: ClipboardSyncRecord,
        relay_change: bool,
    ) -> Result<bool> {
        let preferences = self.sync_preferences();
        if !self.should_send_sync_record(&record) {
            return Ok(false);
        }
        self.repository
            .apply_sync_record(record, preferences.update_system_clipboard, relay_change)
            .await
    }

    /// Restores a record locally, then emits the native paste shortcut at the
    /// host's current focus. This single use case works for text, HTML, images,
    /// and file lists because applications consume the system clipboard.
    pub async fn paste_record(&self, id: u64) -> Result<()> {
        self.paste_record_as(id, ClipboardPasteMode::Source).await
    }

    pub async fn paste_record_as(&self, id: u64, mode: ClipboardPasteMode) -> Result<()> {
        let kind = self.prepare_record_as(id, mode).await?;
        self.paste_prepared(kind).await
    }

    /// Resolve and convert a history record, then place the resulting payload
    /// on the system clipboard without emitting a paste shortcut yet.
    pub async fn prepare_record_as(
        &self,
        id: u64,
        mode: ClipboardPasteMode,
    ) -> Result<ClipboardContentKind> {
        self.repository.activate(id, mode).await
    }

    /// Paste a payload already prepared by `prepare_record_as`.
    pub async fn paste_prepared(&self, kind: ClipboardContentKind) -> Result<()> {
        self.input
            .paste_clipboard(matches!(
                kind,
                ClipboardContentKind::Text | ClipboardContentKind::Html
            ))
            .await
    }
}

fn is_device_syncable_kind(kind: ClipboardContentKind) -> bool {
    // File clipboard entries contain paths that are meaningful only on the
    // source device. Nearby Transfer is the explicit cross-device file path.
    kind != ClipboardContentKind::Files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_paths_never_leave_the_source_device() {
        assert!(!is_device_syncable_kind(ClipboardContentKind::Files));
        assert!(is_device_syncable_kind(ClipboardContentKind::Text));
        assert!(is_device_syncable_kind(ClipboardContentKind::Html));
        assert!(is_device_syncable_kind(ClipboardContentKind::Image));
    }
}
