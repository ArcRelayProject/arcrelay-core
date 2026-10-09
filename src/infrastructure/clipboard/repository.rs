use super::*;

#[async_trait::async_trait]
impl ClipboardRepository for NativeClipboard {
    async fn shutdown(&self) {
        let receipt = self
            .capture_state
            .lock()
            .ok()
            .and_then(|s| s.private_receipt.clone());
        if let Some(receipt) = receipt {
            let _ = self.clear_ephemeral_text(&receipt).await;
        }
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

    async fn export_records(&self, ids: Vec<u64>) -> Result<Vec<ClipboardExportRecord>> {
        self.request(|response| DbCommand::ExportRecords(ids, response))
            .await
    }

    async fn validate_export(&self, versions: Vec<(u64, String)>) -> Result<bool> {
        self.request(|response| DbCommand::ValidateExport(versions, response))
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

    async fn text_preview(
        &self,
        id: u64,
        format: Option<crate::domain::clipboard::ClipboardTextFormat>,
    ) -> Result<crate::domain::clipboard::ClipboardTextPreview> {
        let html = self
            .request(|response| DbCommand::HtmlPayload(id, response))
            .await?;
        let is_html = html.is_some();
        let source = match html {
            Some(html) => html,
            None => self.text_content(id).await?,
        };
        tokio::task::spawn_blocking(move || text_preview::build_preview(source, is_html, format))
            .await
            .map_err(|error| Error::Clipboard(error.to_string()))
    }

    async fn set_files(&self, paths: Vec<String>) -> Result<()> {
        let paths = tokio::task::spawn_blocking(move || validate_file_paths(paths))
            .await
            .map_err(|error| Error::Clipboard(error.to_string()))??;
        if !self.policy().await?.history_enabled {
            return Err(Error::Clipboard(
                "clipboard history is disabled or locked".into(),
            ));
        }
        let payload = ClipboardPayload::Files(paths);
        let fingerprints = clipboard_fingerprints(&payload);
        self.write_system_clipboard(payload.clone(), true)?;
        if let Some(record) = self
            .store_payload(payload, Some("ArcRelay Nearby".into()), true)
            .await?
        {
            self.commit_system_selection(&record, &fingerprints)?;
        }
        Ok(())
    }

    async fn policy(&self) -> Result<ClipboardPolicy> {
        self.request(DbCommand::Policy).await
    }

    async fn update_policy(&self, policy: ClipboardPolicy) -> Result<()> {
        self.request(|response| DbCommand::UpdatePolicy(policy, response))
            .await
    }

    async fn set_ephemeral_text(&self, content: &str) -> Result<String> {
        let context = self
            .context
            .lock()
            .map_err(|_| Error::Clipboard("clipboard lock poisoned".into()))?;
        let mut state = lock_capture_state(&self.capture_state)?;
        let receipt = uuid::Uuid::new_v4().to_string();
        #[cfg(target_os = "macos")]
        let result = {
            let _ = &context;
            macos::write_private(content, &receipt)
        };
        #[cfg(target_os = "windows")]
        let result = {
            let _ = &context;
            private_windows::write(content, &receipt)
        };
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let result = {
            let _ = (&context, receipt, content);
            Err(Error::NotSupported(
                "Private credential copying is not supported on this system".into(),
            ))
        };
        let receipt = result?;
        state.private_receipt = Some(receipt.clone());
        Ok(receipt)
    }

    async fn clear_ephemeral_text(&self, receipt: &str) -> Result<bool> {
        let context = self
            .context
            .lock()
            .map_err(|_| Error::Clipboard("clipboard lock poisoned".into()))?;
        let _state = lock_capture_state(&self.capture_state)?;
        #[cfg(target_os = "macos")]
        {
            let _ = &context;
            Ok(macos::clear_private(receipt))
        }
        #[cfg(target_os = "windows")]
        {
            let _ = &context;
            private_windows::clear(receipt)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = (&context, receipt);
            Ok(false)
        } // Without atomic ownership, preserve the user's newer clipboard.
    }

    async fn clear_owned_ephemeral_text(&self) -> Result<bool> {
        let receipt = lock_capture_state(&self.capture_state)?
            .private_receipt
            .clone();
        if let Some(receipt) = receipt {
            self.clear_ephemeral_text(&receipt).await
        } else {
            Ok(false)
        }
    }

    async fn set_text(&self, content: &str) -> Result<()> {
        let payload = ClipboardPayload::Text(content.to_string());
        let fingerprints = clipboard_fingerprints(&payload);
        self.write_system_clipboard(payload.clone(), true)?;
        if let Some(record) = self
            .store_payload(payload, Some("ArcRelay Mobile".into()), true)
            .await?
        {
            self.commit_system_selection(&record, &fingerprints)?;
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
        let image_file = matches!(
            mode,
            ClipboardPasteMode::ImageJpg | ClipboardPasteMode::ImagePng
        );
        let payload = if image_file {
            tokio::task::spawn_blocking(move || prepare_image_file(payload, mode))
                .await
                .map_err(|error| Error::Clipboard(error.to_string()))??
        } else {
            payload
        };
        let kind = payload_kind(&payload);
        let fingerprints = clipboard_fingerprints(&payload);
        // Local file URLs must not be handed off to another device.
        self.write_system_clipboard(payload.clone(), kind == ClipboardContentKind::Files)?;
        if let Some(record) = self
            .store_payload(payload, Some("ArcRelay".into()), true)
            .await?
        {
            self.commit_system_selection(&record, &fingerprints)?;
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

    async fn app_pins(
        &self,
        app_id: String,
        query: ClipboardQuery,
    ) -> Result<Vec<ClipboardSummary>> {
        let mut entries = self
            .request(|response| DbCommand::AppPins(app_id, query, response))
            .await?;
        for summary in &mut entries {
            hide_local_source_device(summary, &self.local_device_id);
        }
        Ok(entries)
    }

    async fn set_app_pin(&self, id: u64, app_id: String, pinned: bool) -> Result<()> {
        self.request(|response| DbCommand::SetAppPin(id, app_id, pinned, response))
            .await
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

    async fn save_edited(
        &self,
        session: String,
        source_id: u64,
        payload: ClipboardPayload,
    ) -> Result<u64> {
        uuid::Uuid::parse_str(&session)
            .map_err(|_| Error::Clipboard("invalid edit session".into()))?;
        match &payload {
            ClipboardPayload::Text(text) if !text.is_empty() && text.len() <= 1024 * 1024 => {}
            ClipboardPayload::Image { png, width, height }
                if !png.is_empty()
                    && png.len() <= MAX_CAPTURED_IMAGE_BYTES
                    && *width > 0
                    && *height > 0
                    && u64::from(*width) * u64::from(*height) <= 32 * 1024 * 1024 => {}
            _ => {
                return Err(Error::Clipboard(
                    "edited content is empty, unsupported or too large".into(),
                ))
            }
        }
        let hash = content_hash(&payload);
        let summary = summarize(&payload, Some("ArcRelay".into()));
        let (id, record) = self
            .request(|response| DbCommand::SaveEdited {
                session,
                source_id,
                payload,
                hash,
                summary,
                device: self.local_device_id.clone(),
                name: self.local_device_name.clone(),
                response,
            })
            .await?;
        let _ = self.sync_tx.send(record);
        Ok(id)
    }

    async fn copy_edited(&self, id: u64) -> Result<()> {
        if self.edit_origins(vec![id]).await?.is_empty() {
            return Err(Error::Clipboard("edited record is unavailable".into()));
        }
        let mut records = self.export_records(vec![id]).await?;
        let payload = records.remove(0).payload;
        let fingerprints = clipboard_fingerprints(&payload);
        self.write_system_clipboard(payload, true)?;
        let record = self
            .request(|response| DbCommand::SelectEdited(id, self.local_device_id.clone(), response))
            .await?;
        self.commit_system_selection(&record, &fingerprints)?;
        let _ = self.sync_tx.send(record);
        Ok(())
    }

    async fn edit_origins(&self, ids: Vec<u64>) -> Result<Vec<(u64, u64)>> {
        self.request(|response| DbCommand::EditOrigins(ids, response))
            .await
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
