//! Persistent, ordinary clipboard collections. Masking is presentation only.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub use super::totp::{generate_totp, normalize_totp, Totp, TotpCode};
pub const MAX_COLLECTION_BYTES: usize = 256 * 1024;

#[derive(Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionItem {
    pub id: String,
    pub label: String,
    pub value: String,
    pub masked: bool,
    pub totp: Option<Totp>,
}

#[derive(Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClipboardCollection {
    pub id: String,
    pub name: String,
    pub items: Vec<CollectionItem>,
    pub revision: u64,
    pub updated_by_device_id: String,
    pub favorite: bool,
    pub favorite_revision: u64,
    pub favorite_updated_by_device_id: String,
    pub deleted: bool,
}

#[derive(Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollectionVersion {
    pub revision: u64,
    pub device_id: String,
}

#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDraft {
    pub id: Option<String>,
    pub base: Option<CollectionVersion>,
    pub name: String,
    pub items: Vec<CollectionItem>,
}

impl ClipboardCollection {
    pub fn version(&self) -> CollectionVersion {
        CollectionVersion {
            revision: self.revision,
            device_id: self.updated_by_device_id.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if uuid::Uuid::parse_str(&self.id).is_err()
            || self.revision == 0
            || self.revision >= 9_007_199_254_740_991
            || self.favorite_revision >= 9_007_199_254_740_991
            || !valid_author(&self.updated_by_device_id)
            || (self.favorite_revision > 0 && !valid_author(&self.favorite_updated_by_device_id))
            || self.name.trim().is_empty()
            || self.name.chars().count() > 120
            || self.name.chars().any(char::is_control)
            || self.items.len() > 64
        {
            return Err("Invalid collection metadata".into());
        }
        let mut ids = std::collections::HashSet::new();
        for item in &self.items {
            if uuid::Uuid::parse_str(&item.id).is_err()
                || !ids.insert(&item.id)
                || item.label.trim().is_empty()
                || item.label.chars().count() > 120
                || item.value.len() > 64 * 1024
            {
                return Err("Invalid collection item (maximum 64 KiB per value)".into());
            }
            if let Some(totp) = &item.totp {
                normalize_totp(totp.clone())?;
            }
        }
        if self.deleted && !self.items.is_empty() {
            return Err("Deleted collection contains items".into());
        }
        if serde_json::to_vec(self)
            .map_err(|_| "Cannot encode collection")?
            .len()
            > MAX_COLLECTION_BYTES
        {
            return Err("Collection exceeds 256 KiB".into());
        }
        Ok(())
    }

    pub fn item_value(&self, id: &str, now_ms: i64) -> Result<TotpCode, String> {
        if self.deleted {
            return Err("Collection was deleted".into());
        }
        let item = self
            .items
            .iter()
            .find(|item| item.id == id)
            .ok_or("Collection item not found")?;
        match &item.totp {
            Some(config) => generate_totp(config, now_ms),
            None => Ok(TotpCode {
                code: item.value.clone(),
                expires_at_ms: 0,
            }),
        }
    }
}

fn valid_author(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

/// Atomic content and independent favorite registers, using the clipboard order.
pub fn merge_collection(
    current: Option<&ClipboardCollection>,
    incoming: &ClipboardCollection,
    edits: bool,
    favorites: bool,
) -> ClipboardCollection {
    let Some(current) = current else {
        let mut result = incoming.clone();
        if !favorites {
            result.favorite = false;
            result.favorite_revision = 0;
            result.favorite_updated_by_device_id.clear();
        }
        return result;
    };
    let mut result = current.clone();
    if edits
        && (incoming.revision, &incoming.updated_by_device_id)
            > (current.revision, &current.updated_by_device_id)
    {
        result.name = incoming.name.clone();
        result.items = incoming.items.clone();
        result.revision = incoming.revision;
        result.updated_by_device_id = incoming.updated_by_device_id.clone();
        result.deleted = incoming.deleted;
    }
    if favorites
        && (
            incoming.favorite_revision,
            &incoming.favorite_updated_by_device_id,
        ) > (
            current.favorite_revision,
            &current.favorite_updated_by_device_id,
        )
    {
        result.favorite = incoming.favorite;
        result.favorite_revision = incoming.favorite_revision;
        result.favorite_updated_by_device_id = incoming.favorite_updated_by_device_id.clone();
    }
    result
}
