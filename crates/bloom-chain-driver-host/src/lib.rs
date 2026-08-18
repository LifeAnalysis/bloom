//! Machine-owned generic services for verified chain-driver Petals.
//!
//! This crate is the "Machine-owned generic driver services" leg of the
//! verified-chain-Petal architecture (see `docs/architecture/Verified Chain
//! Petals.md`): capability mediation and durable operation state, wired for
//! any chain family. It deliberately carries no chain-specific constant,
//! type, or field name — every chain identity (name, family, genesis,
//! allowed methods, broadcast policy) is operator-configured data, and every
//! verified/asserted fact flowing through [`bloom_chain_action`] is already
//! chain-neutral at the type level.
//!
//! What lives here:
//! - [`host::MediatorHost`]: bridges a Petal's `bloom:chain/read` calls to a
//!   [`bloom_chain_rpc::mediator::Mediator`] (profile-mediated, allowlisted,
//!   genesis-bound, audited).
//! - [`pin`]: content-addressed package pin verification, generalized from
//!   the driver-specific mount code it replaces — any driver package hash
//!   may be pinned, not a single hardcoded constant.
//! - [`profiles`]: the one generic parser that turns operator-configured
//!   JSON into [`bloom_chain_rpc::mediator::ChainRpcProfile`]s, with the
//!   family-agnostic policy gates (mainnet refusal, broadcast = profile flag
//!   AND operator release flag) applied once, here, instead of once per
//!   chain family.
//! - [`driver::ChainDriverHost`] / [`driver::ChainDriverRegistry`]: one
//!   configured chain profile's mediator + durable outbox, keyed by profile
//!   name and dispatched by the profile's `family` field.
//!
//! Config source note: profile configuration is read from an operator JSON
//! file (see [`profiles::load_profiles`]) for now. Migrating that source to
//! installer-validated Petal metadata (a `petal.toml` section, checked by
//! `bloom-petals`' package validation) is tracked as a follow-up and is not
//! a gate for the first verified chain-driver Petal landing on this crate.

#![forbid(unsafe_code)]

pub mod driver;
pub mod host;
pub mod pin;
pub mod profiles;
