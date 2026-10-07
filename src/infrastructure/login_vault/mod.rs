//! Host-only credential persistence. Construction requires neither Tokio nor I/O.
mod crypto;
#[cfg(test)]
mod tests;

use crate::domain::login::*;
use crypto::*;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Contents {
    entries: Vec<LoginEntry>,
    settings: LoginSettings,
}
#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LoginVaultStatus {
    pub session_version: u32,
    pub configured: bool,
    pub unlocked: bool,
    pub expires_in_seconds: u32,
    pub settings: LoginSettings,
    // Identifies the protected OS credential; never contains a secret.
    pub vault_id: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LoginRestorePreview {
    pub count: u32,
    pub titles: Vec<String>,
}

#[derive(Default)]
struct Session {
    version: u32,
    key: Option<Zeroizing<[u8; 32]>>,
    expires: Option<Instant>,
    vault_id: Option<String>,
    settings: LoginSettings,
    failures: u32,
    retry_at: Option<Instant>,
}
impl Session {
    fn expire(&mut self) {
        if self.expires.is_some_and(|t| t <= Instant::now()) {
            if self.key.is_some() {
                self.version = self.version.saturating_add(1);
            }
            self.key = None;
            self.expires = None;
        }
    }
    fn activate(&mut self, key: Zeroizing<[u8; 32]>, e: &Envelope, data: &Contents) {
        self.version = self.version.saturating_add(1);
        self.key = Some(key);
        self.vault_id = Some(e.id.clone());
        self.settings = data.settings.clone();
        self.expires =
            Some(Instant::now() + Duration::from_secs(u64::from(data.settings.unlock_seconds)));
        self.failures = 0;
        self.retry_at = None;
    }
}
pub struct LoginVault {
    path: PathBuf,
    session: Mutex<Session>,
}
impl LoginVault {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            session: Mutex::new(Session::default()),
        }
    }
    fn session(&self) -> Result<std::sync::MutexGuard<'_, Session>, String> {
        let mut s = self
            .session
            .lock()
            .map_err(|_| "Login protection service is unavailable")?;
        s.expire();
        Ok(s)
    }
    fn file_lock(&self) -> Result<fs::File, String> {
        let parent = self.path.parent().ok_or("Invalid login storage path")?;
        fs::create_dir_all(parent).map_err(|_| "Cannot create login storage directory")?;
        let path = self.path.with_extension("lock");
        reject_symlink(&path)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let f = options
            .open(path)
            .map_err(|_| "Cannot access login storage lock")?;
        f.try_lock_exclusive()
            .map_err(|_| "Login storage is in use by another process; retry later")?;
        Ok(f)
    }
    fn envelope(&self) -> Result<Option<Envelope>, String> {
        match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("Cannot access login file".into()),
            Ok(_) => read_envelope(&self.path).map(Some),
        }
    }
    fn contents(&self, s: &Session, e: &Envelope) -> Result<Contents, String> {
        if s.vault_id.as_deref() != Some(e.id.as_str()) {
            return Err("Login protection has changed; unlock again".into());
        }
        let key = s
            .key
            .as_ref()
            .ok_or("Login information is locked; unlock first")?;
        let bytes = open(key, &e.data, &e.aad("data"))?;
        let data: Contents =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid login file contents")?;
        validate_contents(&data)?;
        Ok(data)
    }
    fn save(&self, s: &Session, e: &mut Envelope, data: &Contents) -> Result<(), String> {
        validate_contents(data)?;
        let bytes =
            Zeroizing::new(serde_json::to_vec(data).map_err(|_| "Cannot encode login entries")?);
        e.data = seal(
            s.key.as_ref().ok_or("Login information is locked")?,
            &bytes,
            &e.aad("data"),
        )?;
        write_envelope(&self.path, e)
    }
    pub fn status(&self) -> Result<LoginVaultStatus, String> {
        let mut s = self.session()?;
        let e = match self.envelope() {
            Ok(e) => e,
            Err(e) => {
                if s.key.is_some() {
                    s.version = s.version.saturating_add(1);
                }
                s.key = None;
                s.expires = None;
                return Ok(LoginVaultStatus {
                    session_version: s.version,
                    configured: self.path.exists(),
                    unlocked: false,
                    expires_in_seconds: 0,
                    settings: s.settings.clone(),
                    vault_id: None,
                    error: Some(e),
                });
            }
        };
        if e.as_ref().map(|e| e.id.as_str()) != s.vault_id.as_deref() {
            if s.key.is_some() {
                s.version = s.version.saturating_add(1);
            }
            s.key = None;
            s.expires = None;
        }
        Ok(LoginVaultStatus {
            session_version: s.version,
            configured: e.is_some(),
            unlocked: s.key.is_some(),
            expires_in_seconds: s
                .expires
                .map(|t| t.saturating_duration_since(Instant::now()).as_secs() as u32)
                .unwrap_or(0),
            settings: s.settings.clone(),
            vault_id: e.map(|e| e.id),
            error: None,
        })
    }
    pub fn initialize(&self, password: &str) -> Result<(), String> {
        let mut s = self.session()?;
        let _file_lock = self.file_lock()?;
        if self.envelope()?.is_some() {
            return Err("Login protection is already enabled; unlock existing data".into());
        }
        let data = Contents::default();
        let bytes = Zeroizing::new(serde_json::to_vec(&data).map_err(|_| "Initialization failed")?);
        let (e, key) = Envelope::create("vault", password, &bytes)?;
        write_envelope(&self.path, &e)?;
        s.activate(key, &e, &data);
        Ok(())
    }
    pub fn unlock(&self, password: &str) -> Result<(), String> {
        let mut s = self.session()?;
        if s.retry_at.is_some_and(|t| t > Instant::now()) {
            return Err("Too many verification attempts; retry later".into());
        }
        let e = self.envelope()?.ok_or("Enable login protection first")?;
        let result = (|| {
            let key = e.unlock(password, "vault")?;
            let bytes = open(&key, &e.data, &e.aad("data"))?;
            let data: Contents =
                serde_json::from_slice(&bytes).map_err(|_| "Invalid login file contents")?;
            validate_contents(&data)?;
            Ok((key, data))
        })();
        match result {
            Ok((key, data)) => {
                s.activate(key, &e, &data);
                Ok(())
            }
            Err(error) => {
                s.key = None;
                s.expires = None;
                s.failures = s.failures.saturating_add(1);
                s.retry_at = Some(Instant::now() + Duration::from_secs(1u64 << s.failures.min(5)));
                Err(error)
            }
        }
    }
    /// Called only by a native OS-protected credential adapter after user authentication.
    pub fn unlock_with_device_key(
        &self,
        vault_id: &str,
        key: Zeroizing<[u8; 32]>,
    ) -> Result<(), String> {
        let mut s = self.session()?;
        let e = self.envelope()?.ok_or("Login protection is not enabled")?;
        if e.id != vault_id || e.kind != "vault" || e.version != 1 {
            return Err("Quick unlock credentials have expired; use the master password".into());
        }
        let bytes = open(&key, &e.data, &e.aad("data"))?;
        let data: Contents =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid login file contents")?;
        validate_contents(&data)?;
        s.activate(key, &e, &data);
        Ok(())
    }
    pub fn device_key(&self) -> Result<(String, Zeroizing<[u8; 32]>), String> {
        let s = self.session()?;
        let e = self.envelope()?.ok_or("Login protection is not enabled")?;
        self.contents(&s, &e)?;
        Ok((
            e.id,
            s.key.as_ref().ok_or("Login information is locked")?.clone(),
        ))
    }
    pub fn lock(&self) {
        if let Ok(mut s) = self.session.lock() {
            if s.key.is_some() {
                s.version = s.version.saturating_add(1);
            }
            s.key = None;
            s.expires = None;
        }
    }
    pub fn close(&self) -> bool {
        if let Ok(mut s) = self.session.lock() {
            if s.settings.lock_on_close {
                if s.key.is_some() {
                    s.version = s.version.saturating_add(1);
                }
                s.key = None;
                s.expires = None;
                return true;
            }
        }
        false
    }
    pub fn list(
        &self,
        target: Option<&str>,
        search: &str,
        tag: Option<&str>,
    ) -> Result<Vec<LoginSummary>, String> {
        let s = self.session()?;
        let e = self.envelope()?.ok_or("Enable login protection first")?;
        let data = self.contents(&s, &e)?;
        let search = search.to_lowercase();
        let mut rows: Vec<_> = data
            .entries
            .iter()
            .filter(|entry| {
                (tag.is_none() || entry.tags.iter().any(|t| Some(t.as_str()) == tag))
                    && [&entry.title, &entry.address, &entry.username]
                        .iter()
                        .any(|v| v.to_lowercase().contains(&search))
            })
            .map(|e| e.summary(target))
            .collect();
        rows.sort_by(|a, b| {
            b.matched
                .cmp(&a.matched)
                .then(b.priority.cmp(&a.priority))
                .then(b.favorite.cmp(&a.favorite))
                .then(b.last_used_at_ms.cmp(&a.last_used_at_ms))
                .then(a.title.cmp(&b.title))
                .then(a.id.cmp(&b.id))
        });
        Ok(rows)
    }
    pub fn tags(&self) -> Result<Vec<String>, String> {
        let s = self.session()?;
        let e = self.envelope()?.ok_or("Enable login protection first")?;
        let data = self.contents(&s, &e)?;
        Ok(data
            .entries
            .iter()
            .flat_map(|e| e.tags.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect())
    }
    pub fn upsert(&self, draft: LoginDraft) -> Result<String, String> {
        let s = self.session()?;
        let _file_lock = self.file_lock()?;
        let mut e = self.envelope()?.ok_or("Enable login protection first")?;
        let mut data = self.contents(&s, &e)?;
        let old = draft
            .id
            .as_ref()
            .and_then(|id| data.entries.iter().find(|e| &e.id == id))
            .cloned();
        if draft.id.is_some() && old.is_none() {
            return Err("Login entry was deleted; refresh the list".into());
        }
        let id = draft
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let totp = if draft.keep_totp {
            if draft.totp.is_some() {
                return Err("Invalid TOTP update mode".into());
            }
            old.as_ref().and_then(|e| e.totp.clone())
        } else {
            draft.totp.clone().map(normalize_totp).transpose()?
        };
        let entry = LoginEntry {
            id: id.clone(),
            title: draft.title.trim().into(),
            address: draft.address.trim().into(),
            username: draft.username.clone(),
            password: draft
                .password
                .clone()
                .unwrap_or_else(|| old.as_ref().map(|e| e.password.clone()).unwrap_or_default()),
            totp,
            tags: draft.tags.clone(),
            favorite: draft.favorite,
            apps: draft.apps.clone(),
            updated_at_ms: chrono::Utc::now().timestamp_millis(),
            last_used_at_ms: old.as_ref().map(|e| e.last_used_at_ms).unwrap_or(0),
        };
        data.entries.retain(|e| e.id != id);
        data.entries.push(entry);
        self.save(&s, &mut e, &data)?;
        Ok(id)
    }
    pub fn remove(&self, id: &str) -> Result<(), String> {
        let s = self.session()?;
        let _file_lock = self.file_lock()?;
        let mut e = self.envelope()?.ok_or("Login protection is not enabled")?;
        let mut data = self.contents(&s, &e)?;
        data.entries.retain(|e| e.id != id);
        self.save(&s, &mut e, &data)
    }
    pub fn field(&self, id: &str, field: LoginField) -> Result<Zeroizing<String>, String> {
        let s = self.session()?;
        let e = self.envelope()?.ok_or("Login protection is not enabled")?;
        let data = self.contents(&s, &e)?;
        let entry = data
            .entries
            .iter()
            .find(|e| e.id == id)
            .ok_or("Login entry does not exist")?;
        let value = match field {
            LoginField::Username => entry.username.clone(),
            LoginField::Password => entry.password.clone(),
            LoginField::Totp => {
                let otp = generate_totp(
                    entry.totp.as_ref().ok_or("TOTP is not configured")?,
                    chrono::Utc::now().timestamp_millis(),
                )?;
                if otp.expires_at_ms - chrono::Utc::now().timestamp_millis() < 5000 {
                    return Err("TOTP is about to change; retry insertion later".into());
                }
                otp.code
            }
        };
        if value.is_empty() {
            return Err("This field is empty".into());
        }
        Ok(Zeroizing::new(value))
    }
    pub fn otp(&self, id: &str) -> Result<LoginOtp, String> {
        let s = self.session()?;
        let e = self.envelope()?.ok_or("Login protection is not enabled")?;
        let data = self.contents(&s, &e)?;
        generate_totp(
            data.entries
                .iter()
                .find(|e| e.id == id)
                .and_then(|e| e.totp.as_ref())
                .ok_or("TOTP is not configured")?,
            chrono::Utc::now().timestamp_millis(),
        )
    }
    pub fn touch(&self, id: &str) -> Result<(), String> {
        let s = self.session()?;
        let _file_lock = self.file_lock()?;
        let mut e = self.envelope()?.ok_or("Login protection is not enabled")?;
        let mut data = self.contents(&s, &e)?;
        data.entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or("Login entry does not exist")?
            .last_used_at_ms = chrono::Utc::now().timestamp_millis();
        self.save(&s, &mut e, &data)
    }
    pub fn settings(&self, settings: LoginSettings) -> Result<(), String> {
        settings.validate()?;
        let mut s = self.session()?;
        let _file_lock = self.file_lock()?;
        let mut e = self.envelope()?.ok_or("Login protection is not enabled")?;
        let mut data = self.contents(&s, &e)?;
        data.settings = settings.clone();
        self.save(&s, &mut e, &data)?;
        s.settings = settings;
        // Settings changes never extend the authentication session.
        if let Some(expires) = s.expires {
            s.expires =
                Some(expires.min(
                    Instant::now() + Duration::from_secs(u64::from(s.settings.unlock_seconds)),
                ));
        }
        Ok(())
    }
    pub fn change_password(&self, previous: &str, password: &str) -> Result<(), String> {
        let mut s = self.session()?;
        let _file_lock = self.file_lock()?;
        let e = self.envelope()?.ok_or("Login protection is not enabled")?;
        let verified = e.unlock(previous, "vault")?;
        let data = self.contents(&s, &e)?;
        // Rotate both the data key and vault ID, revoking old device credentials.
        open(&verified, &e.data, &e.aad("data"))?;
        let bytes =
            Zeroizing::new(serde_json::to_vec(&data).map_err(|_| "Passphrase update failed")?);
        let (next, key) = Envelope::create("vault", password, &bytes)?;
        write_envelope(&self.path, &next)?;
        s.activate(key, &next, &data);
        Ok(())
    }
    pub fn export_backup(&self, path: &Path, password: &str) -> Result<(), String> {
        if path == self.path || path.exists() {
            return Err("Choose a new backup filename to preserve existing files".into());
        }
        let s = self.session()?;
        let e = self.envelope()?.ok_or("Login protection is not enabled")?;
        let data = self.contents(&s, &e)?;
        let bytes =
            Zeroizing::new(serde_json::to_vec(&data).map_err(|_| "Backup encoding failed")?);
        let (backup, _) = Envelope::create("backup", password, &bytes)?;
        write_envelope_new(path, &backup)
    }
    fn backup(&self, path: &Path, password: &str) -> Result<Contents, String> {
        // A backup password never unlocks or resets the current vault.
        let s = self.session()?;
        let e = self
            .envelope()?
            .ok_or("Enable local login protection first")?;
        self.contents(&s, &e)?;
        let backup = read_envelope(path)?;
        let key = backup.unlock(password, "backup")?;
        let bytes = open(&key, &backup.data, &backup.aad("data"))?;
        let data = serde_json::from_slice(&bytes).map_err(|_| "Invalid backup contents")?;
        validate_contents(&data)?;
        Ok(data)
    }
    /// Recovery is authorized by the independent backup password, never by an empty/reset master password.
    pub fn preview_recovery(
        &self,
        path: &Path,
        password: &str,
    ) -> Result<LoginRestorePreview, String> {
        let data = decode_backup(path, password)?;
        Ok(LoginRestorePreview {
            count: data.entries.len() as u32,
            titles: data.entries.iter().map(|e| e.title.clone()).collect(),
        })
    }
    pub fn storage_revision(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        if !self.path.exists() {
            return Ok(String::new());
        }
        Ok(format!(
            "{:x}",
            Sha256::digest(read_encrypted_bytes(&self.path)?)
        ))
    }
    pub fn recover_backup(
        &self,
        path: &Path,
        backup_password: &str,
        new_password: &str,
        expected_revision: &str,
    ) -> Result<u32, String> {
        let mut data = decode_backup(path, backup_password)?;
        data.settings = LoginSettings::default();
        for entry in &mut data.entries {
            for app in &mut entry.apps {
                app.enabled = false;
            }
        }
        let bytes =
            Zeroizing::new(serde_json::to_vec(&data).map_err(|_| "Cannot encode restored data")?);
        let (next, key) = Envelope::create("vault", new_password, &bytes)?;
        let mut s = self.session()?;
        let _file_lock = self.file_lock()?;
        if self.storage_revision()? != expected_revision {
            return Err("Current login data has changed; preview the backup again".into());
        }
        // Preserve original ciphertext, including corrupt files, before replacement.
        if self.path.exists() {
            let preserved = self
                .path
                .with_file_name(format!("logins-previous-{}.arclogin", uuid::Uuid::new_v4()));
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&preserved)
                .map_err(|_| "Cannot preserve the original encrypted file")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(|_| "Cannot protect the original encrypted file")?;
            }
            file.write_all(&read_encrypted_bytes(&self.path)?)
                .and_then(|_| file.sync_all())
                .map_err(|_| "Cannot preserve the original encrypted file")?;
        }
        write_envelope(&self.path, &next)?;
        s.activate(key, &next, &data);
        Ok(data.entries.len() as u32)
    }
    pub fn preview_backup(
        &self,
        path: &Path,
        password: &str,
    ) -> Result<LoginRestorePreview, String> {
        let data = self.backup(path, password)?;
        Ok(LoginRestorePreview {
            count: data.entries.len() as u32,
            titles: data.entries.iter().map(|e| e.title.clone()).collect(),
        })
    }
    pub fn restore_backup(
        &self,
        path: &Path,
        password: &str,
        replace: bool,
    ) -> Result<u32, String> {
        let backup = self.backup(path, password)?;
        let s = self.session()?;
        let _file_lock = self.file_lock()?;
        let mut e = self.envelope()?.ok_or("Login protection is not enabled")?;
        let mut data = self.contents(&s, &e)?;
        if replace {
            data.entries.clear();
        }
        let count = backup.entries.len() as u32;
        for mut incoming in backup.entries {
            // Native application identities are local; re-enable rules explicitly.
            for app in &mut incoming.apps {
                app.enabled = false;
            }
            if let Some(existing) = data.entries.iter().find(|e| e.id == incoming.id) {
                if !replace && existing.updated_at_ms > incoming.updated_at_ms {
                    continue;
                }
            }
            data.entries.retain(|e| e.id != incoming.id);
            data.entries.push(incoming);
        }
        self.save(&s, &mut e, &data)?;
        Ok(count)
    }
}

