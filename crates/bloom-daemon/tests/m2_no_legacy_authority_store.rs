use std::fs;

use bloom_daemon::Daemon;
use bloom_proto::{HomeDir, HomeWritePermit};

#[test]
fn production_construction_does_not_create_legacy_auth_store() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("bloom-home");
    let daemon = Daemon::from_home(HomeDir::at(&root)).unwrap();

    assert!(
        !root.join("auth/auth.sqlite").exists(),
        "production Machine construction created the legacy authority database"
    );
    assert!(
        !root.join("auth").exists(),
        "production Machine construction created the legacy authority directory"
    );
    for relative in [
        "keystore",
        "approval-challenges",
        "grants",
        "policy-session",
        "signer-cache",
    ] {
        assert!(
            !root.join(relative).exists(),
            "production Machine construction created legacy state: {relative}"
        );
    }
    drop(daemon);
}

#[test]
fn degraded_construction_does_not_create_legacy_authority_state() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("bloom-home");
    let home = HomeDir::at(&root);
    let permit = std::sync::Arc::new(HomeWritePermit::acquire(&home).unwrap());
    let daemon = Daemon::from_home_with_permit(home, permit).unwrap();

    for relative in [
        "keystore",
        "auth",
        "approval-challenges",
        "grants",
        "policy-session",
        "signer-cache",
    ] {
        assert!(
            !root.join(relative).exists(),
            "Broker-unavailable degraded construction created legacy state: {relative}"
        );
    }
    drop(daemon);
}

#[test]
fn pre_existing_legacy_authority_state_is_ignored_and_unchanged() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("bloom-home");
    let sentinels = [
        (
            "keystore/alice/encrypted.key",
            b"legacy-wallet-key".as_slice(),
        ),
        ("auth/auth.sqlite", b"legacy-auth-db".as_slice()),
        (
            "approval-challenges/pending.json",
            b"legacy-approval-challenge".as_slice(),
        ),
        ("grants/active.json", b"legacy-grant".as_slice()),
        (
            "policy-session/alice/active.json",
            b"legacy-policy-session".as_slice(),
        ),
        ("signer-cache/entry", b"legacy-signer-cache".as_slice()),
    ];
    for (relative, contents) in sentinels {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    let daemon = Daemon::from_home(HomeDir::at(&root)).unwrap();

    for (relative, contents) in sentinels {
        assert_eq!(
            fs::read(root.join(relative)).unwrap(),
            contents,
            "Machine opened, migrated, or rewrote obsolete authority state: {relative}"
        );
    }
    assert!(
        daemon.wallet_projections.cached_wallets().is_err(),
        "legacy keystore content must not seed the public wallet projection"
    );
}
