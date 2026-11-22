//! STAND-IN for Blur's src-tauri/src/portable.rs (v3.10.2).
//!
//! Blur detects a portable install from a marker file next to its exe. Inside
//! F-7WHISPER that marker never applies, so this always reports "not portable".
//! (Only engine/stats.rs asks, and F-7 never records Blur run stats.)

use std::path::PathBuf;

pub fn is_portable() -> bool {
    false
}

pub fn data_dir() -> Option<PathBuf> {
    None
}
