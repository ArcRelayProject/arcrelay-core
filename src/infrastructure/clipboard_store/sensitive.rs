use super::*;
use sea_orm_migration::prelude::{MigrationName, MigrationTrait, SchemaManager};

pub(super) struct SensitiveRulesMigration;
impl MigrationName for SensitiveRulesMigration {
    fn name(&self) -> &str {
        "m20261113_clipboard_sensitive_rules"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for SensitiveRulesMigration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(
            "ALTER TABLE clipboard_state ADD COLUMN sensitive_rules_json TEXT NOT NULL DEFAULT '{}'"
        ).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE clipboard_state DROP COLUMN sensitive_rules_json")
            .await?;
        Ok(())
    }
}

pub(super) fn settings_from_state(
    state: &clipboard_state::Model,
) -> Result<ClipboardSensitiveSettings, DbErr> {
    let mut settings: ClipboardSensitiveSettings =
        serde_json::from_str(&state.sensitive_rules_json)
            .map_err(|e| DbErr::Custom(format!("decode clipboard sensitive rules: {e}")))?;
    // The existing policy column remains authoritative for protocol clients.
    settings.save_sensitive = state.save_sensitive;
    Ok(settings)
}

pub(super) fn classify(
    summary: &mut ClipboardSummary,
    stored: &StoredValues,
    settings: &ClipboardSensitiveSettings,
) {
    summary.sensitive = stored
        .text_payload
        .as_deref()
        .or(stored.plain_text_payload.as_deref())
        .is_some_and(|text| settings.matching_rule(text).is_some());
    if summary.sensitive {
        summary.preview = "••••••••".into();
    }
}

impl SqliteClipboardStore {
    pub(in crate::infrastructure) async fn sensitive_settings(
        &self,
    ) -> Result<ClipboardSensitiveSettings, DbErr> {
        settings_from_state(&self.state().await?)
    }

    pub(in crate::infrastructure) async fn update_sensitive_settings(
        &self,
        mut settings: ClipboardSensitiveSettings,
    ) -> Result<(), DbErr> {
        settings.validate().map_err(|e| DbErr::Custom(e.into()))?;
        for keyword in &mut settings.custom_keywords {
            *keyword = keyword.trim().to_owned();
        }
        let json = serde_json::to_string(&settings).map_err(|e| DbErr::Custom(e.to_string()))?;
        // Update only these columns, leaving concurrent retention/history edits intact.
        self.db.execute(Statement::from_sql_and_values(
            sea_orm::DbBackend::Sqlite,
            "UPDATE clipboard_state SET sensitive_rules_json=?, save_sensitive=?, revision=revision+1 WHERE id=1",
            [json.into(), settings.save_sensitive.into()],
        )).await?;
        Ok(())
    }
}
