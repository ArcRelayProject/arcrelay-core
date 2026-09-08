use super::*;

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn sync_record_requires_payload(
        &self,
        sync_id: &str,
        revision: u64,
        updated_by_device_id: &str,
    ) -> Result<bool, DbErr> {
        let existing = clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::SyncId.eq(sync_id))
            .one(&self.db)
            .await?;
        Ok(existing.is_none_or(|existing| {
            (revision, updated_by_device_id)
                > (
                    existing.sync_revision.max(0) as u64,
                    existing.updated_by_device_id.as_str(),
                )
        }))
    }

    pub(in crate::infrastructure) async fn sync_records(
        &self,
        after_local_id: u64,
        limit: usize,
    ) -> Result<ClipboardSyncPage, DbErr> {
        let after = i64::try_from(after_local_id).unwrap_or(i64::MAX);
        let limit = limit.clamp(1, 200);
        let mut models = clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::Id.gt(after))
            .order_by_asc(clipboard_entry::Column::Id)
            .limit((limit + 1) as u64)
            .all(&self.db)
            .await?;
        let has_more = models.len() > limit;
        if has_more {
            models.pop();
        }
        let next_cursor = has_more.then(|| {
            models
                .last()
                .map(|model| model.id.max(0) as u64)
                .unwrap_or(after_local_id)
        });
        let entry_ids = models.iter().map(|model| model.id).collect::<Vec<_>>();
        let sync_ids = models
            .iter()
            .map(|model| model.sync_id.clone())
            .collect::<Vec<_>>();
        let mut payloads = if entry_ids.is_empty() {
            HashMap::new()
        } else {
            clipboard_payload::Entity::find()
                .filter(clipboard_payload::Column::EntryId.is_in(entry_ids))
                .all(&self.db)
                .await?
                .into_iter()
                .map(|payload| (payload.entry_id, payload))
                .collect::<HashMap<_, _>>()
        };
        let (labels, mut memberships_by_sync_id) =
            self.label_states_for_sync_ids(&sync_ids).await?;
        let mut records = Vec::with_capacity(models.len());
        for model in models {
            let payload = payloads.remove(&model.id);
            let memberships = memberships_by_sync_id
                .remove(&model.sync_id)
                .unwrap_or_default();
            if let Some(record) =
                base_model_to_sync_record(&model, payload.as_ref(), labels.clone(), memberships)?
            {
                records.push((model.id.max(0) as u64, record));
            }
        }
        Ok(ClipboardSyncPage {
            records,
            next_cursor,
        })
    }

    pub(in crate::infrastructure) async fn sync_record(
        &self,
        id: u64,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let Some(model) = clipboard_entry::Entity::find_by_id(id)
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        let payload = clipboard_payload::Entity::find_by_id(id)
            .one(&self.db)
            .await?;
        self.model_to_sync_record(&model, payload.as_ref()).await
    }

    pub(super) async fn active_labels_for_sync_id(
        &self,
        sync_id: &str,
    ) -> Result<Vec<ClipboardLabel>, DbErr> {
        Ok(self
            .active_labels_for_sync_ids(&[sync_id.to_string()])
            .await?
            .remove(sync_id)
            .unwrap_or_default())
    }

    pub(super) async fn active_labels_for_sync_ids(
        &self,
        sync_ids: &[String],
    ) -> Result<HashMap<String, Vec<ClipboardLabel>>, DbErr> {
        self.active_labels_for_sync_ids_on(&self.db, sync_ids).await
    }

    pub(super) async fn active_labels_for_sync_ids_on<C: ConnectionTrait>(
        &self,
        db: &C,
        sync_ids: &[String],
    ) -> Result<HashMap<String, Vec<ClipboardLabel>>, DbErr> {
        if sync_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let memberships = clipboard_entry_label::Entity::find()
            .filter(clipboard_entry_label::Column::EntrySyncId.is_in(sync_ids.to_vec()))
            .filter(clipboard_entry_label::Column::Attached.eq(true))
            .all(db)
            .await?;
        if memberships.is_empty() {
            return Ok(HashMap::new());
        }
        let labels = clipboard_label::Entity::find()
            .filter(
                clipboard_label::Column::Id
                    .is_in(memberships.iter().map(|item| item.label_id.clone())),
            )
            .filter(clipboard_label::Column::Deleted.eq(false))
            .order_by_asc(clipboard_label::Column::NormalizedName)
            .all(db)
            .await?
            .into_iter()
            .map(|model| (model.id.clone(), label_from_model(model)))
            .collect::<HashMap<_, _>>();
        let mut by_sync_id = HashMap::<String, Vec<ClipboardLabel>>::new();
        for membership in memberships {
            if let Some(label) = labels.get(&membership.label_id) {
                by_sync_id
                    .entry(membership.entry_sync_id)
                    .or_default()
                    .push(label.clone());
            }
        }
        for labels in by_sync_id.values_mut() {
            labels.sort_by_key(|label| label.name.to_lowercase());
        }
        Ok(by_sync_id)
    }

    pub(super) async fn label_states_for_sync_id(
        &self,
        sync_id: &str,
    ) -> Result<(Vec<ClipboardLabel>, Vec<ClipboardLabelMembership>), DbErr> {
        let (labels, mut memberships) = self
            .label_states_for_sync_ids(&[sync_id.to_string()])
            .await?;
        Ok((labels, memberships.remove(sync_id).unwrap_or_default()))
    }

    pub(super) async fn label_states_for_sync_ids(
        &self,
        sync_ids: &[String],
    ) -> Result<
        (
            Vec<ClipboardLabel>,
            HashMap<String, Vec<ClipboardLabelMembership>>,
        ),
        DbErr,
    > {
        let memberships = if sync_ids.is_empty() {
            Vec::new()
        } else {
            clipboard_entry_label::Entity::find()
                .filter(clipboard_entry_label::Column::EntrySyncId.is_in(sync_ids.to_vec()))
                .all(&self.db)
                .await?
        };
        let labels = clipboard_label::Entity::find()
            .all(&self.db)
            .await?
            .into_iter()
            .map(label_from_model)
            .collect();
        let mut by_sync_id = HashMap::<String, Vec<ClipboardLabelMembership>>::new();
        for model in memberships {
            by_sync_id
                .entry(model.entry_sync_id)
                .or_default()
                .push(ClipboardLabelMembership {
                    label_id: model.label_id,
                    attached: model.attached,
                    revision: model.revision.max(1) as u64,
                    updated_by_device_id: model.updated_by_device_id,
                });
        }
        Ok((labels, by_sync_id))
    }

    pub(super) async fn model_to_sync_record(
        &self,
        model: &clipboard_entry::Model,
        payload: Option<&clipboard_payload::Model>,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let (labels, memberships) = self.label_states_for_sync_id(&model.sync_id).await?;
        base_model_to_sync_record(model, payload, labels, memberships)
    }

    pub(super) async fn any_sync_record_for_label(
        &self,
        label_id: &str,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let Some(membership) = clipboard_entry_label::Entity::find()
            .filter(clipboard_entry_label::Column::LabelId.eq(label_id))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        let Some(entry) = clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::SyncId.eq(membership.entry_sync_id))
            .one(&self.db)
            .await?
        else {
            return Ok(None);
        };
        let payload = clipboard_payload::Entity::find_by_id(entry.id)
            .one(&self.db)
            .await?;
        self.model_to_sync_record(&entry, payload.as_ref())
            .await
            .map(|record| {
                record.map(|mut record| {
                    record.change_kind = ClipboardSyncChangeKind::Label;
                    record
                })
            })
    }

    pub(super) async fn sync_records_for_label(
        &self,
        label_id: &str,
    ) -> Result<Vec<ClipboardSyncRecord>, DbErr> {
        let memberships = clipboard_entry_label::Entity::find()
            .filter(clipboard_entry_label::Column::LabelId.eq(label_id))
            .all(&self.db)
            .await?;
        let mut records = Vec::new();
        for membership in memberships {
            let Some(entry) = clipboard_entry::Entity::find()
                .filter(clipboard_entry::Column::SyncId.eq(membership.entry_sync_id))
                .one(&self.db)
                .await?
            else {
                continue;
            };
            let payload = clipboard_payload::Entity::find_by_id(entry.id)
                .one(&self.db)
                .await?;
            if let Some(mut record) = self.model_to_sync_record(&entry, payload.as_ref()).await? {
                record.change_kind = ClipboardSyncChangeKind::Label;
                records.push(record);
            }
        }
        Ok(records)
    }

    pub(super) async fn apply_label_definitions<C: ConnectionTrait>(
        &self,
        db: &C,
        labels: &[ClipboardLabel],
    ) -> Result<usize, DbErr> {
        let mut changed = 0;
        for label in labels {
            let current = clipboard_label::Entity::find_by_id(&label.id)
                .one(db)
                .await?;
            if current.as_ref().is_some_and(|current| {
                (label.revision, label.updated_by_device_id.as_str())
                    <= (
                        current.revision.max(0) as u64,
                        current.updated_by_device_id.as_str(),
                    )
            }) {
                continue;
            }
            clipboard_label::Entity::insert(clipboard_label::ActiveModel {
                id: Set(label.id.clone()),
                name: Set(label.name.clone()),
                normalized_name: Set(normalize_label_name(&label.name)),
                color: Set(label.color.clone()),
                revision: Set(label.revision.max(1).min(i64::MAX as u64) as i64),
                updated_by_device_id: Set(label.updated_by_device_id.clone()),
                deleted: Set(label.deleted),
            })
            .on_conflict(
                OnConflict::column(clipboard_label::Column::Id)
                    .update_columns([
                        clipboard_label::Column::Name,
                        clipboard_label::Column::NormalizedName,
                        clipboard_label::Column::Color,
                        clipboard_label::Column::Revision,
                        clipboard_label::Column::UpdatedByDeviceId,
                        clipboard_label::Column::Deleted,
                    ])
                    .to_owned(),
            )
            .exec(db)
            .await?;
            changed += 1;
        }
        Ok(changed)
    }

    pub(super) async fn apply_label_state<C>(
        &self,
        db: &C,
        record: &ClipboardSyncRecord,
    ) -> Result<(), DbErr>
    where
        C: ConnectionTrait,
    {
        self.apply_label_definitions(db, &record.labels).await?;
        for membership in &record.label_memberships {
            let current = clipboard_entry_label::Entity::find_by_id((
                record.sync_id.clone(),
                membership.label_id.clone(),
            ))
            .one(db)
            .await?;
            if current.as_ref().is_some_and(|current| {
                (
                    membership.revision,
                    membership.updated_by_device_id.as_str(),
                ) <= (
                    current.revision.max(0) as u64,
                    current.updated_by_device_id.as_str(),
                )
            }) {
                continue;
            }
            clipboard_entry_label::Entity::insert(clipboard_entry_label::ActiveModel {
                entry_sync_id: Set(record.sync_id.clone()),
                label_id: Set(membership.label_id.clone()),
                attached: Set(membership.attached),
                revision: Set(membership.revision.max(1).min(i64::MAX as u64) as i64),
                updated_by_device_id: Set(membership.updated_by_device_id.clone()),
            })
            .on_conflict(
                OnConflict::columns([
                    clipboard_entry_label::Column::EntrySyncId,
                    clipboard_entry_label::Column::LabelId,
                ])
                .update_columns([
                    clipboard_entry_label::Column::Attached,
                    clipboard_entry_label::Column::Revision,
                    clipboard_entry_label::Column::UpdatedByDeviceId,
                ])
                .to_owned(),
            )
            .exec(db)
            .await?;
        }
        Ok(())
    }

    pub(super) async fn label_state_is_newer<C>(
        &self,
        db: &C,
        record: &ClipboardSyncRecord,
    ) -> Result<bool, DbErr>
    where
        C: ConnectionTrait,
    {
        for label in &record.labels {
            let current = clipboard_label::Entity::find_by_id(&label.id)
                .one(db)
                .await?;
            if current.as_ref().is_none_or(|current| {
                (label.revision, label.updated_by_device_id.as_str())
                    > (
                        current.revision.max(0) as u64,
                        current.updated_by_device_id.as_str(),
                    )
            }) {
                return Ok(true);
            }
        }
        for membership in &record.label_memberships {
            let current = clipboard_entry_label::Entity::find_by_id((
                record.sync_id.clone(),
                membership.label_id.clone(),
            ))
            .one(db)
            .await?;
            if current.as_ref().is_none_or(|current| {
                (
                    membership.revision,
                    membership.updated_by_device_id.as_str(),
                ) > (
                    current.revision.max(0) as u64,
                    current.updated_by_device_id.as_str(),
                )
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(in crate::infrastructure) async fn apply_sync_record(
        &self,
        record: ClipboardSyncRecord,
    ) -> Result<bool, DbErr> {
        self.apply_sync_record_with_timeline(record, None).await
    }

    pub(super) async fn apply_sync_record_with_timeline(
        &self,
        record: ClipboardSyncRecord,
        timeline: Option<(i64, u32)>,
    ) -> Result<bool, DbErr> {
        let transaction = self.db.begin().await?;
        let mut existing = clipboard_entry::Entity::find()
            .filter(clipboard_entry::Column::SyncId.eq(&record.sync_id))
            .one(&transaction)
            .await?;
        let content_is_newer = existing.as_ref().is_none_or(|existing| {
            let incoming = (record.revision, record.updated_by_device_id.as_str());
            let current = (
                existing.sync_revision.max(0) as u64,
                existing.updated_by_device_id.as_str(),
            );
            incoming > current
        });
        let selection_changed = if timeline.is_some()
            && record.live
            && !record.deleted
            && record.change_kind == ClipboardSyncChangeKind::Copy
            && (content_is_newer
                || existing.as_ref().is_some_and(|existing| {
                    (
                        existing.sync_revision.max(0) as u64,
                        existing.updated_by_device_id.as_str(),
                    ) == (record.revision, record.updated_by_device_id.as_str())
                })) {
            Self::remember_selection(
                &transaction,
                &record.sync_id,
                record.captured_at_ms,
                &record.updated_by_device_id,
                record.revision,
            )
            .await?
        } else {
            false
        };
        let favorite_is_newer = existing.as_ref().is_none_or(|existing| {
            (
                record.favorite_revision,
                record.favorite_updated_by_device_id.as_str(),
            ) > (
                existing.favorite_revision.max(0) as u64,
                existing.favorite_updated_by_device_id.as_str(),
            )
        });
        let labels_are_newer = self.label_state_is_newer(&transaction, &record).await?;

        let mut timeline_changed = false;
        if let (Some(model), Some((first, count))) = (existing.as_mut(), timeline) {
            let first = model.first_captured_at_ms.min(first);
            let count = model.copy_count.max(count.min(i32::MAX as u32) as i32);
            let captured = model.captured_at_ms.max(record.captured_at_ms);
            if first != model.first_captured_at_ms
                || count != model.copy_count
                || captured != model.captured_at_ms
            {
                let mut active = model.clone().into_active_model();
                active.first_captured_at_ms = Set(first);
                active.copy_count = Set(count);
                active.captured_at_ms = Set(captured);
                active.updated_at_ms = Set(captured.max(model.last_used_at_ms.unwrap_or_default()));
                *model = active.update(&transaction).await?;
                timeline_changed = true;
            }
        }

        if record.deleted {
            if existing.is_some()
                && !content_is_newer
                && !favorite_is_newer
                && !labels_are_newer
                && !timeline_changed
                && !selection_changed
            {
                transaction.rollback().await?;
                return Ok(false);
            }
            if let Some(existing) = existing {
                let mut active = existing.into_active_model();
                if content_is_newer {
                    active.deleted = Set(true);
                    active.sync_revision = Set(record.revision.max(1).min(i64::MAX as u64) as i64);
                    active.updated_by_device_id = Set(record.updated_by_device_id.clone());
                }
                if favorite_is_newer {
                    active.favorite = Set(record.favorite);
                    active.favorite_revision =
                        Set(record.favorite_revision.max(1).min(i64::MAX as u64) as i64);
                    active.favorite_updated_by_device_id =
                        Set(record.favorite_updated_by_device_id.clone());
                }
                active.update(&transaction).await?;
            } else {
                clipboard_entry::ActiveModel {
                    id: Default::default(),
                    kind: Set(kind_to_i32(record.kind)),
                    text_syntax: Set(encode_text_syntax(&record.text_syntax)?),
                    content_hash: Set(deleted_content_hash(&record.sync_id)),
                    sync_id: Set(record.sync_id.clone()),
                    source_device_id: Set(record.source_device_id.clone()),
                    source_device_name: Set(record.source_device_name.clone()),
                    sync_revision: Set(record.revision.max(1).min(i64::MAX as u64) as i64),
                    updated_by_device_id: Set(record.updated_by_device_id.clone()),
                    deleted: Set(true),
                    preview: Set(record.preview.clone()),
                    search_text: Set(String::new()),
                    source_app: Set(record.source_app.clone()),
                    first_captured_at_ms: Set(
                        timeline.map_or(record.captured_at_ms, |value| value.0)
                    ),
                    captured_at_ms: Set(record.captured_at_ms),
                    last_used_at_ms: Set(None),
                    updated_at_ms: Set(record.captured_at_ms),
                    copy_count: Set(
                        timeline.map_or(1, |value| value.1.max(1).min(i32::MAX as u32) as i32)
                    ),
                    size_bytes: Set(0),
                    character_count: Set(None),
                    storage_bytes: Set(0),
                    item_count: Set(1),
                    width: Set(record.width.map(|value| value.min(i32::MAX as u32) as i32)),
                    height: Set(record.height.map(|value| value.min(i32::MAX as u32) as i32)),
                    sensitive: Set(false),
                    favorite: Set(record.favorite),
                    favorite_revision: Set(
                        record.favorite_revision.max(1).min(i64::MAX as u64) as i64
                    ),
                    favorite_updated_by_device_id: Set(record
                        .favorite_updated_by_device_id
                        .clone()),
                    available: Set(false),
                }
                .insert(&transaction)
                .await?;
            }
            self.apply_label_state(&transaction, &record).await?;
            if timeline.is_some() {
                self.enforce_replica_policy(&transaction, &record).await?;
            }
            bump_revision(&transaction).await?;
            transaction.commit().await?;
            return Ok(true);
        }

        if record.kind == ClipboardContentKind::Files {
            if let Some(existing) = existing {
                if !favorite_is_newer
                    && !labels_are_newer
                    && !timeline_changed
                    && !selection_changed
                {
                    transaction.rollback().await?;
                    return Ok(false);
                }
                if favorite_is_newer {
                    let mut active = existing.into_active_model();
                    active.favorite = Set(record.favorite);
                    active.favorite_revision =
                        Set(record.favorite_revision.max(1).min(i64::MAX as u64) as i64);
                    active.favorite_updated_by_device_id =
                        Set(record.favorite_updated_by_device_id.clone());
                    active.update(&transaction).await?;
                }
            } else {
                clipboard_entry::ActiveModel {
                    id: Default::default(),
                    kind: Set(kind_to_i32(ClipboardContentKind::Files)),
                    text_syntax: Set(encode_text_syntax(&ClipboardTextSyntax::Plain)?),
                    content_hash: Set(deleted_content_hash(&record.sync_id)),
                    sync_id: Set(record.sync_id.clone()),
                    source_device_id: Set(record.source_device_id.clone()),
                    source_device_name: Set(record.source_device_name.clone()),
                    sync_revision: Set(record.revision.max(1).min(i64::MAX as u64) as i64),
                    updated_by_device_id: Set(record.updated_by_device_id.clone()),
                    deleted: Set(false),
                    preview: Set(record.preview.clone()),
                    search_text: Set(record.preview.to_lowercase()),
                    source_app: Set(record.source_app.clone()),
                    first_captured_at_ms: Set(
                        timeline.map_or(record.captured_at_ms, |value| value.0)
                    ),
                    captured_at_ms: Set(record.captured_at_ms),
                    last_used_at_ms: Set(None),
                    updated_at_ms: Set(record.captured_at_ms),
                    copy_count: Set(
                        timeline.map_or(1, |value| value.1.max(1).min(i32::MAX as u32) as i32)
                    ),
                    size_bytes: Set(0),
                    character_count: Set(None),
                    storage_bytes: Set(0),
                    item_count: Set(1),
                    width: Set(None),
                    height: Set(None),
                    sensitive: Set(false),
                    favorite: Set(record.favorite),
                    favorite_revision: Set(
                        record.favorite_revision.max(1).min(i64::MAX as u64) as i64
                    ),
                    favorite_updated_by_device_id: Set(record
                        .favorite_updated_by_device_id
                        .clone()),
                    available: Set(false),
                }
                .insert(&transaction)
                .await?;
            }
            self.apply_label_state(&transaction, &record).await?;
            if timeline.is_some() {
                self.enforce_replica_policy(&transaction, &record).await?;
            }
            bump_revision(&transaction).await?;
            transaction.commit().await?;
            return Ok(true);
        }

        if existing.is_some() && !content_is_newer {
            if !favorite_is_newer && !labels_are_newer && !timeline_changed && !selection_changed {
                transaction.rollback().await?;
                return Ok(false);
            }
            if favorite_is_newer {
                if let Some(existing) = existing.as_ref() {
                    let mut active = existing.clone().into_active_model();
                    active.favorite = Set(record.favorite);
                    active.favorite_revision =
                        Set(record.favorite_revision.max(1).min(i64::MAX as u64) as i64);
                    active.favorite_updated_by_device_id =
                        Set(record.favorite_updated_by_device_id.clone());
                    active.update(&transaction).await?;
                }
            }
            self.apply_label_state(&transaction, &record).await?;
            if timeline.is_some() {
                self.enforce_replica_policy(&transaction, &record).await?;
            }
            bump_revision(&transaction).await?;
            transaction.commit().await?;
            return Ok(true);
        }

        let payload = sync_record_payload(&record)?;
        let stored = StoredValues::from_payload(payload)?;
        let content_hash = content_hash_for_stored(&record, &stored)?;
        let summary = sync_record_summary(&record, &stored);
        let search_text = build_search_text(&summary, &stored);
        let storage_bytes = stored
            .storage_bytes()
            .saturating_add(byte_len(&content_hash))
            .saturating_add(byte_len(&summary.preview))
            .saturating_add(summary.source_app.as_deref().map_or(0, byte_len))
            .saturating_add(byte_len(&search_text));

        let entry_id = if let Some(existing) = existing {
            let id = existing.id;
            let captured_at_ms = if timeline.is_some() {
                record.captured_at_ms.max(existing.captured_at_ms)
            } else {
                record.captured_at_ms
            };
            let updated_at_ms = captured_at_ms.max(existing.last_used_at_ms.unwrap_or_default());
            let mut active = existing.into_active_model();
            active.kind = Set(kind_to_i32(record.kind));
            active.text_syntax = Set(encode_text_syntax(&record.text_syntax)?);
            active.content_hash = Set(content_hash);
            active.preview = Set(summary.preview);
            active.search_text = Set(search_text);
            active.source_app = Set(summary.source_app);
            active.captured_at_ms = Set(captured_at_ms);
            active.first_captured_at_ms =
                Set((*active.first_captured_at_ms.as_ref()).min(captured_at_ms));
            active.updated_at_ms = Set(updated_at_ms);
            active.copy_count = Set(timeline.map_or_else(
                || active.copy_count.as_ref().saturating_add(1),
                |value| (*active.copy_count.as_ref()).max(value.1.min(i32::MAX as u32) as i32),
            ));
            active.size_bytes = Set(summary.size_bytes.min(i64::MAX as u64) as i64);
            active.character_count = Set(summary
                .character_count
                .map(|value| value.min(i64::MAX as u64) as i64));
            active.storage_bytes = Set(storage_bytes);
            active.item_count = Set(1);
            active.width = Set(record.width.map(|value| value.min(i32::MAX as u32) as i32));
            active.height = Set(record.height.map(|value| value.min(i32::MAX as u32) as i32));
            if favorite_is_newer {
                active.favorite = Set(record.favorite);
                active.favorite_revision =
                    Set(record.favorite_revision.max(1).min(i64::MAX as u64) as i64);
                active.favorite_updated_by_device_id =
                    Set(record.favorite_updated_by_device_id.clone());
            }
            active.available = Set(true);
            active.deleted = Set(false);
            active.source_device_id = Set(record.source_device_id.clone());
            active.source_device_name = Set(record.source_device_name.clone());
            active.sync_revision = Set(record.revision.min(i64::MAX as u64) as i64);
            active.updated_by_device_id = Set(record.updated_by_device_id.clone());
            active.update(&transaction).await?;
            id
        } else {
            clipboard_entry::ActiveModel {
                id: Default::default(),
                kind: Set(kind_to_i32(record.kind)),
                text_syntax: Set(encode_text_syntax(&record.text_syntax)?),
                content_hash: Set(content_hash),
                sync_id: Set(record.sync_id.clone()),
                source_device_id: Set(record.source_device_id.clone()),
                source_device_name: Set(record.source_device_name.clone()),
                sync_revision: Set(record.revision.min(i64::MAX as u64) as i64),
                updated_by_device_id: Set(record.updated_by_device_id.clone()),
                deleted: Set(false),
                preview: Set(summary.preview),
                search_text: Set(search_text),
                source_app: Set(summary.source_app),
                first_captured_at_ms: Set(timeline.map_or(record.captured_at_ms, |value| value.0)),
                captured_at_ms: Set(record.captured_at_ms),
                last_used_at_ms: Set(None),
                updated_at_ms: Set(record.captured_at_ms),
                copy_count: Set(
                    timeline.map_or(1, |value| value.1.max(1).min(i32::MAX as u32) as i32)
                ),
                size_bytes: Set(summary.size_bytes.min(i64::MAX as u64) as i64),
                character_count: Set(summary
                    .character_count
                    .map(|value| value.min(i64::MAX as u64) as i64)),
                storage_bytes: Set(storage_bytes),
                item_count: Set(1),
                width: Set(record.width.map(|value| value.min(i32::MAX as u32) as i32)),
                height: Set(record.height.map(|value| value.min(i32::MAX as u32) as i32)),
                sensitive: Set(false),
                favorite: Set(record.favorite),
                favorite_revision: Set(record.favorite_revision.max(1).min(i64::MAX as u64) as i64),
                favorite_updated_by_device_id: Set(record.favorite_updated_by_device_id.clone()),
                available: Set(true),
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
        self.apply_label_state(&transaction, &record).await?;
        if timeline.is_some() {
            self.enforce_replica_policy(&transaction, &record).await?;
        }
        bump_revision(&transaction).await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub(in crate::infrastructure) async fn edit_text(
        &self,
        id: u64,
        content: &str,
        updated_by_device_id: &str,
    ) -> Result<Option<ClipboardSyncRecord>, DbErr> {
        let id = i64::try_from(id).map_err(|_| DbErr::Custom("invalid clipboard id".into()))?;
        let transaction = self.db.begin().await?;
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::RecordNotFound(format!("clipboard record {id}")))?;
        if kind_from_i32(model.kind)? != ClipboardContentKind::Text {
            return Err(DbErr::Custom(
                "only text clipboard records can be edited".into(),
            ));
        }
        let existing_payload = clipboard_payload::Entity::find_by_id(id)
            .one(&transaction)
            .await?
            .ok_or_else(|| DbErr::Custom(format!("clipboard payload {id} is missing")))?;
        if existing_payload.text_payload.as_deref() == Some(content) {
            transaction.rollback().await?;
            return Ok(None);
        }
        let preview = content.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut active = model.into_active_model();
        active.content_hash = Set(text_content_hash(content));
        active.text_syntax = Set(encode_text_syntax(&detect_text_syntax(content))?);
        active.preview = Set(preview.chars().take(140).collect());
        active.search_text = Set(content.to_lowercase());
        active.size_bytes = Set(content.len().min(i64::MAX as usize) as i64);
        active.character_count = Set(Some(content.chars().count().min(i64::MAX as usize) as i64));
        active.storage_bytes = Set(content.len().min(i64::MAX as usize) as i64);
        active.sync_revision = Set(active.sync_revision.as_ref().saturating_add(1).max(1));
        active.updated_by_device_id = Set(updated_by_device_id.to_string());
        active.update(&transaction).await?;
        let stored = StoredValues::from_payload(ClipboardPayload::Text(content.to_string()))?;
        clipboard_payload::Entity::insert(stored.into_active_model(id))
            .on_conflict(
                OnConflict::column(clipboard_payload::Column::EntryId)
                    .update_column(clipboard_payload::Column::TextPayload)
                    .to_owned(),
            )
            .exec(&transaction)
            .await?;
        bump_revision(&transaction).await?;
        transaction.commit().await?;
        Ok(self.sync_record(id.max(0) as u64).await?.map(|mut record| {
            record.change_kind = ClipboardSyncChangeKind::Edit;
            record
        }))
    }
}
