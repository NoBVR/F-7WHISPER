//! F-7WHISPER's API over Blur's engine: one `BlurRun` per F-7 row.
//!
//! Each run gets its own shim `AppHandle` with its own `ClickerState`, builds
//! a Blur `ClickerConfig`, and calls Blur's `engine::start_clicker` on a
//! dedicated thread, exactly as Blur's own worker does.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use tauri::{AppHandle, Manager};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, GetKeyState, VK_CAPITAL};

use crate::engine::worker::{get_click_count, RunControl};
use crate::engine::{
    ClickPointTarget, ClickerConfig, InputType, ProcessListMode, RunOutcome,
};
use crate::error::poisoned_inner;
use crate::{ClickerSettings, ClickerState, IconState, STATUS_EVENT};

/// The generation every F-7 run uses: each run has its own `ClickerState`,
/// so there is never an older or newer Blur run on the same state.
const RUN_GENERATION: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurButton {
    Left,
    Right,
    Middle,
}

impl BlurButton {
    /// The number Blur's `mouse::get_button_flags` expects.
    fn blur_number(self) -> i32 {
        match self {
            BlurButton::Left => 1,
            BlurButton::Right => 2,
            BlurButton::Middle => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurInput {
    Key { vk: u16 },
    Click {
        button: BlurButton,
        at: Option<(i32, i32)>,
    },
}

// ---- process-wide bookkeeping shared by all runs ----

/// Engine threads currently inside `engine::start_clicker`, and how many runs
/// have ever entered it. Guarded together so "was I the only run since I
/// entered?" is answered consistently.
struct EngineRegistry {
    live: u64,
    entered: u64,
}

static REGISTRY: Mutex<EngineRegistry> = Mutex::new(EngineRegistry {
    live: 0,
    entered: 0,
});

#[link(name = "ntdll")]
extern "system" {
    fn NtSetTimerResolution(
        DesiredResolution: u32,
        SetResolution: u8,
        CurrentResolution: *mut u32,
    ) -> u32;
}

/// Blur's engine asks for 1 ms timer resolution while it runs and releases it
/// when it ends. Windows keeps that request per process, not per run, so when
/// one of several runs ends it also drops the request for the others. This
/// puts it back while any run is still live, and releases it after the last.
fn rebalance_timer_resolution(live: u64) {
    let mut current = 0u32;
    unsafe {
        NtSetTimerResolution(10000, u8::from(live > 0), &mut current);
    }
}

// ---- one run ----

struct Shared {
    /// This run's count, taken from its own emitted status payloads.
    emitted_clicks: AtomicI64,
    last_tick: AtomicU64,
    /// Set by the engine thread just before it enters `start_clicker`.
    entered: Mutex<Option<EnteredAt>>,
    finished: AtomicBool,
    outcome: Mutex<Option<RunOutcome>>,
}

#[derive(Clone, Copy)]
struct EnteredAt {
    at: Instant,
    /// `REGISTRY.entered` right after this run entered.
    epoch: u64,
    /// No other run was live when this one entered.
    alone: bool,
}

pub struct BlurRun {
    app: AppHandle,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    final_clicks: Option<i64>,
}

fn invalid(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, msg.to_string())
}

/// Blur's `ClickerState` as Blur's `create_clicker_state` builds it, but
/// already marked as running generation 1 (what Blur's `begin_run` does).
fn new_clicker_state(paused: bool) -> ClickerState {
    ClickerState {
        running: Arc::new(AtomicBool::new(true)),
        run_generation: AtomicU64::new(RUN_GENERATION),
        settings: Mutex::new(ClickerSettings::default()),
        last_error: Mutex::new(None),
        stop_reason: Mutex::new(None),
        active_click_point_index: std::sync::atomic::AtomicI64::new(-1),
        active_click_point_tick: AtomicU64::new(0),
        suppress_hotkey_until_ms: AtomicU64::new(0),
        suppress_hotkey_until_release: AtomicBool::new(false),
        hotkey_capture_active: AtomicBool::new(false),
        click_point_pick_active: AtomicBool::new(false),
        custom_stop_zone_pick_active: AtomicBool::new(false),
        registered_hotkey: Mutex::new(None),
        master_key: Mutex::new(None),
        master_hold_mode: AtomicBool::new(false),
        master_enabled: AtomicBool::new(true),
        master_allowed: AtomicBool::new(true),
        elevation_blocked_window: Mutex::new(None),
        elevation_prompt_generation: AtomicU64::new(0),
        engine_busy: AtomicBool::new(true),
        elevation_suppressed: Mutex::new(Vec::new()),
        elevation_notice_enabled: AtomicBool::new(false),
        last_master_allowed: AtomicBool::new(true),
        settings_initialized: AtomicBool::new(false),
        paused: Arc::new(AtomicBool::new(paused)),
        paused_by_zone: AtomicBool::new(false),
        zone_started_generation: AtomicU64::new(0),
        warning: Mutex::new(None),
        icon_state: Mutex::new(IconState {
            accent_color: String::from("#22c55e"),
            theme: String::from("dark"),
            icon_enabled: true,
            icon_theme: String::from("auto"),
            icon_color: String::from("theme"),
            icons_applied: false,
        }),
        icon_cache: Mutex::new(crate::icon::init_icon_cache()),
    }
}

/// Same as Blur's private `worker::max_double_click_gap_ms`.
fn max_double_click_gap_ms() -> u32 {
    let system_timeout_ms = unsafe { GetDoubleClickTime() };
    ((system_timeout_ms as f64) * 0.9).floor() as u32
}

/// Same test as Blur's `keyboard::caps_lock_enabled`, on the engine thread.
fn caps_lock_on() -> bool {
    unsafe { (GetKeyState(VK_CAPITAL as i32) & 1) != 0 }
}

fn build_config(input: BlurInput, interval_ms: f64) -> ClickerConfig {
    let (input_type, key_code, button, click_points) = match input {
        BlurInput::Key { vk } => (InputType::Keyboard, vk, 1, Vec::new()),
        BlurInput::Click { button, at } => (
            InputType::Mouse,
            0,
            button.blur_number(),
            at.map(|(x, y)| {
                vec![ClickPointTarget {
                    x,
                    y,
                    clicks: 1,
                    radius: 0,
                }]
            })
            .unwrap_or_default(),
        ),
    };
    let is_keyboard = input_type == InputType::Keyboard;
    ClickerConfig {
        interval_secs: interval_ms / 1000.0,
        variation: 0.0,
        limit: 0,
        duty: 45.0,
        time_limit: 0.0,
        button,
        double_click_enabled: false,
        double_click_gap_ms: max_double_click_gap_ms(),
        click_points_enabled: !click_points.is_empty(),
        stop_zones_enabled: false,
        stop_when_complete: false,
        click_points,
        // A non-zero nudge size makes Blur's update_target move back to the click point before
        // every batch (as Blur's own defaults do); a 0% chance means the nudge itself never happens,
        // so each click lands exactly on the point even if the mouse was moved meanwhile.
        cursor_nudge_px: 1.0,
        cursor_nudge_chance_pct: 0.0,
        smoothing: 0,
        stop_zones: Vec::new(),
        corner_stop_enabled: false,
        corner_stop_tl: 0,
        corner_stop_tr: 0,
        corner_stop_bl: 0,
        corner_stop_br: 0,
        edge_stop_enabled: false,
        edge_stop_top: 0,
        edge_stop_right: 0,
        edge_stop_bottom: 0,
        edge_stop_left: 0,
        input_type,
        key_code,
        // Matching the live Caps Lock state means Blur's case logic never adds
        // Shift: F-7 presses the bare key.
        keyboard_uppercase: is_keyboard && caps_lock_on(),
        process_list_enabled: false,
        process_list_mode: ProcessListMode::Whitelist,
        process_list_entries: Vec::new(),
        task_switcher_stop_enabled: false,
    }
}

/// Marks the run finished even if Blur's loop panics.
struct FinishGuard(Arc<Shared>);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::SeqCst);
    }
}

