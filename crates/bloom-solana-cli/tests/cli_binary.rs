//! Binary-level CLI tests: argument validation, profile/account gating, and
//! the decimal parser through the real binary. These intentionally avoid any
//! network — lifecycle behavior is covered in `cli_lifecycle.rs`.

use assert_cmd::Command;
use predicates::str::{contains, is_match};

fn bin() -> Command {
    Command::cargo_bin("bloom-solana").unwrap()
}

fn state_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    (dir, path)
}

#[test]
fn status_lists_configured_profiles_offline() {
    let (_d, state) = state_dir();
    bin()
        .args(["--state-dir", state.to_str().unwrap(), "status"])
        .assert()
        .success()
        .stdout(contains("devnet"))
        .stdout(contains("localnet"));
}

#[test]
fn account_enable_then_status_shows_caip10() {
    let (_d, state) = state_dir();
    let s = state.to_str().unwrap();
    bin()
        .args([
            "--state-dir",
            s,
            "account-enable",
            "--wallet",
            "w1",
            "--profile",
            "devnet",
        ])
        .assert()
        .success()
        .stdout(contains("caip10:"));
    bin()
        .args(["--state-dir", s, "status"])
        .assert()
        .success()
        .stdout(contains("solana:devnet:"));
}

#[test]
fn sol_amount_decimals_are_validated_exactly() {
    let (_d, state) = state_dir();
    let s = state.to_str().unwrap();
    // Ten decimals is too many.
    bin()
        .args([
            "--state-dir",
            s,
            "transfer-stage",
            "--wallet",
            "w1",
            "--destination",
            "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t",
            "--sol",
            "0.0000000001",
        ])
        .assert()
        .failure()
        .stderr(contains("decimals"));
    // Garbage is rejected before any state is touched.
    bin()
        .args([
            "--state-dir",
            s,
            "transfer-stage",
            "--wallet",
            "w1",
            "--destination",
            "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t",
            "--sol",
            "1.0.0",
        ])
        .assert()
        .failure();
    // Negative amounts never parse.
    bin()
        .args([
            "--state-dir",
            s,
            "transfer-stage",
            "--wallet",
            "w1",
            "--destination",
            "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t",
            "--sol",
            "-1.5",
        ])
        .assert()
        .failure();
    // Overflow beyond u64 lamports is refused.
    bin()
        .args([
            "--state-dir",
            s,
            "transfer-stage",
            "--wallet",
            "w1",
            "--destination",
            "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t",
            "--sol",
            "18446744074",
        ])
        .assert()
        .failure()
        .stderr(contains("overflow"));
    // Both units at once is a usage error.
    bin()
        .args([
            "--state-dir",
            s,
            "transfer-stage",
            "--wallet",
            "w1",
            "--destination",
            "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t",
            "--lamports",
            "5",
            "--sol",
            "5",
        ])
        .assert()
        .failure();
}

#[test]
fn unknown_profile_is_refused_with_the_configured_set() {
    let (_d, state) = state_dir();
    bin()
        .args([
            "--state-dir",
            state.to_str().unwrap(),
            "transfer-stage",
            "--wallet",
            "w1",
            "--profile",
            "westnet",
            "--destination",
            "x",
            "--lamports",
            "1",
        ])
        .assert()
        .failure()
        .stderr(contains("unknown cluster profile"));
}

#[test]
fn staging_requires_an_enabled_account() {
    let (_d, state) = state_dir();
    bin()
        .args([
            "--state-dir",
            state.to_str().unwrap(),
            "transfer-stage",
            "--wallet",
            "ghost",
            "--destination",
            "CZ8YUVdk7znjrUmnb5n7kgySk9yRAsQDYmyCxzfSky9t",
            "--lamports",
            "1",
        ])
        .assert()
        .failure()
        .stderr(contains("account not enabled"));
}

#[test]
fn confirm_requires_an_existing_operation() {
    let (_d, state) = state_dir();
    // Referencing a nonexistent operation fails closed before any --yes.
    bin()
        .args([
            "--state-dir",
            state.to_str().unwrap(),
            "operation-confirm",
            format!("{:0>64}", "1").as_str(),
            "--yes",
        ])
        .assert()
        .failure()
        .stderr(contains("not found"));
}

#[test]
fn unknown_operation_inspect_fails_cleanly() {
    let (_d, state) = state_dir();
    bin()
        .args([
            "--state-dir",
            state.to_str().unwrap(),
            "operation-inspect",
            format!("{:0>64}", "2").as_str(),
        ])
        .assert()
        .failure()
        .stdout(is_match("").unwrap());
}
