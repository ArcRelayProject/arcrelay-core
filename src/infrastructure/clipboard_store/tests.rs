use super::*;

#[test]
fn legacy_timeline_migration_preserves_history_and_runs_from_a_plain_thread() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-timeline.sqlite3");
        let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        let seed = store.store(
            ClipboardPayload::Text("preserved legacy content".into()),
            "legacy-timeline-fixture".into(),
            summary(ClipboardContentKind::Text, "preserved legacy content"),
            true, "local", "Local", true,
        ).await.unwrap().unwrap();
        store.db.execute_unprepared(
            "UPDATE clipboard_entries SET first_captured_at_ms = captured_at_ms + 5000,
                copy_count = 9, favorite = 1;
             DELETE FROM seaql_migrations WHERE version = 'm20260908_repair_legacy_clipboard_timeline_v11';",
        ).await.unwrap();
        let revision = store.revision().await.unwrap();
        drop(store);

        let reopened = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        let record = reopened.replica_record(&seed.sync_id).await.unwrap().unwrap();
        assert_eq!(record.first_captured_at_ms, seed.captured_at_ms);
        assert_eq!(record.record.captured_at_ms, seed.captured_at_ms);
        assert_eq!(record.copy_count, 9);
        assert_eq!(record.record.text, seed.text);
        assert_eq!(record.record.revision, seed.revision);
        assert!(record.record.favorite);
        assert_eq!(reopened.revision().await.unwrap(), revision + 1);
        let history = reopened.history(ClipboardQuery::recent(10)).await.unwrap();
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].first_captured_at, history.entries[0].captured_at);
        drop(reopened);
        let reopened = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        assert_eq!(reopened.revision().await.unwrap(), revision + 1);
    });
}

#[tokio::test]
async fn legacy_copy_older_than_local_creation_keeps_a_valid_replica_timeline() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let mut record = store
        .store(
            ClipboardPayload::Text("legacy copy".into()),
            "legacy-copy-fixture".into(),
            summary(ClipboardContentKind::Text, "legacy copy"),
            true,
            "local",
            "Local",
            true,
        )
        .await
        .unwrap()
        .unwrap();
    record.captured_at_ms -= 5000;
    record.revision += 1;
    record.updated_by_device_id = "peer".into();
    record.live = false;
    let id = record.sync_id.clone();
    let captured = record.captured_at_ms;
    assert!(store.apply_sync_record(record).await.unwrap());
    let history = store.history(ClipboardQuery::recent(10)).await.unwrap();
    assert_eq!(
        history.entries[0].first_captured_at.timestamp_millis(),
        captured
    );
    assert_eq!(history.entries[0].captured_at.timestamp_millis(), captured);
    let replica = store.replica_record(&id).await.unwrap().unwrap();
    assert_eq!(replica.first_captured_at_ms, captured);
    assert_eq!(replica.record.captured_at_ms, captured);
}

#[tokio::test]
async fn indexed_search_keeps_literal_cjk_and_punctuation_semantics() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let text = "项目优化测验 100%_done \"quoted\"";
    store
        .store(
            ClipboardPayload::Text(text.into()),
            "search-fixture".into(),
            summary(ClipboardContentKind::Text, text),
            true,
            "local",
            "Local",
            true,
        )
        .await
        .unwrap();
    for search in ["优", "优化", "目优化", "%_", "0%_", "\"quoted\""] {
        let page = store
            .history(ClipboardQuery {
                search: Some(search.into()),
                include_total_count: false,
                ..ClipboardQuery::recent(10)
            })
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 1, "literal query {search}");
        assert_eq!(page.total_count, None);
    }
    assert!(store
        .history(ClipboardQuery {
            search: Some("OR nonexistent".into()),
            ..ClipboardQuery::recent(10)
        })
        .await
        .unwrap()
        .entries
        .is_empty());
    store.clear().await.unwrap();
    assert_eq!(
        SqliteClipboardStore::item_count(&store.db).await.unwrap(),
        0
    );
    assert!(store
        .history(ClipboardQuery {
            search: Some("目优化".into()),
            ..ClipboardQuery::recent(10)
        })
        .await
        .unwrap()
        .entries
        .is_empty());
}

#[tokio::test]
async fn interactive_reader_is_independent_of_an_open_write_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("clipboard.sqlite3");
    let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
    store
        .store(
            ClipboardPayload::Text("committed".into()),
            "reader-fixture".into(),
            summary(ClipboardContentKind::Text, "committed"),
            true,
            "local",
            "Local",
            true,
        )
        .await
        .unwrap();
    let reader = store.reader(Some(&path)).await.unwrap();
    let transaction = store.db.begin().await.unwrap();
    transaction
        .execute_unprepared("UPDATE clipboard_entries SET favorite = 1")
        .await
        .unwrap();
    let page = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        reader.history(ClipboardQuery::recent(10)),
    )
    .await
    .expect("reads must not queue behind a writer")
    .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert!(!page.entries[0].favorite);
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn retention_commits_bounded_batches_and_accounting_matches_rows() {
    let store = timeline_fixture(350).await;
    let mut policy = store.policy().await.unwrap();
    policy.retention_days = 0;
    policy.max_items = 10;
    store.update_policy(policy).await.unwrap();
    let mut previous = SqliteClipboardStore::item_count(&store.db).await.unwrap();
    assert_eq!(previous, 222);
    while store.take_maintenance() {
        store.maintain().await.unwrap();
        let current = SqliteClipboardStore::item_count(&store.db).await.unwrap();
        assert!(previous - current <= 128);
        previous = current;
    }
    assert_eq!(previous, 10);
    let totals = store.db.query_one(Statement::from_string(sea_orm::DbBackend::Sqlite,
        "SELECT (SELECT total_bytes FROM clipboard_statistics WHERE id=1) = SUM(storage_bytes) AS correct FROM clipboard_entries WHERE deleted=0".to_string())).await.unwrap().unwrap();
    assert_eq!(totals.try_get::<i64>("", "correct").unwrap(), 1);
}

