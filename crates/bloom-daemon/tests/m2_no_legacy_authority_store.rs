use bloom_daemon::Daemon;
use bloom_proto::HomeDir;

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
    drop(daemon);
}
