//! Shared types and config for bloom-eth.
//!
//! Re-exports thin wrappers around alloy primitive types plus first-class
//! types for chains, wallets, intents, plans, audit log entries and the
//! daemon's on-disk layout.

#![forbid(unsafe_code)]

pub mod address;
pub mod audit;
pub mod chain;
pub mod config;
pub mod home;
pub mod intent;
pub mod plan;
pub mod policy;
pub mod units;

pub use address::{checksum_address, parse_address, AddressBook, AddressBookError};
pub use audit::{AuditLog, AuditRecord};
pub use chain::{ChainId, ChainRef, ChainSpec};
pub use config::{Config, ConfigError, EnsoConfig, EtherscanConfig};
pub use home::{HomeDir, HomeError};
pub use intent::{
    EnsoIntent, GasStrategy, RawIntent, RawIntentBody, ShellIntent, TxIntent, ValueOrToken,
};
pub use plan::{PlanRender, StagedTx, TokenRef, TxStatus};
pub use policy::{Policy, PolicyCheck, PolicyOutcome};
pub use units::{format_units, parse_amount, parse_eth, parse_units, ParsedAmount};

/// Re-exports of alloy types we use across the workspace.
pub mod prelude {
    pub use alloy::primitives::{Address, Bytes, B256, U256};
}
