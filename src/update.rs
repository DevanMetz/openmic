//! Updates from GitHub Releases: check for a newer version, download its
//! executable beside the running one, verify it against the release's
//! SHA-256 checksum, then swap it in and restart. Nothing installs without
//! the user asking, so an update never cuts off a call.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::dictation::{download_agent, read_download_chunk};

const LATEST_RELEASE: &str = "https://api.github.com/repos/DevanMetz/openmic/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/DevanMetz/openmic/releases";

/// How often a running OpenMic looks for a new release.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Passed to the new executable with the old process id, so it waits for
/// the old copy to quit before taking over.
pub const AFTER_UPDATE_ARG: &str = "--after-update";

/// A `major.minor.patch` version.
pub type Version = (u64, u64, u64);

/// "v0.6.0" or "0.6.0"; pre-release tags such as "v0.7.0-rc1" are not offered.
pub fn parse_version(tag: &str) -> Option<Version> {
    let mut parts = tag.strip_prefix('v').unwrap_or(tag).split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

pub fn current_version() -> Version {
    parse_version(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a plain version")
}

/// A newer release and where to get it.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    /// The release page, with its notes.
    pub page: String,
    exe_url: String,
    sha_url: String,
    size: u64,
}

/// Pick the Windows executable and its checksum out of the latest release,
/// if it is newer than `current`.
fn newer_release(json: &str, current: Version) -> Result<Option<Release>> {
    #[derive(Deserialize)]
    struct Latest {
        tag_name: String,
        html_url: String,
        assets: Vec<Asset>,
    }
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        size: u64,
    }
    let latest: Latest = serde_json::from_str(json).context("read the release list")?;
    if parse_version(&latest.tag_name).is_none_or(|v| v <= current) {
        return Ok(None);
    }
    let exe_name = format!("openmic-{}-windows-x64.exe", latest.tag_name);
    let sha_name = format!("{exe_name}.sha256");
    let find = |name: &str| latest.assets.iter().find(|a| a.name == name);
    let (Some(exe), Some(sha)) = (find(&exe_name), find(&sha_name)) else {
        bail!("{} has no Windows download yet", latest.tag_name);
    };
    Ok(Some(Release {
        exe_url: exe.browser_download_url.clone(),
        sha_url: sha.browser_download_url.clone(),
        size: exe.size,
        tag: latest.tag_name,
        page: latest.html_url,
    }))
}

/// The hash from a `sha256sum`-style line: "<64 hex digits>  <file name>".
fn parse_checksum(text: &str) -> Result<[u8; 32]> {
    let hex = text.split_whitespace().next().unwrap_or_default();
    let mut hash = [0u8; 32];
    if hex.len() != 64 {
        bail!("the release checksum is malformed");
    }
    for (byte, pair) in hash.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).ok().and_then(|p| u8::from_str_radix(p, 16).ok());
        *byte = pair.context("the release checksum is malformed")?;
    }
    Ok(hash)
}

/// Where the running executable is, and the files an update puts beside it.
struct Paths {
    exe: PathBuf,
    /// The verified new executable, waiting to be swapped in.
    staged: PathBuf,
    /// The previous executable, deleted once the new one is running.
    old: PathBuf,
}

impl Paths {
    fn for_exe(exe: PathBuf) -> Self {
        let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self {
            staged: exe.with_file_name(format!("{name}.update")),
            old: exe.with_file_name(format!("{name}.old")),
            exe,
        }
    }

    fn current() -> Result<Self> {
        Ok(Self::for_exe(std::env::current_exe().context("locate OpenMic")?))
    }
}

fn check(agent: &ureq::Agent, url: &str) -> Result<Option<Release>> {
    let json = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", concat!("OpenMic/", env!("CARGO_PKG_VERSION")))
        .call()
        .context("check for updates")?
        .into_body()
        .read_to_string()
        .context("check for updates")?;
    newer_release(&json, current_version())
}

