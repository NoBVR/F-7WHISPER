//! Blur AutoClicker v3.10.2's engine (GPL-3.0, see LICENSE-BLUR-GPL-3.0),
//! run by F-7WHISPER without Tauri.
//!
//! - `engine/` (nine files): byte-identical copies of Blur's src-tauri/src/engine/.
//! - `error.rs`, `app_state.rs`, `settings/mod.rs`: byte-identical copies of Blur's.
//! - `hotkeys.rs`, `icon.rs`, `indicator.rs`, `portable.rs`, `elevation.rs`:
//!   small stand-ins with Blur's signatures (see each file's header).
//! - `tauri` is the `f7-tauri-shim` crate in tauri-shim/.
//! - `f7.rs`: the F-7WHISPER API (`BlurRun` and the `f7_blur_*` C functions).
#![allow(dead_code, unused)]

mod app_state;
mod elevation;
mod engine;
mod error;
mod f7;
mod hotkeys;
mod icon;
mod indicator;
mod portable;
mod settings;

// Crate-root items Blur's files refer to as `crate::...` (as in Blur's lib.rs).
pub(crate) use app_state::{ClickerState, ClickerStatusPayload, IconState};
pub(crate) use settings::ClickerSettings;
pub(crate) const STATUS_EVENT: &str = "clicker-status";

pub use f7::{
    f7_blur_clicks, f7_blur_pause, f7_blur_running, f7_blur_start, f7_blur_stop, BlurButton,
    BlurInput, BlurRun,
};
