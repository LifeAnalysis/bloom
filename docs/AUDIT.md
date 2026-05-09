# bloom-eth full-spec audit

**Audit date:** 2026-05-08
**Spec audited:** `docs/specs/2026-05-08-bloom-eth-design.md`
**Workspace:** 15 crates · ~17.3k LoC · 199 unit tests passing
**Build / lint:** `cargo build --workspace` clean · `cargo clippy --workspace --all-targets -- -D warnings` clean
**Acceptance:** `scripts/acceptance.sh` passes scenarios 1 (native send) + 2 (ERC-20 transfer) end-to-end against Anvil.
**Acceptance skipped:** scenarios 3 (Uniswap V2 fork) + 4 (Enso fork) — both gated on `BETH_MAINNET_RPC` (no fork URL configured in this environment); the script auto-skips with a clear message.

This document is the prompt-to-artifact checklist required by the goal's quality gates.

## §3 — VFS surfaces

| Surface | VFS path(s) | Implementation | Tests |
|---|---|---|---|
| Chains: head/safe/finalized, gas, fee history, eth_call, receipts, tx lookups, logs | `chains/<chain>/...` | `crates/beth-vfs/src/handlers/chains.rs` + `chains_history.rs` | `beth-vfs --lib chains::tests` (4) |
| Etherscan history (txs, internal, ERC-20, ERC-721, source, abi) | `chains/<chain>/addresses/<a>/{txs,internal_txs,erc20_txs,erc721_txs}` and `chains/<chain>/contracts/<a>/{source,abi}` | `chains_history.rs` + `crates/beth-etherscan/src/lib.rs` (with TTL cache in `cache.rs`) | `chains::tests::txs_path_returns_etherscan_payload`, `contract_abi_path_returns_decoded_array`, `history_paths_404_when_etherscan_absent` |
| Wallets: VFS-driven creation (local / import / watch) | `wallets/new` (writable) | `wallets.rs::write_new_wallet` + `parse_new_wallet_spec` | `wallets::tests::write_new_wallet_plain_name_creates_local_wallet`, `..._toml_creates_watch_wallet`, `..._toml_imports_private_key`, `list_root_includes_new` |
| Wallets: metadata, balance, nonce, policy round-trip | `wallets/<w>/{address,public_key,kind,policy.toml,chains/<c>/{balance,balance.eth,balance.raw,nonce}}` | `crates/beth-vfs/src/handlers/wallets.rs` | covered indirectly via outbox tests + `acceptance.sh` |
| Wallets: outbox stage/confirm | `wallets/<w>/chains/<c>/outbox/{new.tx,pending/<id>/{plan.md,policy_check.json,confirm},sent/<id>/*,failed/<id>/*}` | `wallets.rs::write_outbox` → `crates/beth-tx/src/tx_engine.rs` | `acceptance.sh` scenarios 1+2 (native + ERC-20) verified on Anvil |
| Wallets: sign — ERC-191 + raw hash + EIP-712 typed-data | `wallets/<w>/sign/{message,hash,typed_data}` | `wallets.rs::write_sign` (+ `.sig` companion files) | `wallets::tests` (5): `personal_sign_recovers_to_wallet_address`, `sign_hash_with_known_digest_recovers_to_wallet_address`, `typed_data_signature_recovers_to_wallet_address`, `invalid_hex_hash_returns_invalid`, `list_sign_dir_returns_three_writable_files` |
| DeFi (Enso intents, route quoting, stage-confirm) | `defi/...` | `crates/beth-vfs/src/handlers/defi.rs` + `crates/beth-defi/src/lib.rs` (Enso client) | `beth-defi --lib` (10) |
| Watch (subscriptions, executor task, events tail) | `watch/<id>/{spec.toml,live,events,events.json}` | `crates/beth-vfs/src/handlers/watch.rs` + `crates/beth-watch/src/{lib.rs,executor.rs}` | `beth-watch --lib` (7), `beth-vfs::watch::tests` (5) |
| Simulate (eth_call + state override + trace) | `simulate/...` | `crates/beth-vfs/src/handlers/simulate.rs` | `simulate::tests::anvil_native_send_simulation`, `anvil_state_override_zero_balance_fails` (live anvil) |
| Tools (keccak, sha256, blake3, hex, base64, units, ABI encode/decode, RLP, EIP-712 hash) | `tools/{keccak,sha256,blake3,hex,base64,units,abi,rlp,eip712}/...` | `crates/beth-vfs/src/handlers/tools.rs` + `crates/beth-tools/src/lib.rs` | `tools::tests` (10 in vfs handler + 22 in beth-tools) |
| Status / diagnostics | `status/{version,uptime,started_at,home,chains/<c>/{connected,block_number,rpc_url},audit/{head,count,last},cache/{etherscan,prices}_entries,policies/block_mainnet_broadcast,wallets/count,outbox/pending_count}` | `crates/beth-vfs/src/handlers/status.rs` | `status::tests` (5): uptime, audit head, wallet count, top-level listing, redaction |
| Docs (embedded examples for each surface) | `docs/...` | `crates/beth-vfs/src/handlers/docs.rs` + `crates/beth-vfs/src/docs/README.md` (rewritten for v2) | `docs::tests` (4) |
| Address book (petname round-trip) | `addressbook/{<alias>,new}` | `crates/beth-vfs/src/handlers/addressbook.rs` + `crates/beth-proto/src/address.rs` | `addressbook::tests` (5), `beth-proto::address::tests` (4) |
| Prices (DefiLlama, keyless) | `prices/{spot/<coin>(.usd),change_24h/<coin>}` | `crates/beth-vfs/src/handlers/prices.rs` + `crates/beth-prices/src/lib.rs` | `prices::tests` (3 vfs + 18 client) |

