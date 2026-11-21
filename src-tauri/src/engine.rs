use crate::{
    keys::{self, Keyboard, Stroke},
    settings::{clamp, Key, RowSpec},
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

#[derive(Serialize)]
struct Row {
    #[serde(flatten)]
    spec: RowSpec,
    running: bool,
    count: u64,
    #[serde(skip)]
    reported: u64,
    #[serde(skip)]
    stroke: Option<Stroke>,
    #[serde(skip)]
    due: Instant,
}

impl Row {
    fn new(spec: RowSpec) -> Self {
        Self {
            stroke: keys::map(&spec.key),
            spec,
            running: false,
            count: 0,
            reported: 0,
            due: Instant::now(),
        }
    }
}

struct Held {
    stroke: Stroke,
    due: Instant,
}

impl Held {
    fn release(&mut self, keyboard: &mut Keyboard) -> bool {
        if keyboard.send(self.stroke, false) {
            return true;
        }
        self.due = Instant::now() + Duration::from_millis(20);
        false
    }
}

pub struct Rows {
    rows: Vec<Row>,
    // A held stroke keeps its original mapping even after its row is edited or removed.
    held: HashMap<String, Held>,
    keyboard: Keyboard,
    id_sequence: u64,
    exiting: bool,
}

impl Rows {
    pub fn configuration(&self) -> Vec<RowSpec> {
        self.rows.iter().map(|r| r.spec.clone()).collect()
    }

    pub fn snapshot(&mut self) -> Value {
        for row in &mut self.rows {
            row.reported = row.count;
        }
        serde_json::to_value(&self.rows).expect("serializable row fields")
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

    pub fn add(&mut self, key: Key, ms: i64) {
        let id = loop {
            self.id_sequence = self.id_sequence.wrapping_add(1);
            let bits = RandomState::new().hash_one(self.id_sequence);
            let id = format!("{:012x}", bits & 0xffff_ffff_ffff);
            if self.running(&id).is_none() && !self.held.contains_key(&id) {
                break id;
            }
        };
        self.rows.push(Row::new(RowSpec { id, key, interval_ms: clamp(ms) }));
    }

    pub fn edit(&mut self, id: &str, key: Option<Key>, ms: Option<i64>) {
        let Some(index) = self.rows.iter().position(|r| r.spec.id == id) else {
            return;
        };
        let mut spec = self.rows[index].spec.clone();
        if let Some(key) = key {
            spec.key = key;
        }
        if let Some(ms) = ms {
            spec.interval_ms = clamp(ms);
        }
        if spec != self.rows[index].spec {
            self.release(Some(id));
            let row = &mut self.rows[index];
            row.stroke = keys::map(&spec.key);
            row.spec = spec;
            row.due = Instant::now();
        }
    }

    pub fn remove(&mut self, id: &str) {
        self.release(Some(id));
        self.rows.retain(|r| r.spec.id != id);
    }

    fn release(&mut self, id: Option<&str>) {
        let keyboard = &mut self.keyboard;
        self.held.retain(|owner, held| {
            id.is_some_and(|id| id != owner.as_str()) || !held.release(keyboard)
        });
    }

    pub fn set_running(&mut self, id: Option<&str>, running: bool) {
        let now = Instant::now();
        for row in &mut self.rows {
            if id.is_some_and(|id| id != row.spec.id) {
                continue;
            }
            if running && !row.running {
                row.due = now;
            }
            row.running = running;
        }
        if !running {
            self.release(id);
        }
    }

    fn process(&mut self, focused: &AtomicBool) -> Option<Instant> {
        let now = Instant::now();
        let keyboard = &mut self.keyboard;
        self.held.retain(|_, held| held.due > now || !held.release(keyboard));

        let mut next = None;
        for row in self.rows.iter_mut().filter(|r| r.running) {
            let now = Instant::now();
            if row.due <= now {
                let period = Duration::from_millis(row.spec.interval_ms as u64);
                let late = now.duration_since(row.due);
                let remainder = (late.as_nanos() % period.as_nanos()) as u64;
                row.due = now + (period - Duration::from_nanos(remainder));

                if !focused.load(Ordering::Acquire) && !self.held.contains_key(&row.spec.id) {
                    if let Some(stroke) = row.stroke {
                        if keyboard.send(stroke, true) {
                            let hold = (period / 2).min(Duration::from_millis(30));
                            self.held.insert(row.spec.id.clone(), Held {
                                stroke,
                                due: Instant::now() + hold,
                            });
                            row.count = row.count.saturating_add(1);
                        }
                    }
                }
            }
            next = Some(next.map_or(row.due, |t: Instant| t.min(row.due)));
        }
        self.held.values().map(|h| h.due).chain(next).min()
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
    pub fn new(specs: Vec<RowSpec>, keyboard: Keyboard, focused: bool) -> Self {
        let shared = Arc::new(Shared {
            rows: Mutex::new(Rows {
                rows: specs.into_iter().map(Row::new).collect(),
                held: HashMap::new(),
                keyboard,
                id_sequence: 0,
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
                let next = rows.process(&scheduler.focused);
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
