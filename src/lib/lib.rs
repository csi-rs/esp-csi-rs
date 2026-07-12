//! # esp-csi-rs — CSI collection on ESP devices
//!
//! Thin facade over [`esp_csi_rs_core`], the shared engine. This crate keeps the
//! published `esp-csi-rs` name and public API stable; all functionality lives in
//! `esp-csi-rs-core` and is re-exported here in full.
//!
//! See the `esp-csi-rs-core` docs for the full API.

#![no_std]

pub use esp_csi_rs_core::*;

// `#[macro_export]` macros are re-exported explicitly — glob re-export of macros
// is not guaranteed — so `esp_csi_rs::log_ln!` / `log_raw!` keep resolving.
pub use esp_csi_rs_core::{log_ln, log_raw};