fn summary(kind: ClipboardContentKind, preview: &str) -> ClipboardSummary {
    let captured_at = Utc::now();
    ClipboardSummary {
        id: 0,
        kind,
        preview: preview.into(),
        source_app: Some("Safari".into()),
        first_captured_at: captured_at,
        captured_at,
        last_used_at: None,
        size_bytes: preview.len() as u64,
        character_count: matches!(
            kind,
            ClipboardContentKind::Text | ClipboardContentKind::Html
        )
        .then(|| preview.chars().count() as u64),
        item_count: 1,
        width: None,
        height: None,
        sensitive: false,
        favorite: false,
        labels: Vec::new(),
        copy_count: 1,
        available: true,
        sync_id: "test-sync-id".into(),
        source_device_id: None,
        source_device_name: None,
        text_syntax: ClipboardTextSyntax::Plain,
    }
}

async fn timeline_fixture(count: usize) -> SqliteClipboardStore {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    for index in 0..count {
        let text = format!("timeline-{index}");
        store
            .store(
                ClipboardPayload::Text(text.clone()),
                text.clone(),
                summary(ClipboardContentKind::Text, &text),
                true,
                "local",
                "Local",
                true,
            )
            .await
            .unwrap();
    }
    store
        .db
        .execute_unprepared("UPDATE clipboard_entries SET sync_id = printf('%064x', id)")
        .await
        .unwrap();
    // Deliberately identical timestamps exercise the stable sync-ID tie-breaker in both
    // directions without timer-dependent tests.
    clipboard_entry::Entity::update_many()
        .col_expr(
            clipboard_entry::Column::FirstCapturedAtMs,
            Expr::value(1_000_i64),
        )
        .col_expr(
            clipboard_entry::Column::CapturedAtMs,
            Expr::value(1_000_i64),
        )
        .col_expr(clipboard_entry::Column::UpdatedAtMs, Expr::value(1_000_i64))
        .exec(&store.db)
        .await
        .unwrap();
    store
}

