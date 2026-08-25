//! Runtime configuration types.
//!
//! Re-exports [`CsiConfig`] — the runtime-tunable Channel-State-Information
//! collection settings (sub-carrier filtering, PHY-specific feature toggles,
//! and packet-format selection).

mod csi;

pub use csi::CsiConfig;
