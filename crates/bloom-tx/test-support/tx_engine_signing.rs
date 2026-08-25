use alloy::primitives::{B256, Signature};
use alloy::signers::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use bloom_broker_api::{Base64UrlBytes, CryptoSuite, NormalizedSignature};

use super::{ChainClient, StagedTx, TxEngine};

const TEST_SIGNER_PK: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

fn signer() -> PrivateKeySigner {
    TEST_SIGNER_PK
        .parse()
        .expect("valid deterministic test key")
}

pub(super) fn sign_hash(hash: &B256) -> Signature {
    signer().sign_hash_sync(hash).expect("test hash signs")
}

pub(super) fn normalized_signature(
    payload: &[u8],
    crypto_suite: CryptoSuite,
) -> NormalizedSignature {
    let signature = sign_hash(&alloy::primitives::keccak256(payload));
    NormalizedSignature {
        crypto_suite,
        bytes: Base64UrlBytes::from_bytes(&signature.as_bytes()),
    }
}

pub(super) fn transaction_signature(
    engine: &TxEngine,
    staged: &StagedTx,
    chain: &ChainClient,
) -> Signature {
    let unsigned = engine
        .build_unsigned_evm_tx(staged, chain)
        .expect("test transaction builds");
    sign_hash(&TxEngine::unsigned_signing_hash(&unsigned))
}
