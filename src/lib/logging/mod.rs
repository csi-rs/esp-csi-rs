//! Logging backends and CSI-line emission for the crate.
//!
//! See [`logging`](self::logging) for the runtime-selectable transports
//! (`println!`, `defmt`, JTAG, UART, no-op) and the sync/async CSI write
//! paths.

/// CSI logging backends, channel plumbing, and sync/async write paths.
// The `logging::logging` path is part of the public API (examples import
// `esp_csi_rs::logging::logging::init_logger`), so keep the nested module name.
#[allow(clippy::module_inception)]
pub mod logging;
