use super::*;

#[test]
fn unchanged_image_reuses_fingerprint_but_changed_content_does_not() {
    let payload = ClipboardPayload::Image {
        png: vec![1, 2, 3],
        width: 1,
        height: 1,
    };
    let previous = ClipboardFingerprints {
        content_hash: content_hash(&payload),
        semantic_hash: "previous decoded pixels".into(),
    };
    assert_eq!(
        clipboard_fingerprints_reusing(&payload, Some(&previous)).semantic_hash,
        previous.semantic_hash
    );
    let changed = ClipboardPayload::Image {
        png: vec![4, 5, 6],
        width: 1,
        height: 1,
    };
    assert_ne!(
        clipboard_fingerprints_reusing(&changed, Some(&previous)).semantic_hash,
        previous.semantic_hash
    );
}

#[test]
fn clipboard_callback_coalesces_without_waiting_for_worker_capacity() {
    let (changed, receiver) = mpsc::sync_channel(1);
    let mut handler = HostClipboardHandler { changed };

    handler.on_clipboard_change();
    handler.on_clipboard_change();

    assert!(receiver.try_recv().is_ok());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn content_hash_is_stable_and_type_scoped() {
    let first = ClipboardPayload::Text("same content".into());
    let second = ClipboardPayload::Text("same content".into());
    let html = ClipboardPayload::RichText {
        html: "same content".into(),
        plain_text: "same content".into(),
        rtf: None,
    };

    assert_eq!(content_hash(&first), content_hash(&second));
    assert_ne!(content_hash(&first), content_hash(&html));
    assert_eq!(content_hash(&first).len(), 32);
}

#[test]
fn summaries_are_bounded_and_sensitive_values_are_masked() {
    let normal = summarize(&ClipboardPayload::Text("hello   world".into()), None);
    assert_eq!(normal.preview, "hello world");
    assert!(!normal.sensitive);

    let sensitive = summarize(
        &ClipboardPayload::Text("api_key=sk-host-only-secret".into()),
        Some("Terminal".into()),
    );
    assert_eq!(sensitive.preview, "••••••••");
    assert!(sensitive.sensitive);
    assert_eq!(sensitive.source_app.as_deref(), Some("Terminal"));
}

#[test]
fn html_and_files_have_semantic_host_generated_summaries() {
    let html = summarize(
        &ClipboardPayload::RichText {
            html: "<p>Hello <strong>ArcRelay</strong></p>".into(),
            plain_text: "Hello ArcRelay".into(),
            rtf: None,
        },
        None,
    );
    assert_eq!(html.preview, "Hello ArcRelay");
    assert_eq!(html.kind, ClipboardContentKind::Html);

    let files = summarize(
        &ClipboardPayload::Files(vec!["/tmp/report.pdf".into(), "/tmp/design.png".into()]),
        None,
    );
    assert_eq!(files.preview, "2 files · report.pdf, design.png");
    assert_eq!(files.item_count, 2);
}

#[test]
fn safe_html_preview_keeps_structure_and_removes_active_content() {
    let preview = sanitize_html_preview(
        r#"<style>body { color: red }</style><h2 onclick="steal()">Title</h2>
            <p style="color:red">Hello <strong>ArcRelay</strong></p>
            <a href="https://example.com" target="_blank">Example</a>
            <img src="https://example.com/tracker.png" onerror="steal()">
            <script>steal()</script><iframe src="https://example.com"></iframe>"#,
    )
    .unwrap();

    assert!(preview.contains("<h2>Title</h2>"));
    assert!(preview.contains("<strong>ArcRelay</strong>"));
    assert!(preview.contains("<a rel=\"noopener noreferrer\">Example</a>"));
    assert!(!preview.contains("onclick"));
    assert!(!preview.contains("style="));
    assert!(!preview.contains("href="));
    assert!(!preview.contains("target="));
    assert!(!preview.contains("<img"));
    assert!(!preview.contains("<script"));
    assert!(!preview.contains("<iframe"));
    assert!(!preview.contains("steal()"));
}

#[test]
fn safe_html_preview_rejects_oversized_sources() {
    assert!(sanitize_html_preview(&"x".repeat(MAX_SAFE_HTML_SOURCE_BYTES + 1)).is_none());
}

#[test]
fn delayed_handoff_and_own_writes_do_not_become_local_copies() {
    let mut state = ClipboardCaptureState::default();
    let now = Instant::now();
    let written = clipboard_fingerprints(&ClipboardPayload::RichText {
        html: "<b>Hello</b>".into(),
        plain_text: "Hello\r\nworld".into(),
        rtf: None,
    });
    state.remember_write(written, now);
    let echoed = clipboard_fingerprints(&ClipboardPayload::Text("Hello\nworld".into()));
    let later = now + Duration::from_secs(120);
    assert!(!state.accepts(&echoed, NativeClipboardOrigin::Handoff, true, later));
    assert!(!state.accepts(&echoed, NativeClipboardOrigin::ArcRelay, true, later));
    // An application copy of the same content must work even inside the old TTL.
    assert!(state.accepts(&echoed, NativeClipboardOrigin::Local, true, later));
    let new = clipboard_fingerprints(&ClipboardPayload::Text("From an iPhone".into()));
    assert!(state.accepts(&new, NativeClipboardOrigin::Handoff, true, later));
    state.current = Some(new.clone());
    assert!(!state.accepts(&new, NativeClipboardOrigin::Handoff, true, later));
    // After an intervening different copy, the previous content is valid again.
    assert!(state.accepts(&echoed, NativeClipboardOrigin::Handoff, true, later));
}

#[test]
fn platforms_without_markers_keep_bounded_concurrent_write_suppression() {
    let mut state = ClipboardCaptureState::default();
    let now = Instant::now();
    let first = clipboard_fingerprints(&ClipboardPayload::Text("first".into()));
    let second = clipboard_fingerprints(&ClipboardPayload::Text("second".into()));
    state.remember_write(first.clone(), now);
    state.remember_write(second.clone(), now);
    assert!(!state.accepts(&first, NativeClipboardOrigin::Local, false, now));
    assert!(!state.accepts(&second, NativeClipboardOrigin::Local, false, now));
    assert!(state.accepts(&first, NativeClipboardOrigin::Local, true, now));
    assert!(state.accepts(
        &first,
        NativeClipboardOrigin::Local,
        false,
        now + SUPPRESSION_TTL + Duration::from_millis(1)
    ));
    for _ in 0..100 {
        state.remember_write(first.clone(), now);
    }
    assert_eq!(state.writes.len(), MAX_PENDING_SUPPRESSIONS);
}

#[test]
fn local_source_device_is_not_presented_as_remote() {
    let mut summary = summarize(&ClipboardPayload::Text("local".into()), None);
    summary.source_device_id = Some("local-device".into());
    summary.source_device_name = Some("My Mac".into());
    hide_local_source_device(&mut summary, "local-device");
    assert_eq!(summary.source_device_name, None);

    summary.source_device_id = Some("remote-device".into());
    summary.source_device_name = Some("Other Mac".into());
    hide_local_source_device(&mut summary, "local-device");
    assert_eq!(summary.source_device_name.as_deref(), Some("Other Mac"));
}

#[test]
fn copied_image_files_are_recognized_case_insensitively() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("photo.PnG");
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0, 255]).unwrap();
    }
    std::fs::write(&path, bytes).unwrap();
    assert!(matches!(
        image_payload_from_file(path.to_str().unwrap()).unwrap(),
        Some(ClipboardPayload::Image {
            width: 1,
            height: 1,
            ..
        })
    ));
}

