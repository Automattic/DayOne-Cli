//! Public release discovery. This module never uses the account HTTP client or store.

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use semver::Version;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RELEASES_URL: &str = "https://github.com/Automattic/DayOne-Cli/releases";
const GITHUB_API: &str = "https://api.github.com/repos/Automattic/DayOne-Cli/releases";
const NPM_API: &str = "https://registry.npmjs.org/@dayone%2fcli";
const FRESH_FOR: u64 = 24 * 60 * 60;
const RETRY_AFTER: u64 = 60 * 60;
const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_PAGES: usize = 10;
pub const WORKER_COMMAND: &str = "__refresh-updates";

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Read cached release metadata without making network requests.
    #[arg(long)]
    offline: bool,
    /// Release channel. Defaults to preview for prerelease builds, otherwise stable.
    #[arg(long, value_enum)]
    channel: Option<Channel>,
    /// Show changes after this version instead of the installed version.
    #[arg(long, value_name = "VERSION")]
    since: Option<Version>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum Channel {
    Stable,
    Preview,
}

impl Channel {
    fn for_version(version: &Version) -> Self {
        if version.pre.is_empty() {
            Self::Stable
        } else {
            Self::Preview
        }
    }

    fn accepts(self, version: &Version) -> bool {
        matches!(self, Self::Preview) || version.pre.is_empty()
    }

