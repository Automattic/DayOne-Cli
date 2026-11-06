//! Opt-in media download phase (`dayone sync --media`).
//!
//! Entries synced by the pull phase carry moment metadata only. This phase
//! downloads the stored bytes of each moment (and its thumbnail), decrypts
//! them for end-to-end encrypted journals, and writes them under the profile
//! directory. It is idempotent (files that are already present are skipped)
//! and per-file failures never abort the run.

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures_util::stream::{self, StreamExt};
use md5::{Digest, Md5};
use serde::Serialize;

use crate::http::{ApiError, DayOneApiClient};
use crate::models::Moment;
use crate::store::entries::media::{
    MEDIA_DIR_NAME, MomentMediaKind, MomentMediaUpsert, sanitize_path_component,
};
use crate::store::sqlite::Store;
use crate::sync::engine::log_sync;
use crate::sync::phases::pull::EntriesDecryptor;
use crate::sync::phases::pull::crypto::{
    build_entries_decryptor, build_journal_decryptor, decrypt_original_media,
};

/// Parallel downloads. Kept small: each file is held in memory while it is
/// decrypted, and a small bound keeps load on the API modest.
pub(crate) const MEDIA_CONCURRENCY: usize = 4;
const MAX_ATTEMPTS: u32 = 3;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(1_000);
/// Failures listed individually in the JSON output; the rest are only counted.
const MAX_REPORTED_FAILURES: usize = 50;
const PROGRESS_LOG_INTERVAL: usize = 25;
/// Moment types the web client can display; everything else has no download.
const SUPPORTED_MOMENT_TYPES: [&str; 4] = ["image", "video", "audio", "pdfattachment"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaSyncMode {
    /// Originals and thumbnails.
    All,
    /// Thumbnails only (small, enough for most viewing).
    Thumbnails,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaFailure {
    pub journal_id: String,
    pub entry_id: String,
    pub moment_id: String,
    pub kind: &'static str,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaSyncOutput {
    pub mode: MediaSyncMode,
    /// Files this run looked at (originals and thumbnails of live entries).
    pub files_total: usize,
    pub downloaded: usize,
    /// Already present locally and unchanged.
    pub skipped: usize,
    /// The server has no bytes for the file (HTTP 404), e.g. still uploading.
    pub not_on_server: usize,
    pub failed: usize,
    pub bytes_downloaded: u64,
    pub failures: Vec<MediaFailure>,
    /// Set when the phase could not run at all.
    pub error: Option<String>,
}

impl MediaSyncOutput {
    fn new(mode: MediaSyncMode) -> Self {
        Self {
            mode,
            files_total: 0,
            downloaded: 0,
            skipped: 0,
            not_on_server: 0,
            failed: 0,
            bytes_downloaded: 0,
            failures: Vec::new(),
            error: None,
        }
    }

    /// Whether the run should be reported as unsuccessful (non-zero exit).
    pub fn has_failures(&self) -> bool {
        self.failed > 0 || self.error.is_some()
    }
}

#[derive(Debug, Clone)]
struct MediaJob {
    journal_id: String,
    entry_id: String,
    moment_id: String,
    kind: MomentMediaKind,
    source_md5: Option<String>,
    content_type: Option<String>,
    moment_type: Option<String>,
}

struct Written {
    rel_path: String,
    size: i64,
    md5: String,
    bytes_downloaded: u64,
}

enum JobOutcome {
    Written(Written),
    NotOnServer,
}

/// Runs the media phase against the live key material in `store`.
pub(crate) async fn run_media_phase<C: DayOneApiClient>(
    api: &C,
    store: &Store,
    profile_id: i64,
    user_id: Option<&str>,
    mode: MediaSyncMode,
) -> MediaSyncOutput {
    let decryptor = match build_media_decryptor(store, profile_id, user_id) {
        Ok(decryptor) => decryptor,
        Err(err) => {
            log_sync(format!("media decryptor unavailable: {err}"));
            None
        }
    };
    sync_media(api, store, mode, decryptor, RETRY_BASE_DELAY).await
}

fn build_media_decryptor(
    store: &Store,
    profile_id: i64,
    user_id: Option<&str>,
) -> Result<Option<Arc<EntriesDecryptor>>> {
    let Some(master_key) = store.get_encryption_key_for_profile(profile_id)? else {
        return Ok(None);
    };
    let Some(journal_decryptor) = build_journal_decryptor(store, &master_key, user_id)? else {
        return Ok(None);
    };
    Ok(Some(Arc::new(build_entries_decryptor(
        store,
        &journal_decryptor,
    )?)))
}

pub(crate) async fn sync_media<C: DayOneApiClient>(
    api: &C,
    store: &Store,
    mode: MediaSyncMode,
    decryptor: Option<Arc<EntriesDecryptor>>,
    retry_base_delay: Duration,
) -> MediaSyncOutput {
    let mut output = MediaSyncOutput::new(mode);
    let Some(profile_dir) = store.profile_dir().map(Path::to_path_buf) else {
        output.error = Some("profile directory is unavailable".to_owned());
        return output;
    };
    let jobs = match collect_jobs(store, mode) {
        Ok(jobs) => jobs,
        Err(err) => {
            output.error = Some(format!("failed to list local media: {err}"));
            return output;
        }
    };
    output.files_total = jobs.len();

    let mut pending = Vec::new();
    for job in jobs {
        match is_up_to_date(store, &profile_dir, &job) {
            Ok(true) => output.skipped += 1,
            Ok(false) => pending.push(job),
            Err(err) => {
                // An unreadable bookkeeping row just means "download again".
                log_sync(format!("media state check failed, re-downloading: {err}"));
                pending.push(job);
            }
        }
    }
    log_sync(format!(
        "media: {} file(s) to download, {} already present",
        pending.len(),
        output.skipped
    ));

    let total = pending.len();
    let mut finished = 0usize;
    let mut results = stream::iter(pending.into_iter().map(|job| {
        let decryptor = decryptor.clone();
        let profile_dir = profile_dir.clone();
        async move {
            let outcome = process_job(api, &profile_dir, &job, decryptor, retry_base_delay).await;
            (job, outcome)
        }
    }))
    .buffer_unordered(MEDIA_CONCURRENCY);

    while let Some((job, outcome)) = results.next().await {
        finished += 1;
        match outcome {
            Ok(JobOutcome::Written(written)) => {
                let saved = store.upsert_moment_media(&MomentMediaUpsert {
                    journal_id: &job.journal_id,
                    entry_id: &job.entry_id,
                    moment_id: &job.moment_id,
                    kind: job.kind,
                    rel_path: &written.rel_path,
                    file_size_bytes: written.size,
                    md5: &written.md5,
                    source_md5: job.source_md5.as_deref(),
                });
                match saved {
                    Ok(()) => {
                        output.downloaded += 1;
                        output.bytes_downloaded += written.bytes_downloaded;
                    }
                    Err(err) => record_failure(&mut output, &job, &err.to_string()),
                }
            }
            Ok(JobOutcome::NotOnServer) => output.not_on_server += 1,
            Err(err) => {
                let message = format!("{err:#}");
                record_failure(&mut output, &job, &message);
            }
        }
        if finished.is_multiple_of(PROGRESS_LOG_INTERVAL) || finished == total {
            log_sync(format!("media: {finished}/{total} processed"));
        }
    }
    output
}

fn record_failure(output: &mut MediaSyncOutput, job: &MediaJob, message: &str) {
    log_sync(format!(
        "media download failed journal_id={} entry_id={} moment_id={} kind={}: {message}",
        job.journal_id,
        job.entry_id,
        job.moment_id,
        job.kind.as_str()
    ));
    output.failed += 1;
    if output.failures.len() < MAX_REPORTED_FAILURES {
        output.failures.push(MediaFailure {
            journal_id: job.journal_id.clone(),
            entry_id: job.entry_id.clone(),
            moment_id: job.moment_id.clone(),
            kind: job.kind.as_str(),
            error: message.to_owned(),
        });
    }
}

fn collect_jobs(store: &Store, mode: MediaSyncMode) -> Result<Vec<MediaJob>> {
    let mut jobs = Vec::new();
    for record in store.list_entries_with_moments()? {
        let moments: Vec<Moment> = match serde_json::from_str(&record.moments_json) {
            Ok(moments) => moments,
            Err(err) => {
                log_sync(format!(
                    "media: skipping entry_id={} with unreadable moments: {err}",
                    record.entry_id
                ));
                continue;
            }
        };
        for moment in moments {
            let Some(moment_id) = moment.id.as_ref().map(|id| id.as_ref().to_owned()) else {
                continue;
            };
            let supported = moment.kind.as_deref().is_some_and(|kind| {
                SUPPORTED_MOMENT_TYPES.contains(&kind.to_ascii_lowercase().as_str())
            });
            if !supported {
                continue;
            }
            if mode == MediaSyncMode::All {
                jobs.push(MediaJob {
                    journal_id: record.journal_id.clone(),
                    entry_id: record.entry_id.clone(),
                    moment_id: moment_id.clone(),
                    kind: MomentMediaKind::Original,
                    source_md5: moment.md5.clone(),
                    content_type: moment.content_type.clone(),
                    moment_type: moment.kind.clone(),
                });
            }
            if let Some(thumbnail) = moment.thumbnail.as_ref() {
                jobs.push(MediaJob {
                    journal_id: record.journal_id.clone(),
                    entry_id: record.entry_id.clone(),
                    moment_id,
                    kind: MomentMediaKind::Thumbnail,
                    source_md5: thumbnail.md5.clone(),
                    content_type: thumbnail.content_type.clone(),
                    moment_type: moment.kind.clone(),
                });
            }
        }
    }
    Ok(jobs)
}

/// A file is current when it was recorded for the same advertised md5 and is
/// still on disk with the recorded size.
fn is_up_to_date(store: &Store, profile_dir: &Path, job: &MediaJob) -> Result<bool> {
    let Some(row) =
        store.get_moment_media(&job.journal_id, &job.entry_id, &job.moment_id, job.kind)?
    else {
        return Ok(false);
    };
    if row.source_md5 != job.source_md5 {
        return Ok(false);
    }
    Ok(std::fs::metadata(profile_dir.join(&row.rel_path))
        .map(|meta| meta.is_file() && i64::try_from(meta.len()).ok() == Some(row.file_size_bytes))
        .unwrap_or(false))
}

async fn process_job<C: DayOneApiClient>(
    api: &C,
    profile_dir: &Path,
    job: &MediaJob,
    decryptor: Option<Arc<EntriesDecryptor>>,
    retry_base_delay: Duration,
) -> Result<JobOutcome> {
    let thumbnail = job.kind == MomentMediaKind::Thumbnail;
    let Some(blob) = download_with_retry(
        api,
        &job.journal_id,
        &job.moment_id,
        thumbnail,
        retry_base_delay,
    )
    .await?
    else {
        return Ok(JobOutcome::NotOnServer);
    };

    let profile_dir = profile_dir.to_path_buf();
    let job = job.clone();
    tokio::task::spawn_blocking(move || {
        let bytes_downloaded = blob.len() as u64;
        let plain = if is_d1_locked_key_blob(&blob) {
            let decryptor = decryptor.as_deref().ok_or_else(|| {
                anyhow!(
                    "media is end-to-end encrypted; run `dayone auth key-set` and `dayone sync` to load your encryption key"
                )
            })?;
            decrypt_original_media(&blob, decryptor)?
        } else {
            blob
        };
        let md5 = format!("{:x}", Md5::digest(&plain));
        if job.kind == MomentMediaKind::Original
            && let Some(expected) = job.source_md5.as_deref()
            && !expected.eq_ignore_ascii_case(&md5)
        {
            // The advertised md5 can describe the stored (encrypted) form on
            // some clients, so a difference is noted, not treated as fatal.
            log_sync(format!(
                "media md5 differs from entry metadata moment_id={}",
                job.moment_id
            ));
        }
        let rel_path = relative_media_path(&job);
        write_media_file(&profile_dir, &rel_path, &plain)?;
        Ok(JobOutcome::Written(Written {
            rel_path,
            size: i64::try_from(plain.len()).context("media file too large")?,
            md5,
            bytes_downloaded,
        }))
    })
    .await
    .context("media worker panicked")?
}

async fn download_with_retry<C: DayOneApiClient>(
    api: &C,
    journal_id: &str,
    moment_id: &str,
    thumbnail: bool,
    base_delay: Duration,
) -> Result<Option<Vec<u8>>> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match api.download_media(journal_id, moment_id, thumbnail).await {
            Ok(bytes) => return Ok(bytes),
            Err(err) if attempt < MAX_ATTEMPTS && is_transient(&err) => {
                tokio::time::sleep(base_delay * 2u32.pow(attempt - 1)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

fn is_transient(err: &anyhow::Error) -> bool {
    match err.downcast_ref::<ApiError>() {
        Some(api_error) => api_error.status() == 429 || api_error.status() >= 500,
        // No HTTP status: a connection-level failure worth another try.
        None => {
            err.downcast_ref::<reqwest::Error>().is_some() || {
                let text = format!("{err:#}");
                text.contains("failed to send media download request")
                    || text.contains("failed to fetch media from storage")
                    || text.contains("failed to read media download body")
            }
        }
    }
}

/// D1 "locked key" container (format 1 or 2), as used for E2E media.
fn is_d1_locked_key_blob(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[0..2] == b"D1" && bytes[2] == 1 && matches!(bytes[3], 1 | 2)
}

fn relative_media_path(job: &MediaJob) -> String {
    let extension = file_extension(
        job.content_type.as_deref(),
        job.moment_type.as_deref(),
        job.kind,
    );
    let moment = sanitize_path_component(&job.moment_id);
    let name = match job.kind {
        MomentMediaKind::Original => format!("{moment}.{extension}"),
        MomentMediaKind::Thumbnail => format!("{moment}.thumb.{extension}"),
    };
    format!(
        "{MEDIA_DIR_NAME}/{}/{}/{name}",
        sanitize_path_component(&job.journal_id),
        sanitize_path_component(&job.entry_id)
    )
}

fn file_extension(
    content_type: Option<&str>,
    moment_type: Option<&str>,
    kind: MomentMediaKind,
) -> &'static str {
    let mime = content_type
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/tiff" => "tiff",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/x-m4v" => "m4v",
        "video/webm" => "webm",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/webm" => "weba",
        "application/pdf" => "pdf",
        _ if kind == MomentMediaKind::Original
            && moment_type.is_some_and(|t| t.eq_ignore_ascii_case("pdfAttachment")) =>
        {
            "pdf"
        }
        _ => "bin",
    }
}

/// Atomically writes `bytes` to `<profile_dir>/<rel_path>`, creating
/// directories as 0700 and the file as 0600.
fn write_media_file(profile_dir: &Path, rel_path: &str, bytes: &[u8]) -> Result<()> {
    let target = profile_dir.join(rel_path);
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("media path has no parent directory"))?;
    create_private_dirs(profile_dir, parent)?;

    let mut temp_name = target
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    temp_name.push(".part");
    let temp = parent.join(temp_name);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &target)
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(err).with_context(|| format!("failed to write media file {rel_path}"));
    }
    #[cfg(unix)]
    crate::util::set_owner_only_permissions(&target, 0o600)
        .context("failed to secure downloaded media file")?;
    Ok(())
}

