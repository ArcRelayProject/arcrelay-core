use super::*;

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn replica_page(
        &self,
        cursor: Option<ClipboardReplicaCursor>,
        limit: usize,
    ) -> Result<ClipboardReplicaPage, DbErr> {
        let limit = limit.clamp(1, 100);
        let mut query = summary_query()
            .filter(clipboard_entry::Column::Kind.ne(kind_to_i32(ClipboardContentKind::Files)));
        if let Some(cursor) = cursor {
            query = query.filter(
                Condition::any()
                    .add(clipboard_entry::Column::CapturedAtMs.lt(cursor.captured_at_ms))
                    .add(
                        Condition::all()
                            .add(clipboard_entry::Column::CapturedAtMs.eq(cursor.captured_at_ms))
                            .add(clipboard_entry::Column::SyncId.lt(cursor.sync_id)),
                    ),
            );
        }
        let mut models = query
            .order_by_desc(clipboard_entry::Column::CapturedAtMs)
            .order_by_desc(clipboard_entry::Column::SyncId)
            .limit((limit + 1) as u64)
            .all(&self.db)
            .await?;
        let more = models.len() > limit;
        models.truncate(limit);
        let next_cursor = if more {
            models.last().map(|m| ClipboardReplicaCursor {
                captured_at_ms: m.captured_at_ms,
                sync_id: m.sync_id.clone(),
            })
        } else {
            None
        };
        let ids = models.iter().map(|m| m.sync_id.clone()).collect::<Vec<_>>();
        let mut memberships = HashMap::<String, Vec<ClipboardLabelMembership>>::new();
        if !ids.is_empty() {
            for model in clipboard_entry_label::Entity::find()
                .filter(clipboard_entry_label::Column::EntrySyncId.is_in(ids))
                .all(&self.db)
                .await?
            {
                memberships.entry(model.entry_sync_id).or_default().push(
                    ClipboardLabelMembership {
                        label_id: model.label_id,
                        attached: model.attached,
                        revision: model.revision.max(1) as u64,
                        updated_by_device_id: model.updated_by_device_id,
                    },
                );
            }
        }
        let records = models
            .into_iter()
            .map(|model| {
                let labels = memberships.remove(&model.sync_id).unwrap_or_default();
                replica_metadata(&model, labels)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ClipboardReplicaPage {
            records,
            next_cursor,
        })
    }

    pub(in crate::infrastructure) async fn replica_record(
        &self,
        sync_id: &str,
    ) -> Result<Option<ClipboardReplicaRecord>, DbErr> {
        let Some(model) = clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::SyncId.eq(sync_id))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        if kind_from_i32(model.kind)? == ClipboardContentKind::Files {
            return Ok(None);
        }
        let payload = clipboard_payload::Entity::find_by_id(model.id)
            .one(&self.db)
            .await?;
        let (labels, memberships) = self.label_states_for_sync_id(&model.sync_id).await?;
        let mut replica = replica_metadata(&model, memberships)?;
        replica.record.labels = labels;
        if !model.deleted {
            let payload =
                payload.ok_or_else(|| DbErr::Custom("clipboard payload is unavailable".into()))?;
            replica.record.text = payload.text_payload.or(payload.plain_text_payload);
            replica.record.html = payload.html_payload;
            replica.record.rtf = payload.rtf_payload;
            replica.record.image_png = payload.image_png;
        }
        Ok(Some(replica))
    }

    pub(in crate::infrastructure) async fn replica_labels(
        &self,
    ) -> Result<Vec<ClipboardLabel>, DbErr> {
        Ok(clipboard_label::Entity::find()
            .order_by_asc(clipboard_label::Column::Id)
            .all(&self.db)
            .await?
            .into_iter()
            .map(label_from_model)
            .collect())
    }

    pub(in crate::infrastructure) async fn apply_replica_labels(
        &self,
        labels: Vec<ClipboardLabel>,
    ) -> Result<usize, DbErr> {
        let transaction = self.db.begin().await?;
        let changed = self.apply_label_definitions(&transaction, &labels).await?;
        if changed > 0 {
            bump_revision(&transaction).await?;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    pub(in crate::infrastructure) async fn apply_replica_record(
        &self,
        replica: ClipboardReplicaRecord,
    ) -> Result<bool, DbErr> {
        self.check_replica_storage(&replica).await?;
        self.apply_sync_record_with_timeline(
            replica.record,
            Some((replica.first_captured_at_ms, replica.copy_count)),
        )
        .await
    }
}

fn replica_metadata(
    model: &clipboard_entry::Model,
    memberships: Vec<ClipboardLabelMembership>,
) -> Result<ClipboardReplicaRecord, DbErr> {
    Ok(ClipboardReplicaRecord {
        // Older v1 imports could move the last copy before the local first
        // capture. Keep exports valid even while a legacy peer is connected.
        first_captured_at_ms: model.first_captured_at_ms.min(model.captured_at_ms),
        copy_count: model.copy_count.max(1) as u32,
        record: ClipboardSyncRecord {
            sync_id: model.sync_id.clone(),
            kind: kind_from_i32(model.kind)?,
            text: None,
            html: None,
            rtf: None,
            image_png: None,
            width: model.width.map(|v| v.max(0) as u32),
            height: model.height.map(|v| v.max(0) as u32),
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
            labels: Vec::new(),
            label_memberships: memberships,
            deleted: model.deleted,
            change_kind: ClipboardSyncChangeKind::Snapshot,
            live: false,
            text_syntax: decode_text_syntax(&model.text_syntax)?,
        },
    })
}

pub(super) struct RepairLegacyTimelineMigration;
impl sea_orm_migration::MigrationName for RepairLegacyTimelineMigration {
    fn name(&self) -> &str {
        "m20260908_repair_legacy_clipboard_timeline_v11"
    }
}
#[async_trait::async_trait]
impl sea_orm_migration::MigrationTrait for RepairLegacyTimelineMigration {
    async fn up(&self, manager: &sea_orm_migration::SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE clipboard_state SET revision = revision + 1 WHERE EXISTS (
                SELECT 1 FROM clipboard_entries
                WHERE captured_at_ms > 0 AND first_captured_at_ms > captured_at_ms);
             UPDATE clipboard_entries SET first_captured_at_ms = captured_at_ms
             WHERE captured_at_ms > 0 AND first_captured_at_ms > captured_at_ms;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &sea_orm_migration::SchemaManager) -> Result<(), DbErr> {
        // Repaired timestamps are valid for both schema versions. Their former
        // invalid values must not be reconstructed during a downgrade.
        Ok(())
    }
}

// A single durable selection is enough: newer live copies supersede older
// selections, while all historical records are reconciled independently.
pub(super) struct ReplicaStateMigration;
impl sea_orm_migration::MigrationName for ReplicaStateMigration {
    fn name(&self) -> &str {
        "m20260908_clipboard_replica_selection_v10"
    }
}
#[async_trait::async_trait]
impl sea_orm_migration::MigrationTrait for ReplicaStateMigration {
    async fn up(&self, manager: &sea_orm_migration::SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE clipboard_replica_selection (
            id INTEGER PRIMARY KEY CHECK(id = 1), sync_id TEXT NOT NULL,
            captured_at_ms INTEGER NOT NULL, updated_by_device_id TEXT NOT NULL, revision INTEGER NOT NULL);
            CREATE INDEX idx_clipboard_replica_recent ON clipboard_entries(captured_at_ms, sync_id);
            CREATE INDEX idx_clipboard_replica_updated ON clipboard_entries(deleted, updated_at_ms, sync_id);
            CREATE INDEX idx_clipboard_replica_created ON clipboard_entries(deleted, first_captured_at_ms, sync_id);").await?;
        Ok(())
    }
    async fn down(&self, manager: &sea_orm_migration::SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP INDEX idx_clipboard_replica_created;
            DROP INDEX idx_clipboard_replica_updated; DROP INDEX idx_clipboard_replica_recent;
            DROP TABLE clipboard_replica_selection;",
            )
            .await?;
        Ok(())
    }
}
impl SqliteClipboardStore {
    pub(super) async fn remember_selection<C: ConnectionTrait>(
        db: &C,
        sync_id: &str,
        captured_at_ms: i64,
        updated_by_device_id: &str,
        revision: u64,
    ) -> Result<bool, DbErr> {
        let changed = db.execute(Statement::from_sql_and_values(sea_orm::DbBackend::Sqlite,
            "INSERT INTO clipboard_replica_selection (id, sync_id, captured_at_ms, updated_by_device_id, revision)
             VALUES (1, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET sync_id=excluded.sync_id,
             captured_at_ms=excluded.captured_at_ms, updated_by_device_id=excluded.updated_by_device_id, revision=excluded.revision
             WHERE (excluded.captured_at_ms, excluded.updated_by_device_id, excluded.sync_id, excluded.revision) >
             (clipboard_replica_selection.captured_at_ms, clipboard_replica_selection.updated_by_device_id,
              clipboard_replica_selection.sync_id, clipboard_replica_selection.revision)",
            [sync_id.into(), captured_at_ms.into(), updated_by_device_id.into(), (revision.min(i64::MAX as u64) as i64).into()]
        )).await?.rows_affected();
        Ok(changed > 0)
    }

    pub(in crate::infrastructure) async fn replica_selection(
        &self,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let Some(selection) = self.db.query_one(Statement::from_string(sea_orm::DbBackend::Sqlite,
            "SELECT sync_id, captured_at_ms, updated_by_device_id, revision FROM clipboard_replica_selection WHERE id=1".to_owned())).await?
            else { return Ok(None) };
        let id: String = selection.try_get("", "sync_id")?;
        let Some(model) = summary_query()
            .filter(clipboard_entry::Column::SyncId.eq(id))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        if model.deleted
            || kind_from_i32(model.kind)? == ClipboardContentKind::Files
            || model.sync_revision != selection.try_get::<i64>("", "revision")?
            || model.updated_by_device_id
                != selection.try_get::<String>("", "updated_by_device_id")?
        {
            return Ok(None);
        }
        let mut record = replica_metadata(&model, Vec::new())?.record;
        record.captured_at_ms = selection.try_get("", "captured_at_ms")?;
        record.live = true;
        record.change_kind = ClipboardSyncChangeKind::Copy;
        Ok(Some(record))
    }
}

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn check_replica_storage(
        &self,
        replica: &ClipboardReplicaRecord,
    ) -> Result<(), DbErr> {
        let record = &replica.record;
        if record.deleted || record.favorite || record.label_memberships.iter().any(|m| m.attached)
        {
            return Ok(());
        }
        // Existing metadata updates and explicit tombstones must still converge.
        if clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::SyncId.eq(&record.sync_id))
            .count(&self.db)
            .await?
            > 0
        {
            return Ok(());
        }
        let policy = self.policy().await?;
        if policy.retention_days > 0
            && record.captured_at_ms
                < Utc::now().timestamp_millis() - i64::from(policy.retention_days) * 86_400_000
        {
            return Err(DbErr::Custom(
                "record is outside this device's retention period".into(),
            ));
        }
        let totals = self
            .db
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "SELECT item_count, total_bytes FROM clipboard_retention_statistics WHERE scope=1"
                    .to_owned(),
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard accounting is missing".into()))?;
        if totals.try_get::<i64>("", "item_count")? < i64::from(policy.max_items.max(1))
            && (totals.try_get::<i64>("", "total_bytes")?.max(0) as u64) < policy.max_bytes.max(1)
        {
            return Ok(());
        }
        let oldest = summary_query().filter(clipboard_entry::Column::Deleted.eq(false))
            .filter(clipboard_entry::Column::Kind.ne(4))
            .filter(clipboard_entry::Column::Favorite.eq(false))
            .filter(Expr::cust("sync_id NOT IN (SELECT entry_sync_id FROM clipboard_entry_labels WHERE attached=1)"))
            .order_by_asc(clipboard_entry::Column::CapturedAtMs).order_by_asc(clipboard_entry::Column::SyncId)
            .one(&self.db).await?;
        if oldest.is_none_or(|oldest| {
            (record.captured_at_ms, &record.sync_id) <= (oldest.captured_at_ms, &oldest.sync_id)
        }) {
            return Err(DbErr::Custom("record is outside this device's retained history window; increase its history limits to include older records".into()));
        }
        Ok(())
    }
}

impl SqliteClipboardStore {
    pub(super) async fn enforce_replica_policy<C: ConnectionTrait>(
        &self,
        db: &C,
        record: &ClipboardSyncRecord,
    ) -> Result<(), DbErr> {
        let state = clipboard_state::Entity::find_by_id(STATE_ID)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard state is unavailable".into()))?;
        self.prune(db, &policy_from_state(&state)).await?;
        if !record.deleted
            && clipboard_entry::Entity::find()
                .filter(clipboard_entry::Column::SyncId.eq(&record.sync_id))
                .count(db)
                .await?
                == 0
        {
            return Err(DbErr::Custom(
                "clipboard history limits cannot retain this record; increase the history limits"
                    .into(),
            ));
        }
        Ok(())
    }
}