/// Download the release's executable to `dest`, keeping it only if it
/// matches the published size and checksum.
fn download(
    agent: &ureq::Agent,
    release: &Release,
    dest: &Path,
    done: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<()> {
    let checksum = agent
        .get(&release.sha_url)
        .call()
        .and_then(|r| r.into_body().read_to_string())
        .context("download the update's checksum")?;
    let expected = parse_checksum(&checksum)?;

    let partial = dest.with_extension("part");
    let result = (|| -> Result<()> {
        let response = agent.get(&release.exe_url).call().context("download the update")?;
        let mut reader = response.into_body().into_reader();
        let mut out = File::create(&partial).with_context(|| {
            format!("save the update beside OpenMic in {}", partial.parent().unwrap_or(dest).display())
        })?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut got = 0u64;
        loop {
            let n = read_download_chunk(&mut reader, &mut buf, cancel).context("download the update")?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n]).context("save the update")?;
            got += n as u64;
            done.store(got, Ordering::Relaxed);
        }
        out.sync_all().context("save the update")?;
        if got != release.size {
            bail!("the update download was cut short");
        }
        if hasher.finalize().as_slice() != expected {
            bail!("the update failed its checksum; nothing was changed");
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
    fs::rename(&partial, dest).context("save the update")
}

/// Swap the staged executable in. Windows lets a running executable be
/// renamed (not deleted), so the old one steps aside to `.old`.
fn replace(paths: &Paths) -> Result<()> {
    if paths.old.exists() {
        fs::remove_file(&paths.old).with_context(|| format!("remove {}", paths.old.display()))?;
    }
    fs::rename(&paths.exe, &paths.old).context("move the current OpenMic aside")?;
    if let Err(e) = fs::rename(&paths.staged, &paths.exe) {
        let _ = fs::rename(&paths.old, &paths.exe);
        return Err(e).context("put the new OpenMic in place");
    }
    Ok(())
}

/// Undo [`replace`] if the new executable could not be started.
fn roll_back(paths: &Paths) {
    if fs::rename(&paths.exe, &paths.staged).is_ok() {
        let _ = fs::rename(&paths.old, &paths.exe);
    }
}

/// Run at startup, before taking the single-instance lock. After an update,
/// wait for the old copy to finish quitting (it restores the default mic
/// and saves settings on the way out), then tidy its leftovers. Returns
/// whether this launch is the new version starting up.
pub fn finish_update() -> bool {
    let mut args = std::env::args().skip_while(|a| a != AFTER_UPDATE_ARG).skip(1);
    let predecessor = args.next().and_then(|pid| pid.parse::<u32>().ok());
    if let Some(pid) = predecessor {
        wait_for_exit(pid, Duration::from_secs(20));
    }
    if let Ok(paths) = Paths::current() {
        // Present only after an update, or one that was never installed.
        let _ = fs::remove_file(&paths.old);
        let _ = fs::remove_file(&paths.staged);
        let _ = fs::remove_file(paths.staged.with_extension("part"));
    }
    predecessor.is_some()
}

#[cfg(windows)]
fn wait_for_exit(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: plain Win32 calls on a handle we open and close here.
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(process, timeout.as_millis() as u32);
            let _ = CloseHandle(process);
        }
    }
}

#[cfg(not(windows))]
fn wait_for_exit(_pid: u32, _timeout: Duration) {}

/// What the updater is doing, for the window.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// Downloaded and verified; restarting installs it.
    Ready(Release),
    Failed(String),
}

enum Event {
    Checked(Result<Option<Release>>),
    Downloaded(Release, Result<()>),
}