#[test]
fn invalid_image_file_remains_a_file_instead_of_aborting_capture() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("not-an-image.png");
    std::fs::write(&path, b"invalid png").unwrap();
    assert!(image_payload_from_file(path.to_str().unwrap())
        .unwrap()
        .is_none());
}

#[test]
fn rtf_only_capture_gets_a_safe_html_fallback() {
    assert_eq!(plain_text_html("A < B & C"), "<pre>A &lt; B &amp; C</pre>");
}

#[tokio::test]
async fn live_capture_events_follow_successful_commits_only() {
    let store = SqliteClipboardStore::connect(None).await.unwrap();
    let (changes, _) = tokio::sync::broadcast::channel(8);
    let (captures, mut events) = tokio::sync::broadcast::channel(8);
    let (ocr, _) = mpsc::channel();
    for (text, live, capture_origin, expected) in [
        ("startup", false, Some(ClipboardCaptureOrigin::Local), None),
        (
            "native copy",
            true,
            Some(ClipboardCaptureOrigin::Local),
            Some(ClipboardCaptureOrigin::Local),
        ),
        (
            "Handoff arrival",
            true,
            Some(ClipboardCaptureOrigin::Remote),
            Some(ClipboardCaptureOrigin::Remote),
        ),
        ("panel copy", true, None, None),
    ] {
        let payload = ClipboardPayload::Text(text.into());
        let (reply, response) = oneshot::channel();
        handle_database_command(
            &store,
            &changes,
            &captures,
            &ocr,
            DbCommand::Store {
                content_hash: content_hash(&payload),
                summary: summarize(&payload, None),
                payload,
                touch_existing: true,
                source_device_id: "local".into(),
                source_device_name: "Local".into(),
                live,
                capture_origin,
                response: Some(reply),
            },
        )
        .await;
        assert!(response.await.unwrap().unwrap().is_some());
        if let Some(expected) = expected {
            assert_eq!(events.try_recv().unwrap().origin, expected);
        }
        assert!(events.try_recv().is_err());
    }
    let mut policy = store.policy().await.unwrap();
    policy.history_enabled = false;
    store.update_policy(policy).await.unwrap();
    let payload = ClipboardPayload::Text("not saved".into());
    let (reply, response) = oneshot::channel();
    handle_database_command(
        &store,
        &changes,
        &captures,
        &ocr,
        DbCommand::Store {
            content_hash: content_hash(&payload),
            summary: summarize(&payload, None),
            payload,
            touch_existing: true,
            source_device_id: "local".into(),
            source_device_name: "Local".into(),
            live: true,
            capture_origin: Some(ClipboardCaptureOrigin::Local),
            response: Some(reply),
        },
    )
    .await;
    assert!(response.await.unwrap().unwrap().is_none());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn remote_capture_events_exclude_history_and_duplicate_sync() {
    let source = SqliteClipboardStore::connect(None).await.unwrap();
    let target = SqliteClipboardStore::connect(None).await.unwrap();
    let payload = ClipboardPayload::Text("remote copy".into());
    let record = source
        .store(
            payload.clone(),
            content_hash(&payload),
            summarize(&payload, None),
            true,
            "remote",
            "Remote",
            true,
        )
        .await
        .unwrap()
        .unwrap();
    let (changes, _) = tokio::sync::broadcast::channel(8);
    let (captures, mut events) = tokio::sync::broadcast::channel(8);
    let (ocr, _) = mpsc::channel();
    let mut history = record.clone();
    history.live = false;
    history.change_kind = ClipboardSyncChangeKind::Snapshot;
    let (reply, response) = oneshot::channel();
    handle_database_command(
        &target,
        &changes,
        &captures,
        &ocr,
        DbCommand::ApplySync(history, reply),
    )
    .await;
    assert!(response.await.unwrap().unwrap());
    assert!(events.try_recv().is_err());
    let mut fresh = record;
    fresh.revision += 1;
    let (reply, response) = oneshot::channel();
    handle_database_command(
        &target,
        &changes,
        &captures,
        &ocr,
        DbCommand::ApplySync(fresh.clone(), reply),
    )
    .await;
    assert!(response.await.unwrap().unwrap());
    assert_eq!(
        events.try_recv().unwrap().origin,
        ClipboardCaptureOrigin::Remote
    );
    let (reply, response) = oneshot::channel();
    handle_database_command(
        &target,
        &changes,
        &captures,
        &ocr,
        DbCommand::ApplySync(fresh, reply),
    )
    .await;
    assert!(!response.await.unwrap().unwrap());
    assert!(events.try_recv().is_err());
}

#[test]
fn handoff_image_fingerprint_ignores_png_encoding_but_preserves_pixels() {
    fn png_payload(note: &str, pixel: [u8; 4]) -> ClipboardPayload {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_text_chunk("Comment".into(), note.into())
                .unwrap();
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&pixel)
                .unwrap();
        }
        ClipboardPayload::Image {
            png: bytes,
            width: 1,
            height: 1,
        }
    }
    let original = clipboard_fingerprints(&png_payload("original", [255, 0, 0, 255]));
    let reencoded = clipboard_fingerprints(&png_payload("Handoff", [255, 0, 0, 255]));
    let different = clipboard_fingerprints(&png_payload("Handoff", [0, 255, 0, 255]));
    assert_ne!(original.content_hash, reencoded.content_hash);
    assert_eq!(original.semantic_hash, reencoded.semantic_hash);
    let mut state = ClipboardCaptureState {
        current: Some(original),
        ..Default::default()
    };
    assert!(!state.accepts(
        &reencoded,
        NativeClipboardOrigin::Handoff,
        true,
        Instant::now()
    ));
    assert!(state.accepts(
        &different,
        NativeClipboardOrigin::Handoff,
        true,
        Instant::now()
    ));
}

