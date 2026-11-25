use super::{Inbox, Message};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    os::unix::{fs::{MetadataExt, PermissionsExt}, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tempfile::{NamedTempFile, TempDir};

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
    Installing,
    Opened,
    Failed,
}

pub(super) struct Updater {
    inbox: Inbox,
    running: String,
    pub(super) due: Option<Instant>,
    known: Option<(String, Asset)>,
    last: Option<Value>,
    busy: bool,
    stopping: Arc<Mutex<bool>>,
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
            stopping: Arc::new(Mutex::new(false)),
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
        let stopping = self.stopping.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let download = download(&running, &asset, |p| {
                inbox.post(Message::Update(Event::Progress(p)));
            });
            let event = download.and_then(|file| install_image(file, &version, &stopping))
                .unwrap_or_else(|e| {
                    eprintln!("Update install failed: {e}");
                    Event::Failed
                });
            inbox.post(Message::Update(event));
        });
        self.publish("downloading", 0.0)
    }

    pub(super) fn shutdown(&self) {
        // Allow an active mount to detach and the worker to hand off its files before exiting.
        *self.stopping.lock().unwrap_or_else(|e| e.into_inner()) = true;
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
            Event::Installing if self.busy => self.publish("installing", 1.0),
            Event::Opened | Event::Failed if self.busy => {
                self.busy = false;
                self.publish(if matches!(event, Event::Opened) { "opened" } else { "failed" }, 0.0)
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

fn download(running: &str, asset: &Asset, progress: impl Fn(f64)) -> Result<NamedTempFile> {
    let downloads = dirs::home_dir().ok_or("missing home directory")?.join("Downloads");
    fs::create_dir_all(&downloads)?;
    let mut partial = tempfile::Builder::new().suffix(".dmg").tempfile_in(downloads)?;
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
    Ok(partial)
}

fn install_image(mut partial: NamedTempFile, version: &str, stopping: &Mutex<bool>) -> Result<Event> {
    let closing = stopping.lock().unwrap_or_else(|e| e.into_inner());
    if *closing {
        return Err("app is closing".into());
    }
    let path = partial.path().with_file_name(format!("F-7WHISPER-{version}.dmg"));
    if let Some((bundle, work)) = prepare(partial.path())? {
        spawn_helper(&bundle, work.path(), partial.path(), &path)?;
        // The helper owns both paths from here, including cleanup on rollback.
        partial.disable_cleanup(true);
        let _ = work.keep();
        return Ok(Event::Installing);
    }
    partial.persist(&path)?;
    run(Command::new("/usr/bin/open").arg(path))?;
    Ok(Event::Opened)
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{:?} exited with {status}", command.get_program()).into());
    }
    Ok(())
}

fn destination() -> Result<(PathBuf, TempDir)> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if exe.components().any(|c| c.as_os_str() == "AppTranslocation") {
        return Err("running from AppTranslocation".into());
    }
    let macos = exe.parent().filter(|p| p.ends_with("Contents/MacOS"));
    let bundle = macos.and_then(|p| p.ancestors().nth(2))
        .filter(|p| p.extension() == Some(OsStr::new("app")))
        .ok_or("executable is outside an app bundle")?;
    let parent = bundle.parent().ok_or("bundle has no parent")?;
    // Creating the private staging directory also tests effective write access.
    let work = tempfile::Builder::new().prefix(".F-7WHISPER-update-")
        .permissions(fs::Permissions::from_mode(0o700)).tempdir_in(parent)?;
    Ok((bundle.to_owned(), work))
}

fn prepare(dmg: &Path) -> Result<Option<(PathBuf, TempDir)>> {
    let (bundle, work) = match destination() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Using manual update: {e}");
            return Ok(None);
        }
    };
    let mount = tempfile::tempdir()?;
    let device = mount.path().metadata()?.dev();
    let attached = run(Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
        .arg(mount.path()).arg(dmg).stdin(Stdio::null()));
    // An attach error can still leave a filesystem at the mount point.
    let mounted = attached.is_ok() || mount.path().metadata().is_ok_and(|m| m.dev() != device);
    let staged = attached.and_then(|()| stage(mount.path(), &bundle, work.path()));
    if mounted {
        detach(mount)?;
    }
    Ok(staged?.then_some((bundle, work)))
}

