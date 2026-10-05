//! Host tests for the wire contract. The module under test is the parent crate's own source,
//! included by path, so these tests exercise exactly what the firmware compiles.

#[path = "../../src/lib/wire/mod.rs"]
pub mod wire;

/// The reporting-policy state machine, likewise compiled from the parent's source.
#[path = "../../src/lib/csi/policy.rs"]
#[allow(dead_code)]
pub(crate) mod policy;

#[cfg(test)]
mod tests;
