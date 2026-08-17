//! Signed catalog flow: sign → verify → install through the real mount API,
//! and every rejection class through the same path.

use std::sync::Arc;

use bloom_petals::DenyHost;
use bloom_solana_machine::catalog::{build_entry_for_package, key_id_for_seed, sign_entry};
use bloom_solana_machine::mount::{
    load_trusted_catalog_keys, mount_pinned_and_catalog_verified_solana_driver,
};

const PETAL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../petals/solana-driver");

fn fixture_seed() -> [u8; 32] {
    (100..132u8).collect::<Vec<_>>().try_into().unwrap()
}

fn copy_package(tag: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("pkg");
    std::fs::create_dir_all(&target).unwrap();
    copy_dir(std::path::Path::new(PETAL), &target);
    let _ = tag;
    dir
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            if name == "target" {
                continue;
            }
            std::fs::create_dir_all(&to).unwrap();
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

fn signed_package_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = copy_package("signed");
    let pkg = dir.path().join("pkg");
    let seed = fixture_seed();
    let key_id = key_id_for_seed(&seed);
    let refs = ["devnet", "localnet"];
    let entry = build_entry_for_package(&pkg, &key_id, &refs).unwrap();
    let signed = sign_entry(&entry, &seed).unwrap();
    std::fs::write(
        pkg.join("artifacts/catalog-entry.json"),
        serde_json::to_vec_pretty(&signed).unwrap(),
    )
    .unwrap();
    (dir, pkg)
}

fn trusted_keys_file(dir: &std::path::Path) -> std::path::PathBuf {
    use base64::Engine as _;
    let seed = fixture_seed();
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let vk = ed25519_dalek::VerifyingKey::from(&key);
    let path = dir.join("trusted-keys.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&serde_json::json!([{
            "key_id": key_id_for_seed(&seed),
            "verifying_key_base64": base64::engine::general_purpose::STANDARD.encode(vk.as_bytes()),
        }]))
        .unwrap(),
    )
    .unwrap();
    path
}

#[test]
fn signed_catalog_installs_through_the_mount_api() {
    let (pkg_dir, pkg) = signed_package_dir();
    let keys_path = trusted_keys_file(pkg_dir.path());
    let keys = load_trusted_catalog_keys(&keys_path).unwrap();
    let state = tempfile::tempdir().unwrap();
    let vfs = mount_pinned_and_catalog_verified_solana_driver(
        &pkg,
        state.path(),
        Arc::new(DenyHost),
        &keys,
        &[],
    )
    .unwrap();
    let _ = vfs;
}

#[test]
fn missing_catalog_entry_fails_catalog_verified_mount() {
    // Content pin alone still works (mount_pinned_solana_driver), but the
    // catalog-verified posture refuses an unsigned package.
    let dir = copy_package("unsigned");
    let pkg = dir.path().join("pkg");
    let keys_path = trusted_keys_file(dir.path());
    let keys = load_trusted_catalog_keys(&keys_path).unwrap();
    let state = tempfile::tempdir().unwrap();
    let err = mount_pinned_and_catalog_verified_solana_driver(
        &pkg,
        state.path(),
        Arc::new(DenyHost),
        &keys,
        &[],
    )
    .err()
    .expect("unsigned package must fail catalog verification");
    let t = err.to_string();
    assert!(
        t.contains("catalog") || t.contains("No such file"),
        "got: {t}"
    );
}

#[test]
fn tampered_artifact_fails_catalog_install() {
    let (pkg_dir, pkg) = signed_package_dir();
    let keys_path = trusted_keys_file(pkg_dir.path());
    let keys = load_trusted_catalog_keys(&keys_path).unwrap();
    // Tamper with a route artifact after signing.
    let artifact = pkg.join("artifacts/routes/r000001.wasm");
    let mut bytes = std::fs::read(&artifact).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&artifact, &bytes).unwrap();
    let state = tempfile::tempdir().unwrap();
    let err = mount_pinned_and_catalog_verified_solana_driver(
        &pkg,
        state.path(),
        Arc::new(DenyHost),
        &keys,
        &[],
    )
    .err()
    .expect("tampered artifact must fail");
    assert!(err.to_string().to_lowercase().contains("artifact"));
}

#[test]
fn unknown_signing_key_is_rejected_at_mount() {
    let (pkg_dir, pkg) = signed_package_dir();
    // Trusted keys hold a DIFFERENT fixture key.
    let other_seed: [u8; 32] = [77u8; 32];
    let other = ed25519_dalek::SigningKey::from_bytes(&other_seed);
    let vk = ed25519_dalek::VerifyingKey::from(&other);
    let keys = vec![(key_id_for_seed(&other_seed), vk)];
    let state = tempfile::tempdir().unwrap();
    let err = mount_pinned_and_catalog_verified_solana_driver(
        &pkg,
        state.path(),
        Arc::new(DenyHost),
        &keys,
        &[],
    )
    .err()
    .expect("unknown key must fail");
    let t = err.to_string();
    assert!(
        t.contains("UnknownKey") || t.contains("unknown") || t.contains("catalog"),
        "got: {t}"
    );
    let _ = pkg_dir;
}

#[test]
fn in_flight_predecessor_blocks_catalog_install() {
    let (pkg_dir, pkg) = signed_package_dir();
    let keys_path = trusted_keys_file(pkg_dir.path());
    let keys = load_trusted_catalog_keys(&keys_path).unwrap();
    let state = tempfile::tempdir().unwrap();
    let in_flight =
        vec![bloom_solana_machine::mount::PINNED_SOLANA_DRIVER_PACKAGE_HASH.to_string()];
    let err = mount_pinned_and_catalog_verified_solana_driver(
        &pkg,
        state.path(),
        Arc::new(DenyHost),
        &keys,
        &in_flight,
    )
    .err()
    .expect("in-flight predecessor must block takeover");
    assert!(
        err.to_string().contains("PredecessorInFlight") || err.to_string().contains("predecessor")
    );
}
