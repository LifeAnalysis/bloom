//! Etherscan-backed history helpers for the `chains/` subtree.
//!
//! Split out of `chains.rs` to keep the main handler readable. All
//! functions return JSON byte payloads ready to be served by the VFS;
//! lookups for these paths are performed by `chains.rs` directly.

use std::sync::Arc;

use beth_etherscan::{EtherscanClient, EtherscanError, Sort};
use beth_proto::prelude::Address;

use crate::handler::HandlerError;

/// Default page size — 50 records is a reasonable shell-friendly size
/// and keeps free-tier Etherscan callers well below the 10k row cap.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

fn map_err(e: EtherscanError) -> HandlerError {
    match e {
        EtherscanError::Disabled => {
            HandlerError::Unsupported("etherscan endpoint not supported on this chain".into())
        }
        EtherscanError::RateLimit => HandlerError::backend("etherscan rate limited"),
        EtherscanError::Api { status, message } => {
            // "not verified" / "not found" should surface as NotFound.
            let m = message.to_ascii_lowercase();
            if m.contains("not verified") || m.contains("not found") {
                HandlerError::not_found(format!("{status}: {message}"))
            } else {
                HandlerError::backend(format!("etherscan {status}: {message}"))
            }
        }
        other => HandlerError::backend(other.to_string()),
    }
}

fn json_bytes<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, HandlerError> {
    let mut bytes =
        serde_json::to_vec_pretty(v).map_err(|e| HandlerError::backend(e.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub async fn read_txs(
    client: &Arc<EtherscanClient>,
    chain_id: u64,
    addr: Address,
) -> Result<Vec<u8>, HandlerError> {
    let txs = client
        .get_tx_list(
            chain_id,
            addr,
            0,
            99_999_999,
            1,
            DEFAULT_PAGE_SIZE,
            Sort::Desc,
        )
        .await
        .map_err(map_err)?;
    json_bytes(&txs)
}

pub async fn read_internal_txs(
    client: &Arc<EtherscanClient>,
    chain_id: u64,
    addr: Address,
) -> Result<Vec<u8>, HandlerError> {
    let txs = client
        .get_internal_tx_list(
            chain_id,
            addr,
            0,
            99_999_999,
            1,
            DEFAULT_PAGE_SIZE,
            Sort::Desc,
        )
        .await
        .map_err(map_err)?;
    json_bytes(&txs)
}

pub async fn read_erc20_txs(
    client: &Arc<EtherscanClient>,
    chain_id: u64,
    addr: Address,
) -> Result<Vec<u8>, HandlerError> {
    let txs = client
        .get_token_tx(
            chain_id,
            addr,
            None,
            0,
            99_999_999,
            1,
            DEFAULT_PAGE_SIZE,
            Sort::Desc,
        )
        .await
        .map_err(map_err)?;
    json_bytes(&txs)
}

pub async fn read_erc721_txs(
    client: &Arc<EtherscanClient>,
    chain_id: u64,
    addr: Address,
) -> Result<Vec<u8>, HandlerError> {
    let txs = client
        .get_nft_tx(
            chain_id,
            addr,
            None,
            0,
            99_999_999,
            1,
            DEFAULT_PAGE_SIZE,
            Sort::Desc,
        )
        .await
        .map_err(map_err)?;
    json_bytes(&txs)
}

pub async fn read_contract_source(
    client: &Arc<EtherscanClient>,
    chain_id: u64,
    addr: Address,
) -> Result<Vec<u8>, HandlerError> {
    let src = client
        .get_source_code(chain_id, addr)
        .await
        .map_err(map_err)?;
    json_bytes(&src)
}

pub async fn read_contract_abi(
    client: &Arc<EtherscanClient>,
    chain_id: u64,
    addr: Address,
) -> Result<Vec<u8>, HandlerError> {
    let abi = client.get_abi(chain_id, addr).await.map_err(map_err)?;
    json_bytes(&abi)
}
