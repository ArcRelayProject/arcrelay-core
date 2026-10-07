use super::*;

fn draft() -> LoginDraft {
    LoginDraft {
        id: None,
        title: "Private Console".into(),
        address: "https://private.example.test".into(),
        username: "private-user".into(),
        password: Some("private-login-password".into()),
        totp: None,
        keep_totp: false,
        tags: vec!["Work".into()],
        favorite: false,
        apps: vec![LoginAppRule {
            id: "com.apple.Safari".into(),
            name: "Safari".into(),
            enabled: true,
            priority: 100,
        }],
    }
}
#[test]
fn login_constructor_needs_no_runtime_and_does_not_touch_disk() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("missing/vault.json");
    let _vault = LoginVault::new(path.clone());
    assert!(!path.parent().unwrap().exists());
}
#[test]
fn login_vault_encrypts_every_field_restarts_locked_and_rejects_wrong_password() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("vault");
    let vault = LoginVault::new(path.clone());
    vault.initialize("master-password-long").unwrap();
    let id = vault.upsert(draft()).unwrap();
    let raw = fs::read_to_string(&path).unwrap();
    for secret in [
        "Private Console",
        "private-user",
        "private-login-password",
        "com.apple.Safari",
    ] {
        assert!(!raw.contains(secret));
    }
    assert_eq!(
        &*vault.field(&id, LoginField::Password).unwrap(),
        "private-login-password"
    );
    let restarted = LoginVault::new(path);
    assert!(!restarted.status().unwrap().unlocked);
    assert!(restarted.list(None, "", None).is_err());
    assert!(restarted.unlock("incorrect").is_err());
    // A separate process session can authenticate correctly without the failed session's cooldown.
    let restarted = LoginVault::new(vault.path.clone());
    restarted.unlock("master-password-long").unwrap();
    assert_eq!(
        restarted
            .list(Some("com.apple.Safari"), "", None)
            .unwrap()
            .len(),
        1
    );
    assert!(restarted
        .list(Some("other.app"), "not-matching", None)
        .unwrap()
        .is_empty());
    vault.lock();
    assert!(vault.field(&id, LoginField::Password).is_err());
}
#[test]
fn login_rfc6238_vectors_and_otpauth_parameters() {
    let vectors = [
        (59, "94287082", "46119246", "90693936"),
        (1111111109, "07081804", "68084774", "25091201"),
        (1234567890, "89005924", "91819424", "93441116"),
        (20000000000, "65353130", "77737706", "47863826"),
    ];
    for (algorithm, bytes, column) in [("SHA1", 20, 0), ("SHA256", 32, 1), ("SHA512", 64, 2)] {
        let seed = (0..bytes)
            .map(|i| b'1' + (i % 10) as u8)
            .map(|b| if b > b'9' { b'0' } else { b })
            .collect::<Vec<_>>();
        let config = LoginTotp {
            secret: data_encoding::BASE32_NOPAD.encode(&seed),
            algorithm: algorithm.into(),
            digits: 8,
            period: 30,
        };
        for (seconds, sha1, sha256, sha512) in vectors {
            assert_eq!(
                generate_totp(&config, seconds * 1000).unwrap().code,
                [sha1, sha256, sha512][column]
            );
        }
    }
    let parsed = normalize_totp(LoginTotp { secret: "otpauth://totp/Test?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&algorithm=SHA256&digits=8&period=60".into(),
        algorithm: "SHA1".into(), digits: 6, period: 30 }).unwrap();
    assert_eq!(parsed.period, 60);
    assert_eq!(parsed.digits, 8);
    assert!(normalize_totp(LoginTotp {
        secret: "otpauth://hotp/Test?secret=ABC".into(),
        algorithm: "SHA1".into(),
        digits: 6,
        period: 30
    })
    .is_err());
}
#[test]
fn login_backup_is_independent_and_disables_native_rules_on_restore() {
    let temp = tempfile::tempdir().unwrap();
    let vault = LoginVault::new(temp.path().join("vault"));
    vault.initialize("master-password-long").unwrap();
    vault.upsert(draft()).unwrap();
    let backup = temp.path().join("backup.arclogin");
    vault
        .export_backup(&backup, "backup-password-long")
        .unwrap();
    assert!(!fs::read_to_string(&backup)
        .unwrap()
        .contains("private-login-password"));
    assert!(vault
        .preview_backup(&backup, "master-password-long")
        .is_err());
    assert!(vault
        .export_backup(&backup, "backup-password-long")
        .is_err());
    let other = LoginVault::new(temp.path().join("other"));
    other.initialize("another-master-password").unwrap();
    assert_eq!(
        other
            .preview_backup(&backup, "backup-password-long")
            .unwrap()
            .count,
        1
    );
    other
        .restore_backup(&backup, "backup-password-long", false)
        .unwrap();
    assert!(!other.list(Some("com.apple.Safari"), "", None).unwrap()[0].matched);
    assert!(vault
        .preview_backup(&vault.path, "master-password-long")
        .is_err());
}
#[test]
fn login_change_password_revokes_device_key_and_expiration_is_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let vault = LoginVault::new(temp.path().join("vault"));
    vault.initialize("master-password-long").unwrap();
    let id = vault.upsert(draft()).unwrap();
    let (old_id, old_key) = vault.device_key().unwrap();
    vault
        .change_password("master-password-long", "replacement-password")
        .unwrap();
    vault.lock();
    assert!(vault.unlock_with_device_key(&old_id, old_key).is_err());
    vault.unlock("replacement-password").unwrap();
    vault.session.lock().unwrap().expires = Some(Instant::now() - Duration::from_secs(1));
    assert!(vault.field(&id, LoginField::Password).is_err());
}
#[test]
fn login_failed_validation_and_corruption_preserve_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let vault = LoginVault::new(temp.path().join("vault"));
    vault.initialize("master-password-long").unwrap();
    let before = fs::read(&vault.path).unwrap();
    let mut invalid = draft();
    invalid.title = "".into();
    assert!(vault.upsert(invalid).is_err());
    assert_eq!(fs::read(&vault.path).unwrap(), before);
    let mut envelope = vault.envelope().unwrap().unwrap();
    envelope.data.bytes[0] ^= 1;
    write_envelope(&vault.path, &envelope).unwrap();
    assert!(vault.list(None, "", None).is_err());
    let corrupted = fs::read(&vault.path).unwrap();
    assert!(LoginVault::new(vault.path.clone())
        .initialize("replacement-password")
        .is_err());
    assert_eq!(corrupted, fs::read(&vault.path).unwrap());
}

