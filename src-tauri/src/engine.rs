use crate::{
    desktop,
    keys::{self, Keyboard, Stroke},
    settings::{app_name, same_name, Action, Button, Hotkey, Input, Mode, Point, RowSpec},
    wait::{Timer, Wake},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{hash_map::RandomState, HashMap},
    hash::BuildHasher,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const WATCH: Duration = Duration::from_millis(75);

#[derive(Clone, Copy)]
enum Output {
    Key(Stroke),
    Mouse(Button, Option<Point>),
}

impl Output {
    fn map(input: &Input) -> Option<Self> {
        match &input.action {
            Action::Key(key) => keys::map(key).map(Self::Key),
            Action::Click { button, at } => Some(Self::Mouse(*button, *at)),
        }
    }

    fn send(self, keyboard: &mut Keyboard, down: bool) -> bool {
        match self {
            Self::Key(stroke) => keyboard.send(stroke, down),
            Self::Mouse(button, at) => desktop::click(button, at, down),
        }
    }
}

/// Windows and macOS: keys and left / right / middle clicks run on Blur AutoClicker's engine.
/// Back / forward clicks (Blur has no side-button output) stay on this scheduler.
#[cfg(any(windows, target_os = "macos"))]
mod blur {
    use crate::settings::{Action, Button, Input, Key, Point};
    use blur_engine::{BlurButton, BlurInput, BlurRun};
    use std::time::Duration;

    pub const POLL_MIN: Duration = Duration::from_millis(1);
    pub const POLL_MAX: Duration = Duration::from_millis(10);

    #[derive(Clone, Copy)]
    pub enum Plan {
        Key(u16),
        Click(Button, Option<Point>),
    }

    // The VK windows_keys::map sends for this key: left/right modifiers, numpad keys, Vk<n>.
    fn vk(key: &Key) -> u16 {
        match key.code.as_str() {
            "ShiftLeft" => 0xa0,
            "ShiftRight" => 0xa1,
            "ControlLeft" => 0xa2,
            "ControlRight" => 0xa3,
            "AltLeft" => 0xa4,
            "AltRight" => 0xa5,
            "MetaLeft" => 0x5b,
            "MetaRight" => 0x5c,
            "NumpadEnter" => 0x0d,
            "NumpadDivide" => 0x6f,
            "NumpadDecimal" => 0x6e,
            code => match code.strip_prefix("Numpad").map(str::as_bytes) {
                Some([digit @ b'0'..=b'9']) => 0x60 + u16::from(*digit - b'0'),
                _ => key.vk,
            },
        }
    }

    impl Plan {
        /// `mapped` is whether the old scheduler could send this row (keys::map succeeded);
        /// a key it cannot send keeps today's behaviour on the old scheduler.
        pub fn map(input: &Input, mapped: bool) -> Option<Self> {
            match &input.action {
                Action::Key(key) => mapped.then(|| Self::Key(vk(key))),
                Action::Click { button: button @ (Button::Left | Button::Right | Button::Middle), at } => {
                    Some(Self::Click(*button, *at))
                }
                Action::Click { .. } => None,
            }
        }

        pub fn start(self, interval_ms: i64) -> std::io::Result<BlurRun> {
            let input = match self {
                Self::Key(vk) => BlurInput::Key { vk },
                Self::Click(button, at) => BlurInput::Click {
                    button: match button {
                        Button::Right => BlurButton::Right,
                        Button::Middle => BlurButton::Middle,
                        _ => BlurButton::Left,
                    },
                    at: at.map(|p| (p.x, p.y)),
                },
            };
            BlurRun::start(input, interval_ms.max(1) as f64)
        }
    }

    pub struct Active {
        pub run: BlurRun,
        pub paused: bool,
        /// The row's count when this run started; the run's clicks add to it.
        pub base: u64,
    }

    impl Active {
        pub fn count(&self, clicks: i64) -> u64 {
            self.base.saturating_add(clicks.max(0) as u64)
        }
    }
}

/// macOS: Accessibility permission, asked at most every 500 ms (Blur rows poll every 1-10 ms).
#[cfg(target_os = "macos")]
fn trusted_cached(now: Instant) -> bool {
    static LAST: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    match *last {
        Some((at, ok)) if now.saturating_duration_since(at) < Duration::from_millis(500) => ok,
        _ => {
            let ok = keys::trusted();
            *last = Some((now, ok));
            ok
        }
    }
}

fn bound(spec: &RowSpec, key: &Hotkey, mode: Mode) -> bool {
    spec.input.trigger.as_ref().is_some_and(|t| t.mode == mode && t.hotkey.same_binding(key))
}

#[derive(Serialize)]
struct Row {
    #[serde(flatten)]
    spec: RowSpec,
    running: bool,
    count: u64,
    #[serde(skip)]
    reported: u64,
    #[serde(skip)]
    output: Option<Output>,
    #[serde(skip)]
    due: Instant,
    #[cfg(any(windows, target_os = "macos"))]
    #[serde(skip)]
    plan: Option<blur::Plan>,
    #[cfg(any(windows, target_os = "macos"))]
    #[serde(skip)]
    blur: Option<blur::Active>,
}

impl Row {
    fn new(spec: RowSpec) -> Self {
        let output = Output::map(&spec.input);
        Self {
            #[cfg(any(windows, target_os = "macos"))]
            plan: blur::Plan::map(&spec.input, output.is_some()),
            #[cfg(any(windows, target_os = "macos"))]
            blur: None,
            output,
            spec,
            running: false,
            count: 0,
            reported: 0,
            due: Instant::now(),
        }
    }

    fn configure(&mut self, input: Input) {
        self.output = Output::map(&input);
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.plan = blur::Plan::map(&input, self.output.is_some());
        }
        self.spec.input = input;
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn on_blur(&self) -> bool {
        self.plan.is_some()
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn on_blur(&self) -> bool {
        false
    }

    /// Ends this row's Blur run (Blur releases what it holds) and keeps its final count.
    #[cfg(any(windows, target_os = "macos"))]
    fn stop_blur(&mut self) {
        if let Some(active) = self.blur.take() {
            let base = active.base;
            let clicks = active.run.stop();
            self.count = self.count.max(base.saturating_add(clicks.max(0) as u64));
        }
    }
}

struct Held {
    output: Output,
    due: Instant,
}

impl Held {
    fn release(&mut self, keyboard: &mut Keyboard) -> bool {
        if self.output.send(keyboard, false) {
            return true;
        }
        self.due = Instant::now() + Duration::from_millis(20);
        false
    }
}

#[derive(Clone)]
struct Foreground {
    pid: u32,
    name: Option<String>,
}

impl Foreground {
    fn own(&self) -> bool {
        self.pid == std::process::id()
    }

    fn same_app(&self, other: &Self) -> bool {
        self.pid == other.pid || (!self.own() && !other.own()
            && self.name.as_deref().zip(other.name.as_deref())
                .is_some_and(|(a, b)| same_name(a, b)))
    }

    fn allows(&self, filter: Option<&str>) -> bool {
        !self.own() && filter.is_none_or(|wanted| {
            self.name.as_deref().is_some_and(|actual| same_name(wanted, actual))
        })
    }
}

pub struct Rows {
    rows: Vec<Row>,
    held: HashMap<String, Held>,
    keyboard: Keyboard,
    id_sequence: u64,
    switch_stop: bool,
    target: Option<Foreground>,
    last_front: Option<Foreground>,
    exiting: bool,
}

impl Rows {
    pub fn configuration(&self) -> Vec<RowSpec> {
        self.rows.iter().map(|r| r.spec.clone()).collect()
    }

    pub fn snapshot(&mut self, trigger_ok: impl Fn(&Hotkey) -> bool) -> Value {
        for row in &mut self.rows {
            row.reported = row.count;
        }
        let mut value = serde_json::to_value(&self.rows).expect("serializable row fields");
        if let Value::Array(items) = &mut value {
            for (item, row) in items.iter_mut().zip(&self.rows) {
                if let Value::Object(fields) = item {
                    let ok = row.spec.input.trigger.as_ref().is_some_and(|t| trigger_ok(&t.hotkey));
                    fields.insert("triggerOk".into(), Value::Bool(ok));
                }
            }
        }
        value
    }

    /// Every row's activation key, duplicates included.
    pub fn trigger_keys(&self) -> Vec<Hotkey> {
        self.rows.iter()
            .filter_map(|r| r.spec.input.trigger.as_ref().map(|t| t.hotkey.clone()))
            .collect()
    }

    /// Whether pressing `key` would start a row (so the press needs permission to send).
    pub fn trigger_starts(&self, key: &Hotkey) -> bool {
        self.rows.iter().any(|r| {
            !r.running && (bound(&r.spec, key, Mode::Toggle) || bound(&r.spec, key, Mode::Hold))
        })
    }

    pub fn trigger_down(&mut self, key: &Hotkey) {
        let start = self.rows.iter().any(|r| !r.running && bound(&r.spec, key, Mode::Toggle));
        self.set_running_where(|spec| bound(spec, key, Mode::Toggle), start);
        self.set_running_where(|spec| bound(spec, key, Mode::Hold), true);
    }

    pub fn trigger_up(&mut self, key: &Hotkey) {
        self.set_running_where(|spec| bound(spec, key, Mode::Hold), false);
    }

    pub fn counts(&mut self) -> HashMap<String, u64> {
        let mut changes = HashMap::new();
        for row in &mut self.rows {
            if row.reported != row.count {
                row.reported = row.count;
                changes.insert(row.spec.id.clone(), row.count);
            }
        }
        changes
    }

    pub fn any_running(&self) -> bool {
        self.rows.iter().any(|r| r.running)
    }

    pub fn running(&self, id: &str) -> Option<bool> {
        Some(self.rows.iter().find(|r| r.spec.id == id)?.running)
    }

    fn foreground(&mut self) -> Option<Foreground> {
        let pid = desktop::foreground_pid()?;
        if let Some(previous) = &self.last_front {
            if previous.pid == pid && (previous.own() || previous.name.is_some()) {
                return Some(previous.clone());
            }
        }
        let name = if pid == std::process::id() {
            None
        } else {
            desktop::process_name(pid).and_then(|name| app_name(&name))
        };
        let front = Foreground { pid, name };
        self.last_front = Some(front.clone());
        Some(front)
    }

    fn reset_target(&mut self) {
        self.target = if self.switch_stop && self.any_running() {
            self.foreground().filter(|app| !app.own())
        } else {
            None
        };
    }

    pub fn set_switch_stop(&mut self, enabled: bool) {
        if self.switch_stop != enabled {
            self.switch_stop = enabled;
            self.reset_target();
        }
    }

    pub fn add(&mut self, input: Input) {
        let id = loop {
            self.id_sequence = self.id_sequence.wrapping_add(1);
            let bits = RandomState::new().hash_one(self.id_sequence);
            let id = format!("{:012x}", bits & 0xffff_ffff_ffff);
            if self.running(&id).is_none() && !self.held.contains_key(&id) {
                break id;
            }
        };
        self.rows.push(Row::new(RowSpec { id, input }));
    }

    pub fn edit(&mut self, id: &str, input: Input) {
        let Some(index) = self.rows.iter().position(|r| r.spec.id == id) else { return };
        let old = self.rows[index].spec.input.trigger.as_ref().filter(|t| t.mode == Mode::Hold);
        let keeps_hold = old.is_none_or(|old| {
            input.trigger.as_ref().is_some_and(|new| new.hotkey.same_binding(&old.hotkey))
        });
        if keeps_hold {
            // A running row keeps running: process() starts a new Blur run with the
            // new values once the row may press.
            self.release(Some(id));
            #[cfg(any(windows, target_os = "macos"))]
            self.rows[index].stop_blur();
        } else {
            // The old Hold key's release would no longer reach this row.
            self.set_running(Some(id), false);
        }
        let row = &mut self.rows[index];
        row.configure(input);
        row.due = Instant::now();
    }

    pub fn remove(&mut self, id: &str) {
        self.release(Some(id));
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(row) = self.rows.iter_mut().find(|r| r.spec.id == id) {
            row.stop_blur();
        }
        self.rows.retain(|r| r.spec.id != id);
        if !self.any_running() {
            self.target = None;
            self.last_front = None;
        }
    }

    fn release(&mut self, id: Option<&str>) {
        let keyboard = &mut self.keyboard;
        self.held.retain(|owner, held| {
            id.is_some_and(|id| id != owner.as_str()) || !held.release(keyboard)
        });
    }

    pub fn set_running(&mut self, id: Option<&str>, running: bool) {
        self.set_running_where(|spec| id.is_none_or(|id| id == spec.id), running);
    }

    fn set_running_where(&mut self, select: impl Fn(&RowSpec) -> bool, running: bool) {
        let was_running = self.any_running();
        let now = Instant::now();
        for row in self.rows.iter_mut().filter(|r| select(&r.spec)) {
            if running && !row.running {
                row.due = now;
            }
            row.running = running;
            #[cfg(any(windows, target_os = "macos"))]
            if !running {
                row.stop_blur();
            }
        }
        if !running {
            // Release what the stopped rows hold, plus anything left by a removed row.
            let (rows, keyboard) = (&self.rows, &mut self.keyboard);
            self.held.retain(|owner, held| {
                let stopped = rows.iter().find(|r| r.spec.id == *owner)
                    .is_none_or(|r| select(&r.spec));
                !stopped || !held.release(keyboard)
            });
        }
        if !self.any_running() {
            self.target = None;
            self.last_front = None;
        } else if !was_running {
            self.reset_target();
        }
    }

    fn process(&mut self, focused: &AtomicBool) -> (Option<Instant>, bool) {
        let now = Instant::now();
        let keyboard = &mut self.keyboard;
        self.held.retain(|_, held| held.due > now || !held.release(keyboard));

        if !self.any_running() {
            return (self.held.values().map(|h| h.due).min(), false);
        }

        let front = self.foreground();
        if self.switch_stop {
            if let Some(app) = &front {
                if self.target.as_ref().is_some_and(|target| !target.same_app(app)) {
                    self.set_running(None, false);
                    return (self.held.values().map(|h| h.due).min(), true);
                }
                if self.target.is_none() && !app.own() {
                    self.target = Some(app.clone());
                }
            }
        }

        #[cfg(any(windows, target_os = "macos"))]
        let (blur_next, blur_ended) = self.process_blur(focused, front.as_ref(), now);
        #[cfg(not(any(windows, target_os = "macos")))]
        let (blur_next, blur_ended) = (None::<Instant>, false);

        let mut next = self.switch_stop.then(|| now + WATCH);
        next = next.into_iter().chain(blur_next).min();
        for row in self.rows.iter_mut().filter(|r| r.running && !r.on_blur()) {
            let now = Instant::now();
            if row.due <= now {
                let period = Duration::from_millis(row.spec.input.interval_ms as u64);
                let remainder = (now.duration_since(row.due).as_nanos() % period.as_nanos()) as u64;
                row.due = now + (period - Duration::from_nanos(remainder));

                if !focused.load(Ordering::Acquire) && !self.held.contains_key(&row.spec.id) {
                    if let (Some(output), Some(app)) = (row.output, front.as_ref()) {
                        if app.allows(row.spec.input.app.as_deref())
                            && desktop::foreground_pid() == Some(app.pid)
                            && output.send(&mut self.keyboard, true)
                        {
                            self.held.insert(row.spec.id.clone(), Held {
                                output,
                                due: Instant::now() + (period / 2).min(Duration::from_millis(30)),
                            });
                            row.count = row.count.saturating_add(1);
                        }
                    }
                }
            }
            next = Some(next.map_or(row.due, |t| t.min(row.due)));
        }
        (self.held.values().map(|h| h.due).chain(next).min(), blur_ended)
    }

    /// Blur rows: start a run only once the row may press, pause it exactly when the
    /// old scheduler would skip a press, collect counts, and notice runs Blur ended itself.
    /// Returns the next poll time and whether any row stopped.
    #[cfg(any(windows, target_os = "macos"))]
    fn process_blur(
        &mut self,
        focused: &AtomicBool,
        front: Option<&Foreground>,
        now: Instant,
    ) -> (Option<Instant>, bool) {
        let own_focused = focused.load(Ordering::Acquire);
        // macOS: a denied Quartz post has no error result. Gate new and live runs here.
        #[cfg(target_os = "macos")]
        let own_focused = own_focused || !trusted_cached(now);
        let mut poll: Option<Duration> = None;
        let mut ended = false;
        for row in self.rows.iter_mut().filter(|r| r.running) {
            let Some(plan) = row.plan else { continue };
            let blocked = own_focused
                || !front.is_some_and(|app| app.allows(row.spec.input.app.as_deref()));

            if row.blur.as_ref().is_some_and(|active| !active.run.is_running()) {
                row.stop_blur();
                row.running = false;
                ended = true;
                continue;
            }

            match &mut row.blur {
                None if blocked => {}
                None => match plan.start(row.spec.input.interval_ms) {
                    Ok(run) => row.blur = Some(blur::Active { run, paused: false, base: row.count }),
                    Err(_) => {
                        row.running = false;
                        ended = true;
                        continue;
                    }
                },
                Some(active) => {
                    if active.paused != blocked {
                        active.run.set_paused(blocked);
                        active.paused = blocked;
                    }
                    row.count = row.count.max(active.count(active.run.clicks()));
                }
            }
            let period = Duration::from_millis(row.spec.input.interval_ms.max(1) as u64);
            poll = Some(poll.map_or(period, |p| p.min(period)));
        }
        if ended && !self.any_running() {
            self.target = None;
            self.last_front = None;
        }
        (poll.map(|p| now + p.clamp(blur::POLL_MIN, blur::POLL_MAX)), ended)
    }
}

struct Shared {
    rows: Mutex<Rows>,
    focused: AtomicBool,
    wake: Wake,
}

pub struct Engine {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn new(
        specs: Vec<RowSpec>,
        keyboard: Keyboard,
        focused: bool,
        switch_stop: bool,
        on_switch_stop: impl Fn() + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            rows: Mutex::new(Rows {
                rows: specs.into_iter().map(Row::new).collect(),
                held: HashMap::new(),
                keyboard,
                id_sequence: 0,
                switch_stop,
                target: None,
                last_front: None,
                exiting: false,
            }),
            focused: AtomicBool::new(focused),
            wake: Wake::new(),
        });
        let scheduler = shared.clone();
        let worker = thread::spawn(move || {
            let mut timer = Timer::new();
            let mut rows = scheduler.rows.lock().unwrap();
            while !rows.exiting {
                let (next, stopped) = rows.process(&scheduler.focused);
                if stopped {
                    on_switch_stop();
                }
                timer.active(next.is_some());
                rows = timer.wait(&scheduler.wake, &scheduler.rows, rows, next);
            }

            rows.set_running(None, false);
            for _ in 0..5 {
                if rows.held.is_empty() {
                    break;
                }
                drop(rows);
                thread::sleep(Duration::from_millis(10));
                rows = scheduler.rows.lock().unwrap();
                rows.release(None);
            }
        });
        Self { shared, worker: Some(worker) }
    }

    pub fn rows(&self) -> MutexGuard<'_, Rows> {
        self.shared.rows.lock().unwrap()
    }

    pub fn change(&self, change: impl FnOnce(&mut Rows)) {
        let mut rows = self.rows();
        change(&mut rows);
        self.shared.wake.notify();
    }

    pub fn focus(&self, focused: bool) {
        self.shared.focused.store(focused, Ordering::Release);
        let _guard = self.rows();
        self.shared.wake.notify();
    }

    pub fn shutdown(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.change(|rows| rows.exiting = true);
            let _ = worker.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}
