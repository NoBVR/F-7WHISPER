// Changed for macOS by F-7WHISPER on 2026-11-22, from Blur AutoClicker v3.10.2's
// src-tauri/src/engine/process.rs (GPL-3.0, see LICENSE-BLUR-GPL-3.0). Diff it against
// ../engine/process.rs to see every change.

use super::ClickerConfig;
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::error::poisoned_inner;
// macOS: Workspace identifies apps and Quartz supplies physical key state.
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use core_graphics::{event::CGEventFlags, event_source::CGEventSourceStateID};

use image::ImageEncoder;

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const DI_NORMAL: u32 = 0x0003;
const PROCESS_DISPLAY_TITLE_MAX_CHARS: usize = 35;
const PROCESS_PATH_INITIAL_CHARS: usize = 260;
const PROCESS_PATH_MAX_CHARS: usize = 32768;
const ICON_CACHE_MAX_ENTRIES: usize = 128;
const FOREGROUND_CACHE_TTL: Duration = Duration::from_millis(1000);
const FOREGROUND_UNKNOWN_GRACE: Duration = Duration::from_millis(250);

// macOS: the CoreGraphics crate does not wrap this HID query.
extern "C" {
    fn CGEventSourceKeyState(state: CGEventSourceStateID, keycode: u16) -> bool;
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub name: String,
    pub display_name: String,
    pub pid: u32,
    pub icon_base64: Option<String>,
}

#[derive(Default)]
struct IconCache {
    entries: HashMap<String, Option<String>>,
    order: VecDeque<String>,
}

impl IconCache {
    fn get(&self, exe_name: &str) -> Option<Option<String>> {
        self.entries.get(exe_name).cloned()
    }

    fn insert(&mut self, exe_name: &str, icon: Option<String>) {
        if self.entries.contains_key(exe_name) {
            self.entries.insert(exe_name.to_string(), icon);
            return;
        }
        while self.entries.len() >= ICON_CACHE_MAX_ENTRIES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.entries.insert(exe_name.to_string(), icon);
        self.order.push_back(exe_name.to_string());
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

fn icon_cache() -> &'static Mutex<IconCache> {
    static CACHE: OnceLock<Mutex<IconCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(IconCache::default()))
}

fn next_path_buffer_size(current: usize) -> Option<usize> {
    if current >= PROCESS_PATH_MAX_CHARS {
        return None;
    }
    Some(current.saturating_mul(2).min(PROCESS_PATH_MAX_CHARS))
}

// macOS: prefer the executable filename, then the user's localized app name.
fn app_name(app: &NSRunningApplication) -> Option<String> {
    app.executableURL().and_then(|url| url.lastPathComponent())
        .or_else(|| app.localizedName()).map(|s| s.to_string())
}

fn exe_name_from_path(path: &str) -> Option<String> {
    path.rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
}

fn encode_base64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn extract_icon_pixels(exe_path: &str) -> Option<(Vec<u8>, u32, u32)> {
    // macOS: process icons are unavailable in this backend.
    None
}

fn extract_process_icon_base64(exe_path: &str) -> Option<String> {
    let (pixels, w, h) = extract_icon_pixels(exe_path)?;
    let mut rgba = pixels;
    for chunk in rgba.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }
    let mut png_bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut png_bytes));
    encoder
        .write_image(&rgba, w, h, image::ColorType::Rgba8.into())
        .ok()?;
    let b64 = encode_base64(&png_bytes);
    Some(format!("data:image/png;base64,{}", b64))
}

fn get_icon_for_process(exe_name: &str, pid: u32) -> Option<String> {
    {
        let cache = icon_cache().lock().unwrap_or_else(poisoned_inner);
        if let Some(cached) = cache.get(exe_name) {
            return cached;
        }
    }
    // macOS: store the empty icon result in Blur's cache.
    let icon = None;
    let mut cache = icon_cache().lock().unwrap_or_else(poisoned_inner);
    cache.insert(exe_name, icon.clone());
    icon
}

pub fn normalize_process_name(name: &str) -> String {
    let name = name.trim().to_lowercase();
    if name.ends_with(".exe") {
        name
    } else {
        format!("{}.exe", name)
    }
}