#[test]
fn login_recovery_requires_verified_backup_and_preserves_corrupt_original_ciphertext() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("vault");
    let backup = temp.path().join("backup");
    let vault = LoginVault::new(path.clone());
    vault.initialize("original-master-password").unwrap();
    vault.upsert(draft()).unwrap();
    vault
        .export_backup(&backup, "independent-backup-password")
        .unwrap();
    vault.lock();
    let original = fs::read(&path).unwrap();
    let revision = vault.storage_revision().unwrap();
    assert!(vault
        .recover_backup(
            &backup,
            "wrong-password",
            "replacement-master-password",
            &revision
        )
        .is_err());
    assert_eq!(original, fs::read(&path).unwrap());
    fs::write(&path, b"corrupt encrypted file").unwrap();
    let status = vault.status().unwrap();
    assert!(status.configured && !status.unlocked && status.error.is_some());
    assert!(vault
        .recover_backup(
            &backup,
            "independent-backup-password",
            "replacement-master-password",
            &revision
        )
        .is_err());
    let revision = vault.storage_revision().unwrap();
    assert_eq!(
        vault
            .recover_backup(
                &backup,
                "independent-backup-password",
                "replacement-master-password",
                &revision
            )
            .unwrap(),
        1
    );
    assert!(!vault.list(Some("com.apple.Safari"), "", None).unwrap()[0].matched);
    let preserved = fs::read_dir(temp.path())
        .unwrap()
        .flatten()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("logins-previous-")
        })
        .unwrap();
    assert_eq!(
        fs::read(preserved.path()).unwrap(),
        b"corrupt encrypted file"
    );
    vault.lock();
    vault.unlock("replacement-master-password").unwrap();
    assert_eq!(vault.list(None, "", None).unwrap().len(), 1);
}

#[test]
fn login_rules_match_exact_identity_and_respect_search_and_tags() {
    let temp = tempfile::tempdir().unwrap();
    let vault = LoginVault::new(temp.path().join("vault"));
    vault.initialize("master-password-long").unwrap();
    let first = vault.upsert(draft()).unwrap();
    let mut second = draft();
    second.title = "Second Account".into();
    second.apps[0].priority = 20;
    let second = vault.upsert(second).unwrap();
    let rows = vault
        .list(Some("com.apple.Safari"), "", Some("Work"))
        .unwrap();
    assert_eq!(rows[0].id, first);
    assert_eq!(rows[1].id, second);
    assert!(rows.iter().all(|r| r.matched));
    assert!(vault
        .list(Some("Safari"), "", None)
        .unwrap()
        .iter()
        .all(|r| !r.matched));
    assert_eq!(vault.list(None, "second", None).unwrap().len(), 1);
    assert!(vault.list(None, "", Some("Personal")).unwrap().is_empty());
    let mut invalid = draft();
    invalid.apps.push(invalid.apps[0].clone());
    assert!(vault.upsert(invalid).is_err());
}
