//! `chains/<chain>/` — read-only chain views.
//!
//! Subset implemented for v1:
//! - `chains/<chain>/chain_id`
//! - `chains/<chain>/head/number`
//! - `chains/<chain>/head/hash`
//! - `chains/<chain>/head/timestamp`
//! - `chains/<chain>/head/full.json`
//! - `chains/<chain>/blocks/<n>/full.json`
//! - `chains/<chain>/addresses/<addr>/balance` (wei, decimal)
//! - `chains/<chain>/addresses/<addr>/balance.eth`
//! - `chains/<chain>/addresses/<addr>/nonce`
//! - `chains/<chain>/addresses/<addr>/code` (hex bytecode)
//! - `chains/<chain>/addresses/<addr>/tokens/<token>/{balance,balance.raw,balance.formatted,symbol,decimals}`
//! - `chains/<chain>/tx/<hash>/{receipt.json,status,block_number,gas_used,logs.json,full.json}`
//! - `chains/<chain>/gas/current.json`
//!
//! Etherscan-backed (only mounted when an etherscan client is provided):
//! - `chains/<chain>/addresses/<addr>/txs` — recent native txs
//! - `chains/<chain>/addresses/<addr>/internal_txs` — internal txs
//! - `chains/<chain>/addresses/<addr>/erc20_txs` — ERC-20 transfers
//! - `chains/<chain>/addresses/<addr>/erc721_txs` — ERC-721 transfers
//! - `chains/<chain>/contracts/<addr>/source` — verified source
//! - `chains/<chain>/contracts/<addr>/abi` — verified ABI

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use beth_chain::{ChainClient, ChainRegistry};
use beth_etherscan::EtherscanClient;
use beth_proto::{checksum_address, format_units};

use crate::handler::{Entry, Handler, HandlerError};
use crate::path::VfsPath;

use super::chains_history;

#[derive(Clone)]
pub struct ChainsHandler {
    pub registry: ChainRegistry,
    pub etherscan: Option<Arc<EtherscanClient>>,
}

impl ChainsHandler {
    pub fn new(registry: ChainRegistry) -> Self {
        Self {
            registry,
            etherscan: None,
        }
    }

    /// Builder: attach an Etherscan client. Without one, the etherscan-backed
    /// paths return `NotFound` and existing chain reads are unaffected.
    pub fn with_etherscan(mut self, client: Option<Arc<EtherscanClient>>) -> Self {
        self.etherscan = client;
        self
    }

    fn client(&self, name: &str) -> Result<ChainClient, HandlerError> {
        self.registry
            .get(name)
            .ok_or_else(|| HandlerError::not_found(format!("chain '{}'", name)))
    }

    fn etherscan_or_404(&self) -> Result<&Arc<EtherscanClient>, HandlerError> {
        self.etherscan
            .as_ref()
            .ok_or_else(|| HandlerError::not_found("etherscan not configured"))
    }
}

fn parse_addr(s: &str) -> Result<alloy::primitives::Address, HandlerError> {
    s.parse::<alloy::primitives::Address>()
        .map_err(|e| HandlerError::invalid(format!("address: {}", e)))
}

fn err_be(e: impl std::fmt::Display) -> HandlerError {
    HandlerError::backend(e.to_string())
}

/// Files exposed under `addresses/<addr>/`. Etherscan-backed entries are
/// flagged so we only emit them when an etherscan client is configured.
const ADDRESS_FILES_CORE: &[&str] = &[
    "balance",
    "balance.eth",
    "balance.raw",
    "nonce",
    "code",
    "is_contract",
];
const ADDRESS_FILES_ETHERSCAN: &[&str] = &["txs", "internal_txs", "erc20_txs", "erc721_txs"];

const CONTRACT_FILES_ETHERSCAN: &[&str] = &["source", "abi"];

const TX_FILES: &[&str] = &[
    "receipt.json",
    "status",
    "block_number",
    "gas_used",
    "logs.json",
    "full.json",
];

const TOKEN_FILES: &[&str] = &[
    "balance",
    "balance.raw",
    "balance.formatted",
    "symbol",
    "decimals",
];