async fn timeline_page(
    store: &SqliteClipboardStore,
    position: ClipboardTimelinePosition,
    sort_by: ClipboardSortBy,
    limit: usize,
) -> ClipboardTimelinePage {
    store
        .timeline(ClipboardTimelineQuery {
            position,
            sort_by,
            limit,
        })
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn timeline_seeks_deep_anchors_and_pages_both_ways_without_touching_records() {
    let store = timeline_fixture(181).await;
    let before = clipboard_entry::Entity::find()
        .order_by_asc(clipboard_entry::Column::Id)
        .all(&store.db)
        .await
        .unwrap();
    let revision = store.revision().await.unwrap();
    let mut page = timeline_page(
        &store,
        ClipboardTimelinePosition::AroundId(90),
        ClipboardSortBy::UpdatedAt,
        3,
    )
    .await;
    assert_eq!(
        page.entries.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![93, 92, 91, 90, 89, 88, 87]
    );
    assert_eq!(
        page.anchor,
        Some(ClipboardCursor {
            sort_at_ms: 1_000,
            id: 90
        })
    );
    assert_eq!(page.revision, revision);
    assert_eq!(page.total_count, 181);
    let mut ids = page.entries.iter().map(|item| item.id).collect::<Vec<_>>();
    let older = page.older_cursor;
    while let Some(cursor) = page.newer_cursor {
        page = timeline_page(
            &store,
            ClipboardTimelinePosition::NewerThan(cursor),
            ClipboardSortBy::UpdatedAt,
            7,
        )
        .await;
        let mut newer = page.entries.iter().map(|item| item.id).collect::<Vec<_>>();
        newer.extend(ids);
        ids = newer;
    }
    let mut cursor = older;
    while let Some(value) = cursor {
        page = timeline_page(
            &store,
            ClipboardTimelinePosition::OlderThan(value),
            ClipboardSortBy::UpdatedAt,
            7,
        )
        .await;
        ids.extend(page.entries.iter().map(|item| item.id));
        cursor = page.older_cursor;
    }
    assert_eq!(ids, (1..=181).rev().collect::<Vec<_>>());
    assert_eq!(store.revision().await.unwrap(), revision);
    assert_eq!(
        clipboard_entry::Entity::find()
            .order_by_asc(clipboard_entry::Column::Id)
            .all(&store.db)
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn timeline_respects_both_sort_orders_and_endpoints() {
    let store = timeline_fixture(9).await;
    for id in 1..=9_i64 {
        let model = clipboard_entry::Entity::find_by_id(id)
            .one(&store.db)
            .await
            .unwrap()
            .unwrap();
        let mut model = model.into_active_model();
        model.first_captured_at_ms = Set(id * 1_000);
        model.updated_at_ms = Set((10 - id) * 1_000);
        model.update(&store.db).await.unwrap();
    }
    for sort_by in [ClipboardSortBy::CreatedAt, ClipboardSortBy::UpdatedAt] {
        let all = store
            .history(ClipboardQuery {
                sort_by,
                ..ClipboardQuery::recent(20)
            })
            .await
            .unwrap();
        for index in [0, 4, 8] {
            let id = all.entries[index].id;
            let page =
                timeline_page(&store, ClipboardTimelinePosition::AroundId(id), sort_by, 2).await;
            let expected = &all.entries[index.saturating_sub(2)..(index + 3).min(9)];
            assert_eq!(page.entries, expected);
            assert_eq!(page.newer_cursor.is_none(), index == 0);
            assert_eq!(page.older_cursor.is_none(), index == 8);
        }
    }
}

#[tokio::test]
async fn timeline_handles_missing_deleted_unavailable_and_payloadless_records() {
    let store = timeline_fixture(9).await;
    store.delete(5, "local").await.unwrap();
    for id in [5, 999] {
        assert!(store
            .timeline(ClipboardTimelineQuery {
                position: ClipboardTimelinePosition::AroundId(id),
                sort_by: ClipboardSortBy::CreatedAt,
                limit: 3,
            })
            .await
            .unwrap()
            .is_none());
    }
    clipboard_payload::Entity::delete_by_id(4)
        .exec(&store.db)
        .await
        .unwrap();
    clipboard_entry::Entity::update_many()
        .col_expr(clipboard_entry::Column::Available, Expr::value(false))
        .filter(clipboard_entry::Column::Id.eq(4))
        .exec(&store.db)
        .await
        .unwrap();
    let page = timeline_page(
        &store,
        ClipboardTimelinePosition::AroundId(4),
        ClipboardSortBy::CreatedAt,
        3,
    )
    .await;
    assert_eq!(page.total_count, 8);
    assert!(page.entries.iter().all(|item| item.id != 5));
    assert!(
        !page
            .entries
            .iter()
            .find(|item| item.id == 4)
            .unwrap()
            .available
    );
    // A deleted boundary is still a valid keyset cursor.
    let page = timeline_page(
        &store,
        ClipboardTimelinePosition::OlderThan(ClipboardCursor {
            sort_at_ms: 1_000,
            id: 5,
        }),
        ClipboardSortBy::CreatedAt,
        2,
    )
    .await;
    assert_eq!(
        page.entries.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![4, 3]
    );
}

#[tokio::test]
async fn timeline_retains_labels_and_bounds_page_sizes() {
    let store = timeline_fixture(9).await;
    let label = store
        .create_label("Context", "#5B5FF0", "local")
        .await
        .unwrap();
    store
        .set_label_membership(5, &label.id, true, "local")
        .await
        .unwrap();
    let page = timeline_page(
        &store,
        ClipboardTimelinePosition::AroundId(5),
        ClipboardSortBy::CreatedAt,
        0,
    )
    .await;
    assert_eq!(page.entries.len(), 3);
    assert_eq!(page.entries[1].labels, vec![label]);
    let page = timeline_page(
        &store,
        ClipboardTimelinePosition::AroundId(5),
        ClipboardSortBy::CreatedAt,
        usize::MAX,
    )
    .await;
    assert_eq!(page.entries.len(), 9);
    assert!(page.newer_cursor.is_none() && page.older_cursor.is_none());
}

fn sync_text(
    sync_id: &str,
    text: Option<&str>,
    revision: u64,
    updated_by: &str,
    deleted: bool,
) -> ClipboardSyncRecord {
    ClipboardSyncRecord {
        sync_id: sync_id.into(),
        kind: ClipboardContentKind::Text,
        text: text.map(str::to_string),
        html: None,
        rtf: None,
        image_png: None,
        width: None,
        height: None,
        preview: text.unwrap_or("deleted").into(),
        source_app: Some("Tests".into()),
        source_device_id: "source-device".into(),
        source_device_name: "Source Device".into(),
        captured_at_ms: Utc::now().timestamp_millis(),
        revision,
        updated_by_device_id: updated_by.into(),
        favorite: false,
        favorite_revision: 1,
        favorite_updated_by_device_id: updated_by.into(),
        labels: Vec::new(),
        label_memberships: Vec::new(),
        deleted,
        change_kind: if deleted {
            ClipboardSyncChangeKind::Delete
        } else {
            ClipboardSyncChangeKind::Copy
        },
        live: !deleted,
        text_syntax: text.map(detect_text_syntax).unwrap_or_default(),
    }
}

#[tokio::test]
async fn tombstone_arriving_first_rejects_an_older_live_record() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    assert!(store
        .apply_sync_record(sync_text("sync-a", None, 2, "device-b", true))
        .await
        .unwrap());
    assert!(!store
        .apply_sync_record(sync_text("sync-a", Some("stale"), 1, "device-a", false,))
        .await
        .unwrap());
    assert!(store
        .history(ClipboardQuery::recent(20))
        .await
        .unwrap()
        .entries
        .is_empty());
}

#[tokio::test]
async fn equal_revisions_converge_by_updating_device_id() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    assert!(store
        .apply_sync_record(sync_text("sync-b", Some("winner"), 7, "device-z", false,))
        .await
        .unwrap());
    assert!(!store
        .apply_sync_record(sync_text("sync-b", Some("loser"), 7, "device-a", false,))
        .await
        .unwrap());
    let page = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(page.entries[0].preview, "winner");
}

#[tokio::test]
async fn payload_preflight_only_requests_newer_content() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    assert!(store
        .apply_sync_record(sync_text(
            "sync-preflight",
            Some("current"),
            7,
            "device-m",
            false
        ))
        .await
        .unwrap());

    assert!(!store
        .sync_record_requires_payload("sync-preflight", 6, "device-z")
        .await
        .unwrap());
    assert!(!store
        .sync_record_requires_payload("sync-preflight", 7, "device-a")
        .await
        .unwrap());
    assert!(store
        .sync_record_requires_payload("sync-preflight", 7, "device-z")
        .await
        .unwrap());
    assert!(store
        .sync_record_requires_payload("missing", 1, "device-a")
        .await
        .unwrap());
}