fn validate_contents(data: &Contents) -> Result<(), String> {
    data.settings.validate()?;
    if data.entries.len() > 1000 {
        return Err("At most 1000 login entries can be stored".into());
    }
    let mut ids = std::collections::HashSet::new();
    for e in &data.entries {
        if !ids.insert(&e.id)
            || uuid::Uuid::parse_str(&e.id).is_err()
            || e.title.is_empty()
            || e.title.len() > 256
            || e.address.len() > 2048
            || e.username.len() > 4096
            || e.password.len() > 16384
            || e.tags.len() > 20
            || e.tags.iter().any(|t| t.is_empty() || t.len() > 64)
            || e.apps.len() > 32
            || e.apps.iter().any(|a| {
                a.id.is_empty() || a.id.len() > 4096 || a.name.len() > 256 || a.priority > 100
            })
            || (e.username.is_empty() && e.password.is_empty() && e.totp.is_none())
        {
            return Err("Invalid or oversized login entry fields".into());
        }
        let mut apps = std::collections::HashSet::new();
        if e.apps.iter().any(|a| !apps.insert(&a.id)) {
            return Err("Duplicate application rule identifiers".into());
        }
        if let Some(t) = &e.totp {
            normalize_totp(t.clone())?;
        }
    }
    Ok(())
}
fn reject_symlink(path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Login file must not be a symbolic link".into());
    }
    Ok(())
}
fn read_encrypted_bytes(path: &Path) -> Result<Vec<u8>, String> {
    reject_symlink(path)?;
    let f = fs::File::open(path).map_err(|_| "Cannot read encrypted file")?;
    if !f
        .metadata()
        .map_err(|_| "Cannot read encrypted file metadata")?
        .is_file()
    {
        return Err("Select a regular backup file".into());
    }
    let mut bytes = Vec::new();
    f.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read encrypted file")?;
    if bytes.len() > MAX_FILE_BYTES as usize {
        return Err("Encrypted file exceeds the 16 MiB limit".into());
    }
    Ok(bytes)
}
fn read_envelope(path: &Path) -> Result<Envelope, String> {
    serde_json::from_slice(&read_encrypted_bytes(path)?)
        .map_err(|_| "Invalid encrypted file format".into())
}
fn temporary(path: &Path, e: &Envelope) -> Result<tempfile::NamedTempFile, String> {
    reject_symlink(path)?;
    let parent = path.parent().ok_or("Invalid save path")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create save directory")?;
    let bytes = serde_json::to_vec(e).map_err(|_| "Cannot encode encrypted file")?;
    if bytes.len() > MAX_FILE_BYTES as usize {
        return Err("Encrypted file exceeds the 16 MiB limit".into());
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "Cannot create encrypted temporary file")?;
    file.write_all(&bytes)
        .map_err(|_| "Cannot save encrypted file")?;
    file.as_file()
        .sync_all()
        .map_err(|_| "Cannot sync encrypted file")?;
    Ok(file)
}
fn write_envelope(path: &Path, e: &Envelope) -> Result<(), String> {
    temporary(path, e)?
        .persist(path)
        .map_err(|_| "Cannot commit encrypted file; original data is unchanged")?;
    #[cfg(unix)]
    fs::File::open(path.parent().ok_or("Invalid save path")?)
        .and_then(|f| f.sync_all())
        .map_err(|_| "Cannot sync login storage directory")?;
    Ok(())
}
fn write_envelope_new(path: &Path, e: &Envelope) -> Result<(), String> {
    temporary(path, e)?
        .persist_noclobber(path)
        .map_err(|_| "Backup file already exists or cannot be saved")?;
    Ok(())
}

fn decode_backup(path: &Path, password: &str) -> Result<Contents, String> {
    let e = read_envelope(path)?;
    let key = e.unlock(password, "backup")?;
    let bytes = open(&key, &e.data, &e.aad("data"))?;
    let data: Contents = serde_json::from_slice(&bytes).map_err(|_| "Invalid backup contents")?;
    validate_contents(&data)?;
    Ok(data)
}
