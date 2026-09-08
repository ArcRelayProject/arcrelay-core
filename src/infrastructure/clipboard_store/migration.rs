use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(LegacyClipboardSchemaV4),
            Box::new(RecreateClipboardSchemaV5),
            Box::new(CreateClipboardOcrIndexV6),
            Box::new(AddClipboardListMetadataV7),
            Box::new(AddClipboardOcrCharacterAlignmentV8),
            Box::new(super::search_index::SearchAndAccounting),
            Box::new(super::replica::ReplicaStateMigration),
            Box::new(super::replica::RepairLegacyTimelineMigration),
            Box::new(super::retention::SeparateRetentionAccounting),
        ]
    }
}

struct AddClipboardOcrCharacterAlignmentV8;

impl MigrationName for AddClipboardOcrCharacterAlignmentV8 {
    fn name(&self) -> &str {
        "m20260827_add_clipboard_ocr_character_alignment_v8"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddClipboardOcrCharacterAlignmentV8 {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ClipboardOcrBlock::Table)
                    .add_column(ColumnDef::new(ClipboardOcrBlock::CharactersJson).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ClipboardOcrBlock::Table)
                    .drop_column(ClipboardOcrBlock::CharactersJson)
                    .to_owned(),
            )
            .await
    }
}

struct AddClipboardListMetadataV7;

