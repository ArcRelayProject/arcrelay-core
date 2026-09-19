use super::*;
use crate::domain::clipboard::{
    ClipboardExportRecord, CLIPBOARD_EXPORT_MAX_BYTES, CLIPBOARD_EXPORT_MAX_RECORDS,
};

#[derive(FromQueryResult)]
struct ExportMetadata {
    id: i64,
    kind: i32,
    content_hash: String,
    storage_bytes: i64,
    sensitive: bool,
    width: Option<i32>,
    height: Option<i32>,
}

impl ExportMetadata {
    fn into_record(self, stored: clipboard_payload::Model) -> Result<ClipboardExportRecord, DbErr> {
        // Move owned representations into the snapshot instead of cloning large
        // images/text while the database worker also retains the loaded model.
        let missing = || DbErr::Custom("clipboard export payload is incomplete".into());
        let payload = match kind_from_i32(self.kind)? {
            ClipboardContentKind::Text => {
                ClipboardPayload::Text(stored.text_payload.ok_or_else(missing)?)
            }
            ClipboardContentKind::Html => ClipboardPayload::RichText {
                html: stored.html_payload.ok_or_else(missing)?,
                plain_text: stored.plain_text_payload.ok_or_else(missing)?,
                rtf: stored.rtf_payload,
            },
            ClipboardContentKind::Image => ClipboardPayload::Image {
                png: stored.image_png.ok_or_else(missing)?,
                width: self.width.unwrap_or_default().max(0) as u32,
                height: self.height.unwrap_or_default().max(0) as u32,
            },
            ClipboardContentKind::Files => {
                let paths = stored.files_json.ok_or_else(missing)?;
                ClipboardPayload::Files(decode_paths(Some(&paths))?)
            }
        };
        Ok(ClipboardExportRecord {
            id: self.id as u64,
            content_hash: self.content_hash,
            sensitive: self.sensitive,
            payload,
        })
    }
}

