use anyhow::{Result, bail};
use serde::Serialize;

use crate::store::sqlite::Store;
use crate::util::normalize_entry_id;

#[derive(Debug)]
pub struct EntryTagArgs {
    pub journal_id: Option<String>,
    pub entry_id: String,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct EntryTagOutput {
    pub ok: bool,
    pub base_url: String,
    pub profile_id: i64,
    pub journal_id: String,
    pub entry_id: String,
    pub tags: Vec<String>,
    pub queued: bool,
    pub outbox_id: Option<String>,
    pub synced: bool,
}

pub fn execute(store: &Store, base_url: &str, args: EntryTagArgs) -> Result<EntryTagOutput> {
    if args.add.is_empty() && args.remove.is_empty() {
        bail!("provide at least one --add or --remove tag");
    }
    let entry_id = normalize_entry_id(&args.entry_id);
    if entry_id.is_empty() {
        bail!("entry_id must not be empty");
    }
    let journal_id = args.journal_id.as_deref().map(str::trim);
    if journal_id == Some("") {
        bail!("journal_id must not be empty");
    }
    let session = store
        .get_auth_session_for_base_url(base_url)?
        .ok_or_else(crate::telemetry::UserError::auth_required)?;
    let (journal_id, tags, changed) =
        store.update_entry_tags_and_enqueue(&entry_id, journal_id, &args.add, &args.remove)?;
    let outbox_id = changed.then(|| format!("entry:{journal_id}:{entry_id}"));
    Ok(EntryTagOutput {
        ok: true,
        base_url: base_url.to_owned(),
        profile_id: session.profile_id,
        journal_id,
        entry_id,
        tags,
        queued: changed,
        outbox_id,
        synced: false,
    })
}
