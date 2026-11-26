use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
};

#[derive(Clone, Deserialize, Serialize)]
pub struct Key {
    pub label: String,
    pub code: String,
    pub vk: u16,
}

impl Key {
    fn same_key(&self, other: &Self) -> bool {
        if cfg!(windows) { self.vk == other.vk } else { self.code == other.code }
    }

    fn is_mouse(&self) -> bool {
        matches!(self.code.as_str(), "MouseBack" | "MouseForward")
    }

    fn valid(&self) -> bool {
        !self.label.is_empty() && !self.code.trim().is_empty() && (1..255).contains(&self.vk)
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn valid(self) -> bool {
        (-100_000..=100_000).contains(&self.x) && (-100_000..=100_000).contains(&self.y)
    }
}

pub fn app_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()).then(|| name.chars().take(260).collect())
}

pub fn same_name(a: &str, b: &str) -> bool {
    a == b || (cfg!(windows) && a.to_lowercase() == b.to_lowercase())
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Action {
    Key(Key),
    Click { button: Button, at: Option<Point> },
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Hold,
    Toggle,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Trigger {
    pub hotkey: Hotkey,
    pub mode: Mode,
}

impl Trigger {
    fn read(value: Option<&Value>) -> Option<Option<Self>> {
        let value = match value {
            None | Some(Value::Null) => return Some(None),
            Some(value) => value,
        };
        let trigger: Self = serde_json::from_value(value.clone()).ok()?;
        let hotkey = &trigger.hotkey;
        if let Some(with) = &hotkey.with {
            if !with.valid() || with.is_mouse() || hotkey.key.is_mouse() || with.same_key(&hotkey.key)
                || matches!(with.code.as_str(),
                    "ShiftLeft" | "ShiftRight" | "ControlLeft" | "ControlRight"
                    | "AltLeft" | "AltRight" | "MetaLeft" | "MetaRight")
            {
                return None;
            }
        }
        hotkey.key.valid().then_some(Some(trigger))
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    #[serde(flatten)]
    pub action: Action,
    pub interval_ms: f64,
    pub app: Option<String>,
    pub trigger: Option<Trigger>,
}

impl Input {
    pub fn from_page(value: &Value) -> Option<Self> {
        Self::read(value, false)
    }

    fn read(value: &Value, saved_row: bool) -> Option<Self> {
        let kind = match value.get("kind") {
            Some(kind) => kind.as_str()?,
            None if saved_row => "key",
            None => return None,
        };
        let action = match kind {
            "key" => {
                let fields = if saved_row { value } else { value.get("key")? };
                let key: Key = serde_json::from_value(fields.clone()).ok()?;
                if !key.valid() {
                    return None;
                }
                Action::Key(key)
            }
            "click" => {
                let button = serde_json::from_value(value.get("button")?.clone()).ok()?;
                let at: Option<Point> =
                    serde_json::from_value(value.get("at").cloned().unwrap_or(Value::Null)).ok()?;
                if at.is_some_and(|point| !point.valid()) {
                    return None;
                }
                Action::Click { button, at }
            }
            _ => return None,
        };
        let app = match value.get("app") {
            None | Some(Value::Null) => None,
            Some(name) => app_name(name.as_str()?),
        };
        // 1 ms = 1000 presses a second, Blur's ceiling.
        let milliseconds = value.get("intervalMs")?.as_f64()?;
        if !milliseconds.is_finite() {
            return None;
        }
        let interval_ms = ((milliseconds * 1000.0).round() / 1000.0).clamp(1.0, 86_400_000.0);
        let trigger = match Trigger::read(value.get("trigger")) {
            Some(trigger) => trigger,
            None if saved_row => None,
            None => return None,
        };
        Some(Self { action, interval_ms, app, trigger })
    }
}

#[derive(Clone, Serialize)]
pub struct RowSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(flatten)]
    pub input: Input,
}

impl RowSpec {
    pub fn active_trigger(&self) -> Option<&Trigger> {
        self.input.trigger.as_ref().filter(|_| !self.disabled)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Hotkey {
    #[serde(flatten)]
    pub key: Key,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<Key>,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub meta: bool,
}

impl Hotkey {
    fn function(code: &str, vk: u16) -> Self {
        Self {
            key: Key { label: code.into(), code: code.into(), vk },
            with: None,
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
        }
    }

    pub fn same_binding(&self, other: &Self) -> bool {
        let same_keys = match (&self.with, &other.with) {
            (None, None) => self.key.same_key(&other.key),
            (Some(a), Some(b)) => (self.key.same_key(&other.key) && a.same_key(b))
                || (self.key.same_key(b) && a.same_key(&other.key)),
            _ => false,
        };
        same_keys && self.same_modifiers(other)
    }

    fn same_modifiers(&self, other: &Self) -> bool {
        self.ctrl == other.ctrl && self.alt == other.alt
            && self.shift == other.shift && self.meta == other.meta
    }

    /// A two-key hotkey and a one-key hotkey that is one of its keys, with the
    /// same modifiers. The one-key binding would fire as part of the combo.
    pub fn overlaps(&self, other: &Self) -> bool {
        let (pair, single) = match (&self.with, &other.with) {
            (Some(_), None) => (self, other),
            (None, Some(_)) => (other, self),
            _ => return false,
        };
        pair.same_modifiers(single)
            && (single.key.same_key(&pair.key)
                || pair.with.as_ref().is_some_and(|with| single.key.same_key(with)))
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Hotkeys {
    pub start: Option<Hotkey>,
    pub stop: Option<Hotkey>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            start: Some(Hotkey::function("F6", 117)),
            stop: Some(Hotkey::function("F7", 118)),
        }
    }
}

impl Hotkeys {
    pub fn normalized(mut self) -> Self {
        self.start = self.start.filter(|h| h.key.valid());
        self.stop = self.stop.filter(|h| h.key.valid());
        for key in self.start.iter_mut().chain(&mut self.stop) {
            key.with = None;
        }
        if let (Some(start), Some(stop)) = (&self.start, &self.stop) {
            if start.same_binding(stop) {
                self.start = None;
            }
        }
        self
    }

    fn load(value: Option<&Value>) -> Self {
        let defaults = Self::default();
        let binding = |name: &str, default: Option<Hotkey>| {
            match value.and_then(|v| v.get(name)) {
                None => default,
                Some(v) => serde_json::from_value::<Hotkey>(v.clone()).ok(),
            }
        };
        Self {
            start: binding("start", defaults.start),
            stop: binding("stop", defaults.stop),
        }.normalized()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub rows: Vec<RowSpec>,
    pub pinned: bool,
    pub window: Option<Position>,
    pub hotkeys: Hotkeys,
    pub switch_stop: bool,
    pub two_key: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            pinned: false,
            window: None,
            hotkeys: Hotkeys::default(),
            switch_stop: true,
            two_key: true,
        }
    }
}

fn directory() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("F-7WHISPER"))
}

