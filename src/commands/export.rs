use std::collections::HashSet;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use md5::{Digest, Md5};
use serde::Serialize;
use serde_json::{Map, Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zip::{ZipWriter, write::SimpleFileOptions};

use crate::http::client::AttachmentDownloader;
use crate::models::{Entry, Journal, Moment};
use crate::store::sqlite::{EntryListSort, EntryListSortMethod, ListPageRequest, Store};
use crate::store::user_id_from_session_json;
use crate::sync::engine;
use crate::sync::phases::pull::crypto::{
    build_entries_decryptor, build_journal_decryptor, decrypt_original_media,
};
use crate::util::bytes_to_hex_lower;

#[derive(Debug, Serialize)]
pub struct ExportOutput {
    pub ok: bool,
    pub path: String,
    pub journals: usize,
    pub entries: usize,
    pub attachments: usize,
}

pub async fn execute(store: &Store, base_url: &str, output: PathBuf) -> Result<ExportOutput> {
    let path = output.to_str().context("export path must be valid UTF-8")?;
    match output.symlink_metadata() {
        Ok(_) => bail!("export output file already exists"),
        Err(err) if err.kind() == ErrorKind::NotFound => (),
        Err(err) => return Err(err).context("failed to check export output path"),
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::Builder::new()
        .prefix(".dayone-export-")
        .tempfile_in(parent)
        .context("failed to create a temporary ZIP in the output directory")?;
    let sync = engine::run_sync(store, base_url, false).await?;
    ensure!(
        !sync.locked,
        "another sync is in progress; try export again later"
    );
    if let Some(error) = sync.failure {
        return Err(error).context("sync failed before export");
    }
    ensure!(sync.ok, "sync failed before export");
    if let Some(resource) = sync.resources.iter().find(|resource| {
        resource.status == "error"
            && (resource.resource == "journals" || resource.resource.starts_with("entries:"))
    }) {
        bail!(
            "sync failed for {}: {}",
            resource.resource,
            resource.error.as_deref().unwrap_or("unknown error")
        );
    }

    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;
    let downloader = AttachmentDownloader::new(base_url, &session.token)?;
    let mut decryptor = None;
    let mut zip = ZipWriter::new(file.as_file_mut());
    let options = SimpleFileOptions::default().unix_permissions(0o600);
    let mut used_journal_names = HashSet::new();
    let mut media_paths = HashSet::new();
    let mut report = ExportOutput {
        ok: true,
        path: path.to_owned(),
        journals: 0,
        entries: 0,
        attachments: 0,
    };

    let journals = store.list_journals_json_rows_paged(&ListPageRequest::default(), false)?;
    for raw in journals.rows {
        let journal: Journal = serde_json::from_str(&raw).context("invalid local journal")?;
        let name = journal
            .extra
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Journal");
        let name: String = name
            .chars()
            .take(60)
            .map(|ch| {
                if ch.is_control() || "/\\:*?\"<>|".contains(ch) {
                    '_'
                } else {
                    ch
                }
            })
            .collect();
        let name = name.trim_matches([' ', '.']);
        let name = if name.is_empty() { "Journal" } else { name };
        let mut filename = format!("{name}.json");
        for n in 2.. {
            if used_journal_names.insert(filename.to_lowercase()) {
                break;
            }
            filename = format!("{name}-{n}.json");
        }

        let rows = store.list_entries_json_rows_by_journal_id(
            journal.id.as_ref(),
            None,
            EntryListSort::Asc,
            EntryListSortMethod::EntryDate,
        )?;
        let mut entries = Vec::new();
        for raw in rows {
            let entry: Entry = serde_json::from_str(&raw).context("invalid local entry")?;
            let mut exported = export_entry(&entry)?;
            for (order, moment) in entry.moments.iter().enumerate() {
                let id = moment
                    .id
                    .as_ref()
                    .with_context(|| {
                        format!(
                            "attachment {} in entry {} has no ID",
                            order + 1,
                            entry.id.as_ref()
                        )
                    })?
                    .as_ref();
                let data = async {
                    let data = downloader
                        .download(journal.id.as_ref(), id, moment.md5.as_deref())
                        .await?;
                    if !data.starts_with(b"D1") {
                        return Ok(data);
                    }
                    if decryptor.is_none() {
                        let key = store
                            .get_encryption_key_for_profile(session.profile_id)?
                            .context("encryption key required; run `dayone auth key-set`")?;
                        let user_id = user_id_from_session_json(&session.user_json)?;
                        let journals = build_journal_decryptor(store, &key, user_id.as_deref())?
                            .context("journal keys are not available locally")?;
                        decryptor = Some(build_entries_decryptor(store, &journals)?);
                    }
                    decrypt_original_media(
                        &data,
                        decryptor.as_ref().expect("decryptor is initialized"),
                    )
                }
                .await
                .with_context(|| {
                    format!(
                        "failed to export attachment {id} from entry {}",
                        entry.id.as_ref()
                    )
                })?;

                let (field, folder, extension) = media_format(moment);
                let md5 = bytes_to_hex_lower(&Md5::digest(&data));
                let path = format!("{folder}/{md5}.{extension}");
                if media_paths.insert(path.clone()) {
                    zip.start_file(&path, options)?;
                    zip.write_all(&data)?;
                }
                let mut media = json!({
                    "identifier": id, "md5": md5, "fileSize": data.len(),
                    "orderInEntry": order, "width": moment.width, "height": moment.height,
                });
                media[if field == "audios" { "format" } else { "type" }] = json!(extension);
                for key in ["relatedTo", "duration", "isSketch", "transcription"] {
                    if let Some(value) = moment.extra.get(key) {
                        media[key] = value.clone();
                    }
                }
                if field == "relatedMedia" {
                    media["momentType"] = json!(moment.kind);
                }
                if field == "attachments" {
                    media["path"] = json!(path);
                    media["contentType"] = json!(moment.content_type);
                }
                exported
                    .entry(field)
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .expect("media field is an array")
                    .push(media);
                report.attachments += 1;
            }
            entries.push(exported);
        }
        zip.start_file(filename, options.large_file(true))?;
        serde_json::to_writer(
            &mut zip,
            &json!({"metadata": {"version": "1.0"}, "entries": entries}),
        )?;
        report.journals += 1;
        report.entries += entries.len();
    }
    zip.finish()?.sync_all()?;
    file.persist_noclobber(output)
        .map_err(|err| err.error)
        .context("failed to save the export without replacing an existing file")?;
    Ok(report)
}

fn export_entry(entry: &Entry) -> Result<Map<String, Value>> {
    let mut result = Map::from_iter([("uuid".to_owned(), json!(entry.id))]);
    if let Some(body) = &entry.body {
        result.insert("text".into(), json!(body));
    }
    if let Some(rich_text) = entry
        .rich_text_json
        .as_ref()
        .filter(|value| !value.is_null())
    {
        result.insert(
            "richText".into(),
            json!(
                rich_text
                    .as_str()
                    .map_or_else(|| rich_text.to_string(), str::to_owned)
            ),
        );
    }
    for (key, date) in [
        ("creationDate", entry.date.clone()),
        (
            "modifiedDate",
            entry
                .user_edit_date
                .clone()
                .or_else(|| entry.updated_at.as_ref().map(|date| json!(date))),
        ),
    ] {
        if let Some(date) = date.filter(|value| !value.is_null()) {
            let ms = date
                .as_i64()
                .or_else(|| date.as_f64().map(|n| n as i64))
                .or_else(|| date.as_str()?.parse().ok());
            let instant = match ms {
                Some(ms) => OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)?,
                None => {
                    OffsetDateTime::parse(date.as_str().context("invalid entry date")?, &Rfc3339)?
                }
            };
            result.insert(key.into(), json!(instant.format(&Rfc3339)?));
        }
    }
    for (field, aliases) in [
        ("starred", &["starred", "isStarred", "favorite"][..]),
        ("pinned", &["pinned", "isPinned"][..]),
        ("timeZone", &["timeZone", "time_zone"][..]),
        ("entryType", &["entryType", "type"][..]),
        ("tags", &["tags"][..]),
        ("location", &["location"][..]),
        ("weather", &["weather"][..]),
        ("userActivity", &["userActivity"][..]),
        ("creationDevice", &["creationDevice"][..]),
        ("creationOSName", &["creationOSName"][..]),
        ("creationOSVersion", &["creationOSVersion"][..]),
        ("creationDeviceType", &["creationDeviceType"][..]),
    ] {
        if let Some(value) = aliases.iter().find_map(|key| entry.extra.get(*key)) {
            result.insert(field.into(), value.clone());
        }
    }
    Ok(result)
}

fn media_format(moment: &Moment) -> (&'static str, &'static str, String) {
    let mime = moment.content_type.as_deref().unwrap_or_default();
    let (field, folder, default) = match moment.kind.as_deref() {
        Some("image" | "photo") => ("photos", "photos", "jpg"),
        Some("video") => ("videos", "videos", "mp4"),
        Some("audio") => ("audios", "audios", "m4a"),
        Some("pdfAttachment") => ("pdfAttachments", "pdfs", "pdf"),
        _ if mime.starts_with("image/") => ("photos", "photos", "jpg"),
        _ if mime.starts_with("video/") => ("videos", "videos", "mp4"),
        _ if mime.starts_with("audio/") => ("audios", "audios", "m4a"),
        _ if mime == "application/pdf" => ("pdfAttachments", "pdfs", "pdf"),
        _ => ("attachments", "attachments", "bin"),
    };
    let extension = match mime {
        "image/jpeg" => "jpg",
        "image/svg+xml" => "svg",
        "video/quicktime" => "mov",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/mpeg" => "mp3",
        "application/octet-stream" => default,
        _ => mime
            .split_once('/')
            .map(|(_, subtype)| subtype)
            .filter(|subtype| {
                !subtype.is_empty()
                    && subtype.len() <= 12
                    && subtype.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .unwrap_or(default),
    }
    .to_ascii_lowercase();
    if moment
        .extra
        .get("relatedTo")
        .and_then(Value::as_str)
        .is_some()
    {
        ("relatedMedia", "related", extension)
    } else {
        (field, folder, extension)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    use crate::sync::crypto::{
        encrypt_original_media_for_journal, test_support::build_e2e_journal_fixture,
    };

    #[tokio::test]
    async fn export_writes_decrypted_originals_and_discards_failed_archives() {
        let fixture = build_e2e_journal_fixture();
        let ciphertext = encrypt_original_media_for_journal(
            &fixture.journal,
            b"original PDF",
            Some(&fixture.master_key),
            Some(&fixture.user_keys_json),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let outputs = tempfile::tempdir().unwrap();
        let store =
            Store::open_at_with_in_memory_master_keys(dir.path().join("dayone.db")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}");
        let profile = store.get_or_create_profile_for_base_url(&base).unwrap();
        store
            .save_auth_session(
                profile.id,
                "test-token",
                "2026-09-01T00:00:00Z",
                r#"{"id":"2468135790"}"#,
            )
            .unwrap();
        store
            .set_encryption_key_for_profile(profile.id, &fixture.master_key)
            .unwrap();
        let mut journal = fixture.journal;
        journal["name"] = json!("Family/2026");
        store
            .upsert_json_row("journals", "journal-e2e", None, None, &journal.to_string())
            .unwrap();
        store.enable_journal_sync("journal-e2e").unwrap();
        store
            .upsert_entry_json_row(
                "entry-old",
                "journal-e2e",
                None,
                None,
                &json!({
                    "id": "entry-old", "body": "Old entry", "date": 1_700_000_000_123_i64,
                    "moments": [{"id": "photo", "type": "image", "contentType": "image/jpeg", "md5": "3707b04820a4979b3ae22ec87c04c97b"}],
                })
                .to_string(),
            )
            .unwrap();
        store
            .upsert_entry_json_row(
                "entry-deleted",
                "journal-e2e",
                None,
                Some("2026-09-01T00:00:00Z"),
                &json!({
                    "id": "entry-deleted", "moments": [{"id": "deleted"}],
                })
                .to_string(),
            )
            .unwrap();

        let corrupt_media = Arc::new(AtomicBool::new(true));
        let server = {
            let corrupt_media = corrupt_media.clone();
            let base = base.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let stream = stream.unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let url = line.split_whitespace().nth(1).unwrap();
                    if url == "/stop" {
                        break;
                    }
                    let path = url.split('?').next().unwrap();
                    let mut headers = String::new();
                    loop {
                        let start = headers.len();
                        assert_ne!(reader.read_line(&mut headers).unwrap(), 0);
                        if &headers[start..] == "\r\n" {
                            break;
                        }
                    }
                    let stream = reader.get_mut();
                    let headers = headers.to_ascii_lowercase();
                    let body: &[u8] = match path {
                        "/api/users/key" => fixture.user_keys_json.as_bytes(),
                        "/api/v2/sync/entries/journal-e2e/feed" if !url.contains("cursor=") => concat!(r#"{"id":"entry-new","cursor":"next","date":"2026-10-05T12:34:56Z","body":"New entry","moments":[{"id":"pdf","type":"pdfAttachment","contentType":"application/pdf"}]}"#, "\n").as_bytes(),
                        "/api/v2/sync/entries/journal-e2e/feed" => b"",
                        "/api/journals/journal-e2e/attachments/photo/download" |
                        "/api/journals/journal-e2e/attachments/pdf/download" => {
                            assert!(headers.contains("authorization: bearer test-token\r\n"));
                            let id = path.split('/').nth(5).unwrap();
                            if id == "photo" {
                                assert!(url.ends_with("?thumbnail=false&md5=3707b04820a4979b3ae22ec87c04c97b"));
                            } else {
                                assert!(url.ends_with("?thumbnail=false"));
                            }
                            write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: {base}/original/{id}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                            continue;
                        }
                        "/original/photo" | "/original/pdf" => {
                            assert!(!headers.contains("authorization:"));
                            match path {
                                "/original/photo" => b"original photo",
                                _ if corrupt_media.load(Ordering::Relaxed) => &ciphertext[..10],
                                _ => &ciphertext,
                            }
                        }
                        _ if path.ends_with("/download") => panic!("unexpected download {path}"),
                        _ => b"{}",
                    };
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(body).unwrap();
                }
            })
        };

        let output = outputs.path().join("backup.zip");
        let error = execute(&store, &base, output.clone()).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("failed to export attachment pdf from entry entry-new"),
            "{error:#}"
        );
        assert_eq!(outputs.path().read_dir().unwrap().count(), 0);
        corrupt_media.store(false, Ordering::Relaxed);
        let report = execute(&store, &base, output.clone()).await.unwrap();
        assert_eq!(
            (report.journals, report.entries, report.attachments),
            (1, 2, 2)
        );
        assert_eq!(outputs.path().read_dir().unwrap().count(), 1);
        assert_eq!(
            dir.path()
                .read_dir()
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path().is_dir())
                .count(),
            0
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                output.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        TcpStream::connect(address)
            .unwrap()
            .write_all(b"GET /stop HTTP/1.1\r\n\r\n")
            .unwrap();
        server.join().unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&output).unwrap()).unwrap();
        assert_eq!(zip.len(), 3);
        let document: Value =
            serde_json::from_reader(zip.by_name("Family_2026.json").unwrap()).unwrap();
        let old = &document["entries"][0];
        let new = &document["entries"][1];
        assert_eq!(old["uuid"], "entry-old");
        assert_eq!(old["text"], "Old entry");
        assert_eq!(old["creationDate"], "2023-11-14T22:13:20.123Z");
        assert_eq!(new["creationDate"], "2026-10-05T12:34:56Z");
        assert_eq!(new["uuid"], "entry-new");
        let mut assert_media = |media: &Value, id, folder, expected: &[u8]| {
            assert_eq!(media["identifier"], id);
            let path = format!(
                "{folder}/{}.{}",
                media["md5"].as_str().unwrap(),
                media["type"].as_str().unwrap()
            );
            let mut bytes = Vec::new();
            zip.by_name(&path).unwrap().read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, expected);
        };
        assert_eq!(old["photos"][0]["md5"], "3707b04820a4979b3ae22ec87c04c97b");
        assert_eq!(
            new["pdfAttachments"][0]["md5"],
            "a0df64b241147ddc6a5043948f54e892"
        );
        assert_media(&old["photos"][0], "photo", "photos", b"original photo");
        assert_media(&new["pdfAttachments"][0], "pdf", "pdfs", b"original PDF");
        let previous = std::fs::read(&output).unwrap();
        assert!(
            execute(&store, &base, output.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        assert_eq!(std::fs::read(&output).unwrap(), previous);
    }
}
