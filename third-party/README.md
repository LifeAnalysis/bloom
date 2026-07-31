# Machine-safe paid-protocol forks

Bloom carries three narrow source patches because the corresponding published
packages compile concrete local private-key signer conveniences into every
client build, even when callers provide an external implementation of the
abstract Alloy signing traits. Production Bloom Machine must not contain those
implementations; all signatures are returned by Broker after Signer approval.

| Directory | Upstream package | Source version | Bloom patch |
| --- | --- | --- | --- |
| `alloy-heimdall` | [`alloy`](https://github.com/alloy-rs/alloy) | 1.8.3 | Remove `signer-local` from the `essentials` aggregate used by Heimdall's decompiler. Heimdall uses Alloy RPC, ABI, and EVM types but no local signer; every other Alloy feature and API is unchanged. Alloy 2.x remains supplied by crates.io. |
| `mpp` | [`mpp`](https://github.com/tempoxyz/mpp-rs) | 0.10.4 | Use explicit non-local Alloy features; remove the local-signer re-export and convenience Tempo providers while retaining generic charge, session, payload, and parsing APIs. |
| `tempo-alloy` | [`tempo-alloy`](https://github.com/tempoxyz/tempo) | 1.8.0 | Remove the concrete local signer dependency and its convenience `IntoWallet` implementation. Network, transaction, provider, and transport types are unchanged. |
| `x402-chain-eip155` | [`x402-chain-eip155`](https://github.com/x402-rs/x402-rs) | 2.0.0 | Remove the local signer from the client feature and its convenience `SignerLike` implementation. Generic `SignerLike` clients and all EIP-712/header construction used by Bloom are unchanged. |

The versions remain pinned in `Cargo.lock`. When updating any fork, compare it
against the named upstream release, reapply only the authority-boundary patch,
and prove all production Machine feature sets have no normal/build dependency
on `alloy-signer-local` before replacing the pinned source.

Upstream copyright and licensing terms are preserved in each package's license
metadata and included license files where supplied by its crate distribution.
