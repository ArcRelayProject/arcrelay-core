use super::*;

pub(super) fn run_clipboard_capture_worker(worker: ClipboardCaptureWorker) {
    let ClipboardCaptureWorker {
        context,
        changed,
        db,
        capture_state,
        source_provider,
        source_device_id,
        source_device_name,
        sync_tx,
    } = worker;
    while changed.recv().is_ok() {
        std::thread::sleep(Duration::from_millis(20));
        while changed.try_recv().is_ok() {}
        let result = (|| -> Result<()> {
            let mut state = lock_capture_state(&capture_state)?;
            #[cfg(target_os = "macos")]
            let stamp = macos::stamp();
            #[cfg(target_os = "macos")]
            if stamp.origin == NativeClipboardOrigin::ArcRelay {
                return Ok(());
            }
            #[cfg(not(target_os = "macos"))]
            let origin = external_clipboard_origin(&context);
            let Some(payload) = capture_payload(&context)? else {
                // Empty/temporarily unavailable Handoff representations do not
                // erase the last committed content and reopen the echo path.
                return Ok(());
            };
            #[cfg(target_os = "macos")]
            if stamp != macos::stamp() {
                // A writer or lazy Handoff materialization changed the board
                // during capture. The watcher will deliver the newer change.
                return Ok(());
            }
            #[cfg(target_os = "macos")]
            let origin = stamp.origin;
            #[cfg(not(target_os = "macos"))]
            if origin != external_clipboard_origin(&context) {
                return Ok(());
            }
            let source_app = source_application_for_origin(origin, source_provider.as_ref());
            let fingerprints = clipboard_fingerprints(&payload);
            if !state.accepts(
                &fingerprints,
                origin,
                cfg!(target_os = "macos"),
                Instant::now(),
            ) {
                return Ok(());
            }
            let summary = summarize(&payload, source_app);
            let (response_tx, response_rx) = oneshot::channel();
            db.blocking_send(DbCommand::Store {
                payload,
                content_hash: fingerprints.content_hash.clone(),
                summary,
                touch_existing: true,
                source_device_id: source_device_id.clone(),
                source_device_name: source_device_name.clone(),
                live: true,
                capture_origin: Some(match origin {
                    NativeClipboardOrigin::Handoff | NativeClipboardOrigin::RustDesk => {
                        ClipboardCaptureOrigin::Remote
                    }
                    _ => ClipboardCaptureOrigin::Local,
                }),
                response: Some(response_tx),
            })?;
            if let Some(record) = response_rx
                .blocking_recv()
                .map_err(|_| Error::Clipboard("clipboard database response was dropped".into()))??
            {
                state.current = Some(fingerprints);
                state.selection = Some(ClipboardSelection {
                    key: ClipboardSelectionKey::from_record(&record),
                    applied: true,
                });
                let _ = sync_tx.send(record);
            }
            Ok(())
        })();
        if let Err(error) = result {
            warn!(%error, "failed to capture clipboard change");
        }
    }
}