impl BlurRun {
    /// Starts one Blur engine run on its own thread. interval_ms >= 1.0.
    pub fn start(input: BlurInput, interval_ms: f64) -> std::io::Result<BlurRun> {
        Self::start_inner(input, interval_ms, false)
    }

    /// Starts with Blur's pause already set, so the loop idles from its first
    /// iteration. Used by the crate's own tests, which must never send input.
    pub(crate) fn start_paused(input: BlurInput, interval_ms: f64) -> std::io::Result<BlurRun> {
        Self::start_inner(input, interval_ms, true)
    }

    fn start_inner(input: BlurInput, interval_ms: f64, paused: bool) -> std::io::Result<BlurRun> {
        if !interval_ms.is_finite() || interval_ms < 1.0 {
            return Err(invalid("interval_ms must be a finite number >= 1.0"));
        }
        if let BlurInput::Key { vk: 0 } = input {
            // Blur treats key_code 0 as "no key" and would click the mouse instead.
            return Err(invalid("vk must not be 0"));
        }

        let app = AppHandle::new();
        app.manage(new_clicker_state(paused));

        let shared = Arc::new(Shared {
            emitted_clicks: AtomicI64::new(0),
            last_tick: AtomicU64::new(0),
            entered: Mutex::new(None),
            finished: AtomicBool::new(false),
            outcome: Mutex::new(None),
        });

        // Blur's progress emit reads the global count inline, right after
        // this run's thread stored its own value, so it is this run's count.
        // The click-point emit (it bumps activeClickPointTick) reads it after
        // the inter-press sleep, when another run may have overwritten it, so
        // its count is ignored.
        let sink = Arc::clone(&shared);
        app.set_emit_listener(move |event, value| {
            if event != STATUS_EVENT {
                return;
            }
            let tick = value
                .get("activeClickPointTick")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if sink.last_tick.swap(tick, Ordering::SeqCst) != tick {
                return;
            }
            if let Some(count) = value.get("clickCount").and_then(serde_json::Value::as_i64) {
                sink.emitted_clicks.store(count, Ordering::SeqCst);
            }
        });

        let control = RunControl::new(app.clone(), RUN_GENERATION);
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name(String::from("f7-blur-engine"))
            .spawn(move || {
                let _finish = FinishGuard(Arc::clone(&thread_shared));
                let config = build_config(input, interval_ms);
                {
                    let mut reg = REGISTRY.lock().unwrap_or_else(poisoned_inner);
                    reg.live += 1;
                    reg.entered += 1;
                    *thread_shared.entered.lock().unwrap_or_else(poisoned_inner) =
                        Some(EnteredAt {
                            at: Instant::now(),
                            epoch: reg.entered,
                            alone: reg.live == 1,
                        });
                }
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    crate::engine::start_clicker(config, control)
                }));
                {
                    let mut reg = REGISTRY.lock().unwrap_or_else(poisoned_inner);
                    reg.live -= 1;
                    rebalance_timer_resolution(reg.live);
                }
                let outcome = outcome.unwrap_or_else(|_| RunOutcome {
                    stop_reason: String::from("Blur engine panicked"),
                    click_count: thread_shared.emitted_clicks.load(Ordering::SeqCst),
                    elapsed_secs: 0.0,
                    avg_cpu: -1.0,
                });
                *thread_shared.outcome.lock().unwrap_or_else(poisoned_inner) = Some(outcome);
            })?;

        Ok(BlurRun {
            app,
            shared,
            thread: Some(thread),
            final_clicks: None,
        })
    }

    fn state(&self) -> tauri::State<'_, ClickerState> {
        self.app.state::<ClickerState>()
    }

    /// Blur's own pause (ClickerState.paused): the loop idles, holds nothing, keeps its cadence state.
    pub fn set_paused(&self, paused: bool) {
        self.state().paused.store(paused, Ordering::SeqCst);
    }

    /// Presses/clicks sent by this run only. Updates at least every 250 ms while running; exact after stop().
    pub fn clicks(&self) -> i64 {
        if let Some(final_clicks) = self.final_clicks {
            return final_clicks;
        }
        if let Some(outcome) = self.shared.outcome.lock().unwrap_or_else(poisoned_inner).as_ref() {
            return outcome.click_count;
        }
        let emitted = self.shared.emitted_clicks.load(Ordering::SeqCst);
        // While this is the only run that has been live since it entered the
        // engine, Blur's global count is this run's own count. That covers the
        // first press, which Blur does not emit until 250 ms after the run began.
        let entered = *self.shared.entered.lock().unwrap_or_else(poisoned_inner);
        if let Some(entered) = entered {
            let reg = REGISTRY.lock().unwrap_or_else(poisoned_inner);
            let sole = entered.alone && reg.entered == entered.epoch && reg.live == 1;
            // Blur resets the global count as the first step of start_clicker
            // and spends ~5 ms calibrating before its first press; 2 ms keeps
            // a read from landing before that reset.
            if sole && entered.at.elapsed().as_millis() >= 2 && !self.shared.finished.load(Ordering::SeqCst) {
                return emitted.max(get_click_count());
            }
        }
        emitted
    }

    /// False once Blur's loop has ended by itself (e.g. Windows rejected the input).
    pub fn is_running(&self) -> bool {
        !self.shared.finished.load(Ordering::SeqCst)
    }

    pub fn stop_reason(&self) -> Option<String> {
        self.shared
            .outcome
            .lock()
            .unwrap_or_else(poisoned_inner)
            .as_ref()
            .map(|outcome| outcome.stop_reason.clone())
    }

    fn shutdown(&mut self) -> i64 {
        if let Some(final_clicks) = self.final_clicks {
            return final_clicks;
        }
        // RunControl::is_active() turns false, Blur's loop exits and releases
        // anything it holds on the way out.
        self.state().running.store(false, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let final_clicks = self
            .shared
            .outcome
            .lock()
            .unwrap_or_else(poisoned_inner)
            .as_ref()
            .map(|outcome| outcome.click_count)
            .unwrap_or_else(|| self.shared.emitted_clicks.load(Ordering::SeqCst));
        self.final_clicks = Some(final_clicks);
        final_clicks
    }

    /// Ends the run, waits for its thread (Blur releases anything it holds), returns the final count.
    pub fn stop(mut self) -> i64 {
        self.shutdown()
    }
}

