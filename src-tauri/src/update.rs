use super::{Inbox, Message};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    process::Command,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};

const LATEST: &str = "https://api.github.com/repos/NoBVR/F-7WHISPER/releases/latest";
const DMG: &str = "F-7WHISPER-macos.dmg";
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Deserialize)]
pub(super) struct Asset {
    name: String,
    browser_download_url: String,
    size: Option<u64>,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

pub(super) enum Event {
    Checked(Option<(String, Asset)>),
    Progress(f64),
    Finished(bool),
}

pub(super) struct Updater {
    inbox: Inbox,
    running: String,
    pub(super) due: Option<Instant>,
    known: Option<(String, Asset)>,
    last: Option<Value>,
    busy: bool,
}

impl Updater {
    pub(super) fn new(app: &AppHandle) -> Self {
        Self {
            inbox: app.state::<Inbox>().inner().clone(),
            running: app.package_info().version.to_string(),
            due: None,
            known: None,
            last: None,
            busy: false,
        }
    }

    pub(super) fn ready(&mut self) -> Option<Value> {
        self.due.get_or_insert_with(|| Instant::now() + Duration::from_secs(3));
        self.last.clone()
    }

    pub(super) fn tick(&mut self, now: Instant) {
        if !self.due.is_some_and(|due| due <= now) {
            return;
        }
        self.due = Some(now + Duration::from_secs(6 * 60 * 60));
        let running = self.running.clone();
        let inbox = self.inbox.clone();
        tauri::async_runtime::spawn_blocking(move || match check(&running) {
            Ok(found) => inbox.post(Message::Update(Event::Checked(found))),
            Err(e) => eprintln!("Update check failed: {e}"),
        });
    }

    fn publish(&mut self, status: &str, progress: f64) -> Option<Value> {
        let (version, _) = self.known.as_ref()?;
        self.last = Some(json!({ "type": "update", "version": version,
                                 "status": status, "progress": progress }));
        self.last.clone()
    }

    pub(super) fn install(&mut self) -> Option<Value> {
        if self.busy {
            return None;
        }
        let (version, asset) = self.known.clone()?;
        self.busy = true;
        let running = self.running.clone();
        let inbox = self.inbox.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let result = download(&running, &version, &asset, |p| {
                inbox.post(Message::Update(Event::Progress(p)));
            });
            if let Err(e) = &result {
                eprintln!("Update install failed: {e}");
            }
            inbox.post(Message::Update(Event::Finished(result.is_ok())));
        });
        self.publish("downloading", 0.0)
    }

    pub(super) fn handle(&mut self, event: Event) -> Option<Value> {
        match event {
            Event::Checked(found) if !self.busy => {
                let changed = self.known.as_ref().map(|r| &r.0) != found.as_ref().map(|r| &r.0);
                self.known = found;
                if changed {
                    self.last = None;
                    return self.publish("available", 0.0);
                }
                None
            }
            Event::Progress(p) if self.busy => self.publish("downloading", p),
            Event::Finished(ok) if self.busy => {
                self.busy = false;
                self.publish(if ok { "opened" } else { "failed" }, 0.0)
            }
            _ => None,
        }
    }
}

fn numbers(s: &str) -> Result<[u64; 3]> {
    let mut parts = s.split('.');
    let mut result = [0; 3];
    for n in &mut result {
        let p = parts.next().ok_or("missing version component")?;
        if p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()) {
            return Err("invalid version component".into());
        }
        *n = p.parse()?;
    }
    if parts.next().is_some() {
        return Err("extra version component".into());
    }
    Ok(result)
}

fn agent(running: &str) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(&format!("F-7WHISPER/{running}"))
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build()
}

fn check(running: &str) -> Result<Option<(String, Asset)>> {
    let response = agent(running).get(LATEST).set("Accept", "application/vnd.github+json").call()?;
    let release: Release = serde_json::from_reader(response.into_reader())?;
    let version = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    if numbers(version)? <= numbers(running)? {
        return Ok(None);
    }
    let Some(asset) = release.assets.into_iter().find(|a| a.name == DMG) else { return Ok(None) };
    if tauri::Url::parse(&asset.browser_download_url)?.scheme() != "https" {
        return Err("invalid release asset URL".into());
    }
    Ok(Some((version.to_owned(), asset)))
}

fn download(running: &str, version: &str, asset: &Asset, progress: impl Fn(f64)) -> Result<()> {
    let downloads = dirs::home_dir().ok_or("missing home directory")?.join("Downloads");
    fs::create_dir_all(&downloads)?;
    let path = downloads.join(format!("F-7WHISPER-{version}.dmg"));
    let mut partial = tempfile::NamedTempFile::new_in(downloads)?;
    let response = agent(running).get(&asset.browser_download_url).call()?;
    let length = response.header("Content-Length").map(str::parse::<u64>).transpose()?;
    let mut reader = response.into_reader();
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut last = Instant::now();
    let mut buf = [0; 65536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        partial.write_all(&buf[..n])?;
        hash.update(&buf[..n]);
        bytes += n as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            if let Some(total) = length.or(asset.size).filter(|n| *n > 0) {
                progress((bytes as f64 / total as f64).min(1.0));
            }
            last = Instant::now();
        }
    }
    if [length, asset.size].into_iter().flatten().any(|n| n != bytes) {
        return Err("incomplete disk image".into());
    }
    if let Some(expected) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
        if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected) {
            return Err("SHA-256 mismatch".into());
        }
    }
    progress(1.0);
    partial.as_file().sync_all()?;
    partial.persist(&path)?;
    if !Command::new("/usr/bin/open").arg(path).status()?.success() {
        return Err("open failed".into());
    }
    Ok(())
}
