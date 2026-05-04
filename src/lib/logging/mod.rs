//! Logging backends and CSI-line emission for the crate.
//!
//! See [`logging`](self::logging) for the runtime-selectable transports
//! (`println!`, `defmt`, JTAG, UART, no-op) and the sync/async CSI write
//! paths.

/// CSI logging backends, channel plumbing, and sync/async write paths.
pub mod logging;