impl Drop for BlurRun {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---- C ABI for the C# build ----

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
static RUNS: Mutex<BTreeMap<u64, BlurRun>> = Mutex::new(BTreeMap::new());

fn guarded<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

#[no_mangle]
pub extern "C" fn f7_blur_start(
    kind: i32, /*0 key, 1 click*/
    vk: u16,
    button: i32, /*1 left, 2 right, 3 middle*/
    has_at: i32,
    x: i32,
    y: i32,
    interval_ms: f64,
) -> u64 {
    guarded(0, || {
        let input = match kind {
            0 => BlurInput::Key { vk },
            1 => {
                let button = match button {
                    1 => BlurButton::Left,
                    2 => BlurButton::Right,
                    3 => BlurButton::Middle,
                    _ => return 0,
                };
                BlurInput::Click {
                    button,
                    at: (has_at != 0).then_some((x, y)),
                }
            }
            _ => return 0,
        };
        let Ok(run) = BlurRun::start(input, interval_ms) else {
            return 0;
        };
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::SeqCst);
        RUNS.lock().unwrap_or_else(poisoned_inner).insert(handle, run);
        handle
    })
}

#[no_mangle]
pub extern "C" fn f7_blur_pause(handle: u64, paused: i32) {
    guarded((), || {
        if let Some(run) = RUNS.lock().unwrap_or_else(poisoned_inner).get(&handle) {
            run.set_paused(paused != 0);
        }
    })
}