fn export_ids(ids: &[u64]) -> Result<Vec<i64>, DbErr> {
    if ids.is_empty() || ids.len() > CLIPBOARD_EXPORT_MAX_RECORDS {
        return Err(DbErr::Custom(
            "invalid clipboard export selection size".into(),
        ));
    }
    let mut unique = std::collections::HashSet::new();
    ids.iter()
        .map(|id| {
            if *id == 0 || !unique.insert(*id) {
                return Err(DbErr::Custom(
                    "invalid or duplicate clipboard export id".into(),
                ));
            }
            i64::try_from(*id).map_err(|_| DbErr::Custom("invalid clipboard export id".into()))
        })
        .collect()
}

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn export_records(
        &self,
        ids: Vec<u64>,
    ) -> Result<Vec<ClipboardExportRecord>, DbErr> {
        let ids = export_ids(&ids)?;
        let transaction = self.db.begin().await?;
        let entries = clipboard_entry::Entity::find()
            .select_only()
            .columns([
                clipboard_entry::Column::Id,
                clipboard_entry::Column::Kind,
                clipboard_entry::Column::ContentHash,
                clipboard_entry::Column::StorageBytes,
                clipboard_entry::Column::Sensitive,
                clipboard_entry::Column::Width,
                clipboard_entry::Column::Height,
            ])
            .filter(clipboard_entry::Column::Id.is_in(ids.clone()))
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .into_model::<ExportMetadata>()
            .all(&transaction)
            .await?;
        if entries.len() != ids.len() {
            return Err(DbErr::Custom(
                "clipboard export contains a deleted or missing record".into(),
            ));
        }
        // Admit before loading any payload or search_text (which itself contains
        // complete text). storage_bytes includes all retained representations.
        let bytes = entries
            .iter()
            .try_fold(0u64, |total, entry| {
                u64::try_from(entry.storage_bytes)
                    .ok()
                    .and_then(|bytes| total.checked_add(bytes))
            })
            .ok_or_else(|| DbErr::Custom("clipboard export is too large".into()))?;
        if bytes > CLIPBOARD_EXPORT_MAX_BYTES {
            return Err(DbErr::Custom(
                "clipboard export exceeds 128 MiB; select fewer records".into(),
            ));
        }
        let mut entries: HashMap<_, _> =
            entries.into_iter().map(|entry| (entry.id, entry)).collect();
        let mut output = Vec::with_capacity(ids.len());
        for id in ids {
            let entry = entries.remove(&id).expect("validated export id");
            let stored = clipboard_payload::Entity::find_by_id(id)
                .one(&transaction)
                .await?
                .ok_or_else(|| DbErr::Custom("clipboard export payload is missing".into()))?;
            output.push(entry.into_record(stored)?);
        }
        transaction.commit().await?;
        Ok(output)
    }

    pub(in crate::infrastructure) async fn validate_export(
        &self,
        versions: Vec<(u64, String)>,
    ) -> Result<bool, DbErr> {
        let ids = export_ids(&versions.iter().map(|(id, _)| *id).collect::<Vec<_>>())?;
        // Select metadata only; this check never reads a blob or filesystem path.
        let entries = clipboard_entry::Entity::find()
            .select_only()
            .columns([
                clipboard_entry::Column::Id,
                clipboard_entry::Column::ContentHash,
            ])
            .filter(clipboard_entry::Column::Id.is_in(ids))
            .filter(clipboard_entry::Column::Deleted.eq(false))
            .into_tuple::<(i64, String)>()
            .all(&self.db)
            .await?;
        let current: HashMap<_, _> = entries.into_iter().collect();
        Ok(versions
            .iter()
            .all(|(id, hash)| current.get(&(*id as i64)) == Some(hash)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed(store: &SqliteClipboardStore, payload: ClipboardPayload, hash: &str) -> u64 {
        let kind = match &payload {
            ClipboardPayload::Text(_) => ClipboardContentKind::Text,
            ClipboardPayload::RichText { .. } => ClipboardContentKind::Html,
            ClipboardPayload::Image { .. } => ClipboardContentKind::Image,
            ClipboardPayload::Files(_) => ClipboardContentKind::Files,
        };
        let now = Utc::now();
        store
            .store(
                payload,
                hash.into(),
                ClipboardSummary {
                    id: 0,
                    kind,
                    preview: "bounded preview".into(),
                    source_app: None,
                    first_captured_at: now,
                    captured_at: now,
                    last_used_at: None,
                    size_bytes: 1,
                    character_count: None,
                    item_count: 1,
                    width: Some(2),
                    height: Some(1),
                    sensitive: false,
                    favorite: false,
                    labels: vec![],
                    copy_count: 1,
                    available: true,
                    sync_id: hash.into(),
                    source_device_id: None,
                    source_device_name: None,
                    text_syntax: ClipboardTextSyntax::Plain,
                },
                true,
                "local",
                "Local",
                true,
            )
            .await
            .unwrap()
            .unwrap();
        store.current_summary().await.unwrap().unwrap().id
    }

    #[tokio::test]
    async fn export_is_ordered_complete_and_does_not_touch_history() {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        let text = "原文\n".repeat(100);
        let first = seed(&store, ClipboardPayload::Text(text.clone()), "text").await;
        let second = seed(
            &store,
            ClipboardPayload::RichText {
                html: "<b>格式</b>".into(),
                plain_text: "格式".into(),
                rtf: Some("rtf".into()),
            },
            "html",
        )
        .await;
        let before = store.history(ClipboardQuery::recent(10)).await.unwrap();
        let revision = store.revision().await.unwrap();
        let exported = store.export_records(vec![second, first]).await.unwrap();
        assert_eq!(
            exported.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![second, first]
        );
        assert_eq!(exported[1].payload, ClipboardPayload::Text(text));
        assert!(
            matches!(&exported[0].payload, ClipboardPayload::RichText { html, rtf: Some(_), .. } if html == "<b>格式</b>")
        );
        assert_eq!(store.revision().await.unwrap(), revision);
        assert_eq!(
            store
                .history(ClipboardQuery::recent(10))
                .await
                .unwrap()
                .entries,
            before.entries
        );
        assert!(store
            .validate_export(
                exported
                    .iter()
                    .map(|r| (r.id, r.content_hash.clone()))
                    .collect()
            )
            .await
            .unwrap());
        store
            .db
            .execute_unprepared(&format!(
                "UPDATE clipboard_entries SET content_hash='edited' WHERE id={first}"
            ))
            .await
            .unwrap();
        assert!(!store
            .validate_export(vec![(first, "text".into())])
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn export_rejects_deleted_missing_duplicate_and_over_budget_records() {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        let id = seed(
            &store,
            ClipboardPayload::Image {
                png: vec![1, 2, 3],
                width: 2,
                height: 1,
            },
            "image",
        )
        .await;
        assert!(
            matches!(&store.export_records(vec![id]).await.unwrap()[0].payload, ClipboardPayload::Image { png, .. } if png == &[1, 2, 3])
        );
        assert!(store.export_records(vec![]).await.is_err());
        assert!(store.export_records(vec![id, id]).await.is_err());
        assert!(store.export_records(vec![id, id + 1]).await.is_err());
        store
            .db
            .execute_unprepared(&format!(
                "UPDATE clipboard_entries SET storage_bytes={} WHERE id={id}",
                CLIPBOARD_EXPORT_MAX_BYTES + 1
            ))
            .await
            .unwrap();
        assert!(store.export_records(vec![id]).await.is_err());
        store
            .db
            .execute_unprepared(&format!(
                "UPDATE clipboard_entries SET deleted=1, storage_bytes=1 WHERE id={id}"
            ))
            .await
            .unwrap();
        assert!(store.export_records(vec![id]).await.is_err());
        assert!(!store
            .validate_export(vec![(id, "image".into())])
            .await
            .unwrap());
    }

    #[test]
    fn export_ids_rejects_invalid_ranges_and_selection_limits() {
        assert!(export_ids(&[0]).is_err());
        assert!(export_ids(&[u64::MAX]).is_err());
        let ids = (1..=CLIPBOARD_EXPORT_MAX_RECORDS as u64).collect::<Vec<_>>();
        assert_eq!(
            export_ids(&ids).unwrap().len(),
            CLIPBOARD_EXPORT_MAX_RECORDS
        );
        assert!(
            export_ids(&(1..=CLIPBOARD_EXPORT_MAX_RECORDS as u64 + 1).collect::<Vec<_>>()).is_err()
        );
    }

    #[tokio::test]
    async fn export_budget_applies_to_the_whole_selection_and_accepts_the_boundary() {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        let first = seed(&store, ClipboardPayload::Text("one".into()), "one").await;
        let second = seed(&store, ClipboardPayload::Text("two".into()), "two").await;
        store
            .db
            .execute_unprepared(&format!(
                "UPDATE clipboard_entries SET storage_bytes={} WHERE id IN ({first}, {second})",
                CLIPBOARD_EXPORT_MAX_BYTES / 2,
            ))
            .await
            .unwrap();
        assert_eq!(
            store
                .export_records(vec![first, second])
                .await
                .unwrap()
                .len(),
            2
        );
        store
            .db
            .execute_unprepared(&format!(
                "UPDATE clipboard_entries SET storage_bytes=storage_bytes+1 WHERE id={second}",
            ))
            .await
            .unwrap();
        assert!(store.export_records(vec![first, second]).await.is_err());
        store
            .db
            .execute_unprepared(&format!(
                "UPDATE clipboard_entries SET storage_bytes=-1 WHERE id={first}",
            ))
            .await
            .unwrap();
        assert!(store.export_records(vec![first]).await.is_err());
    }

    #[tokio::test]
    async fn missing_payload_aborts_the_entire_batch_without_changing_selection() {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        let first = seed(&store, ClipboardPayload::Text("one".into()), "one").await;
        let second = seed(&store, ClipboardPayload::Text("two".into()), "two").await;
        store
            .db
            .execute_unprepared(&format!(
                "DELETE FROM clipboard_payloads WHERE entry_id={first}",
            ))
            .await
            .unwrap();
        let before = store.current_summary().await.unwrap();
        let revision = store.revision().await.unwrap();
        assert!(store.export_records(vec![second, first]).await.is_err());
        assert_eq!(store.current_summary().await.unwrap(), before);
        assert_eq!(store.revision().await.unwrap(), revision);
        // A failed transaction releases its sole connection for later exports.
        assert_eq!(store.export_records(vec![second]).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn files_export_is_read_only_and_does_not_probe_paths_or_search_text() {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        let paths = vec!["/missing/中文 #100%.png".into(), "relative/folder".into()];
        let id = seed(&store, ClipboardPayload::Files(paths.clone()), "files").await;
        // An invalid UTF-8 search index would fail decoding a full entry model.
        // Export only needs the bounded metadata projection, not indexed content.
        store.db.execute_unprepared(&format!(
            "UPDATE clipboard_entries SET search_text=CAST(X'80' AS TEXT), sensitive=1 WHERE id={id}",
        )).await.unwrap();
        let revision = store.revision().await.unwrap();
        let exported = store.export_records(vec![id]).await.unwrap();
        assert_eq!(exported[0].payload, ClipboardPayload::Files(paths));
        assert!(exported[0].sensitive);
        assert_eq!(store.revision().await.unwrap(), revision);
        store
            .db
            .execute_unprepared(&format!(
                "UPDATE clipboard_payloads SET files_json=NULL WHERE entry_id={id}",
            ))
            .await
            .unwrap();
        assert!(store.export_records(vec![id]).await.is_err());
    }

    #[tokio::test]
    async fn validation_only_queries_live_metadata_without_requiring_payload_storage() {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        let id = seed(&store, ClipboardPayload::Text("one".into()), "one").await;
        store
            .db
            .execute_unprepared("DROP TABLE clipboard_payloads")
            .await
            .unwrap();
        let revision = store.revision().await.unwrap();
        assert!(store
            .validate_export(vec![(id, "one".into())])
            .await
            .unwrap());
        assert!(!store
            .validate_export(vec![(id, "changed".into())])
            .await
            .unwrap());
        assert!(!store
            .validate_export(vec![(id + 1, "one".into())])
            .await
            .unwrap());
        assert!(store
            .validate_export(vec![(id, "one".into()), (id, "one".into())])
            .await
            .is_err());
        assert_eq!(store.revision().await.unwrap(), revision);
    }
}
