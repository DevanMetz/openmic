//! Updates from GitHub Releases: check for a newer version, download its
//! package beside the running executable, verify the SHA-256 checksum and
//! signature, then swap in the new binary and restart. On Linux the package
//! is a `.tar.gz` and only the `openmic` file inside it is installed.
//! Nothing installs without the user asking, so an update never cuts off a call.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::dictation::{check_download_cancelled, download_agent, read_download_chunk};
use crate::logfile;

const LATEST_RELEASE: &str = "https://api.github.com/repos/DevanMetz/openmic/releases/latest";
pub const RELEASES_PAGE: &str = "https://github.com/DevanMetz/openmic/releases";

/// Windows and the published Linux x86_64 build can replace themselves.
/// Tests still exercise the controls on every host. Other Linux architectures
/// have no release archive, so the updater stays off there.
pub fn updates_supported() -> bool {
    #[cfg(test)]
    if UPDATES_UNSUPPORTED.with(std::cell::Cell::get) {
        return false;
    }
    cfg!(test) || package_name("0", std::env::consts::OS, std::env::consts::ARCH).is_some()
}

/// Asset file name for `tag` on `os`/`arch`, when a release publishes one.
fn package_name(tag: &str, os: &str, arch: &str) -> Option<String> {
    match os {
        // Every Windows build looks for the published x64 executable, which
        // is the only Windows asset the updater has ever installed.
        "windows" => Some(format!("openmic-{tag}-windows-x64.exe")),
        "linux" if arch == "x86_64" => Some(format!("openmic-{tag}-linux-x86_64.tar.gz")),
        _ => None,
    }
}