/// Background checks and downloads, and their progress.
pub struct Updater {
    status: Status,
    /// Check once a day on its own (off in development builds and tests).
    automatic: bool,
    last_check: Option<Instant>,
    events: (Sender<Event>, Receiver<Event>),
    wake: Arc<dyn Fn() + Send + Sync>,
    done: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl Updater {
    /// `wake` is called when a check or download finishes.
    pub fn new(automatic: bool, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            status: Status::Idle,
            automatic,
            last_check: None,
            events: crossbeam_channel::unbounded(),
            wake: Arc::new(wake),
            done: Arc::default(),
            cancel: Arc::default(),
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// (bytes downloaded, bytes expected) while downloading.
    pub fn progress(&self) -> Option<(u64, u64)> {
        match &self.status {
            Status::Downloading(release) => Some((self.done.load(Ordering::Relaxed), release.size)),
            _ => None,
        }
    }

    /// Collect finished work, and start the daily check when `enabled`.
    pub fn tick(&mut self, enabled: bool) {
        while let Ok(event) = self.events.1.try_recv() {
            self.status = match event {
                Event::Checked(Ok(Some(release))) => Status::Available(release),
                Event::Checked(Ok(None)) => Status::UpToDate,
                Event::Checked(Err(e)) => Status::Failed(format!("{e:#}")),
                Event::Downloaded(release, Ok(())) => Status::Ready(release),
                Event::Downloaded(release, Err(_)) if self.cancel.load(Ordering::Relaxed) => {
                    Status::Available(release)
                }
                Event::Downloaded(_, Err(e)) => Status::Failed(format!("{e:#}")),
            };
        }
        let idle = matches!(self.status, Status::Idle | Status::UpToDate | Status::Failed(_));
        if enabled && self.automatic && idle && self.last_check.is_none_or(|t| t.elapsed() >= CHECK_EVERY) {
            self.check();
        }
    }

    pub fn check(&mut self) {
        if matches!(self.status, Status::Checking | Status::Downloading(_)) {
            return;
        }
        self.last_check = Some(Instant::now());
        self.status = Status::Checking;
        self.spawn("openmic-update-check", |_, _| Event::Checked(check(&agent(), &feed())));
    }

    /// Download and verify the available release.
    pub fn download(&mut self) {
        let Status::Available(release) = &self.status else { return };
        let release = release.clone();
        let paths = match Paths::current() {
            Ok(paths) => paths,
            Err(e) => {
                self.status = Status::Failed(format!("{e:#}"));
                return;
            }
        };
        self.done.store(0, Ordering::Relaxed);
        self.cancel.store(false, Ordering::Relaxed);
        self.status = Status::Downloading(release.clone());
        self.spawn("openmic-update-download", move |done, cancel| {
            let result = download(&agent(), &release, &paths.staged, done, cancel);
            Event::Downloaded(release, result)
        });
    }

    pub fn cancel_download(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Put the downloaded version in place and start it. The caller then
    /// quits; the new copy waits for that before it opens any devices.
    pub fn install(&mut self) -> Result<()> {
        let Status::Ready(_) = &self.status else { bail!("no update is ready") };
        let paths = Paths::current()?;
        replace(&paths)?;
        let started = std::process::Command::new(&paths.exe)
            .arg(AFTER_UPDATE_ARG)
            .arg(std::process::id().to_string())
            .spawn();
        if let Err(e) = started {
            roll_back(&paths);
            return Err(e).context("start the new OpenMic");
        }
        Ok(())
    }

    fn spawn(&self, name: &str, work: impl FnOnce(&AtomicU64, &AtomicBool) -> Event + Send + 'static) {
        let (events, wake) = (self.events.0.clone(), Arc::clone(&self.wake));
        let (done, cancel) = (Arc::clone(&self.done), Arc::clone(&self.cancel));
        let spawned = thread::Builder::new().name(name.into()).spawn(move || {
            let _ = events.send(work(&done, &cancel));
            wake();
        });
        if let Err(e) = spawned {
            self.events.0.send(Event::Checked(Err(e.into()))).ok();
        }
    }
}

/// Development builds can point at a local mock release to try an update.
fn feed() -> String {
    std::env::var("OPENMIC_UPDATE_FEED")
        .ok()
        .filter(|_| cfg!(debug_assertions))
        .unwrap_or_else(|| LATEST_RELEASE.into())
}

fn agent() -> ureq::Agent {
    download_agent(Duration::from_secs(20))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v0.6.0"), Some((0, 6, 0)));
        assert_eq!(parse_version("0.0.1"), Some((0, 0, 1)));
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
        assert_eq!(parse_version("v0.7.0-rc1"), None, "pre-releases are not offered");
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert_eq!(current_version(), parse_version(env!("CARGO_PKG_VERSION")).unwrap());
    }

    fn release_json(tag: &str, with_checksum: bool) -> String {
        let mut assets = vec![format!(
            r#"{{"name": "openmic-{tag}-windows-x64.exe", "browser_download_url": "https://example.test/{tag}.exe", "size": 5}}"#
        )];
        if with_checksum {
            assets.push(format!(
                r#"{{"name": "openmic-{tag}-windows-x64.exe.sha256", "browser_download_url": "https://example.test/{tag}.sha256", "size": 100}}"#
            ));
        }
        format!(
            r#"{{"tag_name": "{tag}", "html_url": "https://example.test/releases/{tag}", "assets": [{}]}}"#,
            assets.join(",")
        )
    }

    #[test]
    fn only_a_newer_release_with_both_assets_is_offered() {
        let release = newer_release(&release_json("v0.7.0", true), (0, 6, 0)).unwrap().unwrap();
        assert_eq!(release.tag, "v0.7.0");
        assert_eq!(release.exe_url, "https://example.test/v0.7.0.exe");
        assert_eq!(release.sha_url, "https://example.test/v0.7.0.sha256");
        assert_eq!(release.size, 5);

        assert_eq!(newer_release(&release_json("v0.6.0", true), (0, 6, 0)).unwrap(), None);
        assert_eq!(newer_release(&release_json("v0.5.0", true), (0, 6, 0)).unwrap(), None);
        assert!(newer_release(&release_json("v0.7.0", false), (0, 6, 0)).is_err(), "never install unverified");
    }

    #[test]
    fn checksums_are_read_from_sha256sum_lines() {
        let hex = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let hash = parse_checksum(&format!("{hex}  openmic-v0.7.0-windows-x64.exe\r\n")).unwrap();
        assert_eq!(hash.as_slice(), Sha256::digest(b"hello").as_slice());
        assert!(parse_checksum("abc  file.exe").is_err());
        assert!(parse_checksum(&format!("{}zz  f", &hex[..62])).is_err());
    }

    /// Serves `body` for paths ending in ".exe" and `checksum` otherwise.
    fn serve(body: &'static [u8], checksum: String, requests: usize) -> (String, thread::JoinHandle<()>) {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let content = if request.contains(".exe ") { body } else { checksum.as_bytes() };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", content.len())
                    .unwrap();
                stream.write_all(content).unwrap();
            }
        });
        (base, server)
    }

    fn test_release(base: &str, size: u64) -> Release {
        Release {
            tag: "v9.9.9".into(),
            page: String::new(),
            exe_url: format!("{base}/openmic.exe"),
            sha_url: format!("{base}/openmic.exe.sha256"),
            size,
        }
    }

    #[test]
    fn a_verified_download_is_kept() {
        let checksum = format!("{:x}  openmic.exe", Sha256::digest(b"new exe"));
        let (base, server) = serve(b"new exe", checksum, 2);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("openmic.exe.update");
        let done = AtomicU64::new(0);
        download(&agent(), &test_release(&base, 7), &dest, &done, &AtomicBool::new(false)).unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new exe");
        assert_eq!(done.load(Ordering::Relaxed), 7);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no partial file left");
    }

    #[test]
    fn a_tampered_download_is_discarded() {
        let checksum = format!("{:x}  openmic.exe", Sha256::digest(b"the real exe"));
        let (base, server) = serve(b"evil exe", checksum, 2);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("openmic.exe.update");
        let result = download(&agent(), &test_release(&base, 8), &dest, &AtomicU64::new(0), &AtomicBool::new(false));
        server.join().unwrap();
        assert!(result.unwrap_err().to_string().contains("checksum"));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn replacing_swaps_the_executable_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::for_exe(dir.path().join("openmic.exe"));
        fs::write(&paths.exe, "old").unwrap();
        fs::write(&paths.staged, "new").unwrap();
        fs::write(&paths.old, "from an earlier update").unwrap();
        replace(&paths).unwrap();
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "new");
        assert_eq!(fs::read_to_string(&paths.old).unwrap(), "old");
        assert!(!paths.staged.exists());

        roll_back(&paths);
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "old");
        assert_eq!(fs::read_to_string(&paths.staged).unwrap(), "new");
    }

    #[test]
    fn a_missing_update_leaves_the_current_executable_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::for_exe(dir.path().join("openmic.exe"));
        fs::write(&paths.exe, "old").unwrap();
        assert!(replace(&paths).is_err());
        assert_eq!(fs::read_to_string(&paths.exe).unwrap(), "old");
    }

    #[test]
    fn automatic_checks_are_off_unless_enabled() {
        let mut updater = Updater::new(false, || {});
        updater.tick(true);
        assert_eq!(updater.status(), &Status::Idle);
    }
}