impl MigrationName for AddClipboardListMetadataV7 {
    fn name(&self) -> &str {
        "m20260826_add_clipboard_list_metadata_v7"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddClipboardListMetadataV7 {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(ClipboardEntry::Table)
                    .add_column(
                        ColumnDef::new(ClipboardEntry::UpdatedAtMs)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ClipboardEntry::Table)
                    .add_column(ColumnDef::new(ClipboardEntry::CharacterCount).big_integer())
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE clipboard_entries \
                 SET updated_at_ms = MAX(captured_at_ms, COALESCE(last_used_at_ms, 0)), \
                     character_count = CASE kind \
                         WHEN 1 THEN (SELECT LENGTH(text_payload) FROM clipboard_payloads WHERE entry_id = clipboard_entries.id) \
                         WHEN 2 THEN (SELECT LENGTH(plain_text_payload) FROM clipboard_payloads WHERE entry_id = clipboard_entries.id) \
                         ELSE NULL END",
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-clipboard-updated-list")
                    .table(ClipboardEntry::Table)
                    .col(ClipboardEntry::Deleted)
                    .col(ClipboardEntry::UpdatedAtMs)
                    .col(ClipboardEntry::Id)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-clipboard-created-list")
                    .table(ClipboardEntry::Table)
                    .col(ClipboardEntry::Deleted)
                    .col(ClipboardEntry::FirstCapturedAtMs)
                    .col(ClipboardEntry::Id)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx-clipboard-created-list")
                    .table(ClipboardEntry::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx-clipboard-updated-list")
                    .table(ClipboardEntry::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ClipboardEntry::Table)
                    .drop_column(ClipboardEntry::CharacterCount)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(ClipboardEntry::Table)
                    .drop_column(ClipboardEntry::UpdatedAtMs)
                    .to_owned(),
            )
            .await
    }
}

struct CreateClipboardOcrIndexV6;

impl MigrationName for CreateClipboardOcrIndexV6 {
    fn name(&self) -> &str {
        "m20260826_create_clipboard_ocr_index_v6"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateClipboardOcrIndexV6 {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ClipboardImageOcr::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ClipboardImageOcr::EntryId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ClipboardImageOcr::Status)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(ClipboardImageOcr::FullText)
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(ColumnDef::new(ClipboardImageOcr::Error).text())
                    .col(
                        ColumnDef::new(ClipboardImageOcr::ModelVersion)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardImageOcr::UpdatedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-clipboard-image-ocr-entry")
                            .from(ClipboardImageOcr::Table, ClipboardImageOcr::EntryId)
                            .to(ClipboardEntry::Table, ClipboardEntry::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(ClipboardOcrBlock::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ClipboardOcrBlock::EntryId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardOcrBlock::BlockIndex)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ClipboardOcrBlock::Text).text().not_null())
                    .col(
                        ColumnDef::new(ClipboardOcrBlock::Confidence)
                            .double()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ClipboardOcrBlock::Left).integer().not_null())
                    .col(ColumnDef::new(ClipboardOcrBlock::Top).integer().not_null())
                    .col(
                        ColumnDef::new(ClipboardOcrBlock::Width)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardOcrBlock::Height)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ClipboardOcrBlock::PointsJson).text())
                    .primary_key(
                        Index::create()
                            .col(ClipboardOcrBlock::EntryId)
                            .col(ClipboardOcrBlock::BlockIndex),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-clipboard-ocr-block-entry")
                            .from(ClipboardOcrBlock::Table, ClipboardOcrBlock::EntryId)
                            .to(ClipboardImageOcr::Table, ClipboardImageOcr::EntryId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardOcrBlock::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardImageOcr::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

struct LegacyClipboardSchemaV4;

impl MigrationName for LegacyClipboardSchemaV4 {
    fn name(&self) -> &str {
        // Previous releases derived the migration name from this module, so
        // every installed v4 database records the literal version `migration`.
        "migration"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for LegacyClipboardSchemaV4 {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

struct RecreateClipboardSchemaV5;

impl MigrationName for RecreateClipboardSchemaV5 {
    fn name(&self) -> &str {
        "m20260814_recreate_clipboard_schema_v5"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for RecreateClipboardSchemaV5 {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardEntryLabel::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardLabel::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardPayload::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardEntry::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardState::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ClipboardEntry::Table)
                    .col(
                        ColumnDef::new(ClipboardEntry::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ClipboardEntry::Kind).integer().not_null())
                    .col(ColumnDef::new(ClipboardEntry::TextSyntax).text().not_null())
                    .col(
                        ColumnDef::new(ClipboardEntry::ContentHash)
                            .string_len(32)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::SyncId)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::SourceDeviceId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::SourceDeviceName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::SyncRevision)
                            .big_integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::UpdatedByDeviceId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::Deleted)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(ClipboardEntry::Preview).text().not_null())
                    .col(ColumnDef::new(ClipboardEntry::SearchText).text().not_null())
                    .col(ColumnDef::new(ClipboardEntry::SourceApp).text())
                    .col(
                        ColumnDef::new(ClipboardEntry::FirstCapturedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::CapturedAtMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ClipboardEntry::LastUsedAtMs).big_integer())
                    .col(
                        ColumnDef::new(ClipboardEntry::CopyCount)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::SizeBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::StorageBytes)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::ItemCount)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(ColumnDef::new(ClipboardEntry::Width).integer())
                    .col(ColumnDef::new(ClipboardEntry::Height).integer())
                    .col(
                        ColumnDef::new(ClipboardEntry::Sensitive)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::Favorite)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::FavoriteRevision)
                            .big_integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::FavoriteUpdatedByDeviceId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntry::Available)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ClipboardPayload::Table)
                    .col(
                        ColumnDef::new(ClipboardPayload::EntryId)
                            .big_integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ClipboardPayload::TextPayload).text())
                    .col(ColumnDef::new(ClipboardPayload::HtmlPayload).text())
                    .col(ColumnDef::new(ClipboardPayload::PlainTextPayload).text())
                    .col(ColumnDef::new(ClipboardPayload::RtfPayload).text())
                    .col(ColumnDef::new(ClipboardPayload::ImagePng).binary())
                    .col(ColumnDef::new(ClipboardPayload::FilesJson).text())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-clipboard-payload-entry")
                            .from(ClipboardPayload::Table, ClipboardPayload::EntryId)
                            .to(ClipboardEntry::Table, ClipboardEntry::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-clipboard-content-hash")
                    .table(ClipboardEntry::Table)
                    .col(ClipboardEntry::ContentHash)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-clipboard-order")
                    .table(ClipboardEntry::Table)
                    .col(ClipboardEntry::CapturedAtMs)
                    .col(ClipboardEntry::Id)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ClipboardLabel::Table)
                    .col(
                        ColumnDef::new(ClipboardLabel::Id)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ClipboardLabel::Name).text().not_null())
                    .col(
                        ColumnDef::new(ClipboardLabel::NormalizedName)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ClipboardLabel::Color).text().not_null())
                    .col(
                        ColumnDef::new(ClipboardLabel::Revision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardLabel::UpdatedByDeviceId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardLabel::Deleted)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ClipboardEntryLabel::Table)
                    .col(
                        ColumnDef::new(ClipboardEntryLabel::EntrySyncId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntryLabel::LabelId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntryLabel::Attached)
                            .boolean()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntryLabel::Revision)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ClipboardEntryLabel::UpdatedByDeviceId)
                            .text()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(ClipboardEntryLabel::EntrySyncId)
                            .col(ClipboardEntryLabel::LabelId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-clipboard-entry-label-entry")
                            .from(ClipboardEntryLabel::Table, ClipboardEntryLabel::EntrySyncId)
                            .to(ClipboardEntry::Table, ClipboardEntry::SyncId)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-clipboard-entry-label-label")
                            .from(ClipboardEntryLabel::Table, ClipboardEntryLabel::LabelId)
                            .to(ClipboardLabel::Table, ClipboardLabel::Id),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx-clipboard-kind-time")
                    .table(ClipboardEntry::Table)
                    .col(ClipboardEntry::Kind)
                    .col(ClipboardEntry::CapturedAtMs)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ClipboardState::Table)
                    .col(
                        ColumnDef::new(ClipboardState::Id)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ClipboardState::Revision)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(ClipboardState::HistoryEnabled)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ClipboardState::MaxItems)
                            .integer()
                            .not_null()
                            .default(500),
                    )
                    .col(
                        ColumnDef::new(ClipboardState::MaxBytes)
                            .big_integer()
                            .not_null()
                            .default(104857600),
                    )
                    .col(
                        ColumnDef::new(ClipboardState::RetentionDays)
                            .integer()
                            .not_null()
                            .default(30),
                    )
                    .col(
                        ColumnDef::new(ClipboardState::SaveSensitive)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardEntryLabel::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardLabel::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardPayload::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardEntry::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ClipboardState::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ClipboardEntry {
    #[sea_orm(iden = "clipboard_entries")]
    Table,
    Id,
    Kind,
    TextSyntax,
    ContentHash,
    SyncId,
    SourceDeviceId,
    SourceDeviceName,
    SyncRevision,
    UpdatedByDeviceId,
    Deleted,
    Preview,
    SearchText,
    SourceApp,
    FirstCapturedAtMs,
    CapturedAtMs,
    LastUsedAtMs,
    UpdatedAtMs,
    CopyCount,
    SizeBytes,
    CharacterCount,
    StorageBytes,
    ItemCount,
    Width,
    Height,
    Sensitive,
    Favorite,
    FavoriteRevision,
    FavoriteUpdatedByDeviceId,
    Available,
}

#[derive(DeriveIden)]
enum ClipboardLabel {
    #[sea_orm(iden = "clipboard_labels")]
    Table,
    Id,
    Name,
    NormalizedName,
    Color,
    Revision,
    UpdatedByDeviceId,
    Deleted,
}

#[derive(DeriveIden)]
enum ClipboardEntryLabel {
    #[sea_orm(iden = "clipboard_entry_labels")]
    Table,
    EntrySyncId,
    LabelId,
    Attached,
    Revision,
    UpdatedByDeviceId,
}

#[derive(DeriveIden)]
enum ClipboardPayload {
    #[sea_orm(iden = "clipboard_payloads")]
    Table,
    EntryId,
    TextPayload,
    HtmlPayload,
    PlainTextPayload,
    RtfPayload,
    ImagePng,
    FilesJson,
}

#[derive(DeriveIden)]
enum ClipboardState {
    #[sea_orm(iden = "clipboard_state")]
    Table,
    Id,
    Revision,
    HistoryEnabled,
    MaxItems,
    MaxBytes,
    RetentionDays,
    SaveSensitive,
}

#[derive(DeriveIden)]
enum ClipboardImageOcr {
    #[sea_orm(iden = "clipboard_image_ocr")]
    Table,
    EntryId,
    Status,
    FullText,
    Error,
    ModelVersion,
    UpdatedAtMs,
}

#[derive(DeriveIden)]
enum ClipboardOcrBlock {
    #[sea_orm(iden = "clipboard_ocr_blocks")]
    Table,
    EntryId,
    BlockIndex,
    Text,
    Confidence,
    Left,
    Top,
    Width,
    Height,
    PointsJson,
    CharactersJson,
}
