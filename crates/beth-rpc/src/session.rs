//! Block-pinned read sessions — stub for now.
//!
//! WP-5 wires in the real implementation that pins a `(block_number,
//! block_hash)` pair across multi-call operations so the answer doesn't
//! drift if the underlying transport fails over mid-fanout. The shape
//! published here is the minimum the future implementation will rely on
//! — a struct exposed publicly with `'a` borrow over the engine — so
//! call sites coded against this WP keep compiling once WP-5 lands.

use std::marker::PhantomData;

/// Placeholder for a pinned read session. Held by reference to the
/// engine that opened it; today the type is uninhabitable from outside
/// the crate because no constructor exists in the public API. WP-5
/// turns this into a real handle and adds the per-method accessors
/// described in §C.2 of the spec.
#[derive(Debug)]
pub struct Session<'a> {
    _engine: PhantomData<&'a ()>,
}
