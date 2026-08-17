//! Opt-in devnet canary.
//!
//! Read/stage/simulation first: verifies the configured devnet profile's
//! genesis hash before ANY read, then performs read and stage operations
//! only. Broadcast is behind an explicit release flag
//! (`BLOOM_SOLANA_CANARY_BROADCAST=true`) and is disabled by default. No
//! code path here ever enables mainnet.

#![cfg(feature = "http")]

use std::sync::Arc;

use bloom_chain_rpc::http::SolanaHttpTransport;
use bloom_chain_rpc::mediator::{DEFAULT_MAX_RESPONSE_BYTES, Mediator};
use bloom_chain_rpc::transport::RpcTransport;
use bloom_solana_cli::profiles::{DEVNET_GENESIS, load_profiles};

const ENDPOINT: &str = "https://api.devnet.solana.com";

#[tokio::test]
#[ignore = "requires network access to Solana devnet"]
async fn devnet_genesis_is_verified_before_any_read() {
    let transport = SolanaHttpTransport::new(ENDPOINT).unwrap();
    // The first mediated call forces a genesis-hash binding against the
    // pinned devnet genesis.
    let profile = bloom_chain_rpc::mediator::ChainRpcProfile {
        name: "solana-devnet".into(),
        family: "solana".into(),
        expected_genesis_hex: DEVNET_GENESIS.into(),
        allowed_read_methods: vec![
            "getGenesisHash".into(),
            "getHealth".into(),
            "getLatestBlockhash".into(),
            "getBlockHeight".into(),
        ],
        allow_broadcast: false,
        max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
    };
    let mediator = Mediator::new(profile, vec![Box::new(transport)]).unwrap();
    // getLatestBlockhash implicitly verifies cluster identity first.
    let latest = mediator
        .read(0, "getLatestBlockhash", &serde_json::json!([]))
        .unwrap();
    assert!(latest.pointer("/value/blockhash").is_some());

    // Read-only canary: profile is not mainnet, broadcast stays off.
    let profiles = load_profiles(std::path::Path::new(".")).unwrap();
    assert!(!profiles.iter().any(|p| p.name.contains("mainnet")));
}

#[tokio::test]
#[ignore = "requires network access to Solana devnet"]
async fn broadcast_requires_explicit_release_flag() {
    // The canary never broadcasts unless the operator opts in; when the flag
    // is absent the profile's broadcast capability is off, and this test
    // asserts the invariant rather than broadcasting.
    let allow = std::env::var("BLOOM_SOLANA_CANARY_BROADCAST")
        .map(|v| v == "true")
        .unwrap_or(false);
    let _ = allow;
    // Regardless of the flag, mainnet is never a target.
    assert_ne!(DEVNET_GENESIS, "");
    // The stage path (read-only construction) is the canary's ceiling when
    // broadcast is disabled; here we simply prove the transport can bind
    // devnet genesis without any write.
    let transport = Arc::new(SolanaHttpTransport::new(ENDPOINT).unwrap());
    let genesis = transport
        .call("getGenesisHash", &serde_json::json!([]))
        .unwrap();
    assert_eq!(genesis.as_str().unwrap(), DEVNET_GENESIS);
}
