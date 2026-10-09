use super::*;
use sea_orm_migration::prelude::{MigrationName, MigrationTrait, SchemaManager};

pub(super) struct ApplicationPinsMigration;
impl MigrationName for ApplicationPinsMigration {
    fn name(&self) -> &str {
        "m20261009_clipboard_application_pins"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for ApplicationPinsMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE clipboard_app_pins (
                app_id TEXT NOT NULL,
                entry_id INTEGER NOT NULL REFERENCES clipboard_entries(id) ON DELETE CASCADE,
                pinned_at_ms INTEGER NOT NULL,
                PRIMARY KEY (app_id, entry_id)
             );
             CREATE INDEX idx_clipboard_app_pin_entry ON clipboard_app_pins(entry_id);
             CREATE TRIGGER clipboard_app_pin_deleted AFTER UPDATE OF deleted ON clipboard_entries
             WHEN new.deleted=1 BEGIN
                DELETE FROM clipboard_app_pins WHERE entry_id=new.id;
             END;",
            )
            .await
            .map(|_| ())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TRIGGER clipboard_app_pin_deleted; DROP TABLE clipboard_app_pins;",
            )
            .await
            .map(|_| ())
    }
}

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn app_pins(
        &self,
        app_id: &str,
        query: ClipboardQuery,
    ) -> Result<Vec<ClipboardSummary>, DbErr> {
        let db = self.db.begin().await?;
        let models = super::records::filtered_summary_query(&query)?
            .filter(Expr::cust_with_values(
                "id IN (SELECT entry_id FROM clipboard_app_pins WHERE app_id=?)",
                [app_id],
            ))
            // Oldest pin first: re-copying or pasting never changes this order.
            .order_by(
                Expr::cust_with_values(
                    "(SELECT pinned_at_ms FROM clipboard_app_pins WHERE entry_id=id AND app_id=?)",
                    [app_id],
                ),
                Order::Asc,
            )
            .order_by_asc(clipboard_entry::Column::Id)
            .all(&db)
            .await?;
        let ids = models
            .iter()
            .map(|model| model.sync_id.clone())
            .collect::<Vec<_>>();
        let mut labels = self.active_labels_for_sync_ids_on(&db, &ids).await?;
        let entries = models
            .into_iter()
            .map(|model| {
                let labels = labels.remove(&model.sync_id).unwrap_or_default();
                model_to_summary(model, labels)
            })
            .collect::<Result<Vec<_>, _>>()?;
        db.commit().await?;
        Ok(entries)
    }

    pub(in crate::infrastructure) async fn set_app_pin(
        &self,
        id: u64,
        app_id: &str,
        pinned: bool,
    ) -> Result<(), DbErr> {
        if app_id.trim().is_empty() || app_id.len() > 1024 || id > i64::MAX as u64 {
            return Err(DbErr::Custom("invalid clipboard application pin".into()));
        }
        let db = self.db.begin().await?;
        if pinned
            && !clipboard_entry::Entity::find_by_id(id as i64)
                .filter(clipboard_entry::Column::Deleted.eq(false))
                .one(&db)
                .await?
                .is_some()
        {
            return Err(DbErr::Custom("clipboard record not found".into()));
        }
        let statement =
            if pinned {
                Statement::from_sql_and_values(sea_orm::DbBackend::Sqlite,
                "INSERT INTO clipboard_app_pins(app_id, entry_id, pinned_at_ms) VALUES (?, ?, ?)
                 ON CONFLICT(app_id, entry_id) DO NOTHING",
                vec![app_id.into(), (id as i64).into(), Utc::now().timestamp_millis().into()])
            } else {
                Statement::from_sql_and_values(
                    sea_orm::DbBackend::Sqlite,
                    "DELETE FROM clipboard_app_pins WHERE app_id=? AND entry_id=?",
                    vec![app_id.into(), (id as i64).into()],
                )
            };
        if db.execute(statement).await?.rows_affected() > 0 {
            bump_revision(&db).await?;
        }
        db.commit().await
    }
}
