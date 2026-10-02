use super::*;
use sea_orm_migration::prelude::{MigrationName, MigrationTrait, SchemaManager};

pub(super) struct EditorMigration;
impl MigrationName for EditorMigration {
    fn name(&self) -> &str {
        "m20261002_clipboard_edit_commits"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for EditorMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "CREATE TABLE clipboard_edit_commits (session TEXT PRIMARY KEY NOT NULL, entry_id INTEGER UNIQUE NOT NULL REFERENCES clipboard_entries(id) ON DELETE CASCADE, source_id INTEGER NOT NULL, content_hash TEXT NOT NULL)"
        ).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE clipboard_edit_commits")
            .await?;
        Ok(())
    }
}

impl SqliteClipboardStore {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::infrastructure) async fn save_edited(
        &self,
        session: &str,
        source_id: u64,
        payload: ClipboardPayload,
        hash: String,
        summary: ClipboardSummary,
        device: &str,
        name: &str,
    ) -> Result<(u64, ClipboardSyncRecord), DbErr> {
        let source_id =
            i64::try_from(source_id).map_err(|_| DbErr::Custom("invalid source id".into()))?;
        let transaction = self.db.begin().await?;
        if let Some(row) = transaction.query_one(Statement::from_sql_and_values(
            sea_orm::DbBackend::Sqlite, "SELECT entry_id, source_id, content_hash FROM clipboard_edit_commits WHERE session = ?", [session.into()],
        )).await? {
            if row.try_get::<i64>("", "source_id")? != source_id || row.try_get::<String>("", "content_hash")? != hash {
                return Err(DbErr::Custom("edit session was already committed with different content".into()));
            }
            let id: i64 = row.try_get("", "entry_id")?;
            transaction.commit().await?;
            return Ok((id as u64, self.edited_sync_record(id as u64).await?));
        }
        let source = clipboard_entry::Entity::find_by_id(source_id)
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::Custom("original clipboard record is unavailable".into()))?;
        if !source.available || source.kind == kind_to_i32(ClipboardContentKind::Files) {
            return Err(DbErr::Custom(
                "original clipboard record cannot be edited".into(),
            ));
        }
        let state = clipboard_state::Entity::find_by_id(STATE_ID)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard state is unavailable".into()))?;
        let policy = policy_from_state(&state);
        if !policy.history_enabled
            || ((source.sensitive || summary.sensitive) && !policy.save_sensitive)
        {
            return Err(DbErr::Custom(
                "clipboard policy prevents saving this edit".into(),
            ));
        }
        let stored = StoredValues::from_payload(payload)?;
        let search_text = build_search_text(&summary, &stored);
        let storage_bytes = stored
            .storage_bytes()
            .saturating_add(byte_len(&hash))
            .saturating_add(byte_len(&summary.preview))
            .saturating_add(byte_len(&search_text));
        let usage = transaction
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "SELECT item_count, total_bytes FROM clipboard_retention_statistics WHERE scope=1"
                    .to_owned(),
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard storage accounting is unavailable".into()))?;
        let total_bytes = usage.try_get::<i64>("", "total_bytes")?.max(0) as u64;
        let item_count = usage.try_get::<i64>("", "item_count")?.max(0) as u64;
        if total_bytes.saturating_add(storage_bytes as u64) > policy.max_bytes
            || item_count >= u64::from(policy.max_items)
        {
            return Err(DbErr::Custom(
                "clipboard history is full; free space or increase its limit before saving this edit".into(),
            ));
        }
        let now = Utc::now().timestamp_millis();
        let id = clipboard_entry::ActiveModel {
            id: Default::default(),
            kind: Set(kind_to_i32(summary.kind)),
            text_syntax: Set(encode_text_syntax(&summary.text_syntax)?),
            content_hash: Set(hash.clone()),
            sync_id: Set(format!("edit-{session}")),
            source_device_id: Set(device.into()),
            source_device_name: Set(name.into()),
            sync_revision: Set(1),
            updated_by_device_id: Set(device.into()),
            deleted: Set(false),
            preview: Set(summary.preview),
            search_text: Set(search_text),
            source_app: Set(summary.source_app),
            first_captured_at_ms: Set(now),
            captured_at_ms: Set(now),
            last_used_at_ms: Set(None),
            updated_at_ms: Set(now),
            copy_count: Set(1),
            size_bytes: Set(summary.size_bytes as i64),
            character_count: Set(stored.character_count(summary.kind)),
            storage_bytes: Set(storage_bytes),
            item_count: Set(summary.item_count as i32),
            width: Set(summary.width.map(|v| v as i32)),
            height: Set(summary.height.map(|v| v as i32)),
            sensitive: Set(source.sensitive || summary.sensitive),
            favorite: Set(false),
            favorite_revision: Set(1),
            favorite_updated_by_device_id: Set(device.into()),
            available: Set(true),
        }
        .insert(&transaction)
        .await?
        .id;
        stored.into_active_model(id).insert(&transaction).await?;
        transaction.execute(Statement::from_sql_and_values(sea_orm::DbBackend::Sqlite,
            "INSERT INTO clipboard_edit_commits (session, entry_id, source_id, content_hash) VALUES (?, ?, ?, ?)",
            [session.into(), id.into(), source_id.into(), hash.into()],
        )).await?;
        bump_revision(&transaction).await?;
        // Keep the original throughout this operation; normal retention still applies later.
        transaction.commit().await?;
        Ok((id as u64, self.edited_sync_record(id as u64).await?))
    }

    async fn edited_sync_record(&self, id: u64) -> Result<ClipboardSyncRecord, DbErr> {
        let model = clipboard_entry::Entity::find_by_id(id as i64)
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::Custom("saved edit is unavailable".into()))?;
        let payload = clipboard_payload::Entity::find_by_id(model.id)
            .one(&self.db)
            .await?;
        let mut record = self
            .model_to_sync_record(&model, payload.as_ref())
            .await?
            .ok_or_else(|| DbErr::Custom("saved edit payload is unavailable".into()))?;
        record.live = false;
        record.change_kind = ClipboardSyncChangeKind::Snapshot;
        Ok(record)
    }

    pub(in crate::infrastructure) async fn edit_origins(
        &self,
        ids: Vec<u64>,
    ) -> Result<Vec<(u64, u64)>, DbErr> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > 500 {
            return Err(DbErr::Custom("too many edit origin ids".into()));
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let rows = self.db.query_all(Statement::from_sql_and_values(sea_orm::DbBackend::Sqlite,
            format!("SELECT entry_id, source_id FROM clipboard_edit_commits WHERE entry_id IN ({placeholders})"),
            ids.into_iter().map(|id| (id as i64).into()),
        )).await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get::<i64>("", "entry_id")? as u64,
                    row.try_get::<i64>("", "source_id")? as u64,
                ))
            })
            .collect()
    }

    pub(in crate::infrastructure) async fn select_edited(
        &self,
        id: u64,
        device: &str,
    ) -> Result<ClipboardSyncRecord, DbErr> {
        let transaction = self.db.begin().await?;
        let model = clipboard_entry::Entity::find_by_id(id as i64)
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::Custom("saved edit is unavailable".into()))?;
        let observed = transaction
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "SELECT captured_at_ms FROM clipboard_replica_selection WHERE id=1".to_owned(),
            ))
            .await?
            .map(|row| row.try_get::<i64>("", "captured_at_ms"))
            .transpose()?
            .unwrap_or_default();
        let now = Utc::now()
            .timestamp_millis()
            .max(observed.saturating_add(1));
        let mut active = model.into_active_model();
        active.captured_at_ms = Set(now);
        active.last_used_at_ms = Set(Some(now));
        active.updated_at_ms = Set(now);
        active.updated_by_device_id = Set(device.into());
        active.sync_revision = Set(active.sync_revision.as_ref().saturating_add(1));
        let model = active.update(&transaction).await?;
        Self::remember_selection(
            &transaction,
            &model.sync_id,
            now,
            device,
            model.sync_revision as u64,
        )
        .await?;
        bump_revision(&transaction).await?;
        transaction.commit().await?;
        let mut record = self.edited_sync_record(id).await?;
        record.live = true;
        record.change_kind = ClipboardSyncChangeKind::Copy;
        Ok(record)
    }
}
