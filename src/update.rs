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
use crate::logfile;

const LATEST_RELEASE: &str = "https://api.github.com/repos/DevanMetz/openmic/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/DevanMetz/openmic/releases";

/// The release workflow signs each executable with the private half of this
/// Ed25519 key (a GitHub Actions secret); updates without a valid signature
/// are refused, so a tampered release can't install itself.
const RELEASE_KEY: [u8; 32] = hex32("038930dcfe3be8b281437f0fee9a9112ba999e4da32b2f59f6a64db5ce4943d3");

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
    /// The release notes (Markdown).
    pub notes: String,
    exe_name: String,
    exe_url: String,
    sha_url: String,
    sig_url: String,
    size: u64,
}

/// Pick the Windows executable and its checksum out of the latest release,
/// if it is newer than `current`.
fn newer_release(json: &str, current: Version) -> Result<Option<Release>> {
    #[derive(Deserialize)]
    struct Latest {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        body: Option<String>,
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
    let find = |suffix: &str| latest.assets.iter().find(|a| a.name == format!("{exe_name}{suffix}"));
    let (Some(exe), Some(sha)) = (find(""), find(".sha256")) else {
        bail!("{} has no Windows download yet", latest.tag_name);
    };
    let Some(sig) = find(".sig") else {
        bail!("{} isn't signed; download it from the releases page", latest.tag_name);
    };
    Ok(Some(Release {
        exe_url: exe.browser_download_url.clone(),
        sha_url: sha.browser_download_url.clone(),
        sig_url: sig.browser_download_url.clone(),
        exe_name,
        size: exe.size,
        tag: latest.tag_name,
        page: latest.html_url,
        notes: latest.body.unwrap_or_default(),
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

const fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("not lowercase hex"),
    }
}

const fn hex32(hex: &str) -> [u8; 32] {
    let bytes = hex.as_bytes();
    assert!(bytes.len() == 64);
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = hex_digit(bytes[2 * i]) << 4 | hex_digit(bytes[2 * i + 1]);
        i += 1;
    }
    out
}

/// What the release workflow signs: the asset name (so an old signed build
/// can't pose as a newer release) and the executable's SHA-256.
fn signed_message(exe_name: &str, sha256: &[u8]) -> String {
    let hex: String = sha256.iter().map(|b| format!("{b:02x}")).collect();
    format!("openmic-release-v1\n{exe_name}\n{hex}\n")
}

/// Check a hex Ed25519 signature (the `.sig` asset) over the release.
fn verify_signature(key: &[u8; 32], exe_name: &str, sha256: &[u8], signature_hex: &str) -> Result<()> {
    use ed25519_dalek::{Signature, VerifyingKey};
    let hex = signature_hex.trim();
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .map(|i| hex.get(2 * i..2 * i + 2).and_then(|p| u8::from_str_radix(p, 16).ok()))
        .collect::<Option<_>>()
        .filter(|b: &Vec<u8>| hex.len() == 128 && b.len() == 64)
        .context("the update's signature is malformed")?;
    let signature = Signature::from_slice(&bytes).context("the update's signature is malformed")?;
    let key = VerifyingKey::from_bytes(key).context("OpenMic's release key is invalid")?;
    key.verify_strict(signed_message(exe_name, sha256).as_bytes(), &signature)
        .map_err(|_| anyhow::anyhow!("the update's signature doesn't match; nothing was changed"))
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
    key: &[u8; 32],
    dest: &Path,
    done: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<()> {
    let fetch = |url: &str, what: &str| {
        agent
            .get(url)
            .call()
            .and_then(|r| r.into_body().read_to_string())
            .with_context(|| format!("download the update's {what}"))
    };
    let expected = parse_checksum(&fetch(&release.sha_url, "checksum")?)?;
    let signature = fetch(&release.sig_url, "signature")?;

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
        let hash = hasher.finalize();
        if hash.as_slice() != expected {
            bail!("the update failed its checksum; nothing was changed");
        }
        verify_signature(key, &release.exe_name, &hash, &signature)
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
            match &event {
                Event::Checked(Ok(Some(release))) => logfile::info(format_args!("update available: {}", release.tag)),
                Event::Checked(Err(e)) => logfile::warn(format_args!("update check failed: {e:#}")),
                Event::Downloaded(release, Ok(())) => {
                    logfile::info(format_args!("update {} downloaded and verified", release.tag));
                }
                Event::Downloaded(_, Err(e)) => logfile::warn(format_args!("update download stopped: {e:#}")),
                Event::Checked(Ok(None)) => {}
            }
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
            let result = download(&agent(), &release, &RELEASE_KEY, &paths.staged, done, cancel);
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

/// Put the updater in any state, for drawing the window in tests.
#[cfg(test)]
impl Updater {
    pub fn set_status_for_test(&mut self, status: Status) {
        self.status = status;
    }
}

#[cfg(test)]
impl Release {
    pub fn sample(tag: &str) -> Self {
        Self {
            tag: tag.into(),
            page: "https://example.test/release".into(),
            notes: "## Fixes\n\n- Faster **updates**, see [the docs](https://example.test)".into(),
            exe_name: format!("openmic-{tag}-windows-x64.exe"),
            exe_url: String::new(),
            sha_url: String::new(),
            sig_url: String::new(),
            size: 54_000_000,
        }
    }
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

    /// A release listing with the executable plus assets with these suffixes.
    fn release_json(tag: &str, suffixes: &[&str]) -> String {
        let assets: Vec<String> = [""]
            .iter()
            .chain(suffixes)
            .map(|suffix| {
                let ext = if suffix.is_empty() { ".exe" } else { suffix };
                format!(
                    r#"{{"name": "openmic-{tag}-windows-x64.exe{suffix}", "browser_download_url": "https://example.test/{tag}{ext}", "size": 5}}"#
                )
            })
            .collect();
        format!(
            r#"{{"tag_name": "{tag}", "html_url": "https://example.test/releases/{tag}", "assets": [{}]}}"#,
            assets.join(",")
        )
    }

    const SIGNED: &[&str] = &[".sha256", ".sig"];

    #[test]
    fn only_a_newer_signed_release_is_offered() {
        let release = newer_release(&release_json("v0.7.0", SIGNED), (0, 6, 0)).unwrap().unwrap();
        assert_eq!(release.tag, "v0.7.0");
        assert_eq!(release.exe_name, "openmic-v0.7.0-windows-x64.exe");
        assert_eq!(release.exe_url, "https://example.test/v0.7.0.exe");
        assert_eq!(release.sha_url, "https://example.test/v0.7.0.sha256");
        assert_eq!(release.sig_url, "https://example.test/v0.7.0.sig");
        assert_eq!(release.size, 5);
        assert_eq!(release.notes, "", "a release without notes still offers the update");

        let with_notes = release_json("v0.7.0", SIGNED).replacen('{', r###"{"body": "## Fixes\n- Faster","###, 1);
        let release = newer_release(&with_notes, (0, 6, 0)).unwrap().unwrap();
        assert_eq!(release.notes, "## Fixes\n- Faster");

        assert_eq!(newer_release(&release_json("v0.6.0", SIGNED), (0, 6, 0)).unwrap(), None);
        assert_eq!(newer_release(&release_json("v0.5.0", SIGNED), (0, 6, 0)).unwrap(), None);
        assert!(newer_release(&release_json("v0.7.0", &[".sig"]), (0, 6, 0)).is_err());
        let unsigned = newer_release(&release_json("v0.7.0", &[".sha256"]), (0, 6, 0));
        assert!(unsigned.unwrap_err().to_string().contains("isn't signed"), "never install unsigned");
    }

    /// Checks that scripts/sign_release.py and this updater agree, using the
    /// real key: sign a file with the script, then
    /// `OPENMIC_SIGNED_FILE=path cargo test -- --ignored release_script`
    #[test]
    #[ignore]
    fn release_script_signatures_verify_with_the_embedded_key() {
        let path = PathBuf::from(std::env::var("OPENMIC_SIGNED_FILE").expect("set OPENMIC_SIGNED_FILE"));
        let exe = fs::read(&path).unwrap();
        let signature = fs::read_to_string(path.with_extension("exe.sig")).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        verify_signature(&RELEASE_KEY, name, &Sha256::digest(&exe), &signature).unwrap();
    }

    #[test]
    fn the_embedded_release_key_is_a_valid_ed25519_key() {
        assert!(ed25519_dalek::VerifyingKey::from_bytes(&RELEASE_KEY).is_ok());
    }

    /// A throwaway key standing in for the release workflow's secret key.
    fn test_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7; 32])
    }

    fn sign(key: &ed25519_dalek::SigningKey, exe_name: &str, exe: &[u8]) -> String {
        use ed25519_dalek::Signer;
        let signature = key.sign(signed_message(exe_name, &Sha256::digest(exe)).as_bytes());
        signature.to_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn signatures_bind_the_file_name_and_contents() {
        let key = test_key();
        let public = key.verifying_key().to_bytes();
        let name = "openmic-v0.7.0-windows-x64.exe";
        let signature = sign(&key, name, b"exe");
        verify_signature(&public, name, &Sha256::digest(b"exe"), &signature).unwrap();
        assert!(verify_signature(&public, name, &Sha256::digest(b"other"), &signature).is_err());
        assert!(
            verify_signature(&public, "openmic-v0.8.0-windows-x64.exe", &Sha256::digest(b"exe"), &signature).is_err(),
            "an old signed build can't pose as a newer release"
        );
        assert!(verify_signature(&RELEASE_KEY, name, &Sha256::digest(b"exe"), &signature).is_err());
        assert!(verify_signature(&public, name, &Sha256::digest(b"exe"), "abcd").is_err());
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
    /// Each connection answers one request as keep-alive, then hangs up
    /// without a reply if another request arrives on it: the stale pooled
    /// connection that once failed a real update with "peer disconnected".
    fn serve(body: &'static [u8], checksum: String, signature: String) -> (String, thread::JoinHandle<()>) {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let read_request = |reader: &mut std::io::BufReader<std::net::TcpStream>| {
            let mut request = String::new();
            reader.read_line(&mut request).ok()?;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).ok()?;
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            (!request.is_empty()).then_some(request)
        };
        let server = thread::spawn(move || {
            let mut open = Vec::new();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let request = read_request(&mut reader).unwrap();
                let content = if request.contains(".exe ") {
                    body
                } else if request.contains(".sig ") {
                    signature.as_bytes()
                } else {
                    checksum.as_bytes()
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", content.len()).unwrap();
                stream.write_all(content).unwrap();
                open.push(thread::spawn(move || {
                    if read_request(&mut reader).is_some() {
                        drop(stream); // reused: hang up without answering
                    }
                }));
            }
            drop(listener);
            for connection in open {
                let _ = connection.join();
            }
        });
        (base, server)
    }

    fn test_release(base: &str, size: u64) -> Release {
        Release {
            tag: "v9.9.9".into(),
            page: String::new(),
            notes: String::new(),
            exe_name: "openmic.exe".into(),
            exe_url: format!("{base}/openmic.exe"),
            sha_url: format!("{base}/openmic.exe.sha256"),
            sig_url: format!("{base}/openmic.exe.sig"),
            size,
        }
    }

    /// Serve `exe` with a checksum of `listed` and a signature by `signer`,
    /// then download it as the updater does, trusting the test key.
    fn fetch(exe: &'static [u8], listed: &[u8], signer: &ed25519_dalek::SigningKey) -> (Result<()>, tempfile::TempDir, u64) {
        let checksum = format!("{:x}  openmic.exe", Sha256::digest(listed));
        let (base, server) = serve(exe, checksum, sign(signer, "openmic.exe", exe));
        let dir = tempfile::tempdir().unwrap();
        let done = AtomicU64::new(0);
        let trusted = test_key().verifying_key().to_bytes();
        let release = test_release(&base, exe.len() as u64);
        let dest = dir.path().join("openmic.exe.update");
        let result = download(&agent(), &release, &trusted, &dest, &done, &AtomicBool::new(false));
        server.join().unwrap();
        (result, dir, done.load(Ordering::Relaxed))
    }

    #[test]
    fn a_verified_download_is_kept() {
        let (result, dir, done) = fetch(b"new exe", b"new exe", &test_key());
        result.unwrap();
        assert_eq!(fs::read(dir.path().join("openmic.exe.update")).unwrap(), b"new exe");
        assert_eq!(done, 7);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no partial file left");
    }

    #[test]
    fn a_corrupted_download_is_discarded() {
        let (result, dir, _) = fetch(b"bad exe", b"the real exe", &test_key());
        assert!(result.unwrap_err().to_string().contains("checksum"));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// Someone who can edit the release replaces the executable and its
    /// checksum, but can't sign without the release key.
    #[test]
    fn a_replaced_release_without_the_key_is_refused() {
        let forger = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let (result, dir, _) = fetch(b"evil exe", b"evil exe", &forger);
        assert!(result.unwrap_err().to_string().contains("signature"));
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