#[cfg(test)]
thread_local! {
    static UPDATES_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Draw the Linux UI, which has no in-app installer.
#[cfg(test)]
pub struct WithoutUpdater;

#[cfg(test)]
impl WithoutUpdater {
    pub fn enter() -> Self {
        UPDATES_UNSUPPORTED.with(|flag| flag.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for WithoutUpdater {
    fn drop(&mut self) {
        UPDATES_UNSUPPORTED.with(|flag| flag.set(false));
    }
}

/// The release workflow signs each executable with the private half of this
/// Ed25519 key (a GitHub Actions secret); updates without a valid signature
/// are refused, so a tampered release can't install itself.
const RELEASE_KEY: [u8; 32] =
    hex32("038930dcfe3be8b281437f0fee9a9112ba999e4da32b2f59f6a64db5ce4943d3");

/// How often a running OpenMic looks for a new release.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Passed to the new executable with the old process id, so it waits for
/// the old copy to quit before taking over.
pub const AFTER_UPDATE_ARG: &str = "--after-update";

/// A `major.minor.patch` version.
pub type Version = (u64, u64, u64);

/// "v0.6.0" or "0.6.0"; pre-release tags such as "v0.7.0-rc1" are not offered.
pub fn parse_version(tag: &str) -> Option<Version> {
    let mut parts = tag
        .strip_prefix('v')
        .unwrap_or(tag)
        .split('.')
        .map(|p| p.parse::<u64>().ok());
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

/// Pick this host's signed package out of the latest release, if it is newer
/// than `current`.
fn newer_release(json: &str, current: Version) -> Result<Option<Release>> {
    newer_release_for(json, current, std::env::consts::OS, std::env::consts::ARCH)
}

fn newer_release_for(
    json: &str,
    current: Version,
    os: &str,
    arch: &str,
) -> Result<Option<Release>> {
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
    let Some(exe_name) = package_name(&latest.tag_name, os, arch) else {
        bail!(
            "in-app updates are not published for {os} {arch}; download a build from the releases page"
        );
    };
    let find = |suffix: &str| {
        latest
            .assets
            .iter()
            .find(|a| a.name == format!("{exe_name}{suffix}"))
    };
    let (Some(exe), Some(sha)) = (find(""), find(".sha256")) else {
        let kind = if os == "linux" { "Linux" } else { "Windows" };
        bail!("{} has no {kind} download yet", latest.tag_name);
    };
    let Some(sig) = find(".sig") else {
        bail!(
            "{} isn't signed; download it from the releases page",
            latest.tag_name
        );
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
        let pair = std::str::from_utf8(pair)
            .ok()
            .and_then(|p| u8::from_str_radix(p, 16).ok());
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
fn verify_signature(
    key: &[u8; 32],
    exe_name: &str,
    sha256: &[u8],
    signature_hex: &str,
) -> Result<()> {
    use ed25519_dalek::{Signature, VerifyingKey};
    let hex = signature_hex.trim();
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .map(|i| {
            hex.get(2 * i..2 * i + 2)
                .and_then(|p| u8::from_str_radix(p, 16).ok())
        })
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
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            staged: exe.with_file_name(format!("{name}.update")),
            old: exe.with_file_name(format!("{name}.old")),
            exe,
        }
    }

    fn current() -> Result<Self> {
        Ok(Self::for_exe(
            std::env::current_exe().context("locate OpenMic")?,
        ))
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

fn linux_archive(name: &str) -> bool {
    name.ends_with(".tar.gz")
}

/// The release script packs `openmic` under a directory named after the asset.
fn linux_binary_member(asset_name: &str) -> Result<String> {
    let stem = asset_name
        .strip_suffix(".tar.gz")
        .context("the Linux update is not a .tar.gz")?;
    Ok(format!("{stem}/openmic"))
}

/// A signed gzip can still expand. Stop rather than write an enormous file.
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;

/// The `openmic` binary inside a verified `.tar.gz`, and nothing else in it.
fn extract_linux_binary(gzipped: &[u8], asset_name: &str) -> Result<Vec<u8>> {
    let member = linux_binary_member(asset_name)?;
    let decoder = flate2::read::GzDecoder::new(gzipped);
    let mut reader = Limited::new(decoder, MAX_ARCHIVE_BYTES);
    loop {
        let mut header = [0u8; 512];
        read_archive(&mut reader, &mut header)?;
        if header == [0u8; 512] {
            bail!("the update archive has no OpenMic binary");
        }
        check_tar_header(&header)?;
        let name = tar_name(&header)?;
        let size = tar_size(&header)?;
        let kind = header[156];
        if name == member || name == format!("./{member}") {
            if kind != b'0' && kind != 0 {
                bail!("the OpenMic binary in the update is not a regular file");
            }
            let mut binary = vec![0u8; size];
            read_archive(&mut reader, &mut binary)?;
            discard_archive(&mut reader, padded_size(size) - size)?;
            return Ok(binary);
        }
        discard_archive(&mut reader, padded_size(size))?;
    }
}

fn padded_size(size: usize) -> usize {
    size.div_ceil(512) * 512
}

fn check_tar_header(header: &[u8; 512]) -> Result<()> {
    let stored = parse_octal(&header[148..156])?;
    let sum = header
        .iter()
        .copied()
        .enumerate()
        .fold(0u32, |sum, (index, byte)| {
            sum + u32::from(if (148..156).contains(&index) {
                b' '
            } else {
                byte
            })
        });
    if u64::from(sum) != stored {
        bail!("the update archive is damaged");
    }
    Ok(())
}

fn tar_name(header: &[u8; 512]) -> Result<String> {
    let name = tar_field(&header[..100])?;
    let prefix = if &header[257..262] == b"ustar" {
        tar_field(&header[345..500])?
    } else {
        ""
    };
    if prefix.is_empty() {
        Ok(name.to_owned())
    } else {
        Ok(format!("{prefix}/{name}"))
    }
}

fn tar_field(field: &[u8]) -> Result<&str> {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).context("the update archive has a damaged file name")
}

fn tar_size(header: &[u8; 512]) -> Result<usize> {
    let size = parse_octal(&header[124..136])?;
    if size > MAX_BINARY_BYTES {
        bail!("the update archive is too large");
    }
    usize::try_from(size).context("the update archive is too large")
}

fn parse_octal(field: &[u8]) -> Result<u64> {
    let text = std::str::from_utf8(field)
        .ok()
        .context("the update archive is damaged")?;
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).context("the update archive is damaged")
}

fn read_archive(reader: &mut impl Read, buf: &mut [u8]) -> Result<()> {
    reader.read_exact(buf).map_err(archive_io_error)
}

fn discard_archive(reader: &mut impl Read, mut bytes: usize) -> Result<()> {
    let mut buf = [0u8; 8192];
    while bytes > 0 {
        let take = bytes.min(buf.len());
        let n = reader.read(&mut buf[..take]).map_err(archive_io_error)?;
        if n == 0 {
            bail!("the update archive is truncated");
        }
        bytes -= n;
    }
    Ok(())
}

fn archive_io_error(error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        anyhow::anyhow!("the update archive is truncated")
    } else {
        anyhow::anyhow!(error.to_string())
    }
}

/// Counts inflated bytes so a small signed gzip cannot expand without limit.
struct Limited<R> {
    inner: R,
    left: u64,
}

impl<R> Limited<R> {
    fn new(inner: R, left: u64) -> Self {
        Self { inner, left }
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the update archive is too large",
            ));
        }
        let max = usize::try_from(self.left)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let n = self.inner.read(&mut buf[..max])?;
        self.left -= u64::try_from(n).unwrap_or(0);
        Ok(n)
    }
}

/// Write the installable binary to `dest`. A Linux archive contributes only
/// its `openmic` member; a Windows download is already that binary. The
/// previous `dest` stays in place until the new bytes are complete.
fn write_linux_binary(asset_name: &str, gzipped: &[u8], dest: &Path) -> Result<()> {
    let binary = extract_linux_binary(gzipped, asset_name)?;
    let tmp = dest.with_extension("extracting");
    let write = (|| -> Result<()> {
        let mut file =
            File::create(&tmp).with_context(|| format!("save the update in {}", tmp.display()))?;
        file.write_all(&binary).context("save the update")?;
        file.sync_all().context("save the update")?;
        make_executable(&tmp)?;
        fs::rename(&tmp, dest).context("save the update")?;
        Ok(())
    })();
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .with_context(|| format!("mark {} executable", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Download the release's package to `dest`, keeping it only if it matches
/// the published size, checksum, and signature. A Linux `.tar.gz` is extracted
/// to the `openmic` binary; that binary is what gets installed.
fn download(
    agent: &ureq::Agent,
    release: &Release,
    key: &[u8; 32],
    dest: &Path,
    done: &AtomicU64,
    cancel: &AtomicBool,
) -> Result<()> {
    let fetch = |url: &str, what: &str| {
        check_download_cancelled(cancel)?;
        let response = agent
            .get(url)
            .call()
            .and_then(|r| r.into_body().read_to_string());
        check_download_cancelled(cancel)?;
        response.with_context(|| format!("download the update's {what}"))
    };
    let expected = parse_checksum(&fetch(&release.sha_url, "checksum")?)?;
    let signature = fetch(&release.sig_url, "signature")?;

    let partial = dest.with_extension("part");
    let result = (|| -> Result<()> {
        check_download_cancelled(cancel)?;
        let response = agent.get(&release.exe_url).call();
        check_download_cancelled(cancel)?;
        let response = response.context("download the update")?;
        let mut reader = response.into_body().into_reader();
        let mut out = File::create(&partial).with_context(|| {
            format!(
                "save the update beside OpenMic in {}",
                partial.parent().unwrap_or(dest).display()
            )
        })?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut got = 0u64;
        loop {
            let n = read_download_chunk(&mut reader, &mut buf, cancel)
                .context("download the update")?;
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
        verify_signature(key, &release.exe_name, &hash, &signature)?;
        check_download_cancelled(cancel)?;
        if linux_archive(&release.exe_name) {
            let bytes = fs::read(&partial).context("read the downloaded update")?;
            write_linux_binary(&release.exe_name, &bytes, dest)?;
            let _ = fs::remove_file(&partial);
        } else {
            fs::rename(&partial, dest).context("save the update")?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }
    Ok(())
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
/// and saves settings on the way out). Returns whether this launch is the
/// new version starting up. This must not touch the running copy's files.
pub fn wait_for_previous_version() -> bool {
    let mut args = std::env::args()
        .skip_while(|a| a != AFTER_UPDATE_ARG)
        .skip(1);
    let predecessor = args.next().and_then(|pid| pid.parse::<u32>().ok());
    if let Some(pid) = predecessor {
        wait_for_exit(pid, Duration::from_secs(20));
    }
    predecessor.is_some()
}

/// Only the copy that acquired the single-instance lock may clean up.
/// A second launch must leave a running download or staged update intact.
pub fn finish_update() {
    if let Ok(paths) = Paths::current() {
        cleanup_update_files(&paths);
    }
}

fn cleanup_update_files(paths: &Paths) {
    // Present only after an update, or one that was never installed.
    let _ = fs::remove_file(&paths.old);
    let _ = fs::remove_file(&paths.staged);
    let _ = fs::remove_file(paths.staged.with_extension("part"));
    let _ = fs::remove_file(paths.staged.with_extension("extracting"));
}

#[cfg(windows)]
fn wait_for_exit(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: plain Win32 calls on a handle we open and close here.
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(process, timeout.as_millis() as u32);
            let _ = CloseHandle(process);
        }
    }
}

/// `/proc` still lists a zombie. Its locks and devices are already released,
/// which is what the new copy is waiting for. A missing `/proc` entry means
/// the process is gone; if `/proc` itself is unavailable, don't block startup.
#[cfg(not(windows))]
fn wait_for_exit(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if process_has_exited(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(not(windows))]
fn process_has_exited(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => process_state(&stat) == Some("Z"),
    }
}

/// Field 3 of `/proc/<pid>/stat`. `comm` is wrapped in parentheses and may
/// itself contain spaces and `)`.
#[cfg(any(test, not(windows)))]
fn process_state(stat: &str) -> Option<&str> {
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
}

/// What the updater is doing, for the window.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// Keep the release and its error available for an explicit retry.
    DownloadFailed {
        release: Release,
        error: String,
    },
    /// Downloaded and verified; restarting installs it.
    Ready(Release),
    /// The release check failed before a download was offered.
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
    worker: Option<thread::JoinHandle<Event>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    done: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    #[cfg(test)]
    next_download: Option<(crossbeam_channel::Receiver<Result<()>>, u64)>,
    #[cfg(test)]
    next_check: Option<crossbeam_channel::Receiver<Result<Option<Release>>>>,
    #[cfg(test)]
    install_error: Option<String>,
    #[cfg(test)]
    install_attempts: usize,
}

impl Updater {
    /// `wake` is called when a check or download finishes.
    pub fn new(automatic: bool, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            status: Status::Idle,
            automatic,
            last_check: None,
            worker: None,
            wake: Arc::new(wake),
            done: Arc::default(),
            cancel: Arc::default(),
            #[cfg(test)]
            next_download: None,
            #[cfg(test)]
            next_check: None,
            #[cfg(test)]
            install_error: None,
            #[cfg(test)]
            install_attempts: 0,
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
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            let event = self.worker.take().unwrap().join().unwrap_or_else(|_| {
                let error = anyhow::anyhow!("Update task stopped unexpectedly; try again");
                match &self.status {
                    Status::Downloading(release) => Event::Downloaded(release.clone(), Err(error)),
                    _ => Event::Checked(Err(error)),
                }
            });
            match &event {
                Event::Checked(Ok(Some(release))) => {
                    logfile::info(format_args!("update available: {}", release.tag))
                }
                Event::Checked(Err(e)) => logfile::warn(format_args!("update check failed: {e:#}")),
                Event::Downloaded(release, Ok(())) => {
                    logfile::info(format_args!(
                        "update {} downloaded and verified",
                        release.tag
                    ));
                }
                Event::Downloaded(_, Err(e)) => {
                    logfile::warn(format_args!("update download stopped: {e:#}"))
                }
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
                Event::Downloaded(release, Err(e)) => Status::DownloadFailed {
                    release,
                    error: format!("{e:#}"),
                },
            };
        }
        let idle = matches!(
            self.status,
            Status::Idle | Status::UpToDate | Status::Failed(_)
        );
        if enabled
            && self.automatic
            && idle
            && self.last_check.is_none_or(|t| t.elapsed() >= CHECK_EVERY)
        {
            self.check();
        }
    }

    pub fn check(&mut self) {
        if matches!(self.status, Status::Checking | Status::Downloading(_)) {
            return;
        }
        self.last_check = Some(Instant::now());
        self.status = Status::Checking;
        #[cfg(test)]
        if let Some(completion) = self.next_check.take() {
            self.spawn("test-update-check", move |_, _| {
                Event::Checked(
                    completion
                        .recv()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Update check fixture closed"))),
                )
            });
            return;
        }
        self.spawn("openmic-update-check", |_, _| {
            Event::Checked(check(&agent(), &feed()))
        });
    }

    /// Download and verify the available release, or retry a failed download.
    pub fn download(&mut self) {
        let (Status::Available(release) | Status::DownloadFailed { release, .. }) = &self.status
        else {
            return;
        };
        let release = release.clone();
        let paths = match Paths::current() {
            Ok(paths) => paths,
            Err(e) => {
                self.status = Status::DownloadFailed {
                    release,
                    error: format!("{e:#}"),
                };
                return;
            }
        };
        self.done.store(0, Ordering::Relaxed);
        self.cancel.store(false, Ordering::Relaxed);
        self.status = Status::Downloading(release.clone());
        #[cfg(test)]
        let completion = self.next_download.take();
        #[cfg(test)]
        if let Some((_, done)) = &completion {
            self.done.store(*done, Ordering::Relaxed);
        }
        self.spawn("openmic-update-download", move |done, cancel| {
            #[cfg(test)]
            let result = if let Some((completion, _)) = completion {
                completion
                    .recv()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Update download fixture closed")))
                    .and_then(|()| check_download_cancelled(cancel))
            } else {
                download(
                    &agent(),
                    &release,
                    &RELEASE_KEY,
                    &paths.staged,
                    done,
                    cancel,
                )
            };
            #[cfg(not(test))]
            let result = download(
                &agent(),
                &release,
                &RELEASE_KEY,
                &paths.staged,
                done,
                cancel,
            );
            Event::Downloaded(release, result)
        });
    }

    pub fn cancel_download(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn download_cancelling(&self) -> bool {
        matches!(self.status, Status::Downloading(_)) && self.cancel.load(Ordering::Relaxed)
    }

    /// Put the downloaded version in place and start it. The caller then
    /// quits; the new copy waits for that before it opens any devices.
    pub fn install(&mut self) -> Result<()> {
        let Status::Ready(_) = &self.status else {
            bail!("no update is ready")
        };
        #[cfg(test)]
        {
            self.install_attempts += 1;
            if let Some(error) = &self.install_error {
                bail!("{error}");
            }
        }
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

    fn spawn(
        &mut self,
        name: &str,
        work: impl FnOnce(&AtomicU64, &AtomicBool) -> Event + Send + 'static,
    ) {
        let wake = Arc::clone(&self.wake);
        let (done, cancel) = (Arc::clone(&self.done), Arc::clone(&self.cancel));
        let spawned = thread::Builder::new().name(name.into()).spawn(move || {
            let event = work(&done, &cancel);
            wake();
            event
        });
        match spawned {
            Ok(worker) => self.worker = Some(worker),
            Err(error) => {
                let message = format!("Could not start update task: {error}");
                logfile::warn(&message);
                self.status = match &self.status {
                    Status::Downloading(release) => Status::DownloadFailed {
                        release: release.clone(),
                        error: message,
                    },
                    _ => Status::Failed(message),
                };
                (self.wake)();
            }
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

    /// Exercise GUI actions through the real worker lifecycle without networking.
    pub(crate) fn hold_next_download_for_test(
        &mut self,
        done: u64,
    ) -> crossbeam_channel::Sender<Result<()>> {
        let (completion, wait) = crossbeam_channel::bounded(1);
        self.next_download = Some((wait, done));
        completion
    }

    pub(crate) fn hold_next_check_for_test(
        &mut self,
    ) -> crossbeam_channel::Sender<Result<Option<Release>>> {
        let (completion, wait) = crossbeam_channel::bounded(1);
        self.next_check = Some(wait);
        completion
    }

    /// Keep keyboard fixtures from replacing or starting the test executable.
    pub(crate) fn reject_installs_for_test(&mut self, error: &str) {
        self.install_error = Some(error.into());
    }

    pub(crate) fn install_attempts_for_test(&self) -> usize {
        self.install_attempts
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
        assert_eq!(
            parse_version("v0.7.0-rc1"),
            None,
            "pre-releases are not offered"
        );
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert_eq!(
            current_version(),
            parse_version(env!("CARGO_PKG_VERSION")).unwrap()
        );
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

    fn windows_release(json: &str, current: Version) -> Result<Option<Release>> {
        newer_release_for(json, current, "windows", "x86_64")
    }

    #[test]
    fn only_a_newer_signed_release_is_offered() {
        let release = windows_release(&release_json("v0.7.0", SIGNED), (0, 6, 0))
            .unwrap()
            .unwrap();
        assert_eq!(release.tag, "v0.7.0");
        assert_eq!(release.exe_name, "openmic-v0.7.0-windows-x64.exe");
        assert_eq!(release.exe_url, "https://example.test/v0.7.0.exe");
        assert_eq!(release.sha_url, "https://example.test/v0.7.0.sha256");
        assert_eq!(release.sig_url, "https://example.test/v0.7.0.sig");
        assert_eq!(release.size, 5);
        assert_eq!(
            release.notes, "",
            "a release without notes still offers the update"
        );

        let with_notes = release_json("v0.7.0", SIGNED).replacen(
            '{',
            r###"{"body": "## Fixes\n- Faster","###,
            1,
        );
        let release = windows_release(&with_notes, (0, 6, 0)).unwrap().unwrap();
        assert_eq!(release.notes, "## Fixes\n- Faster");

        assert_eq!(
            windows_release(&release_json("v0.6.0", SIGNED), (0, 6, 0)).unwrap(),
            None
        );
        assert_eq!(
            windows_release(&release_json("v0.5.0", SIGNED), (0, 6, 0)).unwrap(),
            None
        );
        assert!(windows_release(&release_json("v0.7.0", &[".sig"]), (0, 6, 0)).is_err());
        let unsigned = windows_release(&release_json("v0.7.0", &[".sha256"]), (0, 6, 0));
        assert!(
            unsigned.unwrap_err().to_string().contains("isn't signed"),
            "never install unsigned"
        );
    }

    #[test]
    fn each_platform_is_offered_only_its_own_signed_package() {
        let tag = "v0.13.0";
        let archive = format!("openmic-{tag}-linux-x86_64.tar.gz");
        let windows = format!("openmic-{tag}-windows-x64.exe");
        let asset = |name: &str, url: &str, size: u64| {
            format!(r#"{{"name":"{name}","browser_download_url":"{url}","size":{size}}}"#)
        };
        let json = format!(
            r#"{{"tag_name":"{tag}","html_url":"https://example.test/{tag}","assets":[{},{},{},{},{},{}]}}"#,
            asset(&windows, "https://example.test/win", 9),
            asset(
                &format!("{windows}.sha256"),
                "https://example.test/win.sha256",
                1
            ),
            asset(&format!("{windows}.sig"), "https://example.test/win.sig", 1),
            asset(&archive, "https://example.test/linux.tar.gz", 4),
            asset(
                &format!("{archive}.sha256"),
                "https://example.test/linux.sha256",
                1
            ),
            asset(
                &format!("{archive}.sig"),
                "https://example.test/linux.sig",
                1
            ),
        );
        let linux = newer_release_for(&json, (0, 12, 0), "linux", "x86_64")
            .unwrap()
            .unwrap();
        assert_eq!(linux.exe_name, archive);
        assert_eq!(linux.exe_url, "https://example.test/linux.tar.gz");
        assert_eq!(linux.sha_url, "https://example.test/linux.sha256");
        assert_eq!(linux.sig_url, "https://example.test/linux.sig");
        assert_eq!(linux.size, 4);

        let windows_release = newer_release_for(&json, (0, 12, 0), "windows", "aarch64")
            .unwrap()
            .unwrap();
        assert_eq!(
            windows_release.exe_name, windows,
            "Windows keeps installing the published x64 executable"
        );

        let without_linux_sig = json.replace(&format!(r#""{archive}.sig""#), r#""other.sig""#);
        let unsigned = newer_release_for(&without_linux_sig, (0, 12, 0), "linux", "x86_64");
        assert!(unsigned.unwrap_err().to_string().contains("isn't signed"));

        let other_arch = newer_release_for(&json, (0, 12, 0), "linux", "aarch64").unwrap_err();
        assert!(other_arch.to_string().contains("not published"));
        assert!(package_name(tag, "linux", "aarch64").is_none());
        assert!(package_name(tag, "linux", "x86_64").is_some());
    }

    /// Checks that scripts/sign_release.py and this updater agree, using the
    /// real key: sign a file with the script, then
    /// `OPENMIC_SIGNED_FILE=path cargo test -- --ignored release_script`
    #[test]
    #[ignore]
    fn release_script_signatures_verify_with_the_embedded_key() {
        let path =
            PathBuf::from(std::env::var("OPENMIC_SIGNED_FILE").expect("set OPENMIC_SIGNED_FILE"));
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
        signature
            .to_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
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
            verify_signature(
                &public,
                "openmic-v0.8.0-windows-x64.exe",
                &Sha256::digest(b"exe"),
                &signature
            )
            .is_err(),
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

    /// Serves `body` for the package (`.exe` or `.tar.gz`) and `checksum` otherwise.
    /// Each connection answers one request as keep-alive, then hangs up
    /// without a reply if another request arrives on it: the stale pooled
    /// connection that once failed a real update with "peer disconnected".
    fn serve(
        body: Vec<u8>,
        checksum: String,
        signature: String,
    ) -> (String, thread::JoinHandle<()>) {
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
                let content: &[u8] = if request.contains(".tar.gz ") || request.contains(".exe ") {
                    &body
                } else if request.contains(".sig ") {
                    signature.as_bytes()
                } else {
                    checksum.as_bytes()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    content.len()
                )
                .unwrap();
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
    fn fetch(
        exe: &'static [u8],
        listed: &[u8],
        signer: &ed25519_dalek::SigningKey,
    ) -> (Result<()>, tempfile::TempDir, u64) {
        let checksum = format!("{:x}  openmic.exe", Sha256::digest(listed));
        let (base, server) = serve(exe.to_vec(), checksum, sign(signer, "openmic.exe", exe));
        let dir = tempfile::tempdir().unwrap();
        let done = AtomicU64::new(0);
        let trusted = test_key().verifying_key().to_bytes();
        let release = test_release(&base, exe.len() as u64);
        let dest = dir.path().join("openmic.exe.update");
        let result = download(
            &agent(),
            &release,
            &trusted,
            &dest,
            &done,
            &AtomicBool::new(false),
        );
        server.join().unwrap();
        (result, dir, done.load(Ordering::Relaxed))
    }

    #[test]
    fn a_verified_download_is_kept() {
        let (result, dir, done) = fetch(b"new exe", b"new exe", &test_key());
        result.unwrap();
        assert_eq!(
            fs::read(dir.path().join("openmic.exe.update")).unwrap(),
            b"new exe"
        );
        assert_eq!(done, 7);
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no partial file left"
        );
    }

    fn write_octal(field: &mut [u8], value: u64) {
        let text = format!("{:0width$o}", value, width = field.len() - 1);
        field[..text.len()].copy_from_slice(text.as_bytes());
    }

    fn ustar_header(name: &str, kind: u8, size: u64) -> [u8; 512] {
        assert!(name.len() < 100);
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        write_octal(&mut header[100..108], 0o644);
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        write_octal(&mut header[124..136], size);
        write_octal(&mut header[136..148], 0);
        header[156] = kind;
        header[257..262].copy_from_slice(b"ustar");
        header[263] = b'0';
        header[264] = b'0';
        header[148..156].fill(b' ');
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        let text = format!("{sum:06o}");
        header[148..154].copy_from_slice(text.as_bytes());
        header[154] = 0;
        header[155] = b' ';
        header
    }

    fn pack_tar_gz(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, kind, data) in entries {
            tar.extend_from_slice(&ustar_header(name, *kind, data.len() as u64));
            tar.extend_from_slice(data);
            tar.extend(std::iter::repeat_n(0, (512 - (data.len() % 512)) % 512));
        }
        tar.extend(std::iter::repeat_n(0, 1024));
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    fn gzip_tar(tar: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(tar).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn a_verified_linux_archive_is_staged_as_its_binary() {
        let asset = "openmic-v9.9.9-linux-x86_64.tar.gz";
        let member = "openmic-v9.9.9-linux-x86_64/openmic";
        let binary = b"linux-openmic-binary";
        let archive = pack_tar_gz(&[
            ("openmic-v9.9.9-linux-x86_64/", b'5', b""),
            (member, b'0', binary),
            ("openmic-v9.9.9-linux-x86_64/README.md", b'0', b"notes"),
        ]);
        let checksum = format!("{:x}  {asset}", Sha256::digest(&archive));
        let (base, server) = serve(
            archive.clone(),
            checksum,
            sign(&test_key(), asset, &archive),
        );
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("openmic.update");
        fs::write(&dest, b"previous staged binary").unwrap();
        let release = Release {
            tag: "v9.9.9".into(),
            page: String::new(),
            notes: String::new(),
            exe_name: asset.into(),
            exe_url: format!("{base}/{asset}"),
            sha_url: format!("{base}/{asset}.sha256"),
            sig_url: format!("{base}/{asset}.sig"),
            size: archive.len() as u64,
        };
        download(
            &agent(),
            &release,
            &test_key().verifying_key().to_bytes(),
            &dest,
            &AtomicU64::new(0),
            &AtomicBool::new(false),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&dest).unwrap(), binary);
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "the archive itself is not installed"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    #[test]
    fn a_linux_archive_without_a_regular_binary_is_refused() {
        let asset = "openmic-v9.9.9-linux-x86_64.tar.gz";
        let member = "openmic-v9.9.9-linux-x86_64/openmic";
        let symlink = pack_tar_gz(&[(member, b'2', b"")]);
        let error = extract_linux_binary(&symlink, asset)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a regular file"), "{error}");

        let traversal = pack_tar_gz(&[("../openmic", b'0', b"evil")]);
        let error = extract_linux_binary(&traversal, asset)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no OpenMic binary"), "{error}");

        let mut header = ustar_header(member, b'0', 0);
        write_octal(&mut header[124..136], MAX_BINARY_BYTES + 1);
        header[148..156].fill(b' ');
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        let text = format!("{sum:06o}");
        header[148..154].copy_from_slice(text.as_bytes());
        header[154] = 0;
        header[155] = b' ';
        let error = extract_linux_binary(&gzip_tar(&header), asset)
            .unwrap_err()
            .to_string();
        assert!(error.contains("too large"), "{error}");

        let mut damaged = ustar_header(member, b'0', 0);
        damaged[148] ^= 1;
        let error = extract_linux_binary(&gzip_tar(&damaged), asset)
            .unwrap_err()
            .to_string();
        assert!(error.contains("damaged"), "{error}");

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("openmic.update");
        fs::write(&dest, b"keep the previous binary").unwrap();
        let error = write_linux_binary(asset, &symlink, &dest)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a regular file"), "{error}");
        assert_eq!(fs::read(&dest).unwrap(), b"keep the previous binary");
        assert!(!dest.with_extension("extracting").exists());
    }

    #[cfg(unix)]
    #[test]
    fn gnu_tar_release_layout_extracts_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let stem = "openmic-v9.9.9-linux-x86_64";
        let root = dir.path().join(stem);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("openmic"), b"from-gnu-tar").unwrap();
        fs::write(root.join("README.md"), b"readme").unwrap();
        fs::write(root.join("LICENSE"), b"mit").unwrap();
        let archive_name = format!("{stem}.tar.gz");
        let status = std::process::Command::new("tar")
            .args(["-czf", &archive_name, stem])
            .current_dir(dir.path())
            .status()
            .expect("tar");
        assert!(status.success());
        let bytes = fs::read(dir.path().join(format!("{stem}.tar.gz"))).unwrap();
        assert_eq!(
            extract_linux_binary(&bytes, &format!("{stem}.tar.gz")).unwrap(),
            b"from-gnu-tar"
        );
    }

    #[test]
    fn process_state_reads_the_field_after_a_comm_that_contains_parentheses() {
        assert_eq!(process_state("1 (openmic) S 0 0"), Some("S"));
        assert_eq!(process_state("1 (a) b) Z 0"), Some("Z"));
        assert_eq!(process_state("nope"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_new_version_waits_until_the_previous_process_has_exited() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(!process_has_exited(pid), "a live process is still running");
        child.kill().unwrap();
        let started = Instant::now();
        wait_for_exit(pid, Duration::from_secs(5));
        assert!(process_has_exited(pid));
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "a zombie is not waited out"
        );
        let _ = child.wait();
        let started = Instant::now();
        wait_for_exit(pid, Duration::from_secs(5));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a reaped process does not block startup"
        );
    }

    #[test]
    fn cancelled_download_skips_requests_and_preserves_the_staged_update() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("openmic.exe.update");
        fs::write(&dest, b"previous verified update").unwrap();
        let done = AtomicU64::new(17);
        // The sample has no request URLs: cancellation must win before any
        // attempt to fetch or parse metadata, or to touch the staged file.
        let result = download(
            &agent(),
            &Release::sample("v9.0.0"),
            &RELEASE_KEY,
            &dest,
            &done,
            &AtomicBool::new(true),
        );
        assert_eq!(result.unwrap_err().to_string(), "download cancelled");
        assert_eq!(done.load(Ordering::Relaxed), 17);
        assert_eq!(fs::read(&dest).unwrap(), b"previous verified update");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn cancellation_during_checksum_fetch_prevents_further_update_requests() {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let cancel = Arc::<AtomicBool>::default();
        let cancelled = Arc::clone(&cancel);
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "checksum request did not arrive");
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept checksum request: {error}"),
                }
            };
            // Accepted sockets can inherit the listener's nonblocking mode on Windows.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert!(request.starts_with("GET /checksum "));
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            // Cancel while the metadata request is in flight. The response
            // is valid, but the signature and executable must not be fetched.
            cancelled.store(true, Ordering::Relaxed);
            let checksum = "0".repeat(64);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{checksum}"
            )
            .unwrap();
        });
        let release = Release {
            sha_url: format!("{base}/checksum"),
            sig_url: format!("{base}/signature"),
            exe_url: format!("{base}/executable"),
            ..Release::sample("v9.0.0")
        };
        let dir = tempfile::tempdir().unwrap();
        let result = download(
            &download_agent(Duration::from_secs(1)),
            &release,
            &RELEASE_KEY,
            &dir.path().join("openmic.exe.update"),
            &AtomicU64::new(0),
            &cancel,
        );
        server.join().unwrap();
        assert_eq!(result.unwrap_err().to_string(), "download cancelled");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
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
    fn startup_cleanup_only_removes_this_executables_update_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::for_exe(dir.path().join("openmic.exe"));
        fs::write(&paths.exe, b"running executable").unwrap();
        let leftovers = [
            &paths.old,
            &paths.staged,
            &paths.staged.with_extension("part"),
            &paths.staged.with_extension("extracting"),
        ];
        for path in leftovers {
            fs::write(path, b"earlier update").unwrap();
        }
        let another_exe = Paths::for_exe(dir.path().join("other.exe"));
        fs::write(&another_exe.staged, b"another executable's update").unwrap();

        cleanup_update_files(&paths);
        assert_eq!(fs::read(&paths.exe).unwrap(), b"running executable");
        for path in leftovers {
            assert!(!path.exists());
        }
        assert_eq!(
            fs::read(&another_exe.staged).unwrap(),
            b"another executable's update"
        );
        cleanup_update_files(&paths); // missing leftovers do not block startup
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    fn wait_for_worker(updater: &Updater) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !updater.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < deadline, "update worker did not exit");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn crashed_update_check_reports_failure_and_accepts_another_check() {
        let mut updater = Updater::new(true, || {});
        updater.last_check = Some(Instant::now());
        updater.status = Status::Checking;
        updater.spawn("test-update-crash", |_, _| {
            panic!("simulated check failure")
        });
        wait_for_worker(&updater);
        updater.tick(true);
        assert!(matches!(updater.status(), Status::Failed(error) if error.contains("try again")));
        assert!(
            updater.worker.is_none(),
            "a crash must not immediately retry the daily check"
        );

        updater.status = Status::Checking;
        updater.spawn("test-update-retry", |_, _| Event::Checked(Ok(None)));
        wait_for_worker(&updater);
        updater.tick(true);
        assert_eq!(updater.status(), &Status::UpToDate);
        assert!(updater.worker.is_none());
    }

    #[test]
    fn crashed_update_download_clears_progress_and_respects_cancellation() {
        for cancelled in [false, true] {
            let mut updater = Updater::new(false, || {});
            let release = Release::sample("v9.0.0");
            updater.status = Status::Downloading(release.clone());
            updater.spawn("test-download-crash", |done, _| {
                done.store(123, Ordering::Relaxed);
                panic!("simulated download failure");
            });
            wait_for_worker(&updater);
            assert_eq!(updater.progress(), Some((123, release.size)));
            if cancelled {
                updater.cancel_download();
            }
            updater.tick(false);
            assert!(updater.progress().is_none());
            assert!(updater.worker.is_none());
            if cancelled {
                assert_eq!(updater.status(), &Status::Available(release));
            } else {
                assert!(
                    matches!(updater.status(), Status::DownloadFailed { release: retained, error }
                    if retained == &release && error.contains("try again"))
                );
            }
        }
    }

    #[test]
    fn failed_update_download_can_retry_without_losing_the_release_or_error() {
        let mut updater = Updater::new(true, || {});
        // A malformed URI fails before networking or any executable writes.
        let release = Release {
            sha_url: "http://[invalid".into(),
            ..Release::sample("v9.0.0")
        };
        updater.status = Status::DownloadFailed {
            release: release.clone(),
            error: "earlier failure".into(),
        };
        updater.done.store(123, Ordering::Relaxed);
        updater.cancel.store(true, Ordering::Relaxed);
        updater.download();
        assert_eq!(updater.status(), &Status::Downloading(release.clone()));
        assert_eq!(updater.progress(), Some((0, release.size)));
        assert!(
            !updater.cancel.load(Ordering::Relaxed),
            "retry clears the previous cancellation"
        );
        wait_for_worker(&updater);
        updater.tick(true);
        assert!(
            matches!(updater.status(), Status::DownloadFailed { release: retained, error }
            if retained == &release && error.contains("checksum"))
        );
        assert!(
            updater.worker.is_none(),
            "an automatic check must not replace a failed download"
        );
        assert!(updater.progress().is_none());
    }

    #[test]
    fn pending_update_download_stays_responsive_and_cancellable() {
        let mut updater = Updater::new(false, || {});
        let release = Release::sample("v9.0.0");
        updater.status = Status::Downloading(release.clone());
        let (release_worker, wait) = crossbeam_channel::bounded::<()>(0);
        let downloaded = release.clone();
        updater.spawn("test-download-cancel", move |_, cancel| {
            let _ = wait.recv_timeout(Duration::from_secs(3));
            assert!(cancel.load(Ordering::Relaxed));
            Event::Downloaded(downloaded, Err(anyhow::anyhow!("download cancelled")))
        });
        updater.tick(false);
        assert_eq!(updater.status(), &Status::Downloading(release.clone()));
        assert!(!updater.download_cancelling());
        updater.cancel_download();
        assert!(
            updater.download_cancelling(),
            "show cancellation while the worker is still stopping"
        );
        release_worker.send(()).unwrap();
        wait_for_worker(&updater);
        updater.tick(false);
        assert_eq!(updater.status(), &Status::Available(release));
        assert!(!updater.download_cancelling());
        assert!(updater.worker.is_none());
    }

    #[test]
    fn completed_update_download_preserves_the_verified_release() {
        let mut updater = Updater::new(false, || {});
        let release = Release::sample("v9.0.0");
        updater.status = Status::Downloading(release.clone());
        let downloaded = release.clone();
        updater.spawn("test-download-done", move |_, _| {
            Event::Downloaded(downloaded, Ok(()))
        });
        wait_for_worker(&updater);
        updater.cancel_download(); // a completed verified download is still ready
        updater.tick(false);
        assert_eq!(updater.status(), &Status::Ready(release));
        assert!(!updater.download_cancelling());
        assert!(updater.progress().is_none());
        assert!(updater.worker.is_none());
    }

    #[test]
    fn automatic_checks_are_off_unless_enabled() {
        let mut updater = Updater::new(false, || {});
        updater.tick(true);
        assert_eq!(updater.status(), &Status::Idle);
    }
}
