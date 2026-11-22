//! STAND-IN for Blur's src-tauri/src/icon.rs (v3.10.2).
//!
//! Blur repaints its window and tray icons when a run starts or stops.
//! F-7WHISPER has no Blur window or tray, so `set_app_icons` is a no-op and
//! `IconCache` (a field type of `ClickerState`) keeps Blur's shape but starts empty.

use image::RgbaImage;
use tauri::AppHandle;

#[derive(Default)]
pub struct IconCache {
    pub activated_dark: Option<RgbaImage>,
    pub activated_light: Option<RgbaImage>,
    pub deactivated_dark: Option<RgbaImage>,
    pub deactivated_light: Option<RgbaImage>,
    pub active_tint_dark: Option<RgbaImage>,
    pub active_tint_light: Option<RgbaImage>,
}

/// Blur's starting cache decodes its bundled .ico files; F-7 has none.
pub fn init_icon_cache() -> IconCache {
    IconCache::default()
}

/// No-op: there is no Blur window or tray icon to repaint.
pub fn set_app_icons(_app: &AppHandle) {}
