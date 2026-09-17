use super::*;

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn image_ocr(
        &self,
        id: u64,
    ) -> Result<Option<ClipboardImageOcr>, DbErr> {
        match self.image_ocr_state(id).await? {
            ImageOcrState::Completed(result) => Ok(Some(result)),
            ImageOcrState::Missing | ImageOcrState::Pending | ImageOcrState::Failed(_) => Ok(None),
        }
    }

    pub(in crate::infrastructure) async fn image_ocr_state(
        &self,
        id: u64,
    ) -> Result<ImageOcrState, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let Some(model) = clipboard_image_ocr::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        else {
            return Ok(ImageOcrState::Missing);
        };
        match model.status {
            OCR_STATUS_PENDING => Ok(ImageOcrState::Pending),
            OCR_STATUS_COMPLETED => {
                let blocks = clipboard_ocr_block::Entity::find()
                    .filter(clipboard_ocr_block::Column::EntryId.eq(id))
                    .order_by_asc(clipboard_ocr_block::Column::BlockIndex)
                    .all(&self.db)
                    .await?
                    .into_iter()
                    .map(ocr_block_from_model)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(ImageOcrState::Completed(ClipboardImageOcr {
                    text: model.full_text,
                    blocks,
                    model_version: model.model_version,
                    updated_at_ms: model.updated_at_ms,
                }))
            }
            OCR_STATUS_FAILED => Ok(ImageOcrState::Failed(
                model
                    .error
                    .unwrap_or_else(|| "clipboard image OCR failed".into()),
            )),
            status => Err(DbErr::Custom(format!(
                "invalid clipboard OCR status {status}"
            ))),
        }
    }

    /// Returns PNG bytes only while the OCR record is still pending. This
    /// makes duplicate queue entries harmless and avoids retaining image bytes
    /// in the queue itself.
    pub(in crate::infrastructure) async fn pending_image_ocr_source(
        &self,
        id: u64,
    ) -> Result<Option<Vec<u8>>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let entry = clipboard_entry::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        if !entry.is_some_and(|entry| {
            !entry.deleted && entry.kind == kind_to_i32(ClipboardContentKind::Image)
        }) {
            return Ok(None);
        }
        let pending = clipboard_image_ocr::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .is_some_and(|model| model.status == OCR_STATUS_PENDING);
        if !pending {
            return Ok(None);
        }
        Ok(clipboard_payload::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .and_then(|payload| payload.image_png))
    }

    pub(in crate::infrastructure) async fn ensure_image_ocr_pending(
        &self,
        sync_id: &str,
        model_version: &str,
        retry_failed: bool,
    ) -> Result<Option<u64>, DbErr> {
        let Some(entry) = clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::SyncId.eq(sync_id))
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .filter(clipboard_entry::Column::Kind.eq(kind_to_i32(ClipboardContentKind::Image)))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        let existing = clipboard_image_ocr::Entity::find_by_id(entry.id)
            .one(&self.db)
            .await?;
        let should_queue = existing.as_ref().is_none_or(|model| {
            model.model_version != model_version
                || model.status == OCR_STATUS_PENDING
                || retry_failed && model.status == OCR_STATUS_FAILED
        });
        if !should_queue {
            return Ok(None);
        }
        let transaction = self.db.begin().await?;
        clipboard_image_ocr::Entity::insert(clipboard_image_ocr::ActiveModel {
            entry_id: Set(entry.id),
            status: Set(OCR_STATUS_PENDING),
            full_text: Set(String::new()),
            error: Set(None),
            model_version: Set(model_version.to_string()),
            updated_at_ms: Set(Utc::now().timestamp_millis()),
        })
        .on_conflict(
            OnConflict::column(clipboard_image_ocr::Column::EntryId)
                .update_columns([
                    clipboard_image_ocr::Column::Status,
                    clipboard_image_ocr::Column::FullText,
                    clipboard_image_ocr::Column::Error,
                    clipboard_image_ocr::Column::ModelVersion,
                    clipboard_image_ocr::Column::UpdatedAtMs,
                ])
                .to_owned(),
        )
        .exec(&transaction)
        .await?;
        clipboard_ocr_block::Entity::delete_many()
            .filter(clipboard_ocr_block::Column::EntryId.eq(entry.id))
            .exec(&transaction)
            .await?;
        transaction.commit().await?;
        Ok(Some(entry.id.max(0) as u64))
    }

    /// Select only stale/missing work and update a bounded batch on the writer.
    /// The keyset cursor visits pending rows once during this startup pass.
    pub(in crate::infrastructure) async fn backfill_image_ocr_batch(
        &self,
        model_version: &str,
        after_id: i64,
    ) -> Result<Vec<u64>, DbErr> {
        let transaction = self.db.begin().await?;
        let rows = transaction.query_all(Statement::from_sql_and_values(
            sea_orm::DbBackend::Sqlite,
            "SELECT e.id FROM clipboard_entries e LEFT JOIN clipboard_image_ocr o ON o.entry_id=e.id WHERE e.id>? AND e.deleted=0 AND e.kind=? AND (o.entry_id IS NULL OR o.model_version<>? OR o.status<>?) ORDER BY e.id LIMIT 32",
            [after_id.into(), kind_to_i32(ClipboardContentKind::Image).into(), model_version.into(), OCR_STATUS_COMPLETED.into()],
        )).await?;
        let ids = rows
            .into_iter()
            .map(|row| row.try_get::<i64>("", "id"))
            .collect::<Result<Vec<_>, _>>()?;
        if !ids.is_empty() {
            let placeholders = std::iter::repeat_n("?", ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let mut values: Vec<sea_orm::Value> =
                vec![model_version.into(), Utc::now().timestamp_millis().into()];
            values.extend(ids.iter().copied().map(Into::into));
            transaction.execute(Statement::from_sql_and_values(
                sea_orm::DbBackend::Sqlite,
                format!("INSERT INTO clipboard_image_ocr(entry_id,status,full_text,error,model_version,updated_at_ms) SELECT id,0,'',NULL,?,? FROM clipboard_entries WHERE id IN ({placeholders}) ON CONFLICT(entry_id) DO UPDATE SET status=0,full_text='',error=NULL,model_version=excluded.model_version,updated_at_ms=excluded.updated_at_ms"),
                values,
            )).await?;
            clipboard_ocr_block::Entity::delete_many()
                .filter(clipboard_ocr_block::Column::EntryId.is_in(ids.clone()))
                .exec(&transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(ids.into_iter().map(|id| id.max(0) as u64).collect())
    }

    pub(in crate::infrastructure) async fn save_image_ocr(
        &self,
        id: u64,
        result: ClipboardImageOcr,
    ) -> Result<(), DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let transaction = self.db.begin().await?;
        let entry = clipboard_entry::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .filter(|entry| {
                !entry.deleted && entry.kind == kind_to_i32(ClipboardContentKind::Image)
            })
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard image {id}")))?;
        clipboard_image_ocr::Entity::insert(clipboard_image_ocr::ActiveModel {
            entry_id: Set(id),
            status: Set(OCR_STATUS_COMPLETED),
            full_text: Set(result.text),
            error: Set(None),
            model_version: Set(result.model_version),
            updated_at_ms: Set(result.updated_at_ms),
        })
        .on_conflict(
            OnConflict::column(clipboard_image_ocr::Column::EntryId)
                .update_columns([
                    clipboard_image_ocr::Column::Status,
                    clipboard_image_ocr::Column::FullText,
                    clipboard_image_ocr::Column::Error,
                    clipboard_image_ocr::Column::ModelVersion,
                    clipboard_image_ocr::Column::UpdatedAtMs,
                ])
                .to_owned(),
        )
        .exec(&transaction)
        .await?;
        clipboard_ocr_block::Entity::delete_many()
            .filter(clipboard_ocr_block::Column::EntryId.eq(id))
            .exec(&transaction)
            .await?;
        if !result.blocks.is_empty() {
            clipboard_ocr_block::Entity::insert_many(
                result
                    .blocks
                    .into_iter()
                    .enumerate()
                    .map(|(index, block)| ocr_block_into_active_model(id, index, block))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .exec(&transaction)
            .await?;
        }
        let _ = entry;
        transaction.commit().await
    }

    pub(in crate::infrastructure) async fn save_image_ocr_failure(
        &self,
        id: u64,
        error: &str,
        model_version: &str,
    ) -> Result<(), DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let transaction = self.db.begin().await?;
        let exists = clipboard_entry::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .is_some_and(|entry| {
                !entry.deleted && entry.kind == kind_to_i32(ClipboardContentKind::Image)
            });
        if !exists {
            transaction.rollback().await?;
            return Ok(());
        }
        clipboard_image_ocr::Entity::insert(clipboard_image_ocr::ActiveModel {
            entry_id: Set(id),
            status: Set(OCR_STATUS_FAILED),
            full_text: Set(String::new()),
            error: Set(Some(error.to_string())),
            model_version: Set(model_version.to_string()),
            updated_at_ms: Set(Utc::now().timestamp_millis()),
        })
        .on_conflict(
            OnConflict::column(clipboard_image_ocr::Column::EntryId)
                .update_columns([
                    clipboard_image_ocr::Column::Status,
                    clipboard_image_ocr::Column::FullText,
                    clipboard_image_ocr::Column::Error,
                    clipboard_image_ocr::Column::ModelVersion,
                    clipboard_image_ocr::Column::UpdatedAtMs,
                ])
                .to_owned(),
        )
        .exec(&transaction)
        .await?;
        clipboard_ocr_block::Entity::delete_many()
            .filter(clipboard_ocr_block::Column::EntryId.eq(id))
            .exec(&transaction)
            .await?;
        transaction.commit().await
    }
}