#[tokio::test]
async fn rich_text_round_trips_with_rtf_and_syntax() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let payload = ClipboardPayload::RichText {
        html: "<p><strong>Hello</strong></p>".into(),
        plain_text: "Hello".into(),
        rtf: Some("{\\rtf1\\b Hello}".into()),
    };
    let summary = ClipboardSummary {
        text_syntax: ClipboardTextSyntax::Markdown,
        ..summary(ClipboardContentKind::Html, "Hello")
    };
    let record = store
        .store(
            payload.clone(),
            "rich-hash".into(),
            summary,
            true,
            "local",
            "Local",
            true,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.kind, ClipboardContentKind::Html);
    assert_eq!(
        record.html.as_deref(),
        Some("<p><strong>Hello</strong></p>")
    );
    assert_eq!(record.rtf.as_deref(), Some("{\\rtf1\\b Hello}"));
    assert_eq!(record.text_syntax, ClipboardTextSyntax::Markdown);
    let stored = store.payload(1).await.unwrap().0;
    assert_eq!(stored, payload);
    assert_eq!(store.text_content(1).await.unwrap(), "Hello");
    let history = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(history.entries[0].character_count, Some(5));
}

#[tokio::test]
async fn history_character_count_uses_full_payload_instead_of_preview() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let content = "字".repeat(200);
    store
        .store(
            ClipboardPayload::Text(content),
            "long-text-hash".into(),
            summary(
                ClipboardContentKind::Text,
                &format!("{}…", "字".repeat(140)),
            ),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();

    let history = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(history.entries[0].preview.chars().count(), 141);
    assert_eq!(history.entries[0].character_count, Some(200));
}

#[tokio::test]
async fn label_membership_updates_only_the_selected_label() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    store
        .store(
            ClipboardPayload::Text("labels".into()),
            "labels-hash".into(),
            summary(ClipboardContentKind::Text, "labels"),
            true,
            "local",
            "Local",
            false,
        )
        .await
        .unwrap();
    let first = store
        .create_label("First", "#112233", "local")
        .await
        .unwrap();
    let second = store
        .create_label("Second", "#445566", "local")
        .await
        .unwrap();
    store
        .set_labels(1, vec![first.id.clone(), second.id.clone()], "local")
        .await
        .unwrap();
    store
        .set_label_membership(1, &first.id, false, "local")
        .await
        .unwrap();
    let record = store.sync_record(1).await.unwrap().unwrap();
    assert!(record
        .label_memberships
        .iter()
        .any(|item| item.label_id == first.id && !item.attached));
    assert!(record
        .label_memberships
        .iter()
        .any(|item| item.label_id == second.id && item.attached));
}

