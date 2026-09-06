use sea_orm::entity::prelude::*;

pub mod clipboard_entry {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
    #[sea_orm(table_name = "clipboard_entries")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub kind: i32,
        #[sea_orm(column_type = "Text")]
        pub text_syntax: String,
        pub content_hash: String,
        #[sea_orm(unique)]
        pub sync_id: String,
        #[sea_orm(column_type = "Text")]
        pub source_device_id: String,
        #[sea_orm(column_type = "Text")]
        pub source_device_name: String,
        pub sync_revision: i64,
        #[sea_orm(column_type = "Text")]
        pub updated_by_device_id: String,
        pub deleted: bool,
        #[sea_orm(column_type = "Text")]
        pub preview: String,
        #[sea_orm(column_type = "Text")]
        pub search_text: String,
        #[sea_orm(column_type = "Text", nullable)]
        pub source_app: Option<String>,
        pub first_captured_at_ms: i64,
        pub captured_at_ms: i64,
        pub last_used_at_ms: Option<i64>,
        pub updated_at_ms: i64,
        pub copy_count: i32,
        pub size_bytes: i64,
        pub character_count: Option<i64>,
        pub storage_bytes: i64,
        pub item_count: i32,
        pub width: Option<i32>,
        pub height: Option<i32>,
        pub sensitive: bool,
        pub favorite: bool,
        pub favorite_revision: i64,
        #[sea_orm(column_type = "Text")]
        pub favorite_updated_by_device_id: String,
        pub available: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod clipboard_label {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
    #[sea_orm(table_name = "clipboard_labels")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
        pub id: String,
        #[sea_orm(column_type = "Text")]
        pub name: String,
        #[sea_orm(column_type = "Text")]
        pub normalized_name: String,
        #[sea_orm(column_type = "Text")]
        pub color: String,
        pub revision: i64,
        #[sea_orm(column_type = "Text")]
        pub updated_by_device_id: String,
        pub deleted: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod clipboard_entry_label {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
    #[sea_orm(table_name = "clipboard_entry_labels")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
        pub entry_sync_id: String,
        #[sea_orm(primary_key, auto_increment = false, column_type = "Text")]
        pub label_id: String,
        pub attached: bool,
        pub revision: i64,
        #[sea_orm(column_type = "Text")]
        pub updated_by_device_id: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod clipboard_payload {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
    #[sea_orm(table_name = "clipboard_payloads")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub entry_id: i64,
        #[sea_orm(column_type = "Text", nullable)]
        pub text_payload: Option<String>,
        #[sea_orm(column_type = "Text", nullable)]
        pub html_payload: Option<String>,
        #[sea_orm(column_type = "Text", nullable)]
        pub plain_text_payload: Option<String>,
        #[sea_orm(column_type = "Text", nullable)]
        pub rtf_payload: Option<String>,
        pub image_png: Option<Vec<u8>>,
        #[sea_orm(column_type = "Text", nullable)]
        pub files_json: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod clipboard_state {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
    #[sea_orm(table_name = "clipboard_state")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i32,
        pub revision: i64,
        pub history_enabled: bool,
        pub max_items: i32,
        pub max_bytes: i64,
        pub retention_days: i32,
        pub save_sensitive: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod clipboard_image_ocr {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "clipboard_image_ocr")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub entry_id: i64,
        pub status: i32,
        #[sea_orm(column_type = "Text")]
        pub full_text: String,
        #[sea_orm(column_type = "Text", nullable)]
        pub error: Option<String>,
        #[sea_orm(column_type = "Text")]
        pub model_version: String,
        pub updated_at_ms: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod clipboard_ocr_block {
    use super::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "clipboard_ocr_blocks")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub entry_id: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub block_index: i32,
        #[sea_orm(column_type = "Text")]
        pub text: String,
        pub confidence: f64,
        pub left: i32,
        pub top: i32,
        pub width: i32,
        pub height: i32,
        #[sea_orm(column_type = "Text", nullable)]
        pub points_json: Option<String>,
        #[sea_orm(column_type = "Text", nullable)]
        pub characters_json: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
