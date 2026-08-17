//! Release catalog signing tool.
//!
//! Reads the installer lineage's Ed25519 seed from a file path (never from
//! the repository, never from arguments beyond the path), builds the catalog
//! entry for a package directory, signs it, and writes the signed entry.
//!
//! Usage:
//! ```text
//! cargo run -p bloom-solana-machine --example catalog-sign -- \
//!     --package petals/solana-driver \
//!     --seed-file /operator/vault/solana-catalog.seed \
//!     --clusters devnet,localnet \
//!     --out petals/solana-driver/artifacts/catalog-entry.json
//! ```
//!
//! The seed file must contain exactly 64 hex characters. The verification
//! key id is printed so operators can register it in the trusted-key file.

use std::path::PathBuf;

fn main() {
    let mut package = None;
    let mut seed_file = None;
    let mut clusters = vec!["devnet".to_string(), "localnet".to_string()];
    let mut out = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--package" => package = args.next().map(PathBuf::from),
            "--seed-file" => seed_file = args.next().map(PathBuf::from),
            "--clusters" => {
                clusters = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter(|c| !c.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            "--out" => out = args.next().map(PathBuf::from),
            other => {
                eprintln!("catalog-sign: unknown argument {other:?}");
                std::process::exit(2);
            }
        }
    }
    let package = package.unwrap_or_else(|| usage("--package is required"));
    let seed_file = seed_file.unwrap_or_else(|| usage("--seed-file is required"));
    let out = out.unwrap_or_else(|| package.join("artifacts/catalog-entry.json"));

    let seed_hex = std::fs::read_to_string(&seed_file)
        .unwrap_or_else(|e| die(format!("read seed file {}: {e}", seed_file.display())));
    let seed_hex = seed_hex.trim();
    if seed_hex.len() != 64 || !seed_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        die("seed file must contain exactly 64 hex characters (32-byte Ed25519 seed)".into());
    }
    let seed: [u8; 32] = hex::decode(seed_hex)
        .expect("length checked")
        .try_into()
        .expect("length checked");

    let key_id = bloom_solana_machine::catalog::key_id_for_seed(&seed);
    let refs: Vec<&str> = clusters.iter().map(String::as_str).collect();
    let entry = bloom_solana_machine::catalog::build_entry_for_package(&package, &key_id, &refs)
        .unwrap_or_else(|e| die(format!("build entry: {e}")));
    let signed = bloom_solana_machine::catalog::sign_entry(&entry, &seed)
        .unwrap_or_else(|e| die(format!("sign: {e}")));
    std::fs::write(&out, serde_json::to_vec_pretty(&signed).unwrap())
        .unwrap_or_else(|e| die(format!("write {}: {e}", out.display())));
    println!("signed catalog entry: {}", out.display());
    println!("signing key id: {key_id}");
    println!("register this key id in the operator trusted-key file to make installs verify");
}

fn usage(message: &str) -> ! {
    eprintln!("catalog-sign: {message}");
    eprintln!(
        "usage: catalog-sign --package <dir> --seed-file <path> [--clusters a,b] [--out <file>]"
    );
    std::process::exit(2);
}

fn die(message: String) -> ! {
    eprintln!("catalog-sign: {message}");
    std::process::exit(1);
}
