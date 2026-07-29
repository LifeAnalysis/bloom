//! Versioned, fail-closed contracts shared by Bloom Machine, Broker, and Signer.
//!
//! This crate contains wire-safe public metadata only. Wallet private keys,
//! decrypted key objects, PRF output, WKEK material, backend credentials, and
//! custody plaintext are deliberately unrepresentable here.

mod approval;
mod ceremony;
mod claims;
mod codec;
mod crypto;
mod envelope;
mod error;
mod ids;
mod methods;
mod policy;
mod provenance;
mod revocation;
mod service;
mod signing;
mod webauthn;

pub use approval::*;
pub use ceremony::*;
pub use claims::*;
pub use codec::*;
pub use crypto::*;
pub use envelope::*;
pub use error::*;
pub use ids::*;
pub use methods::*;
pub use policy::*;
pub use provenance::*;
pub use revocation::*;
pub use service::*;
pub use signing::*;
pub use webauthn::*;

/// Protocol compatibility contract frozen by W1.
pub const PROTOCOL_MAJOR: u16 = 1;

/// First supported minor version.
pub const PROTOCOL_MINOR_MIN: u16 = 0;

/// Latest supported minor version.
pub const PROTOCOL_MINOR_MAX: u16 = 0;
