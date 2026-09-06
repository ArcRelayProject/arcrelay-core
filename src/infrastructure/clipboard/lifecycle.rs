use super::*;

/// The repository owns native workers. Workers receive signals, never a strong
/// reference back to this owner; constructor failure also tears them down.
pub(super) struct ClipboardWorkers {
    pub(super) stopping: tokio::sync::watch::Sender<bool>,
    ocr: mpsc::Sender<Option<OcrJob>>,
    pub(super) watcher: Mutex<Option<clipboard_rs::WatcherShutdown>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}
impl ClipboardWorkers {
    pub(super) fn new(ocr: mpsc::Sender<Option<OcrJob>>) -> Self {
        Self {
            stopping: tokio::sync::watch::channel(false).0,
            ocr,
            watcher: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
        }
    }
    pub(super) fn track(&self, thread: std::thread::JoinHandle<()>) {
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(thread);
    }
    fn stop(&self) {
        self.stopping.send_replace(true);
        let _ = self.ocr.send(None);
        if let Some(watcher) = self
            .watcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            watcher.stop();
        }
    }
    pub(super) async fn shutdown(&self) {
        self.stop();
        let threads = std::mem::take(
            &mut *self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let _ = tokio::task::spawn_blocking(move || {
            for thread in threads {
                let _ = thread.join();
            }
        })
        .await;
    }
}
impl Drop for ClipboardWorkers {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_owner_stops_without_a_tokio_context() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let (tx, rx) = mpsc::channel();
        let workers = ClipboardWorkers::new(tx.clone());
        let stopped = workers.stopping.subscribe();
        start_database_worker(
            None,
            tokio::sync::broadcast::channel(1).0,
            tokio::sync::broadcast::channel(1).0,
            tx,
            &workers,
        )
        .unwrap();
        workers.stop();
        for thread in workers.threads.lock().unwrap().drain(..) {
            thread.join().unwrap();
        }
        assert!(*stopped.borrow());
        assert!(rx.recv().unwrap().is_none());
    }
}
