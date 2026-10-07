//! Intentionally saved credentials. Never part of clipboard history or replication.
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoginAppRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub priority: u8,
}

#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LoginSettings {
    pub unlock_seconds: u32,
    pub lock_on_close: bool,
    pub clear_seconds: u32,
}
impl Default for LoginSettings {
    fn default() -> Self {
        Self {
            unlock_seconds: 300,
            lock_on_close: false,
            clear_seconds: 30,
        }
    }
}
impl LoginSettings {
    pub fn validate(&self) -> Result<(), String> {
        if ![60, 300, 900].contains(&self.unlock_seconds)
            || ![15, 30, 60, 120].contains(&self.clear_seconds)
        {
            return Err("Invalid login protection settings".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize, TS, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase")]
pub struct LoginTotp {
    pub secret: String,
    pub algorithm: String,
    pub digits: u32,
    pub period: u32,
}

#[derive(Clone, Serialize, Deserialize, TS, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase")]
pub struct LoginDraft {
    pub id: Option<String>,
    pub title: String,
    pub address: String,
    pub username: String,
    // None preserves an existing password; Some("") explicitly removes it.
    pub password: Option<String>,
    pub totp: Option<LoginTotp>,
    pub keep_totp: bool,
    pub tags: Vec<String>,
    pub favorite: bool,
    #[zeroize(skip)]
    pub apps: Vec<LoginAppRule>,
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase")]
pub struct LoginEntry {
    pub id: String,
    pub title: String,
    pub address: String,
    pub username: String,
    pub password: String,
    pub totp: Option<LoginTotp>,
    pub tags: Vec<String>,
    pub favorite: bool,
    #[zeroize(skip)]
    pub apps: Vec<LoginAppRule>,
    pub updated_at_ms: i64,
    pub last_used_at_ms: i64,
}

#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LoginSummary {
    pub id: String,
    pub title: String,
    pub address: String,
    pub username: String,
    pub has_password: bool,
    pub has_totp: bool,
    pub totp_algorithm: Option<String>,
    pub totp_digits: Option<u32>,
    pub totp_period: Option<u32>,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub apps: Vec<LoginAppRule>,
    pub matched: bool,
    pub priority: u8,
    pub last_used_at_ms: i64,
}
impl LoginEntry {
    pub fn summary(&self, target: Option<&str>) -> LoginSummary {
        let priority = self
            .apps
            .iter()
            .filter(|a| a.enabled && Some(a.id.as_str()) == target)
            .map(|a| a.priority)
            .max();
        LoginSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            address: self.address.clone(),
            username: self.username.clone(),
            has_password: !self.password.is_empty(),
            has_totp: self.totp.is_some(),
            totp_algorithm: self.totp.as_ref().map(|t| t.algorithm.clone()),
            totp_digits: self.totp.as_ref().map(|t| t.digits),
            totp_period: self.totp.as_ref().map(|t| t.period),
            tags: self.tags.clone(),
            favorite: self.favorite,
            apps: self.apps.clone(),
            matched: priority.is_some(),
            priority: priority.unwrap_or(0),
            last_used_at_ms: self.last_used_at_ms,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LoginField {
    Username,
    Password,
    Totp,
}

#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LoginOtp {
    pub code: String,
    pub expires_at_ms: i64,
}

pub fn normalize_totp(mut config: LoginTotp) -> Result<LoginTotp, String> {
    if config.secret.len() > 8192 {
        return Err("TOTP configuration is too long".into());
    }
    if config.secret.starts_with("otpauth://") {
        let uri = url::Url::parse(&config.secret).map_err(|_| "Invalid TOTP configuration URI")?;
        if uri.host_str() != Some("totp") {
            return Err("Only time-based TOTP is supported".into());
        }
        let mut seen = std::collections::HashSet::new();
        let mut secret = None;
        for (key, value) in uri.query_pairs() {
            if !seen.insert(key.to_string()) {
                return Err("Duplicate TOTP configuration parameters".into());
            }
            match key.as_ref() {
                "secret" => secret = Some(value.to_string()),
                "algorithm" => config.algorithm = value.to_string(),
                "digits" => config.digits = value.parse().map_err(|_| "Invalid TOTP digits")?,
                "period" => config.period = value.parse().map_err(|_| "Invalid TOTP period")?,
                _ => {}
            }
        }
        config.secret = secret.ok_or("TOTP configuration has no secret")?;
    }
    config.secret = config
        .secret
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .flat_map(char::to_uppercase)
        .collect();
    config.secret = config.secret.trim_end_matches('=').to_owned();
    config.algorithm = config.algorithm.to_ascii_uppercase();
    if !["SHA1", "SHA256", "SHA512"].contains(&config.algorithm.as_str())
        || ![6, 8].contains(&config.digits)
        || !(15..=120).contains(&config.period)
    {
        return Err("Invalid TOTP algorithm, digits, or period".into());
    }
    let secret = Zeroizing::new(
        data_encoding::BASE32_NOPAD
            .decode(config.secret.as_bytes())
            .map_err(|_| "TOTP secret must be valid Base32 or an otpauth URI")?,
    );
    if secret.len() < 10 || secret.len() > 128 {
        return Err("Invalid TOTP secret length".into());
    }
    Ok(config)
}

pub fn generate_totp(config: &LoginTotp, now_ms: i64) -> Result<LoginOtp, String> {
    use hmac::{Hmac, Mac};
    if now_ms < 0 {
        return Err("Invalid system time".into());
    }
    let normalized = normalize_totp(config.clone())?;
    let secret = Zeroizing::new(
        data_encoding::BASE32_NOPAD
            .decode(normalized.secret.as_bytes())
            .map_err(|_| "Invalid TOTP configuration")?,
    );
    let counter = (now_ms as u64 / 1000 / u64::from(normalized.period)).to_be_bytes();
    macro_rules! digest {
        ($hash:ty) => {{
            let mut mac =
                <Hmac<$hash> as Mac>::new_from_slice(&secret).map_err(|_| "Invalid TOTP secret")?;
            mac.update(&counter);
            Zeroizing::new(mac.finalize().into_bytes().to_vec())
        }};
    }
    let hash = match normalized.algorithm.as_str() {
        "SHA1" => digest!(sha1::Sha1),
        "SHA256" => digest!(sha2::Sha256),
        "SHA512" => digest!(sha2::Sha512),
        _ => return Err("Invalid TOTP algorithm".into()),
    };
    let offset = usize::from(hash[hash.len() - 1] & 15);
    let value = u32::from_be_bytes(
        hash[offset..offset + 4]
            .try_into()
            .map_err(|_| "TOTP generation failed")?,
    ) & 0x7fffffff;
    let code = format!(
        "{:0width$}",
        value % 10u32.pow(normalized.digits),
        width = normalized.digits as usize
    );
    let step = i64::from(normalized.period) * 1000;
    Ok(LoginOtp {
        code,
        expires_at_ms: (now_ms / step + 1) * step,
    })
}