/// Creates `dir` (and parents below `base`) with owner-only permissions.
fn create_private_dirs(base: &Path, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create media directory {}", dir.display()))?;
    #[cfg(unix)]
    {
        let mut current = dir;
        while current != base && current.starts_with(base) {
            crate::util::set_owner_only_permissions(current, 0o700)
                .context("failed to secure media directory")?;
            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }
    }
    #[cfg(not(unix))]
    let _ = base;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::http::fake::FakeApiClient;
    use crate::sync::crypto::{encrypt_original_media_for_journal, test_support};

    const NO_DELAY: Duration = Duration::from_millis(0);

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_at(dir.path().join("dayone.db")).unwrap();
        (dir, store)
    }

    fn save_entry(store: &Store, journal_id: &str, entry_id: &str, moments: &str) {
        store
            .upsert_entry_json_row(
                entry_id,
                journal_id,
                Some("2026-03-17T00:00:00.000Z"),
                None,
                &format!(r#"{{"id":"{entry_id}","body":"x","moments":{moments}}}"#),
            )
            .unwrap();
    }

    fn photo_moments(md5: &str) -> String {
        format!(
            r#"[{{"id":"m1","momentType":"image","contentType":"image/jpeg","md5":"{md5}",
                "thumbnail":{{"contentType":"image/jpeg","md5":"thumb-md5"}}}}]"#
        )
    }

    async fn run(
        api: &FakeApiClient,
        store: &Store,
        mode: MediaSyncMode,
        decryptor: Option<Arc<EntriesDecryptor>>,
    ) -> MediaSyncOutput {
        sync_media(api, store, mode, decryptor, NO_DELAY).await
    }

    #[tokio::test]
    async fn downloads_originals_and_thumbnails_then_skips_on_rerun() {
        let (dir, store) = store();
        save_entry(&store, "j1", "e1", &photo_moments("orig-md5"));
        let api = FakeApiClient::new()
            .with_media_fixture("j1", "m1", false, Some(b"original-bytes".to_vec()))
            .await
            .with_media_fixture("j1", "m1", true, Some(b"thumb".to_vec()))
            .await;

        let out = run(&api, &store, MediaSyncMode::All, None).await;
        assert_eq!(out.files_total, 2);
        assert_eq!(out.downloaded, 2);
        assert_eq!(out.failed, 0);
        assert!(!out.has_failures());
        assert_eq!(out.bytes_downloaded, 14 + 5);
        let original = dir.path().join("media/j1/e1/m1.jpg");
        assert_eq!(std::fs::read(&original).unwrap(), b"original-bytes");
        assert_eq!(
            std::fs::read(dir.path().join("media/j1/e1/m1.thumb.jpg")).unwrap(),
            b"thumb"
        );
        let row = store
            .get_moment_media("j1", "e1", "m1", MomentMediaKind::Original)
            .unwrap()
            .unwrap();
        assert_eq!(row.file_size_bytes, 14);
        assert_eq!(row.source_md5.as_deref(), Some("orig-md5"));

        // Second run: nothing is requested (the fake has no fixtures left, so
        // any request would be recorded as a failure).
        let rerun = run(&FakeApiClient::new(), &store, MediaSyncMode::All, None).await;
        assert_eq!(rerun.skipped, 2);
        assert_eq!(rerun.downloaded, 0);
        assert!(!rerun.has_failures());
    }

    #[tokio::test]
    async fn redownloads_when_file_missing_or_metadata_changed() {
        let (dir, store) = store();
        save_entry(&store, "j1", "e1", &photo_moments("v1"));
        let api = FakeApiClient::new()
            .with_media_fixture("j1", "m1", false, Some(b"one".to_vec()))
            .await
            .with_media_fixture("j1", "m1", true, Some(b"t".to_vec()))
            .await;
        run(&api, &store, MediaSyncMode::All, None).await;

        // Deleted original is fetched again; the intact thumbnail is not.
        std::fs::remove_file(dir.path().join("media/j1/e1/m1.jpg")).unwrap();
        let api = FakeApiClient::new()
            .with_media_fixture("j1", "m1", false, Some(b"one".to_vec()))
            .await;
        let out = run(&api, &store, MediaSyncMode::All, None).await;
        assert_eq!((out.downloaded, out.skipped), (1, 1));
        assert_eq!(api.media_requests().await.len(), 1);

        // The entry now advertises a different md5: replaced media is fetched.
        save_entry(&store, "j1", "e1", &photo_moments("v2"));
        let api = FakeApiClient::new()
            .with_media_fixture("j1", "m1", false, Some(b"two!".to_vec()))
            .await;
        let out = run(&api, &store, MediaSyncMode::All, None).await;
        assert_eq!((out.downloaded, out.skipped), (1, 1));
        assert_eq!(
            std::fs::read(dir.path().join("media/j1/e1/m1.jpg")).unwrap(),
            b"two!"
        );
    }

    #[tokio::test]
    async fn thumbnails_mode_never_requests_originals() {
        let (_dir, store) = store();
        save_entry(&store, "j1", "e1", &photo_moments("x"));
        let api = FakeApiClient::new()
            .with_media_fixture("j1", "m1", true, Some(b"t".to_vec()))
            .await;
        let out = run(&api, &store, MediaSyncMode::Thumbnails, None).await;
        assert_eq!((out.files_total, out.downloaded), (1, 1));
        assert_eq!(
            api.media_requests().await,
            vec!["DOWNLOAD_MEDIA j1/m1/thumbnail"]
        );
    }

    #[tokio::test]
    async fn one_failing_file_does_not_stop_the_others() {
        let (_dir, store) = store();
        save_entry(
            &store,
            "j1",
            "e1",
            r#"[{"id":"bad","momentType":"image"},{"id":"gone","momentType":"video"},{"id":"ok","momentType":"audio","contentType":"audio/mp4"}]"#,
        );
        let mut api = FakeApiClient::new();
        // 403 is not transient: one attempt only.
        api = api.with_media_http_error("j1", "bad", false, 403).await;
        api = api.with_media_fixture("j1", "gone", false, None).await;
        api = api
            .with_media_fixture("j1", "ok", false, Some(b"audio".to_vec()))
            .await;

        let out = run(&api, &store, MediaSyncMode::All, None).await;
        assert_eq!(out.downloaded, 1);
        assert_eq!(out.not_on_server, 1);
        assert_eq!(out.failed, 1);
        assert!(out.has_failures());
        assert_eq!(out.failures[0].moment_id, "bad");
        assert!(out.failures[0].error.contains("403"));
        assert!(
            store
                .get_moment_media("j1", "e1", "ok", MomentMediaKind::Original)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_moment_media("j1", "e1", "bad", MomentMediaKind::Original)
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn transient_errors_are_retried_then_reported() {
        let (_dir, store) = store();
        save_entry(
            &store,
            "j1",
            "e1",
            r#"[{"id":"flaky","momentType":"image"},{"id":"down","momentType":"image"}]"#,
        );
        let mut api = FakeApiClient::new();
        api = api.with_media_http_error("j1", "flaky", false, 503).await;
        api = api
            .with_media_fixture("j1", "flaky", false, Some(b"finally".to_vec()))
            .await;
        for _ in 0..MAX_ATTEMPTS {
            api = api.with_media_http_error("j1", "down", false, 429).await;
        }
        let out = run(&api, &store, MediaSyncMode::All, None).await;
        assert_eq!((out.downloaded, out.failed), (1, 1));
        assert_eq!(out.failures[0].moment_id, "down");
    }

    #[tokio::test]
    async fn decrypts_end_to_end_encrypted_media() {
        let (dir, store) = store();
        let fixture = test_support::build_e2e_journal_fixture();
        let plaintext = b"\xff\xd8\xff\xe0 secret photo bytes".to_vec();
        let blob = encrypt_original_media_for_journal(
            &fixture.journal,
            &plaintext,
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .unwrap();
        assert_ne!(blob, plaintext);
        save_entry(
            &store,
            "j-e2e",
            "e1",
            r#"[{"id":"m1","momentType":"image","contentType":"image/jpeg"}]"#,
        );
        let api = FakeApiClient::new()
            .with_media_fixture("j-e2e", "m1", false, Some(blob.clone()))
            .await;
        let decryptor = Arc::new(EntriesDecryptor::from_journal_keys(HashMap::from([(
            fixture.fingerprint_hex.clone(),
            fixture.journal_private_key.clone(),
        )])));

        let out = run(&api, &store, MediaSyncMode::All, Some(decryptor)).await;
        assert_eq!(out.downloaded, 1, "{:?}", out.failures);
        // The decrypted bytes are stored, and "downloaded" counts wire bytes.
        assert_eq!(
            std::fs::read(dir.path().join("media/j-e2e/e1/m1.jpg")).unwrap(),
            plaintext
        );
        assert_eq!(out.bytes_downloaded, blob.len() as u64);
        let row = store
            .get_moment_media("j-e2e", "e1", "m1", MomentMediaKind::Original)
            .unwrap()
            .unwrap();
        assert_eq!(row.file_size_bytes, plaintext.len() as i64);
    }

    #[tokio::test]
    async fn encrypted_media_without_keys_fails_closed_and_writes_nothing() {
        let (dir, store) = store();
        let fixture = test_support::build_e2e_journal_fixture();
        let blob = encrypt_original_media_for_journal(
            &fixture.journal,
            b"secret",
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .unwrap();
        save_entry(
            &store,
            "j-e2e",
            "e1",
            r#"[{"id":"m1","momentType":"image"}]"#,
        );
        let api = FakeApiClient::new()
            .with_media_fixture("j-e2e", "m1", false, Some(blob))
            .await;

        let out = run(&api, &store, MediaSyncMode::All, None).await;
        assert_eq!(out.failed, 1);
        assert!(out.failures[0].error.contains("auth key-set"));
        assert!(!dir.path().join("media").exists());
    }

    #[tokio::test]
    async fn wrong_key_does_not_store_ciphertext() {
        let (dir, store) = store();
        let fixture = test_support::build_e2e_journal_fixture();
        let blob = encrypt_original_media_for_journal(
            &fixture.journal,
            b"secret",
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .unwrap();
        save_entry(
            &store,
            "j-e2e",
            "e1",
            r#"[{"id":"m1","momentType":"image"}]"#,
        );
        let api = FakeApiClient::new()
            .with_media_fixture("j-e2e", "m1", false, Some(blob))
            .await;
        let empty = Arc::new(EntriesDecryptor::from_journal_keys(HashMap::new()));
        let out = run(&api, &store, MediaSyncMode::All, Some(empty)).await;
        assert_eq!(out.failed, 1);
        assert!(!dir.path().join("media").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn downloaded_media_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, store) = store();
        save_entry(&store, "j1", "e1", &photo_moments("x"));
        let api = FakeApiClient::new()
            .with_media_fixture("j1", "m1", false, Some(b"o".to_vec()))
            .await
            .with_media_fixture("j1", "m1", true, Some(b"t".to_vec()))
            .await;
        run(&api, &store, MediaSyncMode::All, None).await;
        let mode = |path: &str| {
            std::fs::metadata(dir.path().join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("media"), 0o700);
        assert_eq!(mode("media/j1"), 0o700);
        assert_eq!(mode("media/j1/e1"), 0o700);
        assert_eq!(mode("media/j1/e1/m1.jpg"), 0o600);
        assert_eq!(mode("media/j1/e1/m1.thumb.jpg"), 0o600);
        assert!(!dir.path().join("media/j1/e1/m1.jpg.part").exists());
    }

    #[tokio::test]
    async fn hostile_identifiers_cannot_escape_the_media_directory() {
        let (dir, store) = store();
        save_entry(
            &store,
            "../evil",
            "e1",
            r#"[{"id":"../../m","momentType":"image"}]"#,
        );
        let api = FakeApiClient::new()
            .with_media_fixture("../evil", "../../m", false, Some(b"x".to_vec()))
            .await;
        let out = run(&api, &store, MediaSyncMode::All, None).await;
        assert_eq!(out.downloaded, 1);
        let row = store
            .get_moment_media("../evil", "e1", "../../m", MomentMediaKind::Original)
            .unwrap()
            .unwrap();
        assert!(row.rel_path.starts_with("media/"));
        assert!(!row.rel_path.contains(".."));
        assert!(dir.path().join(&row.rel_path).is_file());
    }

    #[tokio::test]
    async fn unsupported_and_idless_moments_are_ignored() {
        let (_dir, store) = store();
        save_entry(
            &store,
            "j1",
            "e1",
            r#"[{"momentType":"image"},{"id":"x","momentType":"sticker"},{"id":"pdf","momentType":"pdfAttachment"}]"#,
        );
        let api = FakeApiClient::new()
            .with_media_fixture("j1", "pdf", false, Some(b"%PDF".to_vec()))
            .await;
        let out = run(&api, &store, MediaSyncMode::All, None).await;
        assert_eq!((out.files_total, out.downloaded), (1, 1));
        let row = store
            .get_moment_media("j1", "e1", "pdf", MomentMediaKind::Original)
            .unwrap()
            .unwrap();
        assert!(row.rel_path.ends_with("pdf.pdf"));
    }

    #[test]
    fn extension_mapping() {
        use MomentMediaKind::{Original, Thumbnail};
        assert_eq!(file_extension(Some("image/heic"), None, Original), "heic");
        assert_eq!(
            file_extension(Some("video/quicktime"), None, Original),
            "mov"
        );
        assert_eq!(
            file_extension(Some("audio/mp4; codecs=x"), None, Original),
            "m4a"
        );
        assert_eq!(file_extension(None, Some("pdfAttachment"), Original), "pdf");
        assert_eq!(
            file_extension(None, Some("pdfAttachment"), Thumbnail),
            "bin"
        );
        assert_eq!(file_extension(None, Some("image"), Original), "bin");
    }
}
