use super::*;
use sea_orm_migration::prelude::{MigrationName, MigrationTrait, SchemaManager};

/// Shared history and device-local file paths need independent soft budgets.
/// Otherwise identical policies can permanently reject the other peer's oldest
/// shared entry solely because this device has more local file copies.
pub(super) struct SeparateRetentionAccounting;

impl MigrationName for SeparateRetentionAccounting {
    fn name(&self) -> &str {
        "m20260909_separate_clipboard_retention_scopes_v12"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for SeparateRetentionAccounting {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
            CREATE TABLE clipboard_retention_statistics (
                scope INTEGER PRIMARY KEY CHECK(scope IN (1, 2)),
                item_count INTEGER NOT NULL,
                total_bytes INTEGER NOT NULL
            );
            INSERT INTO clipboard_retention_statistics
                SELECT 1, COUNT(*), COALESCE(SUM(storage_bytes), 0)
                FROM clipboard_entries WHERE deleted=0 AND kind<>4;
            INSERT INTO clipboard_retention_statistics
                SELECT 2, COUNT(*), COALESCE(SUM(storage_bytes), 0)
                FROM clipboard_entries WHERE deleted=0 AND kind=4;
            CREATE TRIGGER clipboard_retention_insert AFTER INSERT ON clipboard_entries WHEN new.deleted=0 BEGIN
                UPDATE clipboard_retention_statistics
                SET item_count=item_count+1, total_bytes=total_bytes+new.storage_bytes
                WHERE scope=CASE WHEN new.kind=4 THEN 2 ELSE 1 END;
            END;
            CREATE TRIGGER clipboard_retention_delete AFTER DELETE ON clipboard_entries WHEN old.deleted=0 BEGIN
                UPDATE clipboard_retention_statistics
                SET item_count=item_count-1, total_bytes=total_bytes-old.storage_bytes
                WHERE scope=CASE WHEN old.kind=4 THEN 2 ELSE 1 END;
            END;
            CREATE TRIGGER clipboard_retention_update AFTER UPDATE OF deleted, storage_bytes, kind ON clipboard_entries
            WHEN old.deleted<>new.deleted OR old.storage_bytes<>new.storage_bytes OR old.kind<>new.kind BEGIN
                UPDATE clipboard_retention_statistics
                SET item_count=item_count-(old.deleted=0),
                    total_bytes=total_bytes-CASE WHEN old.deleted=0 THEN old.storage_bytes ELSE 0 END
                WHERE scope=CASE WHEN old.kind=4 THEN 2 ELSE 1 END;
                UPDATE clipboard_retention_statistics
                SET item_count=item_count+(new.deleted=0),
                    total_bytes=total_bytes+CASE WHEN new.deleted=0 THEN new.storage_bytes ELSE 0 END
                WHERE scope=CASE WHEN new.kind=4 THEN 2 ELSE 1 END;
            END;
            UPDATE clipboard_state SET revision=revision+1;
        "#).await.map(|_| ())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TRIGGER clipboard_retention_insert;
             DROP TRIGGER clipboard_retention_delete;
             DROP TRIGGER clipboard_retention_update;
             DROP TABLE clipboard_retention_statistics;",
            )
            .await
            .map(|_| ())
    }
}