pub(super) fn start_database_worker(
    database_path: Option<PathBuf>,
    change_tx: tokio::sync::broadcast::Sender<()>,
    capture_tx: tokio::sync::broadcast::Sender<ClipboardCaptureEvent>,
    ocr_job_tx: mpsc::Sender<Option<OcrJob>>,
    workers: &ClipboardWorkers,
) -> Result<DbClient> {
    let (tx, mut rx) = tokio_mpsc::channel::<DbCommand>(64);
    let (reads, mut read_rx) = tokio_mpsc::channel::<DbCommand>(64);
    let mut stopping = workers.stopping.subscribe();
    let thread = std::thread::Builder::new()
        .name("arcrelay-clipboard-database".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    warn!(%error, "failed to create clipboard database runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                let store = match SqliteClipboardStore::connect(database_path.as_deref()).await {
                    Ok(store) => store,
                    Err(error) => {
                        warn!(%error, "failed to open clipboard database");
                        return;
                    }
                };
                match store.backfill_image_ocr_jobs(OCR_MODEL_VERSION).await {
                    Ok(jobs) => {
                        for id in jobs {
                            let _ = ocr_job_tx.send(Some(OcrJob { id, urgent: false }));
                        }
                    }
                    Err(error) => warn!(%error, "failed to schedule clipboard OCR backfill"),
                }
                let reader = match store.reader(database_path.as_deref()).await {
                    Ok(reader) => Arc::new(reader),
                    Err(error) => { warn!(%error, "failed to open clipboard query connection"); return; }
                };
                let read_changes = change_tx.clone();
                let read_captures = capture_tx.clone();
                let read_ocr = ocr_job_tx.clone();
                let read_task = tokio::spawn(async move {
                    let mut queries = tokio::task::JoinSet::new();
                    loop {
                        if queries.len() == 2 { let _ = queries.join_next().await; }
                        tokio::select! {
                            Some(_) = queries.join_next(), if !queries.is_empty() => {},
                            command = read_rx.recv() => {
                                let Some(command) = command else { break; };
                                let reader = reader.clone();
                                let change_tx = read_changes.clone();
                                let capture_tx = read_captures.clone();
                                let ocr_job_tx = read_ocr.clone();
                                queries.spawn(async move { handle_database_command(&reader, &change_tx, &capture_tx, &ocr_job_tx, command).await; });
                            }
                        }
                    }
                    while queries.join_next().await.is_some() {}
                });
                loop {
                    if *stopping.borrow() { rx.close(); }
                    let command = if store.take_maintenance() {
                        match rx.try_recv() {
                            Ok(command) => {
                                store.maintenance_pending.store(true, std::sync::atomic::Ordering::Release);
                                Some(command)
                            }
                            Err(tokio_mpsc::error::TryRecvError::Empty) => {
                                match store.maintain().await {
                                    Ok(true) => { let _ = change_tx.send(()); }
                                    Ok(false) => {},
                                    Err(error) => warn!(%error, "clipboard maintenance failed"),
                                }
                                tokio::task::yield_now().await;
                                continue;
                            }
                            Err(tokio_mpsc::error::TryRecvError::Disconnected) => None,
                        }
                    } else { tokio::select! { biased;
                        _ = stopping.changed(), if !*stopping.borrow() => { rx.close(); rx.recv().await },
                        command = rx.recv() => command,
                    } };
                    let Some(command) = command else { break; };
                    handle_database_command(&store, &change_tx, &capture_tx, &ocr_job_tx, command).await;
                }
                read_task.abort();
                let _ = read_task.await;
            });
        })
        .map_err(|error| Error::Clipboard(format!("start database worker: {error}")))?;
    workers.track(thread);
    Ok(DbClient { tx, reads })
}

