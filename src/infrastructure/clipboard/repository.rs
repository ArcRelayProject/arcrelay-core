use super::*;

#[async_trait::async_trait]
impl ClipboardRepository for NativeClipboard {
    async fn shutdown(&self) {
        self.workers.shutdown().await;
    }

    fn subscribe_captures(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<ClipboardCaptureEvent>> {
        Some(self.capture_tx.subscribe())
    }

    fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
        self.notifications_available
            .then(|| self.change_tx.subscribe())
    }

    fn subscribe_ocr_changes(&self) -> Option<tokio::sync::broadcast::Receiver<u64>> {
        Some(self.ocr_events.subscribe())
    }

    fn subscribe_sync_changes(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<ClipboardSyncRecord>> {
        Some(self.sync_tx.subscribe())
    }

    async fn revision(&self) -> Result<u64> {
        self.request(DbCommand::Revision).await
    }

    async fn current_summary(&self) -> Result<Option<ClipboardSummary>> {
        let mut summary = self.request(DbCommand::CurrentSummary).await?;
        if let Some(summary) = summary.as_mut() {
            hide_local_source_device(summary, &self.local_device_id);
        }
        Ok(summary)
    }

    async fn history(&self, query: ClipboardQuery) -> Result<ClipboardPage> {
        let mut page = self
            .request(|response| DbCommand::History(query, response))
            .await?;
        for summary in &mut page.entries {
            hide_local_source_device(summary, &self.local_device_id);
        }
        Ok(page)
    }

    async fn timeline(
        &self,
        query: ClipboardTimelineQuery,
    ) -> Result<Option<ClipboardTimelinePage>> {
        let mut page = self
            .request(|response| DbCommand::Timeline(query, response))
            .await?;
        if let Some(page) = page.as_mut() {
            for summary in &mut page.entries {
                hide_local_source_device(summary, &self.local_device_id);
            }
        }
        Ok(page)
    }

    async fn image_png(&self, id: u64) -> Result<Option<Vec<u8>>> {
        self.request(|response| DbCommand::ImagePng(id, response))
            .await
    }

    async fn image_ocr(&self, id: u64) -> Result<Option<ClipboardImageOcr>> {
        self.request(|response| DbCommand::ImageOcr(id, response))
            .await
    }

    async fn file_paths(&self, id: u64) -> Result<Vec<String>> {
        self.request(|response| DbCommand::FilePaths(id, response))
            .await
    }

    async fn text_content(&self, id: u64) -> Result<String> {
        self.request(|response| DbCommand::TextContent(id, response))
            .await
    }

    async fn safe_html_preview(&self, id: u64) -> Result<Option<String>> {
        let Some(html) = self
            .request(|response| DbCommand::HtmlPayload(id, response))
            .await?
        else {
            return Ok(None);
        };
        Ok(sanitize_html_preview(&html))
    }

    async fn policy(&self) -> Result<ClipboardPolicy> {
        self.request(DbCommand::Policy).await
    }

    async fn update_policy(&self, policy: ClipboardPolicy) -> Result<()> {
        self.request(|response| DbCommand::UpdatePolicy(policy, response))
            .await
    }

    async fn set_text(&self, content: &str) -> Result<()> {
        let payload = ClipboardPayload::Text(content.to_string());
        self.write_system_clipboard(payload.clone(), true)?;
        if let Some(record) = self
            .store_payload(payload, Some("ArcRelay Mobile".into()), true)
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(())
    }

    async fn activate(&self, id: u64, mode: ClipboardPasteMode) -> Result<ClipboardContentKind> {
        let (payload, _payload_hash, _kind) = self
            .request(|response| DbCommand::Payload(id, response))
            .await?;
        let payload = match (payload, mode) {
            (
                ClipboardPayload::Image {
                    png: _,
                    width: _,
                    height: _,
                },
                ClipboardPasteMode::PlainText,
            ) => ClipboardPayload::Text(self.wait_for_image_ocr_text(id).await?),
            (payload, mode) => convert_payload(&payload, mode)?,
        };
        let kind = payload_kind(&payload);
        self.write_system_clipboard(payload.clone(), false)?;
        if let Some(record) = self
            .store_payload(payload, Some("ArcRelay".into()), true)
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(kind)
    }

    async fn delete(&self, id: u64) -> Result<()> {
        if let Some(record) = self
            .request(|response| DbCommand::Delete(id, self.local_device_id.clone(), response))
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(())
    }

    async fn set_favorite(&self, id: u64, favorite: bool) -> Result<()> {
        if let Some(record) = self
            .request(|response| {
                DbCommand::SetFavorite(id, favorite, self.local_device_id.clone(), response)
            })
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(())
    }

    async fn labels(&self) -> Result<Vec<ClipboardLabel>> {
        self.request(DbCommand::Labels).await
    }

    async fn create_label(&self, name: &str, color: &str) -> Result<ClipboardLabel> {
        let label = self
            .request(|response| {
                DbCommand::CreateLabel(
                    name.to_string(),
                    color.to_string(),
                    self.local_device_id.clone(),
                    response,
                )
            })
            .await?;
        if let Some(record) = self.sync_records(0, 1).await?.records.into_iter().next() {
            if let Some(mut record) = self
                .request(|response| DbCommand::SyncRecords(record.0.saturating_sub(1), 1, response))
                .await?
                .records
                .into_iter()
                .next()
                .map(|(_, record)| record)
            {
                record.change_kind = ClipboardSyncChangeKind::Label;
                let _ = self.sync_tx.send(record);
            }
        }
        Ok(label)
    }

    async fn update_label(&self, label_id: &str, name: &str, color: &str) -> Result<()> {
        if let Some(record) = self
            .request(|response| {
                DbCommand::UpdateLabel(
                    label_id.to_string(),
                    name.to_string(),
                    color.to_string(),
                    self.local_device_id.clone(),
                    response,
                )
            })
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(())
    }

    async fn delete_label(&self, label_id: &str) -> Result<()> {
        for record in self
            .request(|response| {
                DbCommand::DeleteLabel(label_id.to_string(), self.local_device_id.clone(), response)
            })
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(())
    }

    async fn set_labels(&self, id: u64, label_ids: Vec<String>) -> Result<()> {
        if let Some(record) = self
            .request(|response| {
                DbCommand::SetLabels(id, label_ids, self.local_device_id.clone(), response)
            })
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(())
    }

    async fn set_label_membership(&self, id: u64, label_id: &str, attached: bool) -> Result<()> {
        if let Some(record) = self
            .request(|response| {
                DbCommand::SetLabelMembership(
                    id,
                    label_id.to_string(),
                    attached,
                    self.local_device_id.clone(),
                    response,
                )
            })
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(())
    }

    async fn clear_history(&self) -> Result<()> {
        self.request(DbCommand::Clear).await
    }

    async fn sync_records(&self, after_local_id: u64, limit: usize) -> Result<ClipboardSyncPage> {
        self.request(|response| DbCommand::SyncRecords(after_local_id, limit, response))
            .await
    }

    async fn sync_record_requires_payload(
        &self,
        sync_id: &str,
        revision: u64,
        updated_by_device_id: &str,
    ) -> Result<bool> {
        self.request(|response| {
            DbCommand::SyncRecordRequiresPayload(
                sync_id.to_string(),
                revision,
                updated_by_device_id.to_string(),
                response,
            )
        })
        .await
    }

    async fn apply_sync_record(
        &self,
        record: ClipboardSyncRecord,
        update_system_clipboard: bool,
        relay_change: bool,
    ) -> Result<bool> {
        let should_apply = record.live
            && record.change_kind == ClipboardSyncChangeKind::Copy
            && update_system_clipboard
            && !record.deleted;
        let payload = should_apply.then(|| sync_payload(&record)).transpose()?;
        let relay_record = relay_change.then(|| record.clone());
        let changed = self
            .request(|response| DbCommand::ApplySync(record, response))
            .await?;
        if !changed {
            return Ok(false);
        }
        if let Some(payload) = payload {
            self.write_system_clipboard(payload, true)?;
        }
        if let Some(record) = relay_record {
            let _ = self.sync_tx.send(record);
        }
        Ok(true)
    }

    async fn replica_page(
        &self,
        cursor: Option<ClipboardReplicaCursor>,
        limit: usize,
    ) -> Result<ClipboardReplicaPage> {
        self.request(|response| DbCommand::ReplicaPage(cursor, limit, response))
            .await
    }

    async fn replica_record(&self, sync_id: &str) -> Result<Option<ClipboardReplicaRecord>> {
        self.request(|response| DbCommand::ReplicaRecord(sync_id.to_owned(), response))
            .await
    }

    async fn check_replica_storage(&self, replica: &ClipboardReplicaRecord) -> Result<()> {
        self.request(|response| DbCommand::CheckReplicaStorage(replica.clone(), response))
            .await
    }
    async fn replica_selection(&self) -> Result<Option<ClipboardSyncRecord>> {
        self.request(DbCommand::ReplicaSelection).await
    }

    async fn replica_labels(&self) -> Result<Vec<ClipboardLabel>> {
        self.request(DbCommand::ReplicaLabels).await
    }

    async fn apply_replica_labels(&self, labels: Vec<ClipboardLabel>) -> Result<usize> {
        self.request(|response| DbCommand::ApplyReplicaLabels(labels, response))
            .await
    }

    async fn apply_replica_record(
        &self,
        replica: ClipboardReplicaRecord,
        update_system_clipboard: bool,
    ) -> Result<bool> {
        let record = replica.record.clone();
        let live =
            record.live && record.change_kind == ClipboardSyncChangeKind::Copy && !record.deleted;
        let payload = (live && update_system_clipboard)
            .then(|| sync_payload(&record))
            .transpose()?;
        let changed = self
            .request(|response| DbCommand::ApplyReplica(replica, response))
            .await?;
        if changed {
            let _ = self.sync_tx.send(record.clone());
            if live {
                let _ = self.capture_tx.send(ClipboardCaptureEvent {
                    origin: ClipboardCaptureOrigin::Remote,
                    occurred_at: Instant::now(),
                });
            }
        }
        // A previously committed record may still need its native write retried.
        if let Some(payload) = payload {
            let selected = self.replica_selection().await?;
            if selected.is_some_and(|selected| {
                ClipboardSelectionKey::from_record(&selected)
                    == ClipboardSelectionKey::from_record(&record)
            }) {
                self.write_replica_clipboard(&record, payload)?;
            }
        }
        Ok(changed)
    }

    async fn edit_text(&self, id: u64, content: &str) -> Result<()> {
        if content.is_empty() || content.len() > 1024 * 1024 {
            return Err(Error::Clipboard(
                "edited text must be between 1 byte and 1 MiB".into(),
            ));
        }
        if let Some(record) = self
            .request(|response| {
                DbCommand::EditText(
                    id,
                    content.to_string(),
                    self.local_device_id.clone(),
                    response,
                )
            })
            .await?
        {
            let _ = self.sync_tx.send(record);
        }
        Ok(())
    }
}