fn wide_array_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

fn truncate_title_for_display(title: &str) -> String {
    match title.char_indices().nth(PROCESS_DISPLAY_TITLE_MAX_CHARS) {
        Some((idx, _)) => title[..idx].to_string(),
        None => title.to_string(),
    }
}

struct ForegroundEntry {
    hwnd: usize,
    pid: u32,
    name: String,
    captured: Instant,
}

fn foreground_unknown_persists(unidentified_for: Duration) -> bool {
    unidentified_for >= FOREGROUND_UNKNOWN_GRACE
}

#[derive(Default)]
struct ForegroundCache {
    entry: Option<ForegroundEntry>,
    unidentified_since: Option<Instant>,
}

impl ForegroundCache {
    fn note_identified(&mut self) {
        self.unidentified_since = None;
    }

    fn note_unidentified(&mut self, now: Instant) -> bool {
        let since = *self.unidentified_since.get_or_insert(now);
        foreground_unknown_persists(now.saturating_duration_since(since))
    }

    fn get(&self, hwnd: usize, pid: u32, now: Instant) -> Option<String> {
        let entry = self.entry.as_ref()?;
        if entry.hwnd != hwnd || entry.pid != pid {
            return None;
        }
        if now.saturating_duration_since(entry.captured) >= FOREGROUND_CACHE_TTL {
            return None;
        }
        Some(entry.name.clone())
    }

    fn store(&mut self, hwnd: usize, pid: u32, name: &str, now: Instant) {
        self.entry = Some(ForegroundEntry {
            hwnd,
            pid,
            name: name.to_string(),
            captured: now,
        });
    }
}

fn foreground_cache() -> &'static Mutex<ForegroundCache> {
    static CACHE: OnceLock<Mutex<ForegroundCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ForegroundCache::default()))
}

pub fn reset_foreground_unknown_grace() {
    foreground_cache()
        .lock()
        .unwrap_or_else(poisoned_inner)
        .note_identified();
}

pub fn get_foreground_process_name() -> Option<String> {
    // macOS: sample one frontmost app for both the PID and executable name.
    let app = autoreleasepool(|_| NSWorkspace::sharedWorkspace().frontmostApplication())?;
    let pid = u32::try_from(app.processIdentifier()).ok()?;
    if pid == 0 {
        return None;
    }
    let now = Instant::now();
    // macOS: Workspace identifies the foreground application by PID.
    let key = pid as usize;
    {
        let cache = foreground_cache().lock().unwrap_or_else(poisoned_inner);
        if let Some(name) = cache.get(key, pid, now) {
            return Some(name);
        }
    }
    let name = autoreleasepool(|_| app_name(&app))?;
    let mut cache = foreground_cache().lock().unwrap_or_else(poisoned_inner);
    cache.store(key, pid, &name, now);
    Some(name)
}

fn prefer_titled_pid(existing: u32, candidate: u32, has_title: impl Fn(u32) -> bool) -> u32 {
    if !has_title(existing) && has_title(candidate) {
        candidate
    } else {
        existing
    }
}

pub fn list_running_processes() -> Vec<ProcessInfo> {
    // macOS: use regular Workspace apps and their localized display names.
    let mut pid_title_map = HashMap::new();
    let mut unique_processes: HashMap<String, u32> = HashMap::new();
    autoreleasepool(|_| {
        for app in NSWorkspace::sharedWorkspace().runningApplications().iter() {
            if app.activationPolicy() == NSApplicationActivationPolicy::Regular {
                let Ok(pid) = u32::try_from(app.processIdentifier()) else { continue };
                let Some(exe_name) = app_name(&app) else { continue };
                if let Some(title) = app.localizedName() {
                    pid_title_map.insert(pid, title.to_string());
                }
                let lower_name = exe_name.to_lowercase();
                unique_processes
                    .entry(lower_name)
                    .and_modify(|existing| {
                        *existing =
                            prefer_titled_pid(*existing, pid, |p| pid_title_map.contains_key(&p));
                    })
                    .or_insert(pid);
            }
        }
    });

    let mut result: Vec<ProcessInfo> = unique_processes
        .into_iter()
        .filter_map(|(name, pid)| {
            // macOS: an executable may lack a localized display name.
            let window_title = pid_title_map.get(&pid).unwrap_or(&name);
            let display_name = truncate_title_for_display(window_title);
            let icon_base64 = get_icon_for_process(&name, pid);
            Some(ProcessInfo {
                name,
                display_name,
                pid,
                icon_base64,
            })
        })
        .collect();
    result.sort_by_cached_key(|p| p.display_name.to_lowercase());
    debug_assert!(result
        .windows(2)
        .all(|w| w[0].display_name.to_lowercase() <= w[1].display_name.to_lowercase()));
    result
}

