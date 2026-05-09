//! VFS path router for bloom-eth.
//!
//! This crate translates POSIX-ish path operations (lookup, read, write,
//! list) into calls against the underlying engines (chain, keystore, tx,
//! tools). It is transport-agnostic — the NFS mount adapter
//! (`beth-mount`) and the CLI (`beth`) both call into [`Vfs`] directly.
//!
//! Path semantics follow §3 of the bloom-eth spec. Top-level segments:
//!
//! - `chains/<chain>/...` — read-only chain views
//! - `wallets/<wallet>/...` — managed wallets, including the outbox
//! - `defi/intents/...` — Enso-mediated DeFi intents (stub)
//! - `watch/...` — subscriptions (stub)
//! - `tools/...` — pure helpers (keccak, checksum, units)
//! - `status/...` — daemon health
//! - `docs/...` — vendored docs

#![forbid(unsafe_code)]

pub mod handler;
pub mod handlers;
pub mod path;
pub mod router;

pub use handler::{Entry, EntryKind, Handler, HandlerError};
pub use path::VfsPath;
pub use router::Vfs;