#[async_trait]
impl Handler for ChainsHandler {
    async fn lookup(&self, path: &VfsPath) -> Result<Entry, HandlerError> {
        let segs = path.segments();
        if segs.is_empty() {
            return Ok(Entry::dir(""));
        }
        let chain = &segs[0];
        let _client = self.client(chain)?;
        if segs.len() == 1 {
            return Ok(Entry::dir(chain));
        }
        match segs[1].as_str() {
            "chain_id" if segs.len() == 2 => Ok(Entry::file("chain_id")),
            "head" => match segs.get(2).map(|s| s.as_str()) {
                None => Ok(Entry::dir("head")),
                Some("number") | Some("hash") | Some("timestamp") | Some("full.json") => {
                    Ok(Entry::file(segs.last().unwrap()))
                }
                _ => Err(HandlerError::not_found(path.to_string_path())),
            },
            "blocks" => match segs.len() {
                2 => Ok(Entry::dir("blocks")),
                3 => Ok(Entry::dir(&segs[2])),
                4 if segs[3] == "full.json" => Ok(Entry::file("full.json")),
                _ => Err(HandlerError::not_found(path.to_string_path())),
            },
            "addresses" => match segs.len() {
                2 => Ok(Entry::dir("addresses")),
                3 => Ok(Entry::dir(&segs[2])),
                4 => {
                    let f = segs[3].as_str();
                    if ADDRESS_FILES_CORE.contains(&f) {
                        Ok(Entry::file(f))
                    } else if ADDRESS_FILES_ETHERSCAN.contains(&f) {
                        // Only expose when etherscan is configured.
                        self.etherscan_or_404()?;
                        Ok(Entry::file(f))
                    } else if f == "tokens" {
                        Ok(Entry::dir(f))
                    } else {
                        Err(HandlerError::not_found(path.to_string_path()))
                    }
                }
                5 if segs[3] == "tokens" => Ok(Entry::dir(&segs[4])),
                6 if segs[3] == "tokens" => {
                    let f = segs[5].as_str();
                    if TOKEN_FILES.contains(&f) {
                        Ok(Entry::file(f))
                    } else {
                        Err(HandlerError::not_found(path.to_string_path()))
                    }
                }
                _ => Err(HandlerError::not_found(path.to_string_path())),
            },
            "tx" => match segs.len() {
                2 => Ok(Entry::dir("tx")),
                3 => Ok(Entry::dir(&segs[2])),
                4 => {
                    let f = segs[3].as_str();
                    if TX_FILES.contains(&f) {
                        Ok(Entry::file(f))
                    } else {
                        Err(HandlerError::not_found(path.to_string_path()))
                    }
                }
                _ => Err(HandlerError::not_found(path.to_string_path())),
            },
            "contracts" => match segs.len() {
                2 => Ok(Entry::dir("contracts")),
                3 => {
                    self.etherscan_or_404()?;
                    Ok(Entry::dir(&segs[2]))
                }
                4 => {
                    let f = segs[3].as_str();
                    if !CONTRACT_FILES_ETHERSCAN.contains(&f) {
                        return Err(HandlerError::not_found(path.to_string_path()));
                    }
                    self.etherscan_or_404()?;
                    Ok(Entry::file(f))
                }
                _ => Err(HandlerError::not_found(path.to_string_path())),
            },
            "gas" => match segs.get(2).map(|s| s.as_str()) {
                None => Ok(Entry::dir("gas")),
                Some("current.json") => Ok(Entry::file("current.json")),
                _ => Err(HandlerError::not_found(path.to_string_path())),
            },
            _ => Err(HandlerError::not_found(path.to_string_path())),
        }
    }