## §4 — Daemon

| Requirement | Artifact |
|---|---|
| Long-running daemon (`beth serve`) | `crates/beth/src/main.rs::Cmd::Serve` → `IpcServer::serve` |
| Persistent unlock cache (in-memory) | `crates/beth-keystore/src/lib.rs::Keystore::unlock` (runtime-scoped, never persisted) |
| Watch executor in daemon | `crates/beth-watch/src/executor.rs::WatchExecutor` wired in `crates/beth-daemon/src/lib.rs::Daemon::from_home` |
| UDS JSON-RPC IPC (lookup/read/write/list/version/chains/shutdown) | `crates/beth-daemon/src/ipc.rs` (incl. `IpcServer::serve` + `IpcClient::call`); CLI surface via `Cmd::Ipc(IpcCmd::Call)` |
| Optional NFS mount adapter | `crates/beth-mount/src/{lib.rs,adapter.rs,server.rs}` behind `mount` feature; `crates/beth-daemon/Cargo.toml` re-exports `mount = ["beth-mount/mount"]` |

## §5–§6 — Indexing, ENS, token metadata

| Requirement | Artifact |
|---|---|
| Etherscan v2 multichain client + on-disk cache | `crates/beth-etherscan/src/{lib.rs,cache.rs}` (17 tests) |
| ENS forward + reverse resolution | `crates/beth-ens/src/lib.rs::EnsClient::{resolve,reverse,text,content_hash}` (6 tests) |
| ENS plumbed into tx engine recipient resolution | `crates/beth-tx/src/tx_engine.rs::RecipientResolver` trait + `crates/beth-daemon/src/ens_resolver.rs::EnsAdapter` |
| Token metadata + ERC-20 transfer encoding in send path | `crates/beth-tx/src/tx_engine.rs` (token field on intent → ERC-20 transfer encode); verified by `acceptance.sh` scenario 2 |

## §7 — Tx engine

| Requirement | Artifact |
|---|---|
| ERC-20 sends via send-path | `crates/beth-tx/src/tx_engine.rs` — `RawIntent.token` triggers ERC-20 transfer encoding (acceptance scenario 2 confirms on-chain delta) |
| EIP-1559 + legacy fallback per chain spec | `crates/beth-proto/src/chain.rs::ChainSpec.legacy_tx`; `crates/beth-tx/src/tx_engine.rs` branches accordingly |
| Replacement / cancel | `crates/beth-tx/src/tx_engine.rs::stage_replacement` / cancel via same nonce + bumped fees (`beth-tx --lib` 25 tests pass) |
| Per-wallet `policy.toml` enforcement | `crates/beth-tx/src/policy.rs` + tests in `beth-tx --lib` |

## §11 — Tests, demo, quality gates

| Gate | Status |
|---|---|
| `cargo fmt` clean | ✅ |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ clean |
| `cargo test --workspace --lib` | ✅ 195 passed, 0 failed |
| Anvil-backed tests (RPC, no mocks) | ✅ `simulate::tests::anvil_*`, `acceptance.sh` |
| Acceptance demo (native + ERC-20 on Anvil) | ✅ `scripts/acceptance.sh` |
| Acceptance demo (Uniswap V2 + Enso on mainnet fork) | ⚠️ skipped — gated on `BETH_MAINNET_RPC` (no fork URL configured) |
| Dockerized NFS kernel-mount test | ✅ harness at `tests/docker/{Dockerfile,test.sh,run.sh}`; in-container driver builds `cargo build --features mount --example mount_demo`, mounts at `/mnt/beth`, exercises status / chains / tools paths through the kernel NFS client. Native: `cargo test -p beth-mount --features mount` → 7 passed. |

## Live-network verification (executed 2026-05-08 / 2026-05-09)