impl Settings {
    pub fn load() -> Self {
        Self::read().unwrap_or_default()
    }

    fn read() -> Option<Self> {
        let file = File::open(directory()?.join("settings.json")).ok()?;
        let doc: Value = serde_json::from_reader(file).ok()?;
        let mut seen = HashSet::new();
        let mut rows = Vec::new();
        if let Some(items) = doc.get("rows").and_then(Value::as_array) {
            for item in items {
                let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
                let Some(input) = Input::read(item, true) else { continue };
                if !id.trim().is_empty() && seen.insert(id) {
                    let disabled = item.get("disabled").and_then(Value::as_bool).unwrap_or(false);
                    rows.push(RowSpec { id: id.to_owned(), disabled, input });
                }
            }
        }
        Some(Self {
            rows,
            pinned: doc.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            window: doc.get("window").and_then(|v| serde_json::from_value(v.clone()).ok()),
            hotkeys: Hotkeys::load(doc.get("hotkeys")),
            switch_stop: doc.get("switchStop").and_then(Value::as_bool).unwrap_or(true),
            two_key: doc.get("twoKey").and_then(Value::as_bool).unwrap_or(true),
        })
    }

    pub fn save(&self) -> io::Result<()> {
        let dir = directory().ok_or_else(|| io::Error::other("Missing configuration directory"))?;
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("settings.{}.tmp", std::process::id()));
        let result = (|| {
            let mut file = File::create(&tmp)?;
            serde_json::to_writer(&mut file, self)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, dir.join("settings.json"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}