    async fn read(&self, path: &VfsPath) -> Result<Vec<u8>, HandlerError> {
        let segs = path.segments();
        if segs.is_empty() {
            return Err(HandlerError::NotAFile(path.to_string_path()));
        }
        let chain = &segs[0];
        let client = self.client(chain)?;
        match segs.get(1).map(|s| s.as_str()).unwrap_or("") {
            "chain_id" => {
                let id = client.chain_id().await.map_err(err_be)?;
                Ok(format!("{}\n", id).into_bytes())
            }
            "head" => {
                let block = client
                    .block_latest()
                    .await
                    .map_err(err_be)?
                    .ok_or_else(|| HandlerError::backend("no head block"))?;
                match segs.get(2).map(|s| s.as_str()).unwrap_or("") {
                    "number" => Ok(format!("{}\n", block.header.number).into_bytes()),
                    "hash" => Ok(format!("{:#x}\n", block.header.hash).into_bytes()),
                    "timestamp" => Ok(format!("{}\n", block.header.timestamp).into_bytes()),
                    "full.json" => Ok(serde_json::to_vec_pretty(&block).map_err(err_be)?),
                    _ => Err(HandlerError::NotAFile(path.to_string_path())),
                }
            }
            "blocks" if segs.len() == 4 && segs[3] == "full.json" => {
                let n: u64 = segs[2]
                    .parse()
                    .map_err(|_| HandlerError::invalid("block number"))?;
                let block = client
                    .block_by_number(n)
                    .await
                    .map_err(err_be)?
                    .ok_or_else(|| HandlerError::not_found(format!("block {}", n)))?;
                Ok(serde_json::to_vec_pretty(&block).map_err(err_be)?)
            }
            "addresses" if segs.len() == 4 => {
                let addr = parse_addr(&segs[2])?;
                let spec = client.spec();
                match segs[3].as_str() {
                    "balance" | "balance.raw" => {
                        let bal = client.balance(addr).await.map_err(err_be)?;
                        Ok(format!("{}\n", bal).into_bytes())
                    }
                    "balance.eth" => {
                        let bal = client.balance(addr).await.map_err(err_be)?;
                        Ok(format!(
                            "{} {}\n",
                            format_units(bal, spec.native_decimals),
                            spec.native_symbol
                        )
                        .into_bytes())
                    }
                    "nonce" => {
                        let n = client.nonce(addr).await.map_err(err_be)?;
                        Ok(format!("{}\n", n).into_bytes())
                    }
                    "code" => {
                        let code = client.code(addr).await.map_err(err_be)?;
                        Ok(format!("0x{}\n", hex::encode(&code)).into_bytes())
                    }
                    "is_contract" => {
                        let code = client.code(addr).await.map_err(err_be)?;
                        Ok(format!("{}\n", !code.is_empty()).into_bytes())
                    }
                    "txs" => {
                        let es = self.etherscan_or_404()?;
                        chains_history::read_txs(es, spec.chain_id, addr).await
                    }
                    "internal_txs" => {
                        let es = self.etherscan_or_404()?;
                        chains_history::read_internal_txs(es, spec.chain_id, addr).await
                    }
                    "erc20_txs" => {
                        let es = self.etherscan_or_404()?;
                        chains_history::read_erc20_txs(es, spec.chain_id, addr).await
                    }
                    "erc721_txs" => {
                        let es = self.etherscan_or_404()?;
                        chains_history::read_erc721_txs(es, spec.chain_id, addr).await
                    }
                    _ => Err(HandlerError::NotAFile(path.to_string_path())),
                }
            }
            "addresses" if segs.len() == 6 && segs[3] == "tokens" => {
                let holder = parse_addr(&segs[2])?;
                let token = parse_addr(&segs[4])?;
                match segs[5].as_str() {
                    "balance" | "balance.raw" => {
                        let bal = client
                            .erc20_balance(token, holder)
                            .await
                            .map_err(err_be)?
                            .ok_or_else(|| HandlerError::backend("erc20 balanceOf reverted"))?;
                        Ok(format!("{}\n", bal).into_bytes())
                    }
                    "balance.formatted" => {
                        let bal = client
                            .erc20_balance(token, holder)
                            .await
                            .map_err(err_be)?
                            .ok_or_else(|| HandlerError::backend("erc20 balanceOf reverted"))?;
                        let dec = client
                            .erc20_decimals(token)
                            .await
                            .map_err(err_be)?
                            .unwrap_or(18);
                        let sym = client.erc20_symbol(token).await.map_err(err_be)?;
                        Ok(format!(
                            "{} {}\n",
                            format_units(bal, dec),
                            sym.unwrap_or_else(|| "?".into())
                        )
                        .into_bytes())
                    }
                    "symbol" => {
                        let sym = client.erc20_symbol(token).await.map_err(err_be)?;
                        Ok(format!("{}\n", sym.unwrap_or_default()).into_bytes())
                    }
                    "decimals" => {
                        let dec = client
                            .erc20_decimals(token)
                            .await
                            .map_err(err_be)?
                            .unwrap_or(18);
                        Ok(format!("{}\n", dec).into_bytes())
                    }
                    _ => Err(HandlerError::NotAFile(path.to_string_path())),
                }
            }
            "tx" if segs.len() == 4 => {
                use alloy::primitives::B256;
                let hash = segs[2]
                    .parse::<B256>()
                    .map_err(|e| HandlerError::invalid(format!("tx hash: {e}")))?;
                match segs[3].as_str() {
                    "full.json" => {
                        let tx = client
                            .tx_by_hash(hash)
                            .await
                            .map_err(err_be)?
                            .ok_or_else(|| HandlerError::not_found(format!("tx {hash:#x}")))?;
                        Ok(serde_json::to_vec_pretty(&tx).map_err(err_be)?)
                    }
                    "receipt.json" => {
                        let r =
                            client.receipt(hash).await.map_err(err_be)?.ok_or_else(|| {
                                HandlerError::not_found(format!("receipt {hash:#x}"))
                            })?;
                        Ok(serde_json::to_vec_pretty(&r).map_err(err_be)?)
                    }
                    "status" => {
                        let r =
                            client.receipt(hash).await.map_err(err_be)?.ok_or_else(|| {
                                HandlerError::not_found(format!("receipt {hash:#x}"))
                            })?;
                        let s = if r.status() { "success" } else { "reverted" };
                        Ok(format!("{}\n", s).into_bytes())
                    }
                    "block_number" => {
                        let r =
                            client.receipt(hash).await.map_err(err_be)?.ok_or_else(|| {
                                HandlerError::not_found(format!("receipt {hash:#x}"))
                            })?;
                        Ok(format!("{}\n", r.block_number.unwrap_or(0)).into_bytes())
                    }
                    "gas_used" => {
                        let r =
                            client.receipt(hash).await.map_err(err_be)?.ok_or_else(|| {
                                HandlerError::not_found(format!("receipt {hash:#x}"))
                            })?;
                        Ok(format!("{}\n", r.gas_used).into_bytes())
                    }
                    "logs.json" => {
                        let r =
                            client.receipt(hash).await.map_err(err_be)?.ok_or_else(|| {
                                HandlerError::not_found(format!("receipt {hash:#x}"))
                            })?;
                        Ok(serde_json::to_vec_pretty(&r.inner.logs()).map_err(err_be)?)
                    }
                    _ => Err(HandlerError::NotAFile(path.to_string_path())),
                }
            }
            "contracts" if segs.len() == 4 => {
                let addr = parse_addr(&segs[2])?;
                let spec = client.spec();
                let es = self.etherscan_or_404()?;
                match segs[3].as_str() {
                    "source" => chains_history::read_contract_source(es, spec.chain_id, addr).await,
                    "abi" => chains_history::read_contract_abi(es, spec.chain_id, addr).await,
                    _ => Err(HandlerError::NotAFile(path.to_string_path())),
                }
            }
            "gas" if segs.get(2).map(|s| s.as_str()) == Some("current.json") => {
                let gp = client.gas_price().await.map_err(err_be)?;
                let body = serde_json::json!({ "gas_price_wei": gp });
                Ok(serde_json::to_vec_pretty(&body).unwrap())
            }
            _ => Err(HandlerError::NotAFile(path.to_string_path())),
        }
    }

