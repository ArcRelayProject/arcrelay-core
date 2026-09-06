use super::*;
use sea_orm_migration::prelude::{MigrationName, MigrationTrait, SchemaManager};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct SearchAndAccounting;
impl MigrationName for SearchAndAccounting {
    fn name(&self) -> &str {
        "m20260905_clipboard_search_accounting_v9"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for SearchAndAccounting {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(r#"
            CREATE TABLE clipboard_statistics (id INTEGER PRIMARY KEY CHECK(id = 1), item_count INTEGER NOT NULL, total_bytes INTEGER NOT NULL);
            INSERT INTO clipboard_statistics SELECT 1, COUNT(*), COALESCE(SUM(storage_bytes), 0) FROM clipboard_entries WHERE deleted = 0;
            CREATE TRIGGER clipboard_statistics_insert AFTER INSERT ON clipboard_entries WHEN new.deleted = 0 BEGIN
                UPDATE clipboard_statistics SET item_count = item_count + 1, total_bytes = total_bytes + new.storage_bytes WHERE id = 1;
            END;
            CREATE TRIGGER clipboard_statistics_delete AFTER DELETE ON clipboard_entries WHEN old.deleted = 0 BEGIN
                UPDATE clipboard_statistics SET item_count = item_count - 1, total_bytes = total_bytes - old.storage_bytes WHERE id = 1;
            END;
            CREATE TRIGGER clipboard_statistics_update AFTER UPDATE OF deleted, storage_bytes ON clipboard_entries
            WHEN old.deleted != new.deleted OR old.storage_bytes != new.storage_bytes BEGIN
                UPDATE clipboard_statistics SET
                  item_count = item_count + (new.deleted = 0) - (old.deleted = 0),
                  total_bytes = total_bytes + CASE WHEN new.deleted = 0 THEN new.storage_bytes ELSE 0 END - CASE WHEN old.deleted = 0 THEN old.storage_bytes ELSE 0 END
                WHERE id = 1;
            END;
            CREATE VIEW clipboard_search_documents AS
              SELECT e.id, e.search_text || char(10) || COALESCE(o.full_text, '') AS text
              FROM clipboard_entries e LEFT JOIN clipboard_image_ocr o ON o.entry_id = e.id AND o.status = 1
              WHERE e.deleted = 0 AND e.sensitive = 0;
            CREATE VIRTUAL TABLE clipboard_search USING fts5(text, content='clipboard_search_documents', content_rowid='id', tokenize='trigram');
            INSERT INTO clipboard_search(clipboard_search) VALUES('rebuild');
        "#).await?;
        for (table, key, columns, condition) in [
            ("clipboard_entries", "id", "search_text, sensitive, deleted", "old.search_text != new.search_text OR old.sensitive != new.sensitive OR old.deleted != new.deleted"),
            ("clipboard_image_ocr", "entry_id", "full_text, status", "old.full_text IS NOT new.full_text OR old.status != new.status"),
        ] {
            let mut triggers = vec![
                ("insert", "AFTER", "INSERT".to_string(), "new", false),
                ("delete", "BEFORE", "DELETE".to_string(), "old", true),
                ("before_update", "BEFORE", format!("UPDATE OF {columns}"), "old", true),
                ("after_update", "AFTER", format!("UPDATE OF {columns}"), "new", false),
            ];
            if table == "clipboard_image_ocr" {
                // An OCR row changes an already indexed entry, even on insertion
                // or removal. Remove old tokens and reinsert the current document.
                triggers.push(("before_insert", "BEFORE", "INSERT".into(), "new", true));
                triggers.push(("after_delete", "AFTER", "DELETE".into(), "old", false));
            }
            for (suffix, timing, event, row, delete) in triggers {
                let when = if suffix == "before_insert" { "WHEN NOT EXISTS (SELECT 1 FROM clipboard_image_ocr WHERE entry_id = new.entry_id)".into() } else if suffix.ends_with("update") { format!("WHEN {condition}") } else { String::new() };
                let statement = if delete {
                    format!("INSERT INTO clipboard_search(clipboard_search, rowid, text) SELECT 'delete', id, text FROM clipboard_search_documents WHERE id = {row}.{key};")
                } else {
                    format!("INSERT INTO clipboard_search(rowid, text) SELECT id, text FROM clipboard_search_documents WHERE id = {row}.{key};")
                };
                db.execute_unprepared(&format!("CREATE TRIGGER {table}_search_{suffix} {timing} {event} ON {table} {when} BEGIN {statement} END;")).await?;
            }
        }
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for table in ["clipboard_entries", "clipboard_image_ocr"] {
            for suffix in ["insert", "delete", "before_update", "after_update"] {
                db.execute_unprepared(&format!("DROP TRIGGER {table}_search_{suffix}"))
                    .await?;
            }
        }
        db.execute_unprepared("DROP TRIGGER clipboard_image_ocr_search_before_insert; DROP TRIGGER clipboard_image_ocr_search_after_delete;").await?;
        db.execute_unprepared("DROP TABLE clipboard_search; DROP VIEW clipboard_search_documents; DROP TRIGGER clipboard_statistics_insert; DROP TRIGGER clipboard_statistics_delete; DROP TRIGGER clipboard_statistics_update; DROP TABLE clipboard_statistics;").await?;
        Ok(())
    }
}

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn reader(
        &self,
        path: Option<&Path>,
    ) -> Result<Self, DbErr> {
        let db = if let Some(path) = path {
            let mut options = ConnectOptions::new(format!("sqlite://{}?mode=ro", path.display()));
            options
                .max_connections(2)
                .min_connections(1)
                .sqlx_logging(false);
            Database::connect(options).await?
        } else {
            self.db.clone()
        };
        Ok(Self {
            db,
            maintenance_pending: AtomicBool::new(false),
        })
    }

    pub(in crate::infrastructure) async fn maintain(&self) -> Result<bool, DbErr> {
        let policy = self.policy().await?;
        let transaction = self.db.begin().await?;
        let before = Self::item_count(&transaction).await?;
        self.prune(&transaction, &policy).await?;
        let changed = Self::item_count(&transaction).await? != before;
        if changed {
            bump_revision(&transaction).await?;
        }
        transaction.commit().await?;
        Ok(changed)
    }

    pub(super) async fn item_count<C: ConnectionTrait>(db: &C) -> Result<u64, DbErr> {
        let value = db
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "SELECT item_count FROM clipboard_statistics WHERE id = 1".to_string(),
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("clipboard accounting is missing".into()))?;
        Ok(value.try_get::<i64>("", "item_count")?.max(0) as u64)
    }

    pub(in crate::infrastructure) fn take_maintenance(&self) -> bool {
        self.maintenance_pending.swap(false, Ordering::AcqRel)
    }
}