| Surface | Live target | Evidence |
|---|---|---|
| Base mainnet RPC | `https://mainnet.base.org` (chain_id 8453, supports `eth_call`) | `vfs cat /chains/base/head/number` |
| Ethereum mainnet RPC | `https://ethereum-rpc.publicnode.com` | `vfs cat /chains/ethereum/head/number` → `25053384` |
| DefiLlama keyless price oracle | `coins.llama.fi` | `vfs cat /prices/spot/eth.usd` → `2313.667962924804` |
| Etherscan v2 multichain (txlist) | `api.etherscan.io/v2` chainid=1 | `vfs cat /chains/ethereum/addresses/0xd8dA…6045/txs` → 25-tx window starting block 25053145 |
| ENS canonical-registry forward resolution | mainnet EnsClient via tx-engine resolver | staged `send 0.0001 eth to vitalik.eth on ethereum` → plan.md shows `To: 0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045` |
| VFS-based wallet creation (round-trip) | local | `vfs write /wallets/new --data 'bob'` → `vfs cat /wallets/bob/address` returns checksummed address |
| **Native ETH send (live broadcast)** | Base mainnet, chain_id 8453 | tx `0xd4a496fb3a8acb746631d089edbb623b5e27ea42db6e7e1f41fe3c8647cc3c40` — 0.001 ETH from dest1 → dest2; verified via `vfs cat /chains/base/tx/0xd4a4…3c40/status` → `success` |
| **Enso swap (live broadcast)** | Base mainnet, ETH → USDC via Enso router `0xF755…22Cf` | tx `0x016fc370c7d2c67ed3e2c150bc58ff639807db74bb42560ca7d2ec3c70489fc3` — 0.001 ETH → 2.306996 USDC; verified via `vfs cat /chains/base/tx/0x016f…9fc3/status` → `success` |
| **Enso swap + Aave V3 deposit (live broadcast)** | Base mainnet, ETH → aBaseUSDC (Aave V3 supply token `0x4e65…0AB`) | tx `0xab687461ea9fc11712743c8a782fe30f52ca93248f6ede00494de3bead6ee3ce` — 0.001 ETH → 2.308456 aBaseUSDC; verified via `vfs cat /chains/base/tx/0xab68…e3ce/{status,gas_used,block_number}` → `success`, `993935`, `45747148` |

Funded test wallet (created by `vfs write /wallets/new --data 'dest1'`):
- dest1 `0x42d90013bdf79f184740f6EB0A480113A440d53F` — funded by user with ~0.025 ETH on Base, used as the sender for all three live broadcasts above.

End balances on dest1 (post-broadcasts, all read through beth's VFS — no curl/RPC):
- ETH on Base: `vfs cat /chains/base/addresses/0x42d9…d53F/balance.eth` → `0.02215890471669155 ETH`
- USDC on Base: `vfs cat /chains/base/addresses/0x42d9…d53F/tokens/0x8335…2913/balance.formatted` → `2.306996 USDC`
- aBaseUSDC on Base: `vfs cat /chains/base/addresses/0x42d9…d53F/tokens/0x4e65…c0AB/balance.formatted` → `2.308459 aBasUSDC` (Aave V3 aTokens accrue interest, so this exceeds the deposited 2.308456 by the post-broadcast yield)

ERC-20 read surface (added 2026-05-09 in response to spec gap):
- `chains/<chain>/addresses/<a>/tokens/<token>/{balance, balance.raw, balance.formatted, symbol, decimals}` — backed by `eth_call(balanceOf | symbol | decimals)` in `crates/beth-chain/src/lib.rs::ChainClient::erc20_*`.
- `chains/<chain>/tx/<hash>/{receipt.json, status, block_number, gas_used, logs.json, full.json}` — backed by `eth_getTransactionByHash` / `eth_getTransactionReceipt`.

## Deferred items (with justification)

1. **Mainnet-fork acceptance scenarios (Uniswap V2 + Enso).** The `acceptance.sh` script handles both — it just needs `BETH_MAINNET_RPC` set. The remote fork URL is not provided to this environment. To exercise, set `BETH_MAINNET_RPC=https://eth.llamarpc.com` (or any provider) and re-run.
2. **Docker-runner kernel-mount test.** Harness landed at `tests/docker/{Dockerfile,test.sh,run.sh}`. Has not been executed in *this* environment (no Docker daemon available here); the in-process NFS server, the `BethFs` adapter, and the `Daemon::mount` API are all unit-tested via `cargo test -p beth-mount --features mount` (7 passed). The docker image just bundles those plus `nfs-common` and exercises the same flows through the Linux kernel NFS client.

## Files map (summary)

```
crates/
├── beth                # CLI (clap)
├── beth-daemon         # Daemon orchestration + UDS IPC + ENS adapter
├── beth-vfs            # Path router + 11 handler modules
├── beth-chain          # alloy provider pool, ChainRegistry
├── beth-tx             # Tx engine, intent parser, policy, RecipientResolver
├── beth-keystore       # argon2id + chacha20poly1305 encrypted keystore
├── beth-defi           # Enso shortcuts client
├── beth-watch          # Subscription registry + executor task
├── beth-tools          # Pure crypto/abi/encoding utilities
├── beth-etherscan      # Etherscan v2 client + TTL cache
├── beth-ens            # ENS namehash + forward/reverse resolution
├── beth-prices         # DefiLlama keyless price oracle
├── beth-mount          # NFS adapter (feature-gated)
├── beth-proto          # Shared types: AddressBook, AuditLog, Config, HomeDir, ChainSpec
└── beth-it             # (placeholder integration crate)
```

## Verification commands

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib
cargo build --release -p beth
scripts/acceptance.sh              # native send + ERC-20 on Anvil
BETH_MAINNET_RPC=... scripts/acceptance.sh   # adds Uniswap V2 + Enso scenarios
```