    async fn list(&self, path: &VfsPath) -> Result<Vec<Entry>, HandlerError> {
        let segs = path.segments();
        if segs.is_empty() {
            return Ok(self
                .registry
                .list_names()
                .into_iter()
                .map(|n| Entry::dir(&n))
                .collect());
        }
        let chain = &segs[0];
        let _client = self.client(chain)?;
        match segs.len() {
            1 => {
                let mut entries = vec![
                    Entry::file("chain_id"),
                    Entry::dir("head"),
                    Entry::dir("blocks"),
                    Entry::dir("addresses"),
                    Entry::dir("tx"),
                    Entry::dir("gas"),
                ];
                if self.etherscan.is_some() {
                    entries.push(Entry::dir("contracts"));
                }
                Ok(entries)
            }
            2 if segs[1] == "head" => Ok(vec![
                Entry::file("number"),
                Entry::file("hash"),
                Entry::file("timestamp"),
                Entry::file("full.json"),
            ]),
            2 if segs[1] == "gas" => Ok(vec![Entry::file("current.json")]),
            3 if segs[1] == "addresses" => {
                // /chains/<chain>/addresses/<addr>
                let mut entries: Vec<Entry> =
                    ADDRESS_FILES_CORE.iter().map(|n| Entry::file(n)).collect();
                entries.push(Entry::dir("tokens"));
                if self.etherscan.is_some() {
                    for n in ADDRESS_FILES_ETHERSCAN {
                        entries.push(Entry::file(n));
                    }
                }
                Ok(entries)
            }
            5 if segs[1] == "addresses" && segs[3] == "tokens" => {
                // /chains/<chain>/addresses/<addr>/tokens/<token>
                Ok(TOKEN_FILES.iter().map(|n| Entry::file(n)).collect())
            }
            3 if segs[1] == "tx" => {
                // /chains/<chain>/tx/<hash>
                Ok(TX_FILES.iter().map(|n| Entry::file(n)).collect())
            }
            3 if segs[1] == "contracts" => {
                // /chains/<chain>/contracts/<addr>
                if self.etherscan.is_none() {
                    return Err(HandlerError::not_found(path.to_string_path()));
                }
                Ok(CONTRACT_FILES_ETHERSCAN
                    .iter()
                    .map(|n| Entry::file(n))
                    .collect())
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Per-path TTLs. The router consults this before dispatching the
    /// read; `None` means "always go to the handler". We keep TTLs
    /// short for live data (head, balance, nonce) and longer for
    /// immutable data (chain id, mined tx receipt, etherscan-backed
    /// txs) that doesn't change in practice.
    fn cache_ttl(&self, path: &VfsPath) -> Option<Duration> {
        let segs = path.segments();
        if segs.is_empty() {
            return None;
        }
        match segs.get(1).map(|s| s.as_str()).unwrap_or("") {
            "chain_id" => Some(Duration::from_secs(86_400)),
            "head" => Some(Duration::from_secs(1)),
            "gas" => Some(Duration::from_secs(2)),
            // `tx/<hash>/...` — once mined these never change, so a
            // generous 60s TTL keeps us off the RPC during burst polling.
            "tx" => Some(Duration::from_secs(60)),
            // Address-scoped reads: balance/nonce/code change with the
            // chain head. Etherscan-backed history is rate-limited so
            // we cache it longer.
            "addresses" => match segs.get(3).map(|s| s.as_str()) {
                Some("balance" | "balance.eth" | "balance.raw" | "nonce") => {
                    Some(Duration::from_secs(5))
                }
                Some("code" | "is_contract") => Some(Duration::from_secs(86_400)),
                Some("txs" | "internal_txs" | "erc20_txs" | "erc721_txs") => {
                    Some(Duration::from_secs(30))
                }
                _ => None,
            },
            // Verified source / ABI: effectively immutable.
            "contracts" => Some(Duration::from_secs(7 * 86_400)),
            // Block by number is permanent past finality; we don't know
            // finality here so use a long but bounded TTL.
            "blocks" => Some(Duration::from_secs(300)),
            _ => None,
        }
    }
}

// silence unused `checksum_address` lint while still keeping it exported
const _: fn(&alloy::primitives::Address) -> String = checksum_address;

#[cfg(test)]
mod tests {
    use super::*;
    use beth_proto::ChainSpec;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use url::Url;

    /// Spawn a one-shot HTTP server that returns `body` for the next
    /// connection. Mirrors the prices handler test pattern.
    async fn spawn_canned(body: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                loop {
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        addr
    }

    fn anvil_registry() -> ChainRegistry {
        let spec = ChainSpec::anvil_default();
        let client = ChainClient::new(spec).unwrap();
        let reg = ChainRegistry::default();
        reg.add(client);
        reg
    }

    fn etherscan_to(addr: SocketAddr) -> Arc<EtherscanClient> {
        let url = Url::parse(&format!("http://{addr}/api")).unwrap();
        Arc::new(EtherscanClient::with_base_url("test_key".into(), url))
    }

    #[tokio::test]
    async fn txs_path_returns_etherscan_payload() {
        // Realistic txlist response (single record).
        let body = r#"{"status":"1","message":"OK","result":[{
            "blockNumber":"19000000",
            "timeStamp":"1700000000",
            "hash":"0xabc",
            "nonce":"1",
            "blockHash":"0xbb",
            "transactionIndex":"0",
            "from":"0x0000000000000000000000000000000000000001",
            "to":"0x0000000000000000000000000000000000000002",
            "value":"1000",
            "gas":"21000",
            "gasPrice":"1",
            "isError":"0",
            "txreceipt_status":"1",
            "input":"0x",
            "contractAddress":"",
            "cumulativeGasUsed":"21000",
            "gasUsed":"21000",
            "confirmations":"5",
            "methodId":"",
            "functionName":""
        }]}"#;
        let addr = spawn_canned(body).await;
        let h = ChainsHandler::new(anvil_registry()).with_etherscan(Some(etherscan_to(addr)));

        let chain_name = h.registry.list_names()[0].clone();
        let p = VfsPath::parse(&format!(
            "/{chain}/addresses/0x0000000000000000000000000000000000000001/txs",
            chain = chain_name
        ))
        .unwrap();

        let bytes = h.read(&p).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["hash"], "0xabc");
        assert_eq!(v[0]["from"], "0x0000000000000000000000000000000000000001");
        // Trailing newline for shell ergonomics.
        assert_eq!(*bytes.last().unwrap(), b'\n');
    }