pub(super) async fn handle_database_command(
    store: &SqliteClipboardStore,
    change_tx: &tokio::sync::broadcast::Sender<()>,
    capture_tx: &tokio::sync::broadcast::Sender<ClipboardCaptureEvent>,
    ocr_job_tx: &mpsc::Sender<Option<OcrJob>>,
    command: DbCommand,
) {
    match command {
        DbCommand::Revision(response) => {
            let _ = response.send(store.revision().await.map_err(db_error));
        }
        DbCommand::CurrentSummary(response) => {
            let _ = response.send(store.current_summary().await.map_err(db_error));
        }
        DbCommand::History(query, response) => {
            let _ = response.send(store.history(query).await.map_err(db_error));
        }
        DbCommand::Timeline(query, response) => {
            let _ = response.send(store.timeline(query).await.map_err(db_error));
        }
        DbCommand::ImagePng(id, response) => {
            let _ = response.send(store.image_png(id).await.map_err(db_error));
        }
        DbCommand::ImageOcr(id, response) => {
            let _ = response.send(store.image_ocr(id).await.map_err(db_error));
        }
        DbCommand::ImageOcrState(id, response) => {
            let _ = response.send(store.image_ocr_state(id).await.map_err(db_error));
        }
        DbCommand::PendingImageOcrSource(id, response) => {
            let _ = response.send(store.pending_image_ocr_source(id).await.map_err(db_error));
        }
        DbCommand::StoreImageOcr(id, result, response) => {
            let result = store.save_image_ocr(id, result).await.map_err(db_error);
            let _ = response.send(result);
        }
        DbCommand::StoreImageOcrFailure(id, error, model_version, response) => {
            let result = store
                .save_image_ocr_failure(id, &error, &model_version)
                .await
                .map_err(db_error);
            let _ = response.send(result);
        }
        DbCommand::FilePaths(id, response) => {
            let _ = response.send(store.file_paths(id).await.map_err(db_error));
        }
        DbCommand::TextContent(id, response) => {
            let _ = response.send(store.text_content(id).await.map_err(db_error));
        }
        DbCommand::HtmlPayload(id, response) => {
            let _ = response.send(store.html_payload(id).await.map_err(db_error));
        }
        DbCommand::Policy(response) => {
            let _ = response.send(store.policy().await.map_err(db_error));
        }
        DbCommand::Store {
            payload,
            content_hash,
            summary,
            touch_existing,
            source_device_id,
            source_device_name,
            live,
            capture_origin,
            response,
        } => {
            let result = store
                .store(
                    payload,
                    content_hash,
                    summary,
                    touch_existing,
                    &source_device_id,
                    &source_device_name,
                    live,
                )
                .await
                .map_err(db_error);
            if let Ok(Some(record)) = result.as_ref() {
                if record.kind == ClipboardContentKind::Image {
                    match store
                        .ensure_image_ocr_pending(&record.sync_id, OCR_MODEL_VERSION, true)
                        .await
                    {
                        Ok(Some(id)) => {
                            let _ = ocr_job_tx.send(Some(OcrJob { id, urgent: false }));
                        }
                        Ok(None) => {}
                        Err(error) => warn!(%error, "failed to schedule clipboard image OCR"),
                    }
                }
            }
            if matches!(result, Ok(Some(_))) {
                if let Some(origin) = capture_origin.filter(|_| live) {
                    let _ = capture_tx.send(ClipboardCaptureEvent {
                        origin,
                        occurred_at: Instant::now(),
                    });
                }
                let _ = change_tx.send(());
            }
            if let Some(response) = response {
                let _ = response.send(result);
            } else if let Err(error) = result {
                warn!(%error, "failed to store clipboard history");
            }
        }
        DbCommand::Payload(id, response) => {
            let result = store.payload(id).await.map_err(db_error);
            if result.is_ok() {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::UpdatePolicy(policy, response) => {
            let result = store.update_policy(policy).await.map_err(db_error);
            if result.is_ok() {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::Delete(id, updated_by_device_id, response) => {
            let result = store
                .delete(id, &updated_by_device_id)
                .await
                .map_err(db_error);
            if matches!(result, Ok(Some(_))) {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::SetFavorite(id, favorite, updated_by_device_id, response) => {
            let result = store
                .set_favorite(id, favorite, &updated_by_device_id)
                .await
                .map_err(db_error);
            if matches!(result, Ok(Some(_))) {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::Labels(response) => {
            let _ = response.send(store.labels().await.map_err(db_error));
        }
        DbCommand::CreateLabel(name, color, updated_by_device_id, response) => {
            let result = store
                .create_label(&name, &color, &updated_by_device_id)
                .await
                .map_err(db_error);
            if result.is_ok() {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::UpdateLabel(label_id, name, color, updated_by_device_id, response) => {
            let result = store
                .update_label(&label_id, &name, &color, &updated_by_device_id)
                .await
                .map_err(db_error);
            if result.is_ok() {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::DeleteLabel(label_id, updated_by_device_id, response) => {
            let result = store
                .delete_label(&label_id, &updated_by_device_id)
                .await
                .map_err(db_error);
            if result.is_ok() {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::SetLabels(id, label_ids, updated_by_device_id, response) => {
            let result = store
                .set_labels(id, label_ids, &updated_by_device_id)
                .await
                .map_err(db_error);
            if result.is_ok() {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::SetLabelMembership(id, label_id, attached, updated_by_device_id, response) => {
            let result = store
                .set_label_membership(id, &label_id, attached, &updated_by_device_id)
                .await
                .map_err(db_error);
            if result.is_ok() {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::Clear(response) => {
            let result = store.clear().await.map_err(db_error);
            if result.is_ok() {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::SyncRecords(after, limit, response) => {
            let _ = response.send(store.sync_records(after, limit).await.map_err(db_error));
        }
        DbCommand::SyncRecordRequiresPayload(sync_id, revision, updated_by_device_id, response) => {
            let _ = response.send(
                store
                    .sync_record_requires_payload(&sync_id, revision, &updated_by_device_id)
                    .await
                    .map_err(db_error),
            );
        }
        DbCommand::ReplicaPage(cursor, limit, response) => {
            let _ = response.send(store.replica_page(cursor, limit).await.map_err(db_error));
        }
        DbCommand::ReplicaRecord(sync_id, response) => {
            let _ = response.send(store.replica_record(&sync_id).await.map_err(db_error));
        }
        DbCommand::CheckReplicaStorage(replica, response) => {
            let _ = response.send(
                store
                    .check_replica_storage(&replica)
                    .await
                    .map_err(db_error),
            );
        }
        DbCommand::ReplicaSelection(response) => {
            let _ = response.send(store.replica_selection().await.map_err(db_error));
        }
        DbCommand::ReplicaLabels(response) => {
            let _ = response.send(store.replica_labels().await.map_err(db_error));
        }
        DbCommand::ApplyReplicaLabels(labels, response) => {
            let result = store.apply_replica_labels(labels).await.map_err(db_error);
            if matches!(result, Ok(count) if count > 0) {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
        DbCommand::ApplyReplica(replica, response) => {
            let image = (replica.record.kind == ClipboardContentKind::Image
                && !replica.record.deleted)
                .then(|| replica.record.sync_id.clone());
            let result = store.apply_replica_record(replica).await.map_err(db_error);
            if matches!(result, Ok(true)) {
                let _ = change_tx.send(());
                if let Some(sync_id) = image {
                    if let Ok(Some(id)) = store
                        .ensure_image_ocr_pending(&sync_id, OCR_MODEL_VERSION, true)
                        .await
                    {
                        let _ = ocr_job_tx.send(Some(OcrJob { id, urgent: false }));
                    }
                }
            }
            let _ = response.send(result);
        }
        DbCommand::ApplySync(record, response) => {
            let live_copy = record.live
                && !record.deleted
                && record.change_kind == ClipboardSyncChangeKind::Copy;
            let image_sync_id = (record.kind == ClipboardContentKind::Image && !record.deleted)
                .then(|| record.sync_id.clone());
            let result = store.apply_sync_record(record).await.map_err(db_error);
            if matches!(result, Ok(true)) {
                if live_copy {
                    let _ = capture_tx.send(ClipboardCaptureEvent {
                        origin: ClipboardCaptureOrigin::Remote,
                        occurred_at: Instant::now(),
                    });
                }
                let _ = change_tx.send(());
                if let Some(sync_id) = image_sync_id {
                    match store
                        .ensure_image_ocr_pending(&sync_id, OCR_MODEL_VERSION, true)
                        .await
                    {
                        Ok(Some(id)) => {
                            let _ = ocr_job_tx.send(Some(OcrJob { id, urgent: false }));
                        }
                        Ok(None) => {}
                        Err(error) => {
                            warn!(%error, "failed to schedule synchronized image OCR")
                        }
                    }
                }
            }
            let _ = response.send(result);
        }
        DbCommand::EditText(id, content, updated_by_device_id, response) => {
            let result = store
                .edit_text(id, &content, &updated_by_device_id)
                .await
                .map_err(db_error);
            if matches!(result, Ok(Some(_))) {
                let _ = change_tx.send(());
            }
            let _ = response.send(result);
        }
    }
}

pub(super) fn db_error(error: sea_orm::DbErr) -> Error {
    Error::Clipboard(error.to_string())
}

pub(super) fn blocking_database_request<T>(
    db: &DbClient,
    build: impl FnOnce(DbResponse<T>) -> DbCommand,
) -> Result<T> {
    let (response_tx, response_rx) = oneshot::channel();
    db.blocking_send(build(response_tx))?;
    response_rx
        .blocking_recv()
        .map_err(|_| Error::Clipboard("clipboard database response was dropped".into()))?
}

pub(super) fn start_ocr_worker(
    ocr: Arc<ClipboardOcr>,
    jobs: mpsc::Receiver<Option<OcrJob>>,
    db: DbClient,
    events: tokio::sync::broadcast::Sender<u64>,
    workers: &ClipboardWorkers,
) -> Result<()> {
    std::thread::Builder::new()
        .name("arcrelay-clipboard-ocr".into())
        .spawn(move || {
            let mut urgent_jobs = VecDeque::new();
            let mut background_jobs = VecDeque::new();
            let mut queued = HashSet::new();
            loop {
                while let Ok(job) = jobs.try_recv() {
                    let Some(job) = job else {
                        return;
                    };
                    enqueue_ocr_job(job, &mut urgent_jobs, &mut background_jobs, &mut queued);
                }
                let Some(id) = urgent_jobs
                    .pop_front()
                    .or_else(|| background_jobs.pop_front())
                else {
                    match jobs.recv_timeout(OCR_ENGINE_IDLE_TIMEOUT) {
                        Ok(None) => return,
                        Ok(Some(job)) => {
                            enqueue_ocr_job(
                                job,
                                &mut urgent_jobs,
                                &mut background_jobs,
                                &mut queued,
                            );
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if ocr.release_if_idle(OCR_ENGINE_IDLE_TIMEOUT) {
                                debug!("released idle clipboard OCR engine");
                            }
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            let _ = ocr.release_if_idle(Duration::ZERO);
                            break;
                        }
                    }
                };
                queued.remove(&id);
                let source = match blocking_database_request(&db, |response| {
                    DbCommand::PendingImageOcrSource(id, response)
                }) {
                    Ok(Some(source)) => source,
                    Ok(None) => continue,
                    Err(error) => {
                        warn!(%error, "failed to load clipboard image for OCR");
                        continue;
                    }
                };
                let stored = match ocr.recognize_png(&source) {
                    Ok(result) => blocking_database_request(&db, |response| {
                        DbCommand::StoreImageOcr(id, result, response)
                    }),
                    Err(error) => {
                        let message = error.to_string();
                        blocking_database_request(&db, |response| {
                            DbCommand::StoreImageOcrFailure(
                                id,
                                message,
                                OCR_MODEL_VERSION.to_string(),
                                response,
                            )
                        })
                    }
                };
                match stored {
                    Ok(()) => {
                        let _ = events.send(id);
                    }
                    Err(error) => warn!(%error, "failed to persist clipboard OCR result"),
                }
            }
        })
        .map(|thread| workers.track(thread))
        .map_err(|error| Error::Clipboard(format!("start clipboard OCR worker: {error}")))
}

pub(super) fn enqueue_ocr_job(
    job: OcrJob,
    urgent_jobs: &mut VecDeque<u64>,
    background_jobs: &mut VecDeque<u64>,
    queued: &mut HashSet<u64>,
) {
    if queued.insert(job.id) {
        if job.urgent {
            urgent_jobs.push_back(job.id);
        } else {
            background_jobs.push_back(job.id);
        }
    } else if job.urgent {
        if let Some(index) = background_jobs.iter().position(|id| *id == job.id) {
            background_jobs.remove(index);
            urgent_jobs.push_back(job.id);
        }
    }
}

pub(super) fn start_watcher(
    db: DbClient,
    capture_state: SharedCaptureState,
    source_provider: Arc<dyn WindowManagerRepository>,
    source_device_id: String,
    source_device_name: String,
    sync_tx: tokio::sync::broadcast::Sender<ClipboardSyncRecord>,
    workers: &ClipboardWorkers,
) -> bool {
    let (changed_tx, changed_rx) = mpsc::sync_channel(1);
    let (capture_ready_tx, capture_ready_rx) = mpsc::sync_channel(1);
    let capture_worker = std::thread::Builder::new()
        .name("arcrelay-clipboard-capture".into())
        .spawn(move || {
            let context = match ClipboardContext::new() {
                Ok(context) => context,
                Err(error) => {
                    warn!(%error, "failed to initialize clipboard capture context");
                    let _ = capture_ready_tx.send(false);
                    return;
                }
            };
            let _ = capture_ready_tx.send(true);
            run_clipboard_capture_worker(ClipboardCaptureWorker {
                context,
                changed: changed_rx,
                db,
                capture_state,
                source_provider,
                source_device_id,
                source_device_name,
                sync_tx,
            });
        });
    match capture_worker {
        Ok(thread) => workers.track(thread),
        Err(error) => {
            warn!(%error, "failed to start clipboard capture thread");
            return false;
        }
    }
    match capture_ready_rx.recv_timeout(Duration::from_millis(500)) {
        Ok(true) => {}
        Ok(false) => return false,
        Err(error) => {
            warn!(%error, "clipboard capture worker readiness timed out");
            return false;
        }
    }

    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("arcrelay-clipboard-watcher".into())
        .spawn(move || {
            let mut watcher = match ClipboardWatcherContext::new() {
                Ok(watcher) => watcher,
                Err(error) => {
                    warn!(%error, "failed to initialize clipboard watcher");
                    let _ = ready_tx.send(None);
                    return;
                }
            };
            watcher.add_handler(HostClipboardHandler {
                changed: changed_tx,
            });
            if ready_tx.send(Some(watcher.get_shutdown_channel())).is_err() {
                return;
            }
            watcher.start_watch();
            warn!("native clipboard watcher stopped");
        });
    match worker {
        Ok(thread) => workers.track(thread),
        Err(error) => {
            warn!(%error, "failed to start clipboard watcher thread");
            return false;
        }
    }
    match ready_rx.recv_timeout(Duration::from_millis(500)) {
        Ok(Some(watcher)) => {
            *workers
                .watcher
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(watcher);
            true
        }
        Ok(None) => false,
        Err(error) => {
            warn!(%error, "clipboard watcher readiness timed out");
            false
        }
    }
}
