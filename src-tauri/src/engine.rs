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
}

impl Row {
    fn new(spec: RowSpec) -> Self {
        Self {
            output: Output::map(&spec.input),
            spec,
            running: false,
            count: 0,
            reported: 0,
            due: Instant::now(),
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
        self.release(Some(id));
        let row = &mut self.rows[index];
        row.output = Output::map(&input);
        row.spec.input = input;
        row.due = Instant::now();
    }

    pub fn remove(&mut self, id: &str) {
        self.release(Some(id));
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

        let mut next = self.switch_stop.then(|| now + WATCH);
        for row in self.rows.iter_mut().filter(|r| r.running) {
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
        (self.held.values().map(|h| h.due).chain(next).min(), false)
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
