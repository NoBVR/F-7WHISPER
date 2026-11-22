//! STAND-IN for Blur's src-tauri/src/indicator.rs (v3.10.2).
//!
//! Blur flashes an on-screen indicator window on each press and shows a
//! countdown to the next one. F-7WHISPER has no such window, so both calls the
//! engine makes are no-ops with Blur's signatures.

/// No-op (Blur: flash the indicator for `hold_ms` near `x`, `y`).
pub fn pulse(_hold_ms: u32, _x: i32, _y: i32) {}

/// No-op (Blur: drive the indicator's countdown to the next press).
pub fn schedule(_cycle_start_ms: u64, _hold_ms: u64, _wait_ms: u64) {}
