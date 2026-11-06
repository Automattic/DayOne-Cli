use anyhow::Result;
use serde::Serialize;

use crate::store::sqlite::Store;
use crate::sync::engine;
pub use crate::sync::phases::media::{MediaSyncMode, MediaSyncOutput};

#[derive(Debug, Clone, Copy)]
pub struct SyncArgs {
    pub ignore_cursors: bool,
    /// `Some` downloads (and decrypts) media after the regular sync phases.
    pub media: Option<MediaSyncMode>,
}

#[derive(Debug, Serialize)]
pub struct SyncCommandOutput {
    pub ok: bool,
    pub locked: bool,
    pub fatal_error: Option<String>,
    #[serde(skip)]
    pub failure: Option<anyhow::Error>,
    pub resources: Vec<engine::ResourceSyncOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaSyncOutput>,
}

pub async fn execute(store: &Store, base_url: &str, args: SyncArgs) -> Result<SyncCommandOutput> {
    let output = engine::run_sync(store, base_url, args.ignore_cursors, args.media).await?;
    Ok(SyncCommandOutput {
        ok: output.ok,
        locked: output.locked,
        fatal_error: output.fatal_error,
        failure: output.failure,
        resources: output.resources,
        media: output.media,
    })
}