pub const PROCESS_LIST_BLOCKED_REASON: &str = "Blocked by process list";
pub const PROCESS_LIST_UNKNOWN_REASON: &str =
    "Blocked by process list: foreground application could not be identified";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForegroundMatch {
    Identified { is_in_list: bool },
    Unidentified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessListVerdict {
    Allow,
    Blocked,
    Unidentified,
}

fn process_list_verdict(
    mode: super::ProcessListMode,
    foreground: ForegroundMatch,
) -> ProcessListVerdict {
    match (mode, foreground) {
        (super::ProcessListMode::Whitelist, ForegroundMatch::Identified { is_in_list: true }) => {
            ProcessListVerdict::Allow
        }
        (super::ProcessListMode::Whitelist, ForegroundMatch::Identified { is_in_list: false }) => {
            ProcessListVerdict::Blocked
        }
        (super::ProcessListMode::Whitelist, ForegroundMatch::Unidentified) => {
            ProcessListVerdict::Unidentified
        }
        (super::ProcessListMode::Blacklist, ForegroundMatch::Identified { is_in_list: true }) => {
            ProcessListVerdict::Blocked
        }
        (super::ProcessListMode::Blacklist, ForegroundMatch::Identified { is_in_list: false }) => {
            ProcessListVerdict::Allow
        }
        (super::ProcessListMode::Blacklist, ForegroundMatch::Unidentified) => {
            ProcessListVerdict::Allow
        }
    }
}

pub fn check_process_list(config: &ClickerConfig) -> Option<String> {
    if !config.process_list_enabled {
        return None;
    }
    let foreground = match get_foreground_process_name() {
        Some(name) => {
            foreground_cache()
                .lock()
                .unwrap_or_else(poisoned_inner)
                .note_identified();
            // macOS: compare using the same normalization as Blur's config builder.
            let current = normalize_process_name(&name);
            ForegroundMatch::Identified {
                is_in_list: config
                    .process_list_entries
                    .iter()
                    .any(|entry| entry.enabled && entry.name == current),
            }
        }
        None => ForegroundMatch::Unidentified,
    };
    match process_list_verdict(config.process_list_mode, foreground) {
        ProcessListVerdict::Allow => {
            foreground_cache()
                .lock()
                .unwrap_or_else(poisoned_inner)
                .note_identified();
            None
        }
        ProcessListVerdict::Blocked => Some(String::from(PROCESS_LIST_BLOCKED_REASON)),
        ProcessListVerdict::Unidentified => {
            let persists = foreground_cache()
                .lock()
                .unwrap_or_else(poisoned_inner)
                .note_unidentified(Instant::now());
            persists.then(|| String::from(PROCESS_LIST_UNKNOWN_REASON))
        }
    }
}

const TASK_SWITCHER_CLASSES: &[&str] =
    &["TaskSwitcherWnd", "TaskViewWindow", "WindowsSwitchWindow"];

pub fn is_task_switcher_active() -> bool {
    // macOS: Command plus the physical Tab key opens the app switcher.
    super::hid_flags().contains(CGEventFlags::CGEventFlagCommand)
        && unsafe { CGEventSourceKeyState(CGEventSourceStateID::HIDSystemState, 48) }
}

pub fn is_process_running(name: &str) -> bool {
    let target = normalize_process_name(name);
    // macOS: include background apps when checking executable names.
    autoreleasepool(|_| NSWorkspace::sharedWorkspace().runningApplications().iter()
        .any(|app| app_name(&app).is_some_and(|n| normalize_process_name(&n) == target)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_title_preserves_ascii_behavior() {
        let title = "a".repeat(PROCESS_DISPLAY_TITLE_MAX_CHARS + 1);
        let truncated = truncate_title_for_display(&title);

        assert_eq!(truncated, "a".repeat(PROCESS_DISPLAY_TITLE_MAX_CHARS));
    }

    #[test]
    fn truncate_title_handles_multibyte_at_old_byte_boundary() {
        let title = format!(
            "{}Тест, привіт, дякую",
            "a".repeat(PROCESS_DISPLAY_TITLE_MAX_CHARS - 1)
        );
        let truncated = truncate_title_for_display(&title);

        assert_eq!(
            truncated,
            format!("{}Т", "a".repeat(PROCESS_DISPLAY_TITLE_MAX_CHARS - 1))
        );
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn next_path_buffer_size_grows_to_cap_then_stops() {
        assert_eq!(next_path_buffer_size(260), Some(520));
        assert_eq!(next_path_buffer_size(16384), Some(PROCESS_PATH_MAX_CHARS));
        assert_eq!(next_path_buffer_size(PROCESS_PATH_MAX_CHARS), None);
        assert_eq!(next_path_buffer_size(PROCESS_PATH_MAX_CHARS + 1024), None);

        let mut capacity = PROCESS_PATH_INITIAL_CHARS;
        let mut steps = 0;
        while let Some(next) = next_path_buffer_size(capacity) {
            assert!(next <= PROCESS_PATH_MAX_CHARS);
            assert!(next > capacity);
            capacity = next;
            steps += 1;
            assert!(steps < 64);
        }
        assert_eq!(capacity, PROCESS_PATH_MAX_CHARS);
    }

    #[test]
    fn exe_name_from_path_strips_directories() {
        assert_eq!(
            exe_name_from_path("C:\\Program Files\\App\\app.exe").as_deref(),
            Some("app.exe")
        );
        assert_eq!(exe_name_from_path("app.exe").as_deref(), Some("app.exe"));
        assert_eq!(exe_name_from_path(""), None);
        assert_eq!(exe_name_from_path("C:\\dir\\"), None);
    }

    #[test]
    fn foreground_cache_reuses_entry_only_for_same_hwnd_and_pid() {
        let base = Instant::now();
        let mut cache = ForegroundCache::default();
        assert_eq!(cache.get(100, 7, base), None);

        cache.store(100, 7, "alpha.exe", base);
        assert_eq!(cache.get(100, 7, base).as_deref(), Some("alpha.exe"));
        assert_eq!(
            cache
                .get(100, 7, base + Duration::from_millis(999))
                .as_deref(),
            Some("alpha.exe")
        );
        assert_eq!(cache.get(100, 7, base + FOREGROUND_CACHE_TTL), None);

        assert_eq!(cache.get(200, 7, base), None);
        assert_eq!(cache.get(100, 8, base), None);

        cache.store(200, 9, "beta.exe", base);
        assert_eq!(cache.get(100, 7, base), None);
        assert_eq!(cache.get(200, 9, base).as_deref(), Some("beta.exe"));
    }

    #[test]
    fn foreground_unknown_persists_only_after_grace_window() {
        assert!(!foreground_unknown_persists(Duration::ZERO));
        assert!(!foreground_unknown_persists(Duration::from_millis(10)));
        assert!(!foreground_unknown_persists(
            FOREGROUND_UNKNOWN_GRACE - Duration::from_millis(1)
        ));
        assert!(foreground_unknown_persists(FOREGROUND_UNKNOWN_GRACE));
        assert!(foreground_unknown_persists(FOREGROUND_UNKNOWN_GRACE * 4));
    }

    #[test]
    fn unidentified_foreground_streak_needs_grace_and_resets_on_identification() {
        let base = Instant::now();
        let mut cache = ForegroundCache::default();

        assert!(!cache.note_unidentified(base));
        assert!(!cache.note_unidentified(base + Duration::from_millis(100)));
        assert!(
            !cache.note_unidentified(base + FOREGROUND_UNKNOWN_GRACE - Duration::from_millis(1))
        );
        assert!(cache.note_unidentified(base + FOREGROUND_UNKNOWN_GRACE));
        assert!(cache.note_unidentified(base + Duration::from_millis(900)));

        cache.note_identified();
        assert!(!cache.note_unidentified(base + Duration::from_millis(1000)));
        assert!(!cache.note_unidentified(base + Duration::from_millis(1200)));
        assert!(
            cache.note_unidentified(base + Duration::from_millis(1000) + FOREGROUND_UNKNOWN_GRACE)
        );
    }

    #[test]
    fn resetting_the_foreground_grace_restarts_the_streak() {
        let base = Instant::now();

        {
            let mut cache = foreground_cache().lock().unwrap_or_else(poisoned_inner);
            assert!(!cache.note_unidentified(base));
            assert!(cache.note_unidentified(base + FOREGROUND_UNKNOWN_GRACE));
        }

        reset_foreground_unknown_grace();

        {
            let mut cache = foreground_cache().lock().unwrap_or_else(poisoned_inner);
            assert!(!cache.note_unidentified(base + FOREGROUND_UNKNOWN_GRACE));
            assert!(cache.note_unidentified(base + FOREGROUND_UNKNOWN_GRACE * 2));
        }

        reset_foreground_unknown_grace();
    }

    #[test]
    fn icon_cache_evicts_oldest_entries_at_cap() {
        let mut cache = IconCache::default();
        for index in 0..ICON_CACHE_MAX_ENTRIES {
            cache.insert(&format!("app{}.exe", index), Some(String::from("icon")));
        }
        assert_eq!(cache.len(), ICON_CACHE_MAX_ENTRIES);
        assert_eq!(cache.get("app0.exe").map(|icon| icon.is_some()), Some(true));

        cache.insert("overflow.exe", Some(String::from("icon")));
        assert_eq!(cache.len(), ICON_CACHE_MAX_ENTRIES);
        assert_eq!(cache.get("app0.exe"), None);
        assert_eq!(
            cache.get("overflow.exe").map(|icon| icon.is_some()),
            Some(true)
        );
        assert_eq!(cache.get("app1.exe").map(|icon| icon.is_some()), Some(true));

        for index in 0..(ICON_CACHE_MAX_ENTRIES * 2) {
            cache.insert(&format!("extra{}.exe", index), None);
            assert_eq!(cache.len(), ICON_CACHE_MAX_ENTRIES);
        }
        assert_eq!(cache.get("missing.exe"), None);
    }

    #[test]
    fn process_list_verdict_covers_both_modes_and_foreground_states() {
        use crate::engine::ProcessListMode::{Blacklist, Whitelist};

        assert_eq!(
            process_list_verdict(Whitelist, ForegroundMatch::Identified { is_in_list: true }),
            ProcessListVerdict::Allow
        );
        assert_eq!(
            process_list_verdict(Whitelist, ForegroundMatch::Identified { is_in_list: false }),
            ProcessListVerdict::Blocked
        );
        assert_eq!(
            process_list_verdict(Whitelist, ForegroundMatch::Unidentified),
            ProcessListVerdict::Unidentified
        );
        assert_eq!(
            process_list_verdict(Blacklist, ForegroundMatch::Identified { is_in_list: true }),
            ProcessListVerdict::Blocked
        );
        assert_eq!(
            process_list_verdict(Blacklist, ForegroundMatch::Identified { is_in_list: false }),
            ProcessListVerdict::Allow
        );
        assert_eq!(
            process_list_verdict(Blacklist, ForegroundMatch::Unidentified),
            ProcessListVerdict::Allow
        );
    }

    #[test]
    fn prefer_titled_pid_upgrades_from_windowless_to_titled_instance() {
        let has_title = |pid: u32| pid == 2000;
        assert_eq!(prefer_titled_pid(1000, 2000, has_title), 2000);
        assert_eq!(prefer_titled_pid(2000, 3000, has_title), 2000);
        let none = |_pid: u32| false;
        assert_eq!(prefer_titled_pid(1000, 2000, none), 1000);
    }
}