    fn name(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Preview => "preview",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Installation {
    Npm,
    Standalone,
}

impl Installation {
    fn detect() -> Self {
        if std::env::var("DAYONE_INSTALL_METHOD").as_deref() == Ok("npm") {
            Self::Npm
        } else {
            Self::Standalone
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Standalone => "standalone",
        }
    }

    fn guidance(self, version: Option<&Version>) -> String {
        match (self, version) {
            (Self::Npm, Some(version)) => format!("npm install -g @dayone/cli@{version}"),
            (Self::Npm, None) => "Check again before choosing a version to install.".into(),
            (Self::Standalone, _) => "Download the matching platform binary from the release URL and replace your installed dayone binary.".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Release {
    version: Version,
    url: String,
    notes: Option<String>,
    #[serde(skip)]
    binary_available: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Snapshot {
    checked_at: u64,
    latest_version: Version,
    releases: Vec<Release>,
    notes_complete: bool,
    notes_error: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
struct Cache {
    last_attempt: u64,
    snapshot: Option<Availability>,
}

#[derive(Deserialize, Serialize)]
struct Availability {
    checked_at: u64,
    latest_version: Version,
}

impl From<&Snapshot> for Availability {
    fn from(snapshot: &Snapshot) -> Self {
        Self {
            checked_at: snapshot.checked_at,
            latest_version: snapshot.latest_version.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    UpdateAvailable,
    UpToDate,
    Ahead,
    Unknown,
}

#[derive(Serialize)]
pub struct UpdateReport {
    ok: bool,
    installed_version: Version,
    installation: Installation,
    channel: Channel,
    status: Status,
    update_available: Option<bool>,
    latest_version: Option<Version>,
    checked_at: Option<u64>,
    stale: bool,
    error: Option<String>,
    release_url: String,
    update_instructions: String,
    since: Version,
    releases: Vec<Release>,
    notes_complete: bool,
    notes_error: Option<String>,
}

fn installed_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package version is valid SemVer")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn recent(timestamp: u64, now: u64, interval: u64) -> bool {
    timestamp != 0 && timestamp <= now && now - timestamp < interval
}

fn report(
    installed: Version,
    installation: Installation,
    channel: Channel,
    since: Version,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    now: u64,
) -> UpdateReport {
    let stale = snapshot
        .as_ref()
        .is_none_or(|s| !recent(s.checked_at, now, FRESH_FOR));
    let known = !stale && error.is_none();
    let latest = snapshot.as_ref().map(|s| s.latest_version.clone());
    let status = match latest.as_ref().filter(|_| known) {
        Some(latest) => match latest.cmp_precedence(&installed) {
            std::cmp::Ordering::Greater => Status::UpdateAvailable,
            std::cmp::Ordering::Equal => Status::UpToDate,
            std::cmp::Ordering::Less => Status::Ahead,
        },
        None => Status::Unknown,
    };
    let update_available = known.then_some(matches!(status, Status::UpdateAvailable));
    let release_url = latest
        .as_ref()
        .map(|v| format!("{RELEASES_URL}/tag/v{v}"))
        .unwrap_or_else(|| RELEASES_URL.into());
    let update_instructions = installation.guidance(latest.as_ref());
    let checked_at = snapshot.as_ref().map(|s| s.checked_at);
    let notes_complete = snapshot.as_ref().is_some_and(|s| s.notes_complete);
    let notes_error = snapshot.as_ref().and_then(|s| s.notes_error.clone());
    let releases = snapshot
        .map(|s| {
            s.releases
                .into_iter()
                .filter(|r| {
                    r.version.cmp_precedence(&since).is_gt()
                        && r.version.cmp_precedence(&s.latest_version).is_le()
                        && channel.accepts(&r.version)
                })
                .collect()
        })
        .unwrap_or_default();
    UpdateReport {
        ok: known,
        installed_version: installed,
        installation,
        channel,
        status,
        update_available,
        latest_version: latest,
        checked_at,
        stale,
        error,
        release_url,
        update_instructions,
        since,
        releases,
        notes_complete,
        notes_error,
    }
}

pub async fn execute(args: UpdateArgs) -> UpdateReport {
    let installed = installed_version();
    let installation = Installation::detect();
    let channel = args
        .channel
        .unwrap_or_else(|| Channel::for_version(&installed));
    let since = args.since.unwrap_or_else(|| installed.clone());
    let cache_path = cache_path(installation, channel);
    let cached = cache_path
        .as_deref()
        .and_then(|path| read_json::<Snapshot>(path, MAX_BODY * MAX_PAGES).ok());
    let (snapshot, error) = if args.offline {
        let error = match &cached {
            None => Some("No cached release metadata. Run dayone updates when online.".into()),
            Some(s) if !recent(s.checked_at, now(), FRESH_FOR) => {
                Some("Cached release metadata is stale. Run dayone updates when online.".into())
            }
            _ => None,
        };
        (cached, error)
    } else {
        match fetch(installation, channel).await {
            Ok(snapshot) => {
                if let Some(path) = cache_path.as_deref() {
                    let _ = save_snapshot(path, &snapshot);
                }
                (Some(snapshot), None)
            }
            Err(_) => (
                cached,
                Some(
                    "Could not check public release metadata. Try again later or use --offline."
                        .into(),
                ),
            ),
        }
    };
    report(
        installed,
        installation,
        channel,
        since,
        snapshot,
        error,
        now(),
    )
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    state: String,
}

fn binary_asset() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        os => os,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        arch => arch,
    };
    let variant = if cfg!(target_os = "linux") && !cfg!(feature = "embeddings") {
        "-no-embeddings"
    } else {
        ""
    };
    let extension = if cfg!(windows) { ".exe" } else { "" };
    format!("dayone-{os}-{arch}{variant}{extension}")
}

impl GithubRelease {
    fn into_release(self, channel: Channel) -> Option<Release> {
        let version =
            Version::parse(self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)).ok()?;
        if self.draft
            || !channel.accepts(&version)
            || (self.prerelease && matches!(channel, Channel::Stable))
        {
            return None;
        }
        Some(Release {
            version,
            url: format!("{RELEASES_URL}/tag/{}", self.tag_name),
            notes: self.body,
            binary_available: self
                .assets
                .iter()
                .any(|asset| asset.name == binary_asset() && asset.state == "uploaded"),
        })
    }
}

#[derive(Deserialize)]
struct NpmRelease {
    version: Version,
}

async fn get_json<T: DeserializeOwned>(client: &reqwest::Client, url: &str) -> Result<T> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_BODY {
            bail!("Release metadata exceeds size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body)?)
}

async fn fetch(installation: Installation, channel: Channel) -> Result<Snapshot> {
    // No account client, auth headers, cookies, telemetry or redirects. A proxy's
    // transport details must never be exposed in the report or persisted cache.
    let client = reqwest::Client::builder()
        .user_agent("dayone-cli-update-check")
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        .build()?;
    fetch_from(&client, installation, channel, GITHUB_API, NPM_API).await
}

async fn fetch_from(
    client: &reqwest::Client,
    installation: Installation,
    channel: Channel,
    github: &str,
    npm: &str,
) -> Result<Snapshot> {
    // Availability is separate from notes: npm may publish after GitHub, and
    // rate-limited GitHub notes must not hide a version known to be on npm.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let latest = tokio::time::timeout_at(deadline, async {
        Ok::<_, anyhow::Error>(match installation {
            Installation::Npm => {
                let stable: NpmRelease = get_json(client, &format!("{npm}/latest")).await?;
                if !stable.version.pre.is_empty() {
                    bail!("Invalid stable dist-tag");
                }
                if matches!(channel, Channel::Preview) {
                    let preview: Result<NpmRelease> =
                        get_json(client, &format!("{npm}/next")).await;
                    match preview {
                        Ok(preview) if preview.version.cmp_precedence(&stable.version).is_gt() => {
                            Some(preview.version)
                        }
                        Ok(_) => Some(stable.version),
                        Err(error)
                            if error.downcast_ref::<reqwest::Error>().is_some_and(|e| {
                                e.status() == Some(reqwest::StatusCode::NOT_FOUND)
                            }) =>
                        {
                            Some(stable.version)
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    Some(stable.version)
                }
            }
            Installation::Standalone if matches!(channel, Channel::Stable) => {
                let release: GithubRelease = get_json(client, &format!("{github}/latest")).await?;
                let release = release
                    .into_release(channel)
                    .context("Invalid stable release")?;
                if !release.binary_available {
                    bail!("Platform binary is not available yet");
                }
                Some(release.version)
            }
            Installation::Standalone => None,
        })
    })
    .await
    .context("Release check timed out")??;
    let mut releases = Vec::new();
    let mut complete = false;
    for page in 1..=MAX_PAGES {
        let batch = tokio::time::timeout_at(
            deadline,
            get_json::<Vec<GithubRelease>>(client, &format!("{github}?per_page=100&page={page}")),
        )
        .await;
        let Ok(Ok(batch)) = batch else {
            break;
        };
        complete = batch.len() < 100;
        releases.extend(batch.into_iter().filter_map(|r| r.into_release(channel)));
        if complete {
            break;
        }
    }
    releases.sort_by(|a, b| b.version.cmp_precedence(&a.version));
    releases.dedup_by(|a, b| a.version.cmp_precedence(&b.version).is_eq());
    let latest_version = match latest {
        Some(version) => version,
        None if complete => releases
            .first()
            .filter(|release| release.binary_available)
            .context("No published releases")?
            .version
            .clone(),
        None => bail!("Could not determine preview release"),
    };
    complete &= releases
        .iter()
        .any(|r| r.version == latest_version && r.notes.is_some());
    complete &= releases.iter().all(|r| r.notes.is_some());
    Ok(Snapshot {
        checked_at: now(), latest_version, releases, notes_complete: complete,
        notes_error: (!complete).then(|| "Some release notes are unavailable or the release history is truncated. See the public releases page.".into()),
    })
}

fn cache_path(installation: Installation, channel: Channel) -> Option<PathBuf> {
    let dir = std::env::var_os("DAYONE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::cache_dir().map(|p| p.join("dayone-cli")))?;
    Some(dir.join(format!(
        "updates-{}-{}-{}.json",
        installation.name(),
        channel.name(),
        binary_asset(),
    )))
}

fn read_json<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<T> {
    // Check before opening: opening a FIFO would otherwise block the command.
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        bail!("Cache is not a regular file");
    }
    let mut body = Vec::new();
    File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut body)?;
    if body.len() > limit {
        bail!("Cache too large");
    }
    Ok(serde_json::from_slice(&body)?)
}

fn read_cache(path: &Path) -> Cache {
    read_json(&path.with_extension("notice.json"), 4096).unwrap_or_default()
}

fn lock_cache(path: &Path) -> Result<File> {
    fs::create_dir_all(path.parent().context("Missing cache directory")?)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))?;
    lock.try_lock().context("Update cache busy")?;
    Ok(lock)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    // Callers hold the cache lock. Only this process's temporary file is touched.
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = File::create(&tmp)?;
        serde_json::to_writer(&mut file, value)?;
        file.flush()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn save_snapshot(path: &Path, snapshot: &Snapshot) -> Result<()> {
    let _lock = lock_cache(path)?;
    let mut cache = read_cache(path);
    if cache
        .snapshot
        .as_ref()
        .is_none_or(|s| s.checked_at <= snapshot.checked_at)
    {
        write_json(path, snapshot)?;
        cache.snapshot = Some(Availability::from(snapshot));
        cache.last_attempt = now();
        write_json(&path.with_extension("notice.json"), &cache)?;
    }
    Ok(())
}

fn automatic_checks_disabled() -> bool {
    ["NO_UPDATE_NOTIFIER", "DAYONE_NO_UPDATE_NOTIFIER", "CI"]
        .iter()
        .any(|k| std::env::var_os(k).is_some())
        || std::env::var("npm_config_update_notifier").as_deref() == Ok("false")
}

fn notice(
    snapshot: &Availability,
    installed: &Version,
    installation: Installation,
    now: u64,
) -> Option<String> {
    if !recent(snapshot.checked_at, now, FRESH_FOR)
        || !snapshot.latest_version.cmp_precedence(installed).is_gt()
    {
        return None;
    }
    Some(format!(
        "Update available: {installed} → {}\nChanges: {RELEASES_URL}/tag/v{}\nRun `dayone updates` for release notes and update instructions.\n{}\n",
        snapshot.latest_version,
        snapshot.latest_version,
        installation.guidance(Some(&snapshot.latest_version)),
    ))
}

/// Never waits for HTTP. Local-only commands may show a cached notice but do
/// not initiate a network refresh. Non-TTY agents receive the same stderr notice.
pub fn notify(allow_refresh: bool) {
    if automatic_checks_disabled() {
        return;
    }
    let installed = installed_version();
    let installation = Installation::detect();
    let channel = Channel::for_version(&installed);
    let Some(path) = cache_path(installation, channel) else {
        return;
    };
    let cache = read_cache(&path);
    if let Some(message) = cache
        .snapshot
        .as_ref()
        .and_then(|s| notice(s, &installed, installation, now()))
    {
        // A broken stderr must never fail the user's command.
        let _ = writeln!(std::io::stderr(), "\n{message}");
    }
    if !allow_refresh {
        return;
    }
    let _ = schedule_refresh(&path);
}

fn schedule_refresh(path: &Path) -> Result<()> {
    let _lock = lock_cache(path)?;
    let mut cache = read_cache(path);
    if recent(cache.last_attempt, now(), RETRY_AFTER)
        || cache
            .snapshot
            .as_ref()
            .is_some_and(|s| recent(s.checked_at, now(), FRESH_FOR))
    {
        return Ok(());
    }
    // Reserve before spawning, including on failure. Concurrent invocations and
    // crashed workers cannot create an unbounded stream of background checks.
    cache.last_attempt = now();
    write_json(&path.with_extension("notice.json"), &cache)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg(WORKER_COMMAND)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn()?;
    // Reap while the parent lives; if it exits first, the OS adopts the worker.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Invoked by main before secrets, diagnostics or account initialization.
pub async fn refresh_worker() {
    if automatic_checks_disabled() {
        return;
    }
    let installation = Installation::detect();
    let channel = Channel::for_version(&installed_version());
    if let Some(path) = cache_path(installation, channel)
        && let Ok(snapshot) = fetch(installation, channel).await
    {
        let _ = save_snapshot(&path, &snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn release(tag: &str, notes: &str) -> Value {
        json!({"tag_name": tag, "body": notes, "draft": false, "prerelease": tag.contains('-'),
        "assets": [
            {"name": "dayone-darwin-arm64", "state": "uploaded"},
            {"name": "dayone-darwin-x64", "state": "uploaded"},
            {"name": "dayone-linux-arm64", "state": "uploaded"},
            {"name": "dayone-linux-x64", "state": "uploaded"},
            {"name": "dayone-linux-arm64-no-embeddings", "state": "uploaded"},
            {"name": "dayone-linux-x64-no-embeddings", "state": "uploaded"},
            {"name": "dayone-win32-x64.exe", "state": "uploaded"}
        ]})
    }

    // Real HTTP responses exercise decoding, status handling and pagination.
    // Requests are checked for credential leakage before serving each fixture.
    fn server(responses: Vec<(&'static str, u16, Value)>) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            for (path, status, body) in responses {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "Missing request {path}"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(e) => panic!("{e}"),
                    }
                };
                // Accepted sockets inherit nonblocking mode on some platforms.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                assert_eq!(first.trim(), format!("GET {path} HTTP/1.1"));
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    assert!(!line.to_ascii_lowercase().starts_with("authorization:"));
                    assert!(!line.to_ascii_lowercase().starts_with("cookie:"));
                }
                let body = body.to_string();
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (url, thread)
    }

    #[tokio::test]
    async fn npm_availability_caps_notes_and_semver_orders_intervening_releases() {
        let mut draft = release("v9.0.0", "private draft");
        draft["draft"] = json!(true);
        let (url, thread) = server(vec![
            ("/latest", 200, json!({"version":"0.10.0"})),
            (
                "/?per_page=100&page=1",
                200,
                json!([
                    release("v0.9.0", "Existing feature"),
                    release("v0.12.0", "Not yet on npm"),
                    release("v0.10.0", "New feature"),
                    release("v0.11.0-rc.1", "Preview feature"),
                    draft,
                ]),
            ),
        ]);
        let snapshot = fetch_from(
            &reqwest::Client::new(),
            Installation::Npm,
            Channel::Stable,
            &format!("{url}/"),
            &url,
        )
        .await
        .unwrap();
        thread.join().unwrap();
        let output = serde_json::to_value(report(
            version("0.8.0"),
            Installation::Npm,
            Channel::Stable,
            version("0.8.0"),
            Some(snapshot),
            None,
            now(),
        ))
        .unwrap();
        assert_eq!(output["status"], "update_available");
        assert_eq!(output["latest_version"], "0.10.0");
        assert_eq!(
            output["update_instructions"],
            "npm install -g @dayone/cli@0.10.0"
        );
        assert_eq!(
            output["releases"],
            json!([
                {"version":"0.10.0", "url":"https://github.com/Automattic/DayOne-Cli/releases/tag/v0.10.0", "notes":"New feature"},
                {"version":"0.9.0", "url":"https://github.com/Automattic/DayOne-Cli/releases/tag/v0.9.0", "notes":"Existing feature"},
            ])
        );
        assert_eq!(output["notes_complete"], true);
    }

    #[tokio::test]
    async fn preview_promotes_to_stable_and_missing_notes_do_not_hide_availability() {
        for (next_status, next) in [
            (200, json!({"version":"0.10.0-rc.2"})),
            (404, json!({"error":"not found"})),
        ] {
            let (url, thread) = server(vec![
                ("/latest", 200, json!({"version":"0.10.0"})),
                ("/next", next_status, next),
                (
                    "/?per_page=100&page=1",
                    403,
                    json!({"message":"rate limited"}),
                ),
            ]);
            let snapshot = fetch_from(
                &reqwest::Client::new(),
                Installation::Npm,
                Channel::Preview,
                &format!("{url}/"),
                &url,
            )
            .await
            .unwrap();
            thread.join().unwrap();
            let output = serde_json::to_value(report(
                version("0.10.0-rc.1"),
                Installation::Npm,
                Channel::Preview,
                version("0.10.0-rc.1"),
                Some(snapshot),
                None,
                now(),
            ))
            .unwrap();
            assert_eq!(output["latest_version"], "0.10.0");
            assert_eq!(output["update_available"], true);
            assert_eq!(output["notes_complete"], false);
            assert!(
                output["notes_error"]
                    .as_str()
                    .unwrap()
                    .contains("unavailable")
            );
        }
    }

    #[tokio::test]
    async fn standalone_paginates_and_selects_preview_numerically() {
        let page = vec![release("v0.10.0-rc.2", "Second preview"); 100];
        let (url, thread) = server(vec![
            ("/?per_page=100&page=1", 200, json!(page)),
            (
                "/?per_page=100&page=2",
                200,
                json!([release("v0.10.0-rc.10", "Tenth preview")]),
            ),
        ]);
        let snapshot = fetch_from(
            &reqwest::Client::new(),
            Installation::Standalone,
            Channel::Preview,
            &format!("{url}/"),
            &url,
        )
        .await
        .unwrap();
        thread.join().unwrap();
        let output = serde_json::to_value(report(
            version("0.10.0-rc.2"),
            Installation::Standalone,
            Channel::Preview,
            version("0.10.0-rc.2"),
            Some(snapshot),
            None,
            now(),
        ))
        .unwrap();
        assert_eq!(output["latest_version"], "0.10.0-rc.10");
        assert_eq!(output["releases"].as_array().unwrap().len(), 1);
        assert_eq!(output["releases"][0]["notes"], "Tenth preview");
        assert!(
            !output["update_instructions"]
                .as_str()
                .unwrap()
                .contains("npm")
        );
    }

    #[tokio::test]
    async fn invalid_or_failed_version_checks_are_not_up_to_date() {
        for (status, body) in [
            (503, json!({"error":"unavailable"})),
            (200, json!({"version":"not-semver"})),
            (200, json!({"version":"1.0.0-rc.1"})),
        ] {
            let (url, thread) = server(vec![("/latest", status, body)]);
            let result = fetch_from(
                &reqwest::Client::new(),
                Installation::Npm,
                Channel::Stable,
                &url,
                &url,
            )
            .await;
            thread.join().unwrap();
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn standalone_requires_the_uploaded_platform_binary() {
        for (assets, available) in [
            (json!([]), false),
            (
                json!([{"name":"another-platform", "state":"uploaded"}]),
                false,
            ),
            (json!([{"name":binary_asset(), "state":"starter"}]), false),
            (release("v99.0.0", "New feature")["assets"].clone(), true),
        ] {
            let mut published = release("v99.0.0", "New feature");
            published["assets"] = assets;
            for channel in [Channel::Stable, Channel::Preview] {
                let mut responses = match channel {
                    Channel::Stable => vec![("/latest", 200, published.clone())],
                    Channel::Preview => vec![("/?per_page=100&page=1", 200, json!([published]))],
                };
                if available && matches!(channel, Channel::Stable) {
                    responses.push(("/?per_page=100&page=1", 200, json!([published])));
                }
                let (url, thread) = server(responses);
                let result = fetch_from(
                    &reqwest::Client::new(),
                    Installation::Standalone,
                    channel,
                    &url,
                    &url,
                )
                .await;
                thread.join().unwrap();
                let (snapshot, error) = match result {
                    Ok(snapshot) => (Some(snapshot), None),
                    Err(_) => (None, Some("Check failed".into())),
                };
                let output = serde_json::to_value(report(
                    version("1.0.0"),
                    Installation::Standalone,
                    channel,
                    version("1.0.0"),
                    snapshot,
                    error,
                    now(),
                ))
                .unwrap();
                assert_eq!(
                    output["status"],
                    if available {
                        "update_available"
                    } else {
                        "unknown"
                    }
                );
                assert_eq!(
                    output["update_available"],
                    if available { json!(true) } else { Value::Null }
                );
            }
        }
    }

    #[tokio::test]
    async fn notes_deadline_preserves_known_npm_availability() {
        let (npm, thread) = server(vec![("/latest", 200, json!({"version":"99.0.0"}))]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let github = format!("http://{}", listener.local_addr().unwrap());
        let slow_notes = tokio::spawn(async move {
            let (_connection, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        // The client has no request timeout, so this exercises the overall deadline.
        let snapshot = fetch_from(
            &reqwest::Client::new(),
            Installation::Npm,
            Channel::Stable,
            &github,
            &npm,
        )
        .await
        .unwrap();
        slow_notes.abort();
        thread.join().unwrap();
        let output = serde_json::to_value(report(
            version("1.0.0"),
            Installation::Npm,
            Channel::Stable,
            version("1.0.0"),
            Some(snapshot),
            None,
            now(),
        ))
        .unwrap();
        assert_eq!(output["latest_version"], "99.0.0");
        assert_eq!(output["update_available"], true);
        assert_eq!(output["notes_complete"], false);
    }

    #[test]
    fn cache_freshness_and_version_relations_control_reports_and_notices() {
        let snapshot = Snapshot {
            checked_at: 100_000,
            latest_version: version("1.2.0"),
            releases: vec![],
            notes_complete: true,
            notes_error: None,
        };
        for (installed, time, status, available) in [
            ("1.1.9", 100_000, "update_available", json!(true)),
            ("1.2.0+local", 100_000, "up_to_date", json!(false)),
            ("2.0.0", 100_000, "ahead", json!(false)),
            ("1.1.9", 186_399, "update_available", json!(true)),
            ("1.1.9", 186_400, "unknown", Value::Null),
            ("1.1.9", 99_999, "unknown", Value::Null),
        ] {
            let output = serde_json::to_value(report(
                version(installed),
                Installation::Npm,
                Channel::Stable,
                version(installed),
                Some(snapshot.clone()),
                None,
                time,
            ))
            .unwrap();
            assert_eq!(output["status"], status);
            assert_eq!(output["update_available"], available);
            assert_eq!(
                notice(
                    &Availability::from(&snapshot),
                    &version(installed),
                    Installation::Npm,
                    time
                )
                .is_some(),
                status == "update_available"
            );
        }
        let output = serde_json::to_value(report(
            version("1.1.9"),
            Installation::Npm,
            Channel::Stable,
            version("1.1.9"),
            Some(snapshot),
            Some("Check failed".into()),
            100_000,
        ))
        .unwrap();
        assert_eq!(output["status"], "unknown");
        assert_eq!(output["latest_version"], "1.2.0");
        assert_eq!(output["update_available"], Value::Null);
    }
}
