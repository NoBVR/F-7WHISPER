use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
};

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Key {
    pub label: String,
    pub code: String,
    pub vk: u16,
}

impl Key {
    pub fn valid(&self) -> bool {
        !self.label.is_empty() && !self.code.trim().is_empty() && (1..255).contains(&self.vk)
    }
}

pub fn clamp(ms: i64) -> i64 {
    ms.clamp(20, 86_400_000)
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowSpec {
    pub id: String,
    #[serde(flatten)]
    pub key: Key,
    pub interval_ms: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

#[derive(Default, Serialize)]
pub struct Settings {
    pub rows: Vec<RowSpec>,
    pub pinned: bool,
    pub window: Option<Bounds>,
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

        for item in doc.get("rows")?.as_array()? {
            let Ok(mut row) = serde_json::from_value::<RowSpec>(item.clone()) else {
                continue;
            };
            if row.id.trim().is_empty() || !row.key.valid() || !seen.insert(row.id.clone()) {
                continue;
            }
            row.interval_ms = clamp(row.interval_ms);
            rows.push(row);
        }

        Some(Self {
            rows,
            pinned: doc.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            window: doc.get("window").and_then(|v| serde_json::from_value(v.clone()).ok()),
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