#[tokio::test]
async fn sync_history_includes_file_metadata_without_payload() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    store
        .store(
            ClipboardPayload::Files(vec![file.path().to_string_lossy().into_owned()]),
            "file-hash".into(),
            summary(ClipboardContentKind::Files, "report.txt"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();
    store
        .store(
            ClipboardPayload::Text("share me".into()),
            "text-hash".into(),
            summary(ClipboardContentKind::Text, "share me"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();
    let page = store.sync_records(0, 1).await.unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].1.kind, ClipboardContentKind::Files);
    assert!(page.records[0].1.text.is_none());
    let next = store
        .sync_records(page.next_cursor.unwrap(), 1)
        .await
        .unwrap();
    assert_eq!(next.records[0].1.text.as_deref(), Some("share me"));
}

#[tokio::test]
async fn deduplicates_globally_and_supports_search_and_pinning() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    store
        .store(
            ClipboardPayload::Text("hello world".into()),
            "hash-a".into(),
            summary(ClipboardContentKind::Text, "hello world"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();
    store
        .store(
            ClipboardPayload::Text("second".into()),
            "hash-b".into(),
            summary(ClipboardContentKind::Text, "second"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();
    store
        .store(
            ClipboardPayload::Text("hello world".into()),
            "hash-a".into(),
            summary(ClipboardContentKind::Text, "hello world"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();

    let page = store
        .history(ClipboardQuery {
            search: Some("HELLO".into()),
            ..ClipboardQuery::recent(20)
        })
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.total_count, Some(1));
    assert_eq!(page.entries[0].copy_count, 2);
    assert_eq!(page.entries[0].character_count, Some(11));

    let recent = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(recent.entries.len(), 2);
    assert_eq!(recent.total_count, Some(2));
    assert_eq!(recent.entries[0].preview, "hello world");

    store
        .set_favorite(page.entries[0].id, true, "local-device")
        .await
        .unwrap();
    let page = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert!(page.entries[0].favorite);
}

#[tokio::test]
async fn supports_created_and_updated_history_ordering() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    store
        .store(
            ClipboardPayload::Text("first".into()),
            "hash-first".into(),
            summary(ClipboardContentKind::Text, "first"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();
    store
        .store(
            ClipboardPayload::Text("second".into()),
            "hash-second".into(),
            summary(ClipboardContentKind::Text, "second"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();

    let created = store
        .history(ClipboardQuery {
            sort_by: ClipboardSortBy::CreatedAt,
            ..ClipboardQuery::recent(20)
        })
        .await
        .unwrap();
    assert_eq!(created.entries[0].preview, "second");

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let revision = store.revision().await.unwrap();
    store.payload(created.entries[1].id).await.unwrap();
    assert!(store.revision().await.unwrap() > revision);

    let updated = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(updated.entries[0].preview, "first");

    let created = store
        .history(ClipboardQuery {
            sort_by: ClipboardSortBy::CreatedAt,
            ..ClipboardQuery::recent(20)
        })
        .await
        .unwrap();
    assert_eq!(created.entries[0].preview, "second");
}

#[tokio::test]
async fn updated_history_cursor_uses_effective_update_time() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    for (hash, text) in [
        ("hash-first", "first"),
        ("hash-second", "second"),
        ("hash-third", "third"),
    ] {
        store
            .store(
                ClipboardPayload::Text(text.into()),
                hash.into(),
                summary(ClipboardContentKind::Text, text),
                true,
                "local-device",
                "Local Device",
                true,
            )
            .await
            .unwrap();
    }
    let all = store.history(ClipboardQuery::recent(20)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    store.payload(all.entries[2].id).await.unwrap();

    let first_page = store.history(ClipboardQuery::recent(2)).await.unwrap();
    assert_eq!(first_page.entries.len(), 2);
    assert_eq!(first_page.total_count, Some(3));
    assert_eq!(first_page.entries[0].preview, "first");

    let second_page = store
        .history(ClipboardQuery {
            limit: 2,
            cursor: first_page.next_cursor,
            ..ClipboardQuery::recent(2)
        })
        .await
        .unwrap();
    assert_eq!(second_page.entries.len(), 1);
    assert_eq!(second_page.total_count, Some(3));
    assert_eq!(second_page.entries[0].preview, "second");
}

#[tokio::test]
async fn policy_can_stop_capture_and_sensitive_storage() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let mut policy = store.policy().await.unwrap();
    policy.history_enabled = false;
    store.update_policy(policy).await.unwrap();
    assert!(store
        .store(
            ClipboardPayload::Text("hidden".into()),
            "hash-hidden".into(),
            summary(ClipboardContentKind::Text, "hidden"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap()
        .is_none());
    assert!(store
        .history(ClipboardQuery::recent(20))
        .await
        .unwrap()
        .entries
        .is_empty());
}

#[tokio::test]
async fn persists_to_database_path_with_spaces() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("clipboard history.sqlite3");

    {
        let store = SqliteClipboardStore::connect(Some(&database_path))
            .await
            .unwrap();
        store
            .store(
                ClipboardPayload::Text("persisted text".into()),
                "hash-persisted".into(),
                summary(ClipboardContentKind::Text, "persisted text"),
                true,
                "local-device",
                "Local Device",
                true,
            )
            .await
            .unwrap();
    }

    let reopened = SqliteClipboardStore::connect(Some(&database_path))
        .await
        .unwrap();
    let page = reopened.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].preview, "persisted text");
}

#[tokio::test]
async fn protocol_v5_recreates_an_applied_v4_schema() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("clipboard-v4.sqlite3");
    let url = format!("sqlite://{}?mode=rwc", database_path.display());
    let legacy = Database::connect(url).await.unwrap();
    for statement in [
            "CREATE TABLE seaql_migrations (version varchar NOT NULL PRIMARY KEY, applied_at bigint NOT NULL)",
            "INSERT INTO seaql_migrations (version, applied_at) VALUES ('migration', 1)",
            "CREATE TABLE clipboard_entries (id integer NOT NULL PRIMARY KEY, kind integer NOT NULL)",
            "INSERT INTO clipboard_entries (id, kind) VALUES (1, 1)",
        ] {
            legacy
                .execute(Statement::from_string(
                    sea_orm::DbBackend::Sqlite,
                    statement.to_string(),
                ))
                .await
                .unwrap();
        }
    legacy.close().await.unwrap();

    let store = SqliteClipboardStore::connect(Some(&database_path))
        .await
        .unwrap();
    assert!(store
        .history(ClipboardQuery::recent(20))
        .await
        .unwrap()
        .entries
        .is_empty());
    store
        .store(
            ClipboardPayload::Text("v5".into()),
            "v5-hash".into(),
            summary(ClipboardContentKind::Text, "v5"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn startup_capture_does_not_touch_an_existing_record() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    store
        .store(
            ClipboardPayload::Text("existing".into()),
            "hash-existing".into(),
            summary(ClipboardContentKind::Text, "existing"),
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();

    assert!(store
        .store(
            ClipboardPayload::Text("existing".into()),
            "hash-existing".into(),
            summary(ClipboardContentKind::Text, "existing"),
            false,
            "local-device",
            "Local Device",
            false,
        )
        .await
        .unwrap()
        .is_none());

    let page = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].copy_count, 1);
}

#[tokio::test]
async fn file_retention_counts_the_stored_path_not_the_file_size() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    let path = file.path().to_string_lossy().into_owned();
    let mut file_summary = summary(ClipboardContentKind::Files, "large.iso");
    file_summary.size_bytes = 10 * 1024 * 1024 * 1024;

    let mut policy = store.policy().await.unwrap();
    policy.max_bytes = 1024;
    store.update_policy(policy).await.unwrap();
    store
        .store(
            ClipboardPayload::Files(vec![path]),
            "hash-large-file-path".into(),
            file_summary,
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();

    let page = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].size_bytes, 10 * 1024 * 1024 * 1024);
}

#[tokio::test]
async fn history_reads_metadata_without_loading_large_image_blob() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let mut image_summary = summary(ClipboardContentKind::Image, "large image");
    image_summary.size_bytes = 8 * 1024 * 1024;
    image_summary.width = Some(2048);
    image_summary.height = Some(2048);
    store
        .store(
            ClipboardPayload::Image {
                png: vec![7; 8 * 1024 * 1024],
                width: 2048,
                height: 2048,
            },
            "large-image-hash".into(),
            image_summary,
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap();

    let page = store.history(ClipboardQuery::recent(20)).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].preview, "large image");
    let plan = store
            .db
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "EXPLAIN QUERY PLAN SELECT id, preview FROM clipboard_entries ORDER BY favorite DESC, captured_at_ms DESC, id DESC LIMIT 21",
            ))
            .await
            .unwrap();
    assert!(!plan.is_empty());
    assert_eq!(
        store
            .image_png(page.entries[0].id)
            .await
            .unwrap()
            .unwrap()
            .len(),
        8 * 1024 * 1024
    );
}

