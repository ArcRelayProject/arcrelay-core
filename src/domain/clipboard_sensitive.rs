use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Device-local rules for newly captured and edited clipboard text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(default, rename_all = "camelCase")]
pub struct ClipboardSensitiveSettings {
    pub enabled: bool,
    pub credential_assignments: bool,
    pub bearer_tokens: bool,
    pub private_keys: bool,
    pub custom_keywords: Vec<String>,
    pub save_sensitive: bool,
}

impl Default for ClipboardSensitiveSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            credential_assignments: true,
            bearer_tokens: true,
            private_keys: true,
            custom_keywords: Vec::new(),
            save_sensitive: false,
        }
    }
}

impl ClipboardSensitiveSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.custom_keywords.len() > 100 {
            return Err("use at most 100 custom clipboard keywords");
        }
        if self.custom_keywords.iter().any(|word| {
            word.trim().is_empty() || word.len() > 256 || word.chars().any(char::is_control)
        }) {
            return Err(
                "clipboard keywords must contain 1 to 256 bytes without control characters",
            );
        }
        Ok(())
    }

    /// Returns a rule identifier only, never the matched value.
    pub fn matching_rule(&self, value: &str) -> Option<&'static str> {
        if !self.enabled {
            return None;
        }
        if self.custom_keywords.iter().any(|keyword| {
            !keyword.trim().is_empty()
                && value
                    .as_bytes()
                    .windows(keyword.len())
                    .any(|part| part.eq_ignore_ascii_case(keyword.as_bytes()))
        }) {
            return Some("custom_keyword");
        }
        let text = value.trim();
        if self.private_keys && private_key(text) {
            return Some("private_key");
        }
        // Built-ins recognize standalone credentials, not examples embedded in
        // documents or code. Custom literal rules deliberately scan all text.
        if text.len() > 4096 || text.contains(['\n', '\r']) {
            return None;
        }
        if self.credential_assignments {
            static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
            let pattern = ASSIGNMENT.get_or_init(|| {
                Regex::new(r#"(?i)\A(?:password|passwd|passcode|api_key|api-key|access_token|refresh_token|secret)\s*[:=]\s*(.+)\z"#).unwrap()
            });
            if pattern
                .captures(text)
                .is_some_and(|parts| credential_value(&parts[1]))
            {
                return Some("credential_assignment");
            }
        }
        if self.bearer_tokens {
            static BEARER: OnceLock<Regex> = OnceLock::new();
            let pattern = BEARER.get_or_init(|| {
                Regex::new(r"(?i)\A(?:authorization:\s*)?bearer\s+([a-z0-9._~+/-]{16,}=*)\z")
                    .unwrap()
            });
            if pattern
                .captures(text)
                .is_some_and(|parts| credential_value(&parts[1]))
            {
                return Some("bearer_token");
            }
        }
        None
    }
}

fn credential_value(value: &str) -> bool {
    let value = value.trim();
    let value = if value.len() >= 2
        && (value.starts_with('"') && value.ends_with('"')
            || value.starts_with('\'') && value.ends_with('\''))
    {
        &value[1..value.len().saturating_sub(1)]
    } else {
        value
    };
    let lower = value.to_ascii_lowercase();
    value.len() >= 8
        && !value.chars().any(char::is_whitespace)
        && value.chars().any(char::is_alphanumeric)
        && !value.contains(['<', '>', '{', '}', '$', '`'])
        && !["example", "placeholder", "your_", "your-", "changeme"]
            .iter()
            .any(|word| lower.contains(word))
}

fn private_key(value: &str) -> bool {
    [
        "PRIVATE KEY",
        "RSA PRIVATE KEY",
        "EC PRIVATE KEY",
        "OPENSSH PRIVATE KEY",
        "ENCRYPTED PRIVATE KEY",
    ]
    .iter()
    .any(|kind| {
        let begin = format!("-----BEGIN {kind}-----");
        let end = format!("-----END {kind}-----");
        value
            .strip_prefix(&begin)
            .and_then(|rest| rest.strip_suffix(&end))
            .is_some_and(|body| body.trim().len() >= 32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documentation_and_placeholder_values_are_not_credentials() {
        let settings = ClipboardSensitiveSettings::default();
        for text in [
            "This document explains password management.",
            "password",
            "password=",
            "password=your_password",
            "password=<replace-me>",
            "api_key=${API_KEY}",
            "password = \"example-value\"",
            "password = \"\"",
            "password='",
            "See this example: password=actual-value-1234",
            "# Configuration\npassword=actual-value-1234\nUse a strong password.",
        ] {
            assert_eq!(settings.matching_rule(text), None, "{text}");
        }
        let long = format!(
            "{}\npassword=real-looking-value-123",
            "Technical documentation. ".repeat(20_000)
        );
        assert_eq!(settings.matching_rule(&long), None);
    }

    #[test]
    fn builtins_can_be_disabled_individually() {
        let mut settings = ClipboardSensitiveSettings::default();
        let cases = [
            ("PASSWORD=actual-value-1234", "credential_assignment"),
            ("api_key=sk-host-only-secret", "credential_assignment"),
            ("Authorization: Bearer abc1234567890.XYZ987654321", "bearer_token"),
            ("-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----", "private_key"),
        ];
        for (text, rule) in cases {
            assert_eq!(settings.matching_rule(text), Some(rule));
        }
        settings.credential_assignments = false;
        assert_eq!(settings.matching_rule(cases[0].0), None);
        settings.bearer_tokens = false;
        assert_eq!(settings.matching_rule(cases[2].0), None);
        settings.private_keys = false;
        assert_eq!(settings.matching_rule(cases[3].0), None);
    }

    #[test]
    fn custom_literals_scan_documents_and_obey_the_master_switch() {
        let mut settings = ClipboardSensitiveSettings {
            custom_keywords: vec!["Internal-Only".into(), "内部资料".into()],
            ..Default::default()
        };
        for text in ["A long INTERNAL-only document", "这是内部资料。"] {
            assert_eq!(settings.matching_rule(text), Some("custom_keyword"));
        }
        assert_eq!(settings.matching_rule("An external document"), None);
        settings.enabled = false;
        assert_eq!(
            settings.matching_rule("内部资料 password=actual-value-1234"),
            None
        );
    }

    #[test]
    fn invalid_rules_are_rejected_and_empty_legacy_rules_do_not_panic() {
        for keywords in [
            vec!["".into()],
            vec!["\n".into()],
            vec!["a".repeat(257)],
            vec!["a".into(); 101],
        ] {
            let settings = ClipboardSensitiveSettings {
                custom_keywords: keywords,
                ..Default::default()
            };
            assert!(settings.validate().is_err());
            settings.matching_rule("ordinary text");
        }
    }
}
