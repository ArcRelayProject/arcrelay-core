use super::*;
use crate::domain::collections::*;
use sea_orm_migration::prelude::{MigrationName, MigrationTrait, SchemaManager};

pub(super) struct CollectionsMigration;
impl MigrationName for CollectionsMigration {
    fn name(&self) -> &str {
        "m20261014_clipboard_collections"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for CollectionsMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared("CREATE TABLE clipboard_collections (id TEXT PRIMARY KEY NOT NULL, document TEXT NOT NULL)").await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE clipboard_collections")
            .await?;
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> DbErr {
    DbErr::Custom(message.into())
}
async fn get<C: ConnectionTrait>(db: &C, id: &str) -> Result<Option<ClipboardCollection>, DbErr> {
    db.query_one(Statement::from_sql_and_values(
        sea_orm::DbBackend::Sqlite,
        "SELECT document FROM clipboard_collections WHERE id = ?",
        [id.into()],
    ))
    .await?
    .map(|row| {
        let value: String = row.try_get("", "document")?;
        serde_json::from_str(&value).map_err(|_| invalid("Invalid stored collection"))
    })
    .transpose()
}
async fn put<C: ConnectionTrait>(db: &C, value: &ClipboardCollection) -> Result<(), DbErr> {
    value.validate().map_err(invalid)?;
    let document = serde_json::to_string(value).map_err(|_| invalid("Cannot encode collection"))?;
    db.execute(Statement::from_sql_and_values(sea_orm::DbBackend::Sqlite,
        "INSERT INTO clipboard_collections (id, document) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET document = excluded.document",
        [value.id.clone().into(), document.into()])).await?;
    bump_revision(db).await
}

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn collections(
        &self,
        after: &str,
    ) -> Result<Vec<ClipboardCollection>, DbErr> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                sea_orm::DbBackend::Sqlite,
                "SELECT document FROM clipboard_collections WHERE id > ? ORDER BY id LIMIT 100",
                [after.into()],
            ))
            .await?;
        let mut result = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let document: String = row.try_get("", "document")?;
            if bytes + document.len() > 512 * 1024 {
                break;
            }
            bytes += document.len();
            result.push(
                serde_json::from_str(&document)
                    .map_err(|_| invalid("Invalid stored collection"))?,
            );
        }
        Ok(result)
    }
    pub(in crate::infrastructure) async fn collection(
        &self,
        id: &str,
    ) -> Result<Option<ClipboardCollection>, DbErr> {
        get(&self.db, id).await
    }

    pub(in crate::infrastructure) async fn save_collection(
        &self,
        draft: CollectionDraft,
        device: &str,
    ) -> Result<ClipboardCollection, DbErr> {
        let txn = self.db.begin().await?;
        let old = match draft.id.as_deref() {
            Some(id) => get(&txn, id).await?,
            None => None,
        };
        if draft.id.is_some()
            && (old.as_ref().is_none_or(|c| c.deleted)
                || old.as_ref().map(ClipboardCollection::version) != draft.base)
        {
            return Err(invalid(
                "COLLECTION_STALE: reload the collection or save as a new collection",
            ));
        }
        let mut items = draft.items;
        for item in &mut items {
            if let Some(totp) = item.totp.take() {
                item.totp = Some(normalize_totp(totp).map_err(invalid)?);
                item.value.clear();
            }
        }
        let value = ClipboardCollection {
            id: old
                .as_ref()
                .map(|c| c.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            name: draft.name.trim().to_owned(),
            items,
            revision: old.as_ref().map_or(1, |c| c.revision.saturating_add(1)),
            updated_by_device_id: device.to_owned(),
            favorite: old.as_ref().is_some_and(|c| c.favorite),
            favorite_revision: old.as_ref().map_or(1, |c| c.favorite_revision),
            favorite_updated_by_device_id: old.as_ref().map_or_else(
                || device.to_owned(),
                |c| c.favorite_updated_by_device_id.clone(),
            ),
            deleted: false,
        };
        put(&txn, &value).await?;
        txn.commit().await?;
        Ok(value)
    }

    pub(in crate::infrastructure) async fn change_collection(
        &self,
        id: &str,
        base: Option<CollectionVersion>,
        favorite: Option<bool>,
        device: &str,
    ) -> Result<ClipboardCollection, DbErr> {
        let txn = self.db.begin().await?;
        let mut value = get(&txn, id)
            .await?
            .filter(|c| !c.deleted)
            .ok_or_else(|| invalid("Collection not found"))?;
        if let Some(favorite) = favorite {
            value.favorite = favorite;
            value.favorite_revision = value.favorite_revision.saturating_add(1);
            value.favorite_updated_by_device_id = device.to_owned();
        } else {
            if base != Some(value.version()) {
                return Err(invalid("COLLECTION_STALE: reload before deleting"));
            }
            value.deleted = true;
            value.items.clear();
            value.revision = value.revision.saturating_add(1);
            value.updated_by_device_id = device.to_owned();
        }
        put(&txn, &value).await?;
        txn.commit().await?;
        Ok(value)
    }

    pub(in crate::infrastructure) async fn apply_collection(
        &self,
        incoming: ClipboardCollection,
        edits: bool,
        favorites: bool,
    ) -> Result<bool, DbErr> {
        incoming.validate().map_err(invalid)?;
        let txn = self.db.begin().await?;
        let old = get(&txn, &incoming.id).await?;
        if incoming.deleted && old.is_none() && !edits {
            return Ok(false);
        }
        let merged = merge_collection(old.as_ref(), &incoming, edits, favorites);
        if old.as_ref() == Some(&merged) {
            return Ok(false);
        }
        put(&txn, &merged).await?;
        txn.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn draft() -> CollectionDraft {
        CollectionDraft {
            id: None,
            base: None,
            name: "Address".into(),
            items: vec![CollectionItem {
                id: uuid::Uuid::new_v4().to_string(),
                label: "Postal code".into(),
                value: "00123".into(),
                masked: false,
                totp: None,
            }],
        }
    }
    #[tokio::test]
    async fn collection_is_durable_and_independent_of_history_and_selection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clipboard.sqlite");
        let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        let saved = store.save_collection(draft(), "a").await.unwrap();
        store.clear().await.unwrap();
        assert!(store.current_summary().await.unwrap().is_none());
        assert!(store.replica_selection().await.unwrap().is_none());
        drop(store);
        let store = SqliteClipboardStore::connect(Some(&path)).await.unwrap();
        assert_eq!(
            store.collection(&saved.id).await.unwrap().unwrap().items[0].value,
            "00123"
        );
    }
    #[tokio::test]
    async fn stale_edits_and_deletes_cannot_overwrite_remote_updates() {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        let saved = store.save_collection(draft(), "a").await.unwrap();
        let mut remote = saved.clone();
        remote.revision += 1;
        remote.updated_by_device_id = "b".into();
        remote.items[0].value = "remote".into();
        assert!(store
            .apply_collection(remote.clone(), true, true)
            .await
            .unwrap());
        let mut stale = draft();
        stale.id = Some(saved.id.clone());
        stale.base = Some(saved.version());
        assert!(store.save_collection(stale, "a").await.is_err());
        assert!(store
            .change_collection(&saved.id, Some(saved.version()), None, "a")
            .await
            .is_err());
        assert!(!store.apply_collection(saved, true, true).await.unwrap());
        assert!(!store.apply_collection(remote, true, true).await.unwrap());
    }
    #[tokio::test]
    async fn sync_preferences_preserve_content_and_merge_favorites_independently() {
        let store = SqliteClipboardStore::connect(None).await.unwrap();
        let saved = store.save_collection(draft(), "a").await.unwrap();
        let mut remote = saved.clone();
        remote.revision += 1;
        remote.items[0].value = "changed".into();
        remote.favorite = true;
        remote.favorite_revision += 1;
        store
            .apply_collection(remote.clone(), false, true)
            .await
            .unwrap();
        let value = store.collection(&saved.id).await.unwrap().unwrap();
        assert_eq!(value.items[0].value, "00123");
        assert!(value.favorite);
        store
            .apply_collection(remote.clone(), true, false)
            .await
            .unwrap();
        assert_eq!(
            store.collection(&saved.id).await.unwrap().unwrap().items[0].value,
            "changed"
        );
        let deleted = store
            .change_collection(&saved.id, Some(remote.version()), None, "a")
            .await
            .unwrap();
        assert!(deleted.items.is_empty());
        assert!(!store.apply_collection(remote, true, true).await.unwrap());
    }
}