#[tokio::test]
async fn persisted_image_ocr_round_trips_blocks_and_participates_in_search() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("clipboard OCR.sqlite3");
    let store = SqliteClipboardStore::connect(Some(&database_path))
        .await
        .unwrap();
    let mut image_summary = summary(ClipboardContentKind::Image, "image 640 × 480");
    image_summary.width = Some(640);
    image_summary.height = Some(480);
    let record = store
        .store(
            ClipboardPayload::Image {
                png: vec![1, 2, 3],
                width: 640,
                height: 480,
            },
            "ocr-image-hash".into(),
            image_summary,
            true,
            "local-device",
            "Local Device",
            true,
        )
        .await
        .unwrap()
        .unwrap();
    let id = store
        .ensure_image_ocr_pending(&record.sync_id, "test-model", false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store.pending_image_ocr_source(id).await.unwrap(),
        Some(vec![1, 2, 3])
    );
    let points = [
        crate::domain::clipboard::ClipboardOcrPoint { x: 10.5, y: 20.5 },
        crate::domain::clipboard::ClipboardOcrPoint { x: 110.5, y: 20.5 },
        crate::domain::clipboard::ClipboardOcrPoint { x: 110.5, y: 60.5 },
        crate::domain::clipboard::ClipboardOcrPoint { x: 10.5, y: 60.5 },
    ];
    store
        .save_image_ocr(
            id,
            ClipboardImageOcr {
                text: "ArcRelay 图片文字搜索".into(),
                blocks: vec![ClipboardOcrBlock {
                    text: "图片文字搜索".into(),
                    confidence: 0.97,
                    left: 10,
                    top: 20,
                    width: 101,
                    height: 41,
                    points: Some(points),
                    characters: vec![crate::domain::clipboard::ClipboardOcrCharacter {
                        text: "图".into(),
                        confidence: 0.98,
                        points,
                    }],
                }],
                model_version: "test-model".into(),
                updated_at_ms: 1234,
            },
        )
        .await
        .unwrap();

    let persisted = store.image_ocr(id).await.unwrap().unwrap();
    assert_eq!(persisted.text, "ArcRelay 图片文字搜索");
    assert_eq!(persisted.blocks.len(), 1);
    assert_eq!(persisted.blocks[0].left, 10);
    assert_eq!(persisted.blocks[0].width, 101);
    assert_eq!(persisted.blocks[0].points, Some(points));
    assert_eq!(persisted.blocks[0].characters[0].text, "图");
    assert_eq!(persisted.blocks[0].characters[0].points, points);
    assert!(store.pending_image_ocr_source(id).await.unwrap().is_none());

    let mut query = ClipboardQuery::recent(20);
    query.search = Some("文字搜索".into());
    let page = store.history(query).await.unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].id, id);

    drop(store);
    let reopened = SqliteClipboardStore::connect(Some(&database_path))
        .await
        .unwrap();
    let persisted = reopened.image_ocr(id).await.unwrap().unwrap();
    assert_eq!(persisted.blocks[0].points, Some(points));
    assert_eq!(persisted.blocks[0].characters[0].text, "图");
    let mut query = ClipboardQuery::recent(20);
    query.search = Some("ArcRelay".into());
    assert_eq!(reopened.history(query).await.unwrap().entries.len(), 1);
}