fn detach(mount: TempDir) -> Result<()> {
    let detach = |force| {
        let mut cmd = Command::new("/usr/bin/hdiutil");
        cmd.arg("detach");
        if force {
            cmd.arg("-force");
        }
        run(cmd.arg(mount.path()))
    };
    if detach(false).is_err() {
        if let Err(e) = detach(true) {
            // Never recursively clean a directory that is still mounted.
            return Err(format!("could not detach {}: {e}", mount.keep().display()).into());
        }
    }
    Ok(())
}

fn find_app(root: &Path) -> Result<PathBuf> {
    let mut other = None;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension() == Some(OsStr::new("app")) && entry.file_type()?.is_dir() {
            if entry.file_name() == "F-7WHISPER.app" {
                return Ok(path);
            }
            other.get_or_insert(path);
        }
    }
    other.ok_or_else(|| "no top-level app in disk image".into())
}

fn verify(bundle: &Path, app: &Path) -> Result<()> {
    let output = Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(bundle).output()?;
    if !output.status.success() {
        return Err(format!("codesign -d failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    let requirements = std::str::from_utf8(&output.stdout)?;
    let rule = requirements.lines().find_map(|line| line.strip_prefix("designated => "))
        .filter(|r| !r.trim().is_empty() && !r.contains("cdhash"))
        .ok_or("missing or ad-hoc designated requirement")?;
    run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"])
        .arg(format!("-R={rule}")).arg(app))
}

fn stage(root: &Path, bundle: &Path, work: &Path) -> Result<bool> {
    let app = find_app(root)?;
    if let Err(e) = verify(bundle, &app) {
        eprintln!("Using manual update: {e}");
        return Ok(false);
    }
    let staged = work.join("new").join(bundle.file_name().ok_or("bundle has no name")?);
    run(Command::new("/usr/bin/ditto").arg(&app).arg(&staged))?;
    let _ = Command::new("/usr/bin/xattr").args(["-dr", "com.apple.quarantine"])
        .arg(&staged).status();
    Ok(true)
}

fn spawn_helper(bundle: &Path, work: &Path, dmg: &Path, manual: &Path) -> Result<()> {
    const INSTALL: &str = r#"
pid=$1 bundle=$2 work=$3 dmg=$4 manual=$5
name=${bundle##*/}
parent=${bundle%/*}/
backup="$work/$name"
staged="$work/new/$name"

manual_install() {
    if [ ! -d "$manual" ] && /bin/mv -fh "$dmg" "$manual"; then
        /usr/bin/open "$manual"
    else
        /usr/bin/open "$dmg"
    fi
    exit 1
}

seconds=0
while kill -0 "$pid" 2>/dev/null; do
    if [ "$seconds" -ge 30 ]; then
        /bin/rm -rf "$work"
        manual_install
    fi
    /bin/sleep 1
    seconds=$((seconds + 1))
done

if /bin/mv "$bundle" "$work/"; then
    if /bin/mv "$staged" "$parent"; then
        if /usr/bin/open "$bundle"; then
            /bin/rm -f "$dmg"
            /bin/rm -rf "$work"
            exit 0
        fi
        # Move the unlaunchable replacement aside before restoring the old app.
        if ! /bin/mv "$bundle" "$work/new/"; then
            /usr/bin/open "$backup"
            manual_install
        fi
    fi
    if ! /bin/mv "$backup" "$parent"; then
        /usr/bin/open "$backup"
        manual_install
    fi
fi
/bin/rm -rf "$work"
/usr/bin/open "$bundle"
manual_install
"#;
    Command::new("/bin/sh").args(["-c", INSTALL, "f7whisper-update"])
        .arg(std::process::id().to_string()).arg(bundle).arg(work).arg(dmg).arg(manual)
        .current_dir("/").process_group(0)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    Ok(())
}
