//! Test-only adapter exercising the production SQLite store without a system clipboard.
use super::clipboard_store::SqliteClipboardStore;
use crate::{
    domain::{clipboard::*, input_control::*},
    error::Result,
};
use std::{path::Path, sync::Arc};

pub struct TestClipboardRepository {
    store: SqliteClipboardStore,
    events: tokio::sync::broadcast::Sender<ClipboardSyncRecord>,
}
impl TestClipboardRepository {
    pub async fn open(path: Option<&Path>) -> Result<Self> {
        Ok(Self {
            store: SqliteClipboardStore::connect(path)
                .await
                .map_err(db_error)?,
            events: tokio::sync::broadcast::channel(128).0,
        })
    }
    pub fn service(
        self,
    ) -> Arc<crate::application::clipboard_service::ClipboardApplicationService> {
        Arc::new(
            crate::application::clipboard_service::ClipboardApplicationService::new(
                Arc::new(self),
                Arc::new(NoInput),
            ),
        )
    }
}
fn db_error(error: sea_orm::DbErr) -> crate::Error {
    crate::Error::Clipboard(error.to_string())
}
#[async_trait::async_trait]
impl ClipboardRepository for TestClipboardRepository {
    fn subscribe_sync_changes(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<ClipboardSyncRecord>> {
        Some(self.events.subscribe())
    }
    async fn revision(&self) -> Result<u64> {
        self.store.revision().await.map_err(db_error)
    }
    async fn current_summary(&self) -> crate::error::Result<Option<ClipboardSummary>> {
        self.store.current_summary().await.map_err(db_error)
    }
    async fn history(&self, query: ClipboardQuery) -> crate::error::Result<ClipboardPage> {
        self.store.history(query).await.map_err(db_error)
    }
    async fn timeline(
        &self,
        query: ClipboardTimelineQuery,
    ) -> crate::error::Result<Option<ClipboardTimelinePage>> {
        self.store.timeline(query).await.map_err(db_error)
    }
    async fn image_png(&self, id: u64) -> crate::error::Result<Option<Vec<u8>>> {
        self.store.image_png(id).await.map_err(db_error)
    }
    async fn image_ocr(&self, _id: u64) -> crate::error::Result<Option<ClipboardImageOcr>> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn file_paths(&self, id: u64) -> crate::error::Result<Vec<String>> {
        self.store.file_paths(id).await.map_err(db_error)
    }
    async fn text_content(&self, id: u64) -> crate::error::Result<String> {
        self.store.text_content(id).await.map_err(db_error)
    }
    async fn safe_html_preview(&self, _id: u64) -> crate::error::Result<Option<String>> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn policy(&self) -> crate::error::Result<ClipboardPolicy> {
        self.store.policy().await.map_err(db_error)
    }
    async fn update_policy(&self, policy: ClipboardPolicy) -> crate::error::Result<()> {
        self.store.update_policy(policy).await.map_err(db_error)
    }
    async fn set_text(&self, _content: &str) -> crate::error::Result<()> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn activate(
        &self,
        _id: u64,
        _mode: ClipboardPasteMode,
    ) -> crate::error::Result<ClipboardContentKind> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn delete(&self, _id: u64) -> crate::error::Result<()> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn set_favorite(&self, _id: u64, _favorite: bool) -> crate::error::Result<()> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn labels(&self) -> crate::error::Result<Vec<ClipboardLabel>> {
        self.store.labels().await.map_err(db_error)
    }
    async fn create_label(
        &self,
        _name: &str,
        _color: &str,
    ) -> crate::error::Result<ClipboardLabel> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn update_label(
        &self,
        _label_id: &str,
        _name: &str,
        _color: &str,
    ) -> crate::error::Result<()> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn delete_label(&self, _label_id: &str) -> crate::error::Result<()> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn set_labels(&self, _id: u64, _label_ids: Vec<String>) -> crate::error::Result<()> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn set_label_membership(
        &self,
        _id: u64,
        _label_id: &str,
        _attached: bool,
    ) -> crate::error::Result<()> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
    async fn clear_history(&self) -> crate::error::Result<()> {
        self.store.clear().await.map_err(db_error)
    }
    async fn sync_records(
        &self,
        after_local_id: u64,
        limit: usize,
    ) -> crate::error::Result<ClipboardSyncPage> {
        self.store
            .sync_records(after_local_id, limit)
            .await
            .map_err(db_error)
    }
    async fn sync_record_requires_payload(
        &self,
        sync_id: &str,
        revision: u64,
        updated_by_device_id: &str,
    ) -> crate::error::Result<bool> {
        self.store
            .sync_record_requires_payload(sync_id, revision, updated_by_device_id)
            .await
            .map_err(db_error)
    }
    async fn apply_sync_record(
        &self,
        record: ClipboardSyncRecord,
        _update_system_clipboard: bool,
        _relay_change: bool,
    ) -> crate::error::Result<bool> {
        self.store.apply_sync_record(record).await.map_err(db_error)
    }
    async fn replica_page(
        &self,
        _cursor: Option<ClipboardReplicaCursor>,
        _limit: usize,
    ) -> crate::error::Result<ClipboardReplicaPage> {
        self.store
            .replica_page(_cursor, _limit)
            .await
            .map_err(db_error)
    }
    async fn replica_record(
        &self,
        _sync_id: &str,
    ) -> crate::error::Result<Option<ClipboardReplicaRecord>> {
        self.store.replica_record(_sync_id).await.map_err(db_error)
    }
    async fn check_replica_storage(&self, replica: &ClipboardReplicaRecord) -> Result<()> {
        self.store
            .check_replica_storage(replica)
            .await
            .map_err(db_error)
    }
    async fn replica_selection(&self) -> Result<Option<ClipboardSyncRecord>> {
        self.store.replica_selection().await.map_err(db_error)
    }
    async fn replica_labels(&self) -> crate::error::Result<Vec<ClipboardLabel>> {
        self.store.replica_labels().await.map_err(db_error)
    }
    async fn apply_replica_labels(
        &self,
        _labels: Vec<ClipboardLabel>,
    ) -> crate::error::Result<usize> {
        self.store
            .apply_replica_labels(_labels)
            .await
            .map_err(db_error)
    }
    async fn apply_replica_record(
        &self,
        replica: ClipboardReplicaRecord,
        _update_system_clipboard: bool,
    ) -> crate::error::Result<bool> {
        let record = replica.record.clone();
        let changed = self
            .store
            .apply_replica_record(replica)
            .await
            .map_err(db_error)?;
        if changed {
            let _ = self.events.send(record);
        }
        Ok(changed)
    }
    async fn edit_text(&self, _id: u64, _content: &str) -> crate::error::Result<()> {
        Err(crate::Error::Clipboard(
            "native operation is unavailable in a database test".into(),
        ))
    }
}
struct NoInput;
#[async_trait::async_trait]
impl InputControlRepository for NoInput {
    fn permission_state(&self) -> InputPermissionState {
        InputPermissionState::Unsupported
    }
    fn open_permission_settings(&self) -> Result<()> {
        unreachable!("test must not use native input")
    }
    fn validate_events(&self, _: &[InputEvent]) -> Result<()> {
        unreachable!("test must not use native input")
    }
    async fn apply_events(&self, _: &[InputEvent]) -> Result<()> {
        unreachable!("test must not use native input")
    }
    async fn paste_clipboard(&self, _: bool) -> Result<()> {
        unreachable!("test must not use native input")
    }
    async fn release_all(&self) -> Result<()> {
        Ok(())
    }
}
