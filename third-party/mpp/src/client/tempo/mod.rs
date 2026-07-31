//! Tempo-specific client implementations.
//!
//! Contains the Tempo payment providers, transaction building,
//! signing strategies, charge builder, and channel operations.

pub mod autoswap;
pub mod charge;
mod error;
#[path = "session/channel_ops.rs"]
pub mod session_channel_ops;
pub mod session {
    pub use super::session_channel_ops as channel_ops;
}
pub mod signing;

pub use autoswap::AutoswapConfig;
pub use error::TempoClientError;

/// Static max fee per gas: 41 gwei (`base_fee * 2 + priority_fee`).
///
/// Tempo networks use a fixed 20 gwei base fee. Using 2× base fee
/// plus priority ensures the transaction is always accepted.
pub const MAX_FEE_PER_GAS: u128 = 20_000_000_000 * 2 + 1_000_000_000; // 41 gwei

/// Static max priority fee per gas: 1 gwei.
pub const MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;
