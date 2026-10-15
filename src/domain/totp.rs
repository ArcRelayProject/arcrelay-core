use serde::{Deserialize, Serialize};
use ts_rs::TS;
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Totp {
    pub secret: String,
    pub algorithm: String,
    pub digits: u32,
    pub period: u32,
}

#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TotpCode {
    pub code: String,
    pub expires_at_ms: i64,
}

pub fn normalize_totp(mut config: Totp) -> Result<Totp, String> {
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

pub fn generate_totp(config: &Totp, now_ms: i64) -> Result<TotpCode, String> {
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
    Ok(TotpCode {
        code,
        expires_at_ms: (now_ms / step + 1) * step,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rfc6238_vectors_and_uri_validation() {
        for (algorithm, secret, expected) in [
            ("SHA1", "12345678901234567890", "94287082"),
            ("SHA256", "12345678901234567890123456789012", "46119246"),
            (
                "SHA512",
                "1234567890123456789012345678901234567890123456789012345678901234",
                "90693936",
            ),
        ] {
            let config = Totp {
                secret: data_encoding::BASE32_NOPAD.encode(secret.as_bytes()),
                algorithm: algorithm.into(),
                digits: 8,
                period: 30,
            };
            let code = generate_totp(&config, 59_000).unwrap();
            assert_eq!(code.code, expected);
            assert_eq!(code.expires_at_ms, 60_000);
        }
        let config = Totp {
            secret: "otpauth://totp/example?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=8"
                .into(),
            algorithm: "SHA1".into(),
            digits: 6,
            period: 30,
        };
        assert_eq!(generate_totp(&config, 59_000).unwrap().code, "94287082");
        let duplicate = Totp {
            secret: format!("{}&digits=6", config.secret),
            ..config
        };
        assert!(normalize_totp(duplicate).is_err());
    }
}
