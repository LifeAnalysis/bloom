//! Publish the verifier differential corpus as a release artifact.
//!
//! Writes the frozen golden vectors (message, digests, signature, keys) and
//! the recorded corpus digest to the target JSON file. The digest matches
//! `bloom_solana_machine::catalog::REQUIRED_VERIFIER_CORPUS_DIGEST`; any
//! change to the corpus must go through catalog review.

use std::path::Path;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| usage("missing output path"));
    let path = Path::new(&out);
    let corpus = serde_json::json!({
        "schema": "bloom.solana.verifier-corpus/1",
        "verifier_id": bloom_solana::VERIFIER_ID,
        "golden": {
            "fee_payer_base58": bloom_solana::golden::FEE_PAYER,
            "destination_base58": bloom_solana::golden::DESTINATION,
            "lamports": bloom_solana::golden::LAMPORTS,
            "blockhash_hex": bloom_solana::golden::BLOCKHASH_HEX,
            "message_hex": bloom_solana::golden::MESSAGE_HEX,
            "message_digest_hex": bloom_solana::golden::MESSAGE_DIGEST_HEX,
            "signature_hex": bloom_solana::golden::SIGNATURE_HEX,
            "signing_convention": "ed25519 over the raw serialized message bytes (no pre-hash)",
        },
        "corpus_digest": bloom_solana_machine::catalog::REQUIRED_VERIFIER_CORPUS_DIGEST,
        "coverage": [
            "golden vector reproduced by the independent codec",
            "byte-identical differential against solana-message 4.5.0 / solana-system-interface 3.3.0",
            "genuine Anza Transaction::verify over the golden signature",
            "signature fails against the SHA-256 digest (commitment-only confusion)",
            "1200 single-byte mutation digest-binding sweep",
            "economic-field mutations rejected with recomputed digest",
            "blockhash mutation remains structurally valid (machine-asserted)",
        ],
    });
    std::fs::write(path, serde_json::to_vec_pretty(&corpus).unwrap())
        .unwrap_or_else(|e| die(format!("write {}: {e}", path.display())));
    println!(
        "corpus published: {} (digest {})",
        path.display(),
        bloom_solana_machine::catalog::REQUIRED_VERIFIER_CORPUS_DIGEST
    );
}

fn usage(message: &str) -> ! {
    eprintln!("corpus_publish: {message}");
    eprintln!("usage: corpus_publish <output.json>");
    std::process::exit(2);
}

fn die(message: String) -> ! {
    eprintln!("corpus_publish: {message}");
    std::process::exit(1);
}
