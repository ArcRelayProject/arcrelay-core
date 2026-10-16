//! Local application associations; these never enter collection replica documents.
use super::*;
use sea_orm_migration::prelude::{MigrationName, MigrationTrait, SchemaManager};

pub(super) struct CollectionPinsMigration;
impl MigrationName for CollectionPinsMigration {
    fn name(&self) -> &str {
        "m20261015_clipboard_collection_app_pins"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for CollectionPinsMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "CREATE TABLE clipboard_collection_app_pins (
                app_id TEXT NOT NULL,
                collection_id TEXT NOT NULL REFERENCES clipboard_collections(id) ON DELETE CASCADE,
                pinned_at_ms INTEGER NOT NULL,
                PRIMARY KEY(app_id, collection_id)
            );
            CREATE INDEX idx_collection_pin_collection ON clipboard_collection_app_pins(collection_id);"
        ).await.map(|_| ())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE clipboard_collection_app_pins")
            .await
            .map(|_| ())
    }
}

fn validate_app(app_id: &str) -> Result<(), DbErr> {
    if app_id.trim().is_empty() || app_id.len() > 1024 || app_id.chars().any(char::is_control) {
        return Err(DbErr::Custom("invalid clipboard application".into()));
    }
    Ok(())
}

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn collection_app_pins(
        &self,
        app_id: &str,
    ) -> Result<Vec<String>, DbErr> {
        validate_app(app_id)?;
        let rows = self.db.query_all(Statement::from_sql_and_values(
            sea_orm::DbBackend::Sqlite,
            "SELECT collection_id FROM clipboard_collection_app_pins WHERE app_id=? ORDER BY pinned_at_ms, collection_id",
            [app_id.into()],
        )).await?;
        rows.into_iter()
            .map(|row| row.try_get("", "collection_id"))
            .collect()
    }

    pub(in crate::infrastructure) async fn set_collection_app_pin(
        &self,
        id: &str,
        app_id: &str,
        pinned: bool,
    ) -> Result<(), DbErr> {
        validate_app(app_id)?;
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(DbErr::Custom("invalid collection id".into()));
        }
        let db = self.db.begin().await?;
        if pinned
            && super::collections::get(&db, id)
                .await?
                .is_none_or(|c| c.deleted)
        {
            return Err(DbErr::Custom("Collection not found".into()));
        }
        let statement = if pinned {
            Statement::from_sql_and_values(sea_orm::DbBackend::Sqlite,
                "INSERT INTO clipboard_collection_app_pins(app_id, collection_id, pinned_at_ms) VALUES(?,?,?) ON CONFLICT(app_id,collection_id) DO NOTHING",
                vec![app_id.into(), id.into(), Utc::now().timestamp_millis().into()])
        } else {
            Statement::from_sql_and_values(
                sea_orm::DbBackend::Sqlite,
                "DELETE FROM clipboard_collection_app_pins WHERE app_id=? AND collection_id=?",
                vec![app_id.into(), id.into()],
            )
        };
        if db.execute(statement).await?.rows_affected() > 0 {
            bump_revision(&db).await?;
        }
        db.commit().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::collections::{ClipboardCollection, CollectionDraft};
    async fn collection(store: &SqliteClipboardStore) -> ClipboardCollection {
        store
            .save_collection(
                CollectionDraft {
                    id: None,
                    base: None,
                    name: "Work".into(),
                    items: vec![],
                },
                "local",
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pins_are_durable_app_scoped_idempotent_and_do_not_change_replicated_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboard.sqlite");
        let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        let saved = collection(&store).await;
        store
            .set_collection_app_pin(&saved.id, "macos:safari", true)
            .await
            .unwrap();
        store
            .set_collection_app_pin(&saved.id, "macos:editor", true)
            .await
            .unwrap();
        let revision = store.revision().await.unwrap();
        store
            .set_collection_app_pin(&saved.id, "macos:safari", true)
            .await
            .unwrap();
        assert_eq!(store.revision().await.unwrap(), revision);
        assert!(store.collection(&saved.id).await.unwrap().unwrap() == saved);
        assert!(store
            .collection_app_pins("macos:other")
            .await
            .unwrap()
            .is_empty());
        store
            .set_collection_app_pin(&saved.id, "macos:editor", false)
            .await
            .unwrap();
        drop(store);
        let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        assert_eq!(
            store.collection_app_pins("macos:safari").await.unwrap(),
            vec![saved.id]
        );
        assert!(store
            .collection_app_pins("macos:editor")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn local_and_remote_deletion_clear_pins_and_deleted_or_missing_records_cannot_be_pinned()
    {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        for remote in [false, true] {
            let saved = collection(&store).await;
            store
                .set_collection_app_pin(&saved.id, "macos:safari", true)
                .await
                .unwrap();
            if remote {
                let mut deleted = saved.clone();
                deleted.deleted = true;
                deleted.items.clear();
                deleted.revision += 1;
                store.apply_collection(deleted, true, true).await.unwrap();
            } else {
                store
                    .change_collection(&saved.id, Some(saved.version()), None, "local")
                    .await
                    .unwrap();
            }
            assert!(store
                .collection_app_pins("macos:safari")
                .await
                .unwrap()
                .is_empty());
            assert!(store
                .set_collection_app_pin(&saved.id, "macos:safari", true)
                .await
                .is_err());
        }
        assert!(store
            .set_collection_app_pin(&uuid::Uuid::new_v4().to_string(), "macos:safari", true)
            .await
            .is_err());
        assert!(store.collection_app_pins(" ").await.is_err());
    }
}
