//! STAND-IN for Blur's src-tauri/src/elevation.rs (v3.10.2), made by F-7WHISPER on 2026-11-22.
//!
//! Blur's file watches for elevated foreground windows and shows a prompt.
//! F-7WHISPER only needs the `BlockedWindow` type, because it is a field of
//! `ClickerState` and `ClickerStatusPayload`. Copied unchanged from Blur.

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedWindow {
    #[serde(skip_serializing)]
    pub handle: isize,
    #[serde(skip_serializing)]
    pub pid: u32,
    pub name: String,
    pub title: String,
}