#[tokio::test]
async fn relayed_copy_identity_survives_three_devices_and_reconnect() {
    let source = SqliteClipboardStore::connect(None).await.unwrap();
    let middle = SqliteClipboardStore::connect(None).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("third.sqlite3");
    let third = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
    let payload = ClipboardPayload::Text("same explicitly copied text".into());
    let first = source
        .store(
            payload.clone(),
            content_hash(&payload),
            summarize(&payload, None),
            true,
            "source",
            "Source",
            true,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(middle.apply_sync_record(first.clone()).await.unwrap());
    assert!(third.apply_sync_record(first.clone()).await.unwrap());
    // Direct and relayed delivery keep the original tuple; a round trip is inert.
    assert!(!source.apply_sync_record(first.clone()).await.unwrap());
    assert!(!middle.apply_sync_record(first.clone()).await.unwrap());
    assert!(!third.apply_sync_record(first.clone()).await.unwrap());
    drop(third);
    let third = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
    assert!(!third.apply_sync_record(first.clone()).await.unwrap());
    let mut snapshot = first.clone();
    snapshot.live = false;
    snapshot.change_kind = ClipboardSyncChangeKind::Snapshot;
    assert!(!third.apply_sync_record(snapshot).await.unwrap());
    assert_eq!(
        third.current_summary().await.unwrap().unwrap().copy_count,
        1
    );
    // A real second copy retains its content identity but advances event version.
    let second = source
        .store(
            payload.clone(),
            content_hash(&payload),
            summarize(&payload, None),
            true,
            "source",
            "Source",
            true,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.sync_id, first.sync_id);
    assert!(second.revision > first.revision);
    assert!(middle.apply_sync_record(second.clone()).await.unwrap());
    assert!(third.apply_sync_record(second.clone()).await.unwrap());
    assert!(!third.apply_sync_record(second).await.unwrap());
    assert_eq!(
        third.current_summary().await.unwrap().unwrap().copy_count,
        2
    );
}