    #[tokio::test]
    async fn contract_abi_path_returns_decoded_array() {
        let body = r#"{"status":"1","message":"OK","result":"[{\"type\":\"function\",\"name\":\"foo\"}]"}"#;
        let addr = spawn_canned(body).await;
        let h = ChainsHandler::new(anvil_registry()).with_etherscan(Some(etherscan_to(addr)));

        let chain_name = h.registry.list_names()[0].clone();
        let p = VfsPath::parse(&format!(
            "/{chain}/contracts/0x0000000000000000000000000000000000000001/abi",
            chain = chain_name
        ))
        .unwrap();

        let bytes = h.read(&p).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array());
        assert_eq!(v[0]["name"], "foo");
    }

    #[tokio::test]
    async fn history_paths_404_when_etherscan_absent() {
        let h = ChainsHandler::new(anvil_registry());
        let chain_name = h.registry.list_names()[0].clone();

        let p = VfsPath::parse(&format!(
            "/{chain}/addresses/0x0000000000000000000000000000000000000001/txs",
            chain = chain_name
        ))
        .unwrap();
        match h.lookup(&p).await {
            Err(HandlerError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }

        let p = VfsPath::parse(&format!(
            "/{chain}/contracts/0x0000000000000000000000000000000000000001/source",
            chain = chain_name
        ))
        .unwrap();
        match h.lookup(&p).await {
            Err(HandlerError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn core_paths_unaffected_without_etherscan() {
        // Sanity: existing behaviour must work when etherscan is None.
        let h = ChainsHandler::new(anvil_registry());
        let chain_name = h.registry.list_names()[0].clone();
        let p = VfsPath::parse(&format!("/{chain_name}/chain_id")).unwrap();
        let entry = h.lookup(&p).await.unwrap();
        assert_eq!(entry.name, "chain_id");
    }
}
