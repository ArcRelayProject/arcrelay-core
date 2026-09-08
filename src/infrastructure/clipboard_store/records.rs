use super::*;

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn connect(
        database_path: Option<&Path>,
    ) -> Result<Self, DbErr> {
        let url = database_path.map_or_else(
            || "sqlite::memory:".to_string(),
            |path| format!("sqlite://{}?mode=rwc", path.display()),
        );
        let mut options = ConnectOptions::new(url.clone());
        options
            .max_connections(1)
            .min_connections(1)
            .sqlx_logging(false);
        let db = Database::connect(options).await?;
        db.execute(Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "PRAGMA foreign_keys = ON".to_string(),
        ))
        .await?;
        db.execute(Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "PRAGMA busy_timeout = 3000".to_string(),
        ))
        .await?;
        if database_path.is_some() {
            db.execute(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "PRAGMA journal_mode = WAL".to_string(),
            ))
            .await?;
            db.execute(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "PRAGMA synchronous = NORMAL".to_string(),
            ))
            .await?;
            db.execute(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "PRAGMA auto_vacuum = INCREMENTAL".to_string(),
            ))
            .await?;
        }
        Migrator::up(&db, None).await?;
        let store = Self {
            db,
            maintenance_pending: std::sync::atomic::AtomicBool::new(true),
        };
        store.ensure_state().await?;
        Ok(store)
    }

    async fn ensure_state(&self) -> Result<(), DbErr> {
        if clipboard_state::Entity::find_by_id(STATE_ID)
            .one(&self.db)
            .await?
            .is_none()
        {
            let policy = ClipboardPolicy::default();
            clipboard_state::ActiveModel {
                id: Set(STATE_ID),
                revision: Set(0),
                history_enabled: Set(policy.history_enabled),
                max_items: Set(policy.max_items as i32),
                max_bytes: Set(policy.max_bytes as i64),
                retention_days: Set(policy.retention_days as i32),
                save_sensitive: Set(policy.save_sensitive),
            }
            .insert(&self.db)
            .await?;
        }
        Ok(())
    }

    pub(in crate::infrastructure) async fn revision(&self) -> Result<u64, DbErr> {
        Ok(self.state().await?.revision.max(0) as u64)
    }

    pub(in crate::infrastructure) async fn policy(&self) -> Result<ClipboardPolicy, DbErr> {
        Ok(policy_from_state(&self.state().await?))
    }

    pub(in crate::infrastructure) async fn update_policy(
        &self,
        policy: ClipboardPolicy,
    ) -> Result<(), DbErr> {
        let transaction = self.db.begin().await?;
        let state = clipboard_state::Entity::find_by_id(STATE_ID)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard state is unavailable".into()))?;
        let mut active = state.into_active_model();
        active.history_enabled = Set(policy.history_enabled);
        active.max_items = Set(policy.max_items as i32);
        active.max_bytes = Set(policy.max_bytes as i64);
        active.retention_days = Set(policy.retention_days as i32);
        active.save_sensitive = Set(policy.save_sensitive);
        active.revision = Set(next_revision(active.revision.as_ref()));
        active.update(&transaction).await?;
        self.prune(&transaction, &policy).await?;
        transaction.commit().await
    }

    // This boundary keeps the complete capture provenance explicit at the
    // single transactional write site.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::infrastructure) async fn store(
        &self,
        payload: ClipboardPayload,
        content_hash: String,
        mut summary: ClipboardSummary,
        touch_existing: bool,
        source_device_id: &str,
        source_device_name: &str,
        live: bool,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let transaction = self.db.begin().await?;
        let state = clipboard_state::Entity::find_by_id(STATE_ID)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard state is unavailable".into()))?;
        let policy = policy_from_state(&state);
        if !policy.history_enabled || (summary.sensitive && !policy.save_sensitive) {
            transaction.rollback().await?;
            return Ok(None);
        }

        let wall_time = Utc::now().timestamp_millis();
        // Advance past the latest observed copy even if another device's wall
        // clock is slightly ahead. Independent concurrent copies still use the
        // deterministic device/record tie-break on receipt.
        let now_ms = if live {
            let observed = transaction
                .query_one(Statement::from_string(
                    sea_orm::DbBackend::Sqlite,
                    "SELECT captured_at_ms FROM clipboard_replica_selection WHERE id=1".to_owned(),
                ))
                .await?
                .map(|row| row.try_get::<i64>("", "captured_at_ms"))
                .transpose()?
                .unwrap_or_default();
            wall_time.max(observed.saturating_add(1))
        } else {
            wall_time
        };
        let stored = StoredValues::from_payload(payload)?;
        let character_count = stored.character_count(summary.kind);
        let sync_id = sync_id_from_hash(&content_hash);
        summary.captured_at = Utc
            .timestamp_millis_opt(now_ms)
            .single()
            .unwrap_or_else(Utc::now);
        let search_text = build_search_text(&summary, &stored);
        let storage_bytes = stored
            .storage_bytes()
            .saturating_add(byte_len(&content_hash))
            .saturating_add(byte_len(&summary.preview))
            .saturating_add(summary.source_app.as_deref().map_or(0, byte_len))
            .saturating_add(byte_len(&search_text));
        if !touch_existing
            && clipboard_entry::Entity::find()
                .filter(clipboard_entry::Column::ContentHash.eq(&content_hash))
                .one(&transaction)
                .await?
                .is_some()
        {
            transaction.rollback().await?;
            return Ok(None);
        }
        let existing = clipboard_entry::Entity::find()
            .filter(
                Condition::any()
                    .add(clipboard_entry::Column::ContentHash.eq(&content_hash))
                    .add(clipboard_entry::Column::SyncId.eq(&sync_id)),
            )
            .order_by_desc(clipboard_entry::Column::CapturedAtMs)
            .order_by_desc(clipboard_entry::Column::Id)
            .one(&transaction)
            .await?;
        let entry_id = if let Some(existing) = existing {
            let id = existing.id;
            let existing_sync_id = existing.sync_id.clone();
            let updated_at_ms = now_ms.max(existing.last_used_at_ms.unwrap_or_default());
            let mut active = existing.into_active_model();
            active.kind = Set(kind_to_i32(summary.kind));
            active.text_syntax = Set(encode_text_syntax(&summary.text_syntax)?);
            active.sync_id = Set(existing_sync_id);
            active.preview = Set(summary.preview);
            active.search_text = Set(search_text);
            active.source_app = Set(summary.source_app);
            active.captured_at_ms = Set(now_ms);
            active.updated_at_ms = Set(updated_at_ms);
            active.copy_count = Set(active.copy_count.as_ref().saturating_add(1));
            active.size_bytes = Set(summary.size_bytes.min(i64::MAX as u64) as i64);
            active.character_count = Set(character_count);
            active.storage_bytes = Set(storage_bytes);
            active.item_count = Set(summary.item_count.min(i32::MAX as u32) as i32);
            active.width = Set(summary.width.map(|value| value.min(i32::MAX as u32) as i32));
            active.height = Set(summary
                .height
                .map(|value| value.min(i32::MAX as u32) as i32));
            active.sensitive = Set(summary.sensitive);
            active.available = Set(stored.available);
            active.deleted = Set(false);
            if active.source_device_id.as_ref().is_empty() {
                active.source_device_id = Set(source_device_id.to_string());
                active.source_device_name = Set(source_device_name.to_string());
            }
            active.updated_by_device_id = Set(source_device_id.to_string());
            active.sync_revision = Set(active.sync_revision.as_ref().saturating_add(1).max(1));
            active.update(&transaction).await?;
            id
        } else {
            clipboard_entry::ActiveModel {
                id: Default::default(),
                kind: Set(kind_to_i32(summary.kind)),
                text_syntax: Set(encode_text_syntax(&summary.text_syntax)?),
                content_hash: Set(content_hash),
                sync_id: Set(sync_id),
                source_device_id: Set(source_device_id.to_string()),
                source_device_name: Set(source_device_name.to_string()),
                sync_revision: Set(1),
                updated_by_device_id: Set(source_device_id.to_string()),
                deleted: Set(false),
                preview: Set(summary.preview),
                search_text: Set(search_text),
                source_app: Set(summary.source_app),
                first_captured_at_ms: Set(now_ms),
                captured_at_ms: Set(now_ms),
                last_used_at_ms: Set(None),
                updated_at_ms: Set(now_ms),
                copy_count: Set(1),
                size_bytes: Set(summary.size_bytes.min(i64::MAX as u64) as i64),
                character_count: Set(character_count),
                storage_bytes: Set(storage_bytes),
                item_count: Set(summary.item_count.min(i32::MAX as u32) as i32),
                width: Set(summary.width.map(|value| value.min(i32::MAX as u32) as i32)),
                height: Set(summary
                    .height
                    .map(|value| value.min(i32::MAX as u32) as i32)),
                sensitive: Set(summary.sensitive),
                favorite: Set(false),
                favorite_revision: Set(1),
                favorite_updated_by_device_id: Set(source_device_id.to_string()),
                available: Set(stored.available),
            }
            .insert(&transaction)
            .await?
            .id
        };
        clipboard_payload::Entity::insert(stored.into_active_model(entry_id))
            .on_conflict(
                OnConflict::column(clipboard_payload::Column::EntryId)
                    .update_columns([
                        clipboard_payload::Column::TextPayload,
                        clipboard_payload::Column::HtmlPayload,
                        clipboard_payload::Column::PlainTextPayload,
                        clipboard_payload::Column::RtfPayload,
                        clipboard_payload::Column::ImagePng,
                        clipboard_payload::Column::FilesJson,
                    ])
                    .to_owned(),
            )
            .exec(&transaction)
            .await?;
        if live {
            let model = clipboard_entry::Entity::find_by_id(entry_id)
                .one(&transaction)
                .await?
                .ok_or_else(|| DbErr::Custom("stored clipboard record is unavailable".into()))?;
            Self::remember_selection(
                &transaction,
                &model.sync_id,
                model.captured_at_ms,
                &model.updated_by_device_id,
                model.sync_revision.max(1) as u64,
            )
            .await?;
        }
        bump_revision(&transaction).await?;
        self.prune(&transaction, &policy).await?;
        transaction.commit().await?;
        let model = clipboard_entry::Entity::find_by_id(entry_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::Custom("stored clipboard record is unavailable".into()))?;
        let payload = clipboard_payload::Entity::find_by_id(entry_id)
            .one(&self.db)
            .await?;
        Ok(self
            .model_to_sync_record(&model, payload.as_ref())
            .await?
            .map(|mut record| {
                record.live = live;
                record.change_kind = if live {
                    ClipboardSyncChangeKind::Copy
                } else {
                    ClipboardSyncChangeKind::Snapshot
                };
                record
            }))
    }

    pub(in crate::infrastructure) async fn history(
        &self,
        query: ClipboardQuery,
    ) -> Result<ClipboardPage, DbErr> {
        let db = self.db.begin().await?;
        let unfiltered = !query.favorite_only
            && query.kinds.is_empty()
            && query.label_ids.is_empty()
            && query
                .search
                .as_deref()
                .is_none_or(|search| search.trim().is_empty());
        let limit = query.limit.clamp(1, 200);
        let mut finder = summary_query().filter(clipboard_entry::Column::Deleted.eq(false));
        if query.favorite_only {
            finder = finder.filter(clipboard_entry::Column::Favorite.eq(true));
        }
        if !query.label_ids.is_empty() {
            let matching_entries = clipboard_entry_label::Entity::find()
                .select_only()
                .column(clipboard_entry_label::Column::EntrySyncId)
                .filter(clipboard_entry_label::Column::LabelId.is_in(query.label_ids))
                .filter(clipboard_entry_label::Column::Attached.eq(true))
                .into_query();
            finder = finder.filter(clipboard_entry::Column::SyncId.in_subquery(matching_entries));
        }
        if !query.kinds.is_empty() {
            let kinds = query.kinds.into_iter().map(kind_to_i32).collect::<Vec<_>>();
            finder = finder.filter(clipboard_entry::Column::Kind.is_in(kinds));
        }
        if let Some(search) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if search.chars().count() > 256 {
                return Err(DbErr::Custom("clipboard search is too long".into()));
            }
            let search = search.to_lowercase();
            let condition = if search.chars().count() >= 3 {
                // Quoted FTS phrases retain literal substring semantics, including
                // CJK and punctuation. Parameters never become FTS operators.
                Expr::cust_with_values(
                    "id IN (SELECT rowid FROM clipboard_search WHERE clipboard_search MATCH ?)",
                    [format!("\"{}\"", search.replace('"', "\"\""))],
                )
            } else {
                let pattern = format!(
                    "%{}%",
                    search
                        .replace('\\', "\\\\")
                        .replace('%', "\\%")
                        .replace('_', "\\_")
                );
                Expr::cust_with_values("id IN (SELECT id FROM clipboard_search_documents WHERE text LIKE ? ESCAPE '\\')", [pattern])
            };
            finder = finder.filter(condition);
        }
        let total_count = if query.include_total_count {
            Some(if unfiltered {
                Self::item_count(&db).await?
            } else {
                finder.clone().count(&db).await?
            })
        } else {
            None
        };
        if let Some(cursor) = query.cursor {
            finder = finder.filter(cursor_condition(cursor, query.sort_by));
        }
        let sort_expression = sort_expression(query.sort_by);
        let mut models = finder
            .order_by(sort_expression, Order::Desc)
            .order_by_desc(clipboard_entry::Column::SyncId)
            .limit((limit + 1) as u64)
            .all(&db)
            .await?;
        let has_more = models.len() > limit;
        if has_more {
            models.pop();
        }
        let next_cursor = has_more && !models.is_empty();
        let cursor = next_cursor.then(|| {
            let last = models.last().expect("checked non-empty clipboard page");
            ClipboardCursor {
                sort_at_ms: sort_timestamp(last, query.sort_by),
                id: last.id.max(0) as u64,
            }
        });
        let sync_ids = models
            .iter()
            .map(|model| model.sync_id.clone())
            .collect::<Vec<_>>();
        let mut labels_by_sync_id = self.active_labels_for_sync_ids_on(&db, &sync_ids).await?;
        let mut entries = Vec::with_capacity(models.len());
        for model in models {
            let labels = labels_by_sync_id.remove(&model.sync_id).unwrap_or_default();
            entries.push(model_to_summary(model, labels)?);
        }
        db.commit().await?;
        Ok(ClipboardPage {
            entries,
            next_cursor: cursor,
            total_count,
        })
    }

    pub(in crate::infrastructure) async fn current_summary(
        &self,
    ) -> Result<Option<ClipboardSummary>, DbErr> {
        let model = summary_query()
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .order_by_desc(clipboard_entry::Column::CapturedAtMs)
            .order_by_desc(clipboard_entry::Column::SyncId)
            .one(&self.db)
            .await?;
        let Some(model) = model else {
            return Ok(None);
        };
        let labels = self.active_labels_for_sync_id(&model.sync_id).await?;
        model_to_summary(model, labels).map(Some)
    }

    pub(in crate::infrastructure) async fn image_png(
        &self,
        id: u64,
    ) -> Result<Option<Vec<u8>>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        if !model.is_some_and(|entry| entry.kind == kind_to_i32(ClipboardContentKind::Image)) {
            return Ok(None);
        }
        Ok(clipboard_payload::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .and_then(|payload| payload.image_png))
    }

    pub(in crate::infrastructure) async fn file_paths(
        &self,
        id: u64,
    ) -> Result<Vec<String>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard record {id}")))?;
        if model.deleted || kind_from_i32(model.kind)? != ClipboardContentKind::Files {
            return Err(DbErr::Custom(
                "clipboard record is not an available file list".into(),
            ));
        }
        let stored = clipboard_payload::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::Custom(format!("clipboard payload {id} is missing")))?;
        let paths = decode_paths(stored.files_json.as_deref())?;
        if paths.is_empty() || paths.iter().any(|path| !Path::new(path).exists()) {
            return Err(DbErr::Custom(
                "one or more clipboard files are unavailable".into(),
            ));
        }
        Ok(paths)
    }

    pub(in crate::infrastructure) async fn text_content(&self, id: u64) -> Result<String, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard record {id}")))?;
        let kind = kind_from_i32(model.kind)?;
        if model.deleted
            || !matches!(
                kind,
                ClipboardContentKind::Text | ClipboardContentKind::Html
            )
        {
            return Err(DbErr::Custom(
                "clipboard record does not contain readable text".into(),
            ));
        }
        let payload = clipboard_payload::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::Custom(format!("clipboard text payload {id} is missing")))?;
        match kind {
            ClipboardContentKind::Text => payload.text_payload,
            ClipboardContentKind::Html => payload.plain_text_payload,
            ClipboardContentKind::Image | ClipboardContentKind::Files => None,
        }
        .ok_or_else(|| DbErr::Custom(format!("clipboard text payload {id} is missing")))
    }

    pub(in crate::infrastructure) async fn html_payload(
        &self,
        id: u64,
    ) -> Result<Option<String>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        if !model.is_some_and(|entry| {
            entry.kind == kind_to_i32(ClipboardContentKind::Html) && !entry.sensitive
        }) {
            return Ok(None);
        }
        Ok(clipboard_payload::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .and_then(|payload| payload.html_payload))
    }

    pub(in crate::infrastructure) async fn payload(
        &self,
        id: u64,
    ) -> Result<(ClipboardPayload, String, ClipboardContentKind), DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard record {id}")))?;
        let stored = clipboard_payload::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::Custom(format!("clipboard payload {id} is missing")))?;
        let payload = model_to_payload(&model, &stored)?;
        if let ClipboardPayload::Files(paths) = &payload {
            if paths.iter().any(|path| !Path::new(path).exists()) {
                return Err(DbErr::Custom(format!(
                    "clipboard record {id} references a missing file"
                )));
            }
        }
        let transaction = self.db.begin().await?;
        let now_ms = Utc::now().timestamp_millis();
        let mut active = model.clone().into_active_model();
        active.last_used_at_ms = Set(Some(now_ms));
        active.updated_at_ms = Set(now_ms.max(model.captured_at_ms));
        active.update(&transaction).await?;
        bump_revision(&transaction).await?;
        transaction.commit().await?;
        Ok((payload, model.content_hash, kind_from_i32(model.kind)?))
    }

    pub(in crate::infrastructure) async fn delete(
        &self,
        id: u64,
        updated_by_device_id: &str,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let transaction = self.db.begin().await?;
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard record {id}")))?;
        if model.deleted {
            transaction.rollback().await?;
            return Ok(None);
        }
        let mut active = model.into_active_model();
        active.deleted = Set(true);
        active.sync_revision = Set(active.sync_revision.as_ref().saturating_add(1).max(1));
        active.updated_by_device_id = Set(updated_by_device_id.to_string());
        active.update(&transaction).await?;
        bump_revision(&transaction).await?;
        transaction.commit().await?;
        Ok(self.sync_record(id.max(0) as u64).await?.map(|mut record| {
            record.change_kind = ClipboardSyncChangeKind::Delete;
            record
        }))
    }

    pub(in crate::infrastructure) async fn set_favorite(
        &self,
        id: u64,
        favorite: bool,
        updated_by_device_id: &str,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let transaction = self.db.begin().await?;
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard record {id}")))?;
        if model.deleted || model.favorite == favorite {
            transaction.rollback().await?;
            return Ok(None);
        }
        let mut active = model.into_active_model();
        active.favorite = Set(favorite);
        active.favorite_revision = Set(active.favorite_revision.as_ref().saturating_add(1).max(1));
        active.favorite_updated_by_device_id = Set(updated_by_device_id.to_string());
        active.update(&transaction).await?;
        bump_revision(&transaction).await?;
        let policy = policy_from_state(
            &clipboard_state::Entity::find_by_id(STATE_ID)
                .one(&transaction)
                .await?
                .ok_or_else(|| DbErr::Custom("clipboard state is unavailable".into()))?,
        );
        self.prune(&transaction, &policy).await?;
        transaction.commit().await?;
        Ok(self.sync_record(id.max(0) as u64).await?.map(|mut record| {
            record.change_kind = ClipboardSyncChangeKind::Favorite;
            record
        }))
    }

    pub(in crate::infrastructure) async fn labels(&self) -> Result<Vec<ClipboardLabel>, DbErr> {
        clipboard_label::Entity::find()
            .filter(clipboard_label::Column::Deleted.eq(false))
            .order_by_asc(clipboard_label::Column::NormalizedName)
            .all(&self.db)
            .await
            .map(|models| models.into_iter().map(label_from_model).collect())
    }

    pub(in crate::infrastructure) async fn create_label(
        &self,
        name: &str,
        color: &str,
        updated_by_device_id: &str,
    ) -> Result<ClipboardLabel, DbErr> {
        let name = validate_label_name(name)?;
        let color = validate_label_color(color)?;
        let model = clipboard_label::ActiveModel {
            id: Set(uuid::Uuid::new_v4().to_string()),
            name: Set(name.clone()),
            normalized_name: Set(normalize_label_name(&name)),
            color: Set(color),
            revision: Set(1),
            updated_by_device_id: Set(updated_by_device_id.to_string()),
            deleted: Set(false),
        }
        .insert(&self.db)
        .await?;
        bump_revision(&self.db).await?;
        Ok(label_from_model(model))
    }

    pub(in crate::infrastructure) async fn update_label(
        &self,
        label_id: &str,
        name: &str,
        color: &str,
        updated_by_device_id: &str,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let model = clipboard_label::Entity::find_by_id(label_id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard label {label_id}")))?;
        let mut active = model.into_active_model();
        let name = validate_label_name(name)?;
        active.name = Set(name.clone());
        active.normalized_name = Set(normalize_label_name(&name));
        active.color = Set(validate_label_color(color)?);
        active.deleted = Set(false);
        active.revision = Set(active.revision.as_ref().saturating_add(1).max(1));
        active.updated_by_device_id = Set(updated_by_device_id.to_string());
        active.update(&self.db).await?;
        bump_revision(&self.db).await?;
        self.any_sync_record_for_label(label_id).await
    }

    pub(in crate::infrastructure) async fn delete_label(
        &self,
        label_id: &str,
        updated_by_device_id: &str,
    ) -> Result<Vec<ClipboardSyncRecord>, DbErr> {
        let transaction = self.db.begin().await?;
        let model = clipboard_label::Entity::find_by_id(label_id)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard label {label_id}")))?;
        let mut active = model.into_active_model();
        active.deleted = Set(true);
        active.revision = Set(active.revision.as_ref().saturating_add(1).max(1));
        active.updated_by_device_id = Set(updated_by_device_id.to_string());
        active.update(&transaction).await?;
        let memberships = clipboard_entry_label::Entity::find()
            .filter(clipboard_entry_label::Column::LabelId.eq(label_id))
            .all(&transaction)
            .await?;
        for membership in memberships {
            let mut active = membership.into_active_model();
            active.attached = Set(false);
            active.revision = Set(active.revision.as_ref().saturating_add(1).max(1));
            active.updated_by_device_id = Set(updated_by_device_id.to_string());
            active.update(&transaction).await?;
        }
        bump_revision(&transaction).await?;
        transaction.commit().await?;
        self.sync_records_for_label(label_id).await
    }

    pub(in crate::infrastructure) async fn set_labels(
        &self,
        id: u64,
        label_ids: Vec<String>,
        updated_by_device_id: &str,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let entry = clipboard_entry::Entity::find_by_id(id)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard record {id}")))?;
        let desired = label_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let known = clipboard_label::Entity::find()
            .filter(clipboard_label::Column::Deleted.eq(false))
            .all(&self.db)
            .await?;
        if desired
            .iter()
            .any(|id| !known.iter().any(|label| &label.id == id))
        {
            return Err(DbErr::Custom("clipboard label is unavailable".into()));
        }
        let existing = clipboard_entry_label::Entity::find()
            .filter(clipboard_entry_label::Column::EntrySyncId.eq(&entry.sync_id))
            .all(&self.db)
            .await?;
        for label in known {
            let attached = desired.contains(&label.id);
            if let Some(model) = existing.iter().find(|item| item.label_id == label.id) {
                if model.attached == attached {
                    continue;
                }
                let mut active = model.clone().into_active_model();
                active.attached = Set(attached);
                active.revision = Set(active.revision.as_ref().saturating_add(1).max(1));
                active.updated_by_device_id = Set(updated_by_device_id.to_string());
                active.update(&self.db).await?;
            } else if attached {
                clipboard_entry_label::ActiveModel {
                    entry_sync_id: Set(entry.sync_id.clone()),
                    label_id: Set(label.id),
                    attached: Set(true),
                    revision: Set(1),
                    updated_by_device_id: Set(updated_by_device_id.to_string()),
                }
                .insert(&self.db)
                .await?;
            }
        }
        bump_revision(&self.db).await?;
        Ok(self.sync_record(id.max(0) as u64).await?.map(|mut record| {
            record.change_kind = ClipboardSyncChangeKind::Label;
            record
        }))
    }

    pub(in crate::infrastructure) async fn set_label_membership(
        &self,
        id: u64,
        label_id: &str,
        attached: bool,
        updated_by_device_id: &str,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let transaction = self.db.begin().await?;
        let entry = clipboard_entry::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard record {id}")))?;
        let label = clipboard_label::Entity::find_by_id(label_id.to_string())
            .one(&transaction)
            .await?
            .filter(|label| !label.deleted)
            .ok_or_else(|| DbErr::Custom("clipboard label is unavailable".into()))?;
        let existing =
            clipboard_entry_label::Entity::find_by_id((entry.sync_id.clone(), label.id.clone()))
                .one(&transaction)
                .await?;
        if existing
            .as_ref()
            .is_some_and(|item| item.attached == attached)
            || existing.is_none() && !attached
        {
            transaction.rollback().await?;
            return Ok(None);
        }
        if let Some(existing) = existing {
            let mut active = existing.into_active_model();
            active.attached = Set(attached);
            active.revision = Set(active.revision.as_ref().saturating_add(1).max(1));
            active.updated_by_device_id = Set(updated_by_device_id.to_string());
            active.update(&transaction).await?;
        } else {
            clipboard_entry_label::ActiveModel {
                entry_sync_id: Set(entry.sync_id),
                label_id: Set(label.id),
                attached: Set(true),
                revision: Set(1),
                updated_by_device_id: Set(updated_by_device_id.to_string()),
            }
            .insert(&transaction)
            .await?;
        }
        bump_revision(&transaction).await?;
        transaction.commit().await?;
        Ok(self.sync_record(id.max(0) as u64).await?.map(|mut record| {
            record.change_kind = ClipboardSyncChangeKind::Label;
            record
        }))
    }

    pub(in crate::infrastructure) async fn clear(&self) -> Result<(), DbErr> {
        let transaction = self.db.begin().await?;
        clipboard_entry::Entity::delete_many()
            .exec(&transaction)
            .await?;
        bump_revision(&transaction).await?;
        transaction.commit().await?;
        let _ = self
            .db
            .execute(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "PRAGMA wal_checkpoint(TRUNCATE)".to_string(),
            ))
            .await;
        let _ = self
            .db
            .execute(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "PRAGMA incremental_vacuum".to_string(),
            ))
            .await;
        Ok(())
    }

    async fn state(&self) -> Result<clipboard_state::Model, DbErr> {
        clipboard_state::Entity::find_by_id(STATE_ID)
            .one(&self.db)
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard state is unavailable".into()))
    }

    pub(super) async fn prune<C>(&self, db: &C, policy: &ClipboardPolicy) -> Result<(), DbErr>
    where
        C: ConnectionTrait,
    {
        if policy.retention_days > 0 {
            let cutoff = Utc::now().timestamp_millis()
                - i64::from(policy.retention_days) * 24 * 60 * 60 * 1000;
            let expired = clipboard_entry::Entity::find()
                .select_only()
                .column(clipboard_entry::Column::Id)
                .filter(clipboard_entry::Column::Favorite.eq(false))
                .filter(clipboard_entry::Column::Deleted.eq(false))
                .filter(
                    clipboard_entry::Column::SyncId.not_in_subquery(
                        sea_orm::sea_query::Query::select()
                            .column(clipboard_entry_label::Column::EntrySyncId)
                            .from(clipboard_entry_label::Entity)
                            .and_where(clipboard_entry_label::Column::Attached.eq(true))
                            .to_owned(),
                    ),
                )
                .filter(clipboard_entry::Column::CapturedAtMs.lt(cutoff))
                .order_by_asc(clipboard_entry::Column::CapturedAtMs)
                .order_by_asc(clipboard_entry::Column::SyncId)
                .limit(128)
                .into_query();
            let expired_count = clipboard_entry::Entity::delete_many()
                .filter(clipboard_entry::Column::Id.in_subquery(expired))
                .exec(db)
                .await?
                .rows_affected;
            if expired_count == 128 {
                self.maintenance_pending
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }

        let totals = db
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "SELECT item_count, total_bytes FROM clipboard_statistics WHERE id = 1".to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard prune totals are unavailable".into()))?;
        let mut count = totals.try_get::<i64>("", "item_count")?.max(0) as u64;
        let mut bytes = totals.try_get::<i64>("", "total_bytes")?.max(0) as u64;
        let max_items = u64::from(policy.max_items.max(1));
        let max_bytes = policy.max_bytes.max(1);
        if count <= max_items && bytes <= max_bytes {
            return Ok(());
        }

        let models = clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .filter(clipboard_entry::Column::Favorite.eq(false))
            .filter(
                clipboard_entry::Column::SyncId.not_in_subquery(
                    sea_orm::sea_query::Query::select()
                        .column(clipboard_entry_label::Column::EntrySyncId)
                        .from(clipboard_entry_label::Entity)
                        .and_where(clipboard_entry_label::Column::Attached.eq(true))
                        .to_owned(),
                ),
            )
            .select_only()
            .column(clipboard_entry::Column::Id)
            .column(clipboard_entry::Column::StorageBytes)
            .order_by_asc(clipboard_entry::Column::CapturedAtMs)
            .order_by_asc(clipboard_entry::Column::SyncId)
            .limit(128)
            .into_model::<PruneCandidate>()
            .all(db)
            .await?;
        let eligible = models.len();
        let mut delete_ids = Vec::new();
        for model in models {
            if count <= max_items && bytes <= max_bytes {
                break;
            }
            delete_ids.push(model.id);
            count = count.saturating_sub(1);
            bytes = bytes.saturating_sub(model.storage_bytes.max(0) as u64);
        }
        if eligible == 128 && (count > max_items || bytes > max_bytes) {
            self.maintenance_pending
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if !delete_ids.is_empty() {
            clipboard_entry::Entity::delete_many()
                .filter(clipboard_entry::Column::Id.is_in(delete_ids))
                .exec(db)
                .await?;
        }
        Ok(())
    }
}