/// Unknown handle -> 0.
#[no_mangle]
pub extern "C" fn f7_blur_clicks(handle: u64) -> i64 {
    guarded(0, || {
        RUNS.lock()
            .unwrap_or_else(poisoned_inner)
            .get(&handle)
            .map_or(0, BlurRun::clicks)
    })
}

/// Unknown handle -> 0.
#[no_mangle]
pub extern "C" fn f7_blur_running(handle: u64) -> i32 {
    guarded(0, || {
        RUNS.lock()
            .unwrap_or_else(poisoned_inner)
            .get(&handle)
            .map_or(0, |run| i32::from(run.is_running()))
    })
}

/// Stops, joins, frees; unknown handle -> -1.
#[no_mangle]
pub extern "C" fn f7_blur_stop(handle: u64) -> i64 {
    guarded(-1, || {
        // Take it out first so the join below never holds the table lock.
        let run = RUNS.lock().unwrap_or_else(poisoned_inner).remove(&handle);
        run.map_or(-1, BlurRun::stop)
    })
}

#[cfg(test)]
mod tests {
    //! These never send input: runs are started already paused with a key
    //! input (no click point, so Blur never moves the cursor), or not at all.
    use super::*;
    use std::time::Duration;

    const VK_F24: u16 = 0x87;