#[tokio::test]
#[ignore = "reproducible search scenario; run explicitly in performance CI"]
async fn performance_clipboard_search_10000() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteClipboardStore::connect(Some(&directory.path().join("performance.sqlite3")))
        .await
        .unwrap();
    let mut policy = store.policy().await.unwrap();
    policy.max_items = 10_000;
    policy.retention_days = 0;
    store.update_policy(policy).await.unwrap();
    for i in 0..10_000 {
        let text = format!(
            "document {i:05} searchable clipboard content {}",
            "background ".repeat(30)
        );
        store
            .store(
                ClipboardPayload::Text(text.clone()),
                format!("perf-{i}"),
                summary(ClipboardContentKind::Text, &text),
                true,
                "local",
                "Local",
                true,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        SqliteClipboardStore::item_count(&store.db).await.unwrap(),
        10_000
    );
    let mut elapsed = Vec::new();
    for i in 0..101 {
        let start = std::time::Instant::now();
        let page = store
            .history(ClipboardQuery {
                search: Some(format!("{:05}", i * 97)),
                include_total_count: false,
                ..ClipboardQuery::recent(20)
            })
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert!(page.total_count.is_none());
        if i != 0 {
            elapsed.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    elapsed.sort_by(f64::total_cmp);
    println!(
        "performance clipboard-search rows=10000 samples=100 p50_ms={:.3} p95_ms={:.3}",
        elapsed[50], elapsed[95]
    );
    // A generous regression ceiling catches accidental full payload scans while
    // allowing different CI hardware. Track detailed samples before tightening.
    assert!(
        elapsed[95] < 500.0,
        "interactive search exceeded its latency budget"
    );
}

#[tokio::test]
async fn local_copy_advances_past_an_observed_peer_with_a_faster_clock() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let seed = store
        .store(
            ClipboardPayload::Text("remote".into()),
            "remote-hash".into(),
            summary(ClipboardContentKind::Text, "remote"),
            true,
            "peer",
            "Peer",
            true,
        )
        .await
        .unwrap()
        .unwrap();
    let mut remote = store.replica_record(&seed.sync_id).await.unwrap().unwrap();
    remote.record.captured_at_ms = Utc::now().timestamp_millis() + 5_000;
    remote.record.revision += 1;
    remote.record.live = true;
    remote.record.change_kind = ClipboardSyncChangeKind::Copy;
    let observed = remote.record.captured_at_ms;
    store.apply_replica_record(remote).await.unwrap();
    let local = store
        .store(
            ClipboardPayload::Text("local".into()),
            "local-hash".into(),
            summary(ClipboardContentKind::Text, "local"),
            true,
            "local",
            "Local",
            true,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(local.captured_at_ms > observed);
    assert_eq!(
        store.replica_selection().await.unwrap().unwrap().sync_id,
        local.sync_id
    );
}

#[tokio::test]
async fn local_file_retention_cannot_evict_shared_history_and_remains_bounded() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let mut policy = store.policy().await.unwrap();
    policy.retention_days = 0;
    policy.max_items = 2;
    store.update_policy(policy).await.unwrap();
    for index in 0..2 {
        store
            .store(
                ClipboardPayload::Text(format!("shared {index}")),
                format!("shared-{index}"),
                summary(ClipboardContentKind::Text, "shared"),
                true,
                "local",
                "Local",
                true,
            )
            .await
            .unwrap();
    }
    let shared = store.replica_page(None, 10).await.unwrap();
    for index in 0..4 {
        store
            .store(
                ClipboardPayload::Files(vec![format!("/local/file-{index}")]),
                format!("file-{index}"),
                summary(ClipboardContentKind::Files, "file"),
                true,
                "local",
                "Local",
                false,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        store.replica_page(None, 10).await.unwrap().records,
        shared.records
    );
    let history = store.history(ClipboardQuery::recent(10)).await.unwrap();
    assert_eq!(history.entries.len(), 4);
    assert_eq!(
        history
            .entries
            .iter()
            .filter(|r| r.kind == ClipboardContentKind::Files)
            .count(),
        2
    );
    let totals = store
        .db
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "SELECT SUM(item_count) AS count FROM clipboard_retention_statistics".to_string(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(totals.try_get::<i64>("", "count").unwrap(), 4);
    // Deletion and clearing maintain both accounting scopes and the UI total.
    store.delete(history.entries[0].id, "local").await.unwrap();
    store.clear().await.unwrap();
    assert_eq!(
        SqliteClipboardStore::item_count(&store.db).await.unwrap(),
        0
    );
    let totals = store.db.query_one(Statement::from_string(sea_orm::DbBackend::Sqlite,
        "SELECT SUM(item_count) AS count, SUM(total_bytes) AS bytes FROM clipboard_retention_statistics".to_string())).await.unwrap().unwrap();
    assert_eq!(totals.try_get::<i64>("", "count").unwrap(), 0);
    assert_eq!(totals.try_get::<i64>("", "bytes").unwrap(), 0);
}

#[test]
fn retention_scope_migration_preserves_existing_history_without_a_runtime_context() {
    use sea_orm_migration::{MigrationTrait, SchemaManager};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retention-scopes.sqlite3");
        let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        store.store(ClipboardPayload::Text("preserved".into()), "preserved".into(),
            summary(ClipboardContentKind::Text, "preserved"), true, "local", "Local", true).await.unwrap();
        store.store(ClipboardPayload::Files(vec!["/local/path".into()]), "local-file".into(),
            summary(ClipboardContentKind::Files, "file"), true, "local", "Local", false).await.unwrap();
        retention::SeparateRetentionAccounting.down(&SchemaManager::new(&store.db)).await.unwrap();
        store.db.execute_unprepared("DELETE FROM seaql_migrations WHERE version='m20260909_separate_clipboard_retention_scopes_v12'").await.unwrap();
        let before = store.history(ClipboardQuery::recent(10)).await.unwrap();
        let selection = store.replica_selection().await.unwrap();
        let revision = store.revision().await.unwrap();
        drop(store);
        let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        assert_eq!(store.history(ClipboardQuery::recent(10)).await.unwrap(), before);
        assert_eq!(store.replica_selection().await.unwrap(), selection);
        assert_eq!(store.revision().await.unwrap(), revision + 1);
        let totals = store.db.query_all(Statement::from_string(sea_orm::DbBackend::Sqlite,
            "SELECT scope,item_count,total_bytes FROM clipboard_retention_statistics ORDER BY scope".to_string())).await.unwrap();
        assert_eq!(totals.len(), 2);
        for row in totals { assert_eq!(row.try_get::<i64>("", "item_count").unwrap(), 1); assert!(row.try_get::<i64>("", "total_bytes").unwrap() > 0); }
        drop(store);
        let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        assert_eq!(store.revision().await.unwrap(), revision + 1);
    });
}

#[tokio::test]
async fn local_file_byte_budget_does_not_reduce_shared_content_capacity() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let text = "x".repeat(60);
    store
        .store(
            ClipboardPayload::Text(text.clone()),
            "shared-bytes".into(),
            summary(ClipboardContentKind::Text, &text),
            true,
            "local",
            "Local",
            true,
        )
        .await
        .unwrap();
    store
        .store(
            ClipboardPayload::Files(vec![format!("/{}-0", "f".repeat(128))]),
            "file-bytes-0".into(),
            summary(ClipboardContentKind::Files, "file"),
            true,
            "local",
            "Local",
            true,
        )
        .await
        .unwrap();
    let shared = store.replica_page(None, 10).await.unwrap().records;
    let totals = store
        .db
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "SELECT MAX(total_bytes) AS bytes FROM clipboard_retention_statistics".to_string(),
        ))
        .await
        .unwrap()
        .unwrap();
    let limit = totals.try_get::<i64>("", "bytes").unwrap();
    let mut policy = store.policy().await.unwrap();
    policy.retention_days = 0;
    policy.max_items = 100;
    policy.max_bytes = limit as u64;
    store.update_policy(policy).await.unwrap();
    for index in 1..4 {
        let path = format!("/{}-{index}", "f".repeat(128));
        store
            .store(
                ClipboardPayload::Files(vec![path]),
                format!("file-bytes-{index}"),
                summary(ClipboardContentKind::Files, "file"),
                true,
                "local",
                "Local",
                true,
            )
            .await
            .unwrap();
    }
    assert_eq!(store.replica_page(None, 10).await.unwrap().records, shared);
    assert_eq!(
        store
            .history(ClipboardQuery::recent(10))
            .await
            .unwrap()
            .entries
            .len(),
        2
    );
    let totals = store.db.query_all(Statement::from_string(sea_orm::DbBackend::Sqlite,
        "SELECT s.item_count, s.total_bytes, COUNT(e.id) AS actual_count, COALESCE(SUM(e.storage_bytes),0) AS actual_bytes FROM clipboard_retention_statistics s LEFT JOIN clipboard_entries e ON e.deleted=0 AND s.scope=CASE WHEN e.kind=4 THEN 2 ELSE 1 END GROUP BY s.scope ORDER BY s.scope".to_string())).await.unwrap();
    for row in totals {
        assert_eq!(
            row.try_get::<i64>("", "item_count").unwrap(),
            row.try_get::<i64>("", "actual_count").unwrap()
        );
        let bytes = row.try_get::<i64>("", "total_bytes").unwrap();
        assert_eq!(bytes, row.try_get::<i64>("", "actual_bytes").unwrap());
        assert!(bytes <= limit);
    }
}