    #[test]
    fn rejects_bad_arguments_without_starting() {
        assert!(BlurRun::start(BlurInput::Key { vk: VK_F24 }, 0.5).is_err());
        assert!(BlurRun::start(BlurInput::Key { vk: VK_F24 }, f64::NAN).is_err());
        assert!(BlurRun::start(BlurInput::Key { vk: 0 }, 100.0).is_err());
        assert_eq!(f7_blur_start(0, 0, 1, 0, 0, 0, 100.0), 0);
        assert_eq!(f7_blur_start(1, 0, 9, 0, 0, 0, 100.0), 0);
        assert_eq!(f7_blur_start(7, VK_F24, 1, 0, 0, 0, 100.0), 0);
    }

    #[test]
    fn unknown_handles_are_harmless() {
        assert_eq!(f7_blur_stop(u64::MAX), -1);
        assert_eq!(f7_blur_clicks(u64::MAX), 0);
        assert_eq!(f7_blur_running(u64::MAX), 0);
        f7_blur_pause(u64::MAX, 1);
    }

    #[test]
    fn config_matches_f7_rows() {
        let config = build_config(
            BlurInput::Click {
                button: BlurButton::Right,
                at: Some((-5, 7)),
            },
            40.0,
        );
        assert!((config.interval_secs - 0.04).abs() < f64::EPSILON);
        assert_eq!(config.button, 2);
        assert!(config.use_click_points());
        assert_eq!(
            config.click_points,
            vec![ClickPointTarget {
                x: -5,
                y: 7,
                clicks: 1,
                radius: 0
            }]
        );
        assert_eq!(config.input_type, InputType::Mouse);
        assert!(!config.keyboard_uppercase);

        let config = build_config(BlurInput::Key { vk: VK_F24 }, 1000.0);
        assert_eq!(config.input_type, InputType::Keyboard);
        assert_eq!(config.key_code, VK_F24);
        assert!(!config.use_click_points());
        assert_eq!(config.duty, 45.0);
    }

    #[test]
    fn a_paused_run_idles_and_stops_cleanly() {
        let run = BlurRun::start_paused(BlurInput::Key { vk: VK_F24 }, 50.0)
            .expect("the run starts");
        std::thread::sleep(Duration::from_millis(150));
        assert!(run.is_running());
        assert_eq!(run.clicks(), 0);
        assert_eq!(run.stop_reason(), None);
        // Keep it paused; never unpause in a test.
        run.set_paused(true);
        assert_eq!(run.stop(), 0);
    }

    #[test]
    fn dropping_a_paused_run_stops_it() {
        let run = BlurRun::start_paused(BlurInput::Key { vk: VK_F24 }, 50.0)
            .expect("the run starts");
        let shared = Arc::clone(&run.shared);
        drop(run);
        assert!(shared.finished.load(Ordering::SeqCst));
        assert_eq!(
            shared
                .outcome
                .lock()
                .unwrap()
                .as_ref()
                .map(|o| o.stop_reason.clone()),
            Some(String::from("Stopped"))
        );
    }
}
