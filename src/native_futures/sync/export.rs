use super::{store::*, *};
use crate::native_futures::{
    config::{Config, Source, resolved},
    reader,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use rocksdb::{DB, Direction, IteratorMode, WriteBatch};
use std::{collections::BTreeMap, io::Read, path::Path as FsPath, sync::Arc};
mod checkpoint;
mod wal;
pub use checkpoint::adopt_source_checkpoint;

// v3: the two append-only domestic logs. Older export state is not reused; the
// cutover re-captures the source into a fresh replica.
const HEAD: &[u8] = b"meta/head/v3";
const PENDING: &[u8] = b"meta/pending/v3";
#[derive(Clone, Deserialize, Serialize)]
struct Head {
    store_id: String,
    cursor: u64,
    counts: [u64; CF_COUNT],
    captured_at_us: i64,
    #[serde(default)]
    source_sequence: Option<u64>,
    #[serde(default)]
    source_cf_ids: Option<[u32; CF_COUNT]>,
}
#[derive(Clone, Deserialize, Serialize)]
struct Capture {
    after: u64,
    until: u64,
    counts: [u64; CF_COUNT],
    captured_at_us: i64,
    ready: bool,
    #[serde(default)]
    source_sequence: Option<u64>,
    #[serde(default)]
    source_cf_ids: Option<[u32; CF_COUNT]>,
}

#[derive(Debug)]
struct Conflict(&'static str);
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Conflict {}
fn conflict<T>(message: &'static str) -> Result<T> {
    Err(Conflict(message).into())
}

fn identity(path: &FsPath) -> Result<String> {
    let mut bytes = vec![];
    std::fs::File::open(path.join("IDENTITY"))?
        .take(4097)
        .read_to_end(&mut bytes)?;
    ensure!(
        !bytes.is_empty() && bytes.len() <= 4096,
        "invalid execution database identity"
    );
    Ok(hex::encode(Sha256::digest(&bytes)))
}
fn hash_at(db: &DB, cursor: u64) -> Result<String> {
    if cursor == 0 {
        return Ok(GENESIS.into());
    }
    let change =
        get::<Change>(db, &seq_key(b'J', cursor))?.context("replication checkpoint is missing")?;
    ensure!(
        change.sequence == cursor,
        "replication checkpoint sequence mismatch"
    );
    change.decode()?;
    Ok(change.record_sha256)
}

/// Unpublished journal tails can be discarded after a capture crash. A fully
/// captured cut is replayed idempotently into the hash index before publication.
fn recover(db: &DB) -> Result<()> {
    let Some(plan) = get::<Capture>(db, PENDING)? else {
        return Ok(());
    };
    let mut head = get::<Head>(db, HEAD)?.context("missing export head")?;
    ensure!(head.cursor == plan.after, "export pending head mismatch");
    if !plan.ready {
        let mut batch = WriteBatch::default();
        if let Some(next) = head.cursor.checked_add(1) {
            batch.delete_range(seq_key(b'J', next), b"K".to_vec());
        }
        batch.delete(PENDING);
        write(db, batch)?;
        return Ok(());
    }
    let mut batch = WriteBatch::default();
    let mut count = 0;
    let mut cursor = plan.after;
    let mut previous = hash_at(db, cursor)?;
    if cursor < plan.until {
        for item in db.iterator(IteratorMode::From(
            &seq_key(b'J', cursor + 1),
            Direction::Forward,
        )) {
            let (key, value) = item?;
            if key.first() != Some(&b'J') || cursor == plan.until {
                break;
            }
            let change: Change = serde_json::from_slice(&value)?;
            ensure!(
                change.sequence == cursor + 1
                    && key.as_ref() == seq_key(b'J', change.sequence)
                    && change.previous_sha256 == previous,
                "export journal sequence gap"
            );
            let (raw_key, raw_value) = change.decode()?;
            previous = change.record_sha256.clone();
            batch.put(
                hash_key(change.column_family, &raw_key),
                Sha256::digest(&raw_value),
            );
            cursor = change.sequence;
            count += 1;
            if count == 500 {
                write(db, batch)?;
                batch = WriteBatch::default();
                count = 0;
            }
        }
    }
    ensure!(cursor == plan.until, "incomplete ready export capture");
    head.cursor = plan.until;
    head.counts = plan.counts;
    head.captured_at_us = plan.captured_at_us;
    head.source_sequence = plan.source_sequence;
    head.source_cf_ids = plan.source_cf_ids;
    batch.put(HEAD, serde_json::to_vec(&head)?);
    batch.put(seq_key(b'B', head.cursor), serde_json::to_vec(&plan)?);
    batch.delete(PENDING);
    write(db, batch)
}

fn capture(db: &DB, source: &Source, secondary: &FsPath, journal_path: &FsPath) -> Result<()> {
    let started = std::time::Instant::now();
    tracing::info!(source_id=%source.id, "capturing native source view");
    let execution_id = identity(&source.rocksdb_path)?;
    let store_id = hex::encode(Sha256::digest(format!(
        "{execution_id}\n{}",
        identity(journal_path)?
    )));
    if let Some(head) = get::<Head>(db, HEAD)? {
        if head.store_id != store_id {
            return conflict("execution store identity changed; provision a new source explicitly");
        }
    } else {
        let head = Head {
            store_id: store_id.clone(),
            cursor: 0,
            counts: [0; CF_COUNT],
            captured_at_us: 0,
            source_sequence: None,
            source_cf_ids: None,
        };
        let mut batch = WriteBatch::default();
        batch.put(HEAD, serde_json::to_vec(&head)?);
        write(db, batch)?;
    }
    recover(db)?;
    let view = reader::open_secondary(source, secondary)?;
    ensure!(
        identity(&source.rocksdb_path)? == execution_id,
        "execution store changed while opening secondary"
    );
    let head = get::<Head>(db, HEAD)?.context("export head")?;
    let source_sequence = view.latest_sequence_number();
    let source_cf_ids = wal::column_family_ids(&view)?;
    ensure!(
        head.source_sequence
            .is_none_or(|previous| previous <= source_sequence),
        "native source sequence moved backwards"
    );
    ensure!(
        head.source_cf_ids.is_none_or(|ids| ids == source_cf_ids),
        "native source column families were replaced"
    );
    let mut plan = Capture {
        after: head.cursor,
        until: head.cursor,
        counts: [0; CF_COUNT],
        captured_at_us: crate::native_futures::now_us(),
        ready: false,
        source_sequence: Some(source_sequence),
        source_cf_ids: Some(source_cf_ids),
    };
    let mut start = WriteBatch::default();
    start.put(PENDING, serde_json::to_vec(&plan)?);
    write(db, start)?;
    let incremental = if head.source_sequence.is_some() {
        plan.counts = head.counts;
        wal::capture(db, &view, &head, &mut plan)?
    } else {
        false
    };
    if !incremental {
        // Discard only an unpublished incremental attempt before auditing a
        // complete fixed source view. Its published hash index is unchanged.
        recover(db)?;
        plan.until = head.cursor;
        plan.counts = [0; CF_COUNT];
        let mut start = WriteBatch::default();
        start.put(PENDING, serde_json::to_vec(&plan)?);
        write(db, start)?;
        capture_full(db, &view, &head, &mut plan)?;
    }
    ensure!(
        identity(&source.rocksdb_path)? == execution_id,
        "execution store changed during export capture"
    );
    plan.ready = true;
    let mut batch = WriteBatch::default();
    batch.put(PENDING, serde_json::to_vec(&plan)?);
    write(db, batch)?;
    recover(db)?;
    tracing::info!(source_id=%source.id, records=plan.counts.iter().sum::<u64>(),
        changes=plan.until-plan.after, elapsed_ms=started.elapsed().as_millis(), incremental,
        "captured native source view");
    Ok(())
}

fn capture_full(db: &DB, view: &DB, head: &Head, plan: &mut Capture) -> Result<()> {
    let mut batch = WriteBatch::default();
    let mut bytes = 0;
    let mut new_keys = [0_u64; CF_COUNT];
    let mut previous = hash_at(db, head.cursor)?;
    for cf in NativeCf::ALL {
        for item in view.iterator_cf(
            view.cf_handle(cf.name())
                .context("native export column family")?,
            IteratorMode::Start,
        ) {
            let (key, value) = item?;
            plan.counts[cf.index()] = plan.counts[cf.index()]
                .checked_add(1)
                .context("native record count overflow")?;
            let hash = Sha256::digest(&value);
            let old = db.get(hash_key(cf, &key))?;
            if old.as_deref() == Some(&hash[..]) {
                continue;
            }
            if old.is_none() {
                new_keys[cf.index()] += 1;
            }
            if old.is_some() {
                return conflict("append-only native record was modified");
            }
            plan.until = plan
                .until
                .checked_add(1)
                .context("native sync sequence exhausted")?;
            let change = Change::new(plan.until, cf, &key, &value, &previous)?;
            previous = change.record_sha256.clone();
            let encoded = serde_json::to_vec(&change)?;
            bytes += encoded.len();
            batch.put(seq_key(b'J', change.sequence), encoded);
            if bytes >= 4 * 1024 * 1024 {
                write(db, batch)?;
                batch = WriteBatch::default();
                bytes = 0;
            }
        }
    }
    for cf in NativeCf::ALL {
        if plan.counts[cf.index()]
            != head.counts[cf.index()]
                .checked_add(new_keys[cf.index()])
                .context("native count overflow")?
        {
            return conflict("source records disappeared; original history must be preserved");
        }
    }
    write(db, batch)?;
    Ok(())
}

struct Journal {
    db: DB,
    directory_id: String,
}

fn open_journal(config: &Config, source_id: &str) -> Result<Journal> {
    let settings = config
        .sync_export
        .as_ref()
        .context("sync_export is not configured")?;
    let root = resolved(&settings.journal_root)?;
    let path = root.join(source_id);
    ensure!(
        resolved(&path)?.starts_with(&root),
        "export journal escapes configured root"
    );
    std::fs::create_dir_all(&root)?;
    let db = open_owned(&path, "cn_futures_sync_export", source_id, &[])?;
    Ok(Journal {
        db,
        directory_id: identity(&path)?,
    })
}

#[cfg(test)]
pub(super) fn page(config: &Config, source_id: &str, request: Request) -> Result<Page> {
    let db = open_journal(config, source_id)?;
    page_with_db(config, source_id, request, &db.db)
}

fn page_with_db(config: &Config, source_id: &str, request: Request, db: &DB) -> Result<Page> {
    if request.mode != crate::runtime_mode::RuntimeMode::CnFutures {
        return conflict("synchronization mode mismatch");
    }
    let settings = config.sync_export.as_ref().context("sync_export")?;
    let source = config
        .sources
        .iter()
        .find(|s| s.id == source_id && s.enabled && s.sync.is_none())
        .context("export source unavailable")?;
    let path = resolved(&settings.journal_root)?.join(source_id);
    let existing = get::<Head>(db, HEAD)?;
    if request.after > existing.as_ref().map_or(0, |h| h.cursor)
        || request
            .store_id
            .as_ref()
            .is_some_and(|id| existing.as_ref().is_none_or(|h| h.store_id != *id))
    {
        return conflict("replication source or journal was reset or rolled back");
    }
    if hash_at(db, request.after)? != request.after_sha256 {
        return conflict("replication checkpoint hash mismatch");
    }
    if request.until.is_none() {
        capture(db, source, &config.secondary_root.join(source_id), &path)?;
    } else {
        recover(db)?;
    }
    let head = get::<Head>(db, HEAD)?.context("no captured export head")?;
    if request
        .store_id
        .as_ref()
        .is_some_and(|id| *id != head.store_id)
    {
        return conflict("remote execution store identity does not match local cursor");
    }
    let until = request.until.unwrap_or(head.cursor);
    if until > head.cursor || request.after > until {
        return conflict("replication cursor exceeds captured source head");
    }
    let cut = get::<Capture>(db, &seq_key(b'B', until))?
        .context("requested replication cut is not available")?;
    let mut changes = vec![];
    let mut next = request.after;
    let mut next_sha256 = request.after_sha256.clone();
    let mut bytes = 0;
    if next < until {
        for item in db.iterator(IteratorMode::From(
            &seq_key(b'J', next + 1),
            Direction::Forward,
        )) {
            let (key, value) = item?;
            if key.first() != Some(&b'J') || next == until {
                break;
            }
            let change: Change = serde_json::from_slice(&value)?;
            ensure!(
                change.sequence == next + 1
                    && key.as_ref() == seq_key(b'J', change.sequence)
                    && change.previous_sha256 == next_sha256,
                "native export journal gap"
            );
            if !changes.is_empty()
                && (changes.len() >= settings.batch_records
                    || bytes + value.len() > settings.batch_bytes)
            {
                break;
            }
            bytes += value.len();
            next = change.sequence;
            next_sha256 = change.record_sha256.clone();
            changes.push(change);
        }
        ensure!(
            next > request.after,
            "empty page before replication boundary"
        );
    }
    Ok(Page {
        mode: config.mode,
        source_id: source_id.into(),
        store_id: head.store_id,
        after: request.after,
        after_sha256: request.after_sha256,
        next,
        until,
        next_sha256,
        until_sha256: hash_at(db, until)?,
        captured_at_us: cut.captured_at_us,
        changes,
    })
}

struct ExportState {
    config: Config,
    token_hash: [u8; 32],
    journals: BTreeMap<String, Arc<tokio::sync::Mutex<Option<Journal>>>>,
    viz: Option<(reqwest::Url, reqwest::Client)>,
}
#[derive(Clone)]
pub struct ExportService(Arc<ExportState>);
impl ExportService {
    pub fn new(config: Config) -> Result<Self> {
        config.validate()?;
        let settings = config
            .sync_export
            .as_ref()
            .context("configure [sync_export] before serving")?;
        let secret = super::settings::token(&settings.token_env)?;
        Self::with_token(config, &secret)
    }
    pub(super) fn with_token(config: Config, secret: &str) -> Result<Self> {
        config.validate()?;
        let settings = config.sync_export.as_ref().context("sync_export")?;
        let journals = settings
            .source_ids
            .iter()
            .map(|id| (id.clone(), Arc::default()))
            .collect();
        let viz = settings
            .account_funds_url
            .as_deref()
            .map(|url| -> Result<_> {
                Ok((
                    reqwest::Url::parse(url)?,
                    reqwest::Client::builder()
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .timeout(std::time::Duration::from_secs(5))
                        .build()?,
                ))
            })
            .transpose()?;
        Ok(Self(Arc::new(ExportState {
            config,
            token_hash: Sha256::digest(secret.as_bytes()).into(),
            journals,
            viz,
        })))
    }
    fn authorized(&self, headers: &HeaderMap) -> bool {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|v| {
                let hash = Sha256::digest(v.as_bytes());
                hash.iter()
                    .zip(&self.0.token_hash)
                    .fold(0_u8, |sum, (a, b)| sum | (a ^ b))
                    == 0
            })
    }
    pub fn router(self) -> Router {
        Router::new()
            .route("/api/native/sync/{id}", post(export_page))
            .route(
                "/api/native/sync/{id}/account-funds",
                axum::routing::get(export_funds),
            )
            .with_state(self)
            .layer(
                tower_http::compression::CompressionLayer::new()
                    .gzip(true)
                    .quality(tower_http::compression::CompressionLevel::Fastest),
            )
    }
}

async fn export_page(
    State(service): State<ExportService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Response {
    if !service.authorized(&headers) {
        return unauthorized();
    }
    let Some(lock) = service.0.journals.get(&id).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let guard = lock.lock_owned().await;
    let state = service.0.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut journal = guard;
        if let Some(cached) = journal.as_ref() {
            let root = &state
                .config
                .sync_export
                .as_ref()
                .context("sync_export")?
                .journal_root;
            if !identity(&root.join(&id)).is_ok_and(|value| value == cached.directory_id) {
                *journal = None;
                return conflict(
                    "export journal directory changed; preserve history and review the source",
                );
            }
        }
        if journal.is_none() {
            *journal = Some(open_journal(&state.config, &id)?);
        }
        let result = page_with_db(
            &state.config,
            &id,
            request,
            &journal.as_ref().expect("journal initialized").db,
        );
        if result
            .as_ref()
            .is_err_and(|error| error.downcast_ref::<rocksdb::Error>().is_some())
        {
            *journal = None;
        }
        result
    })
    .await;
    match result {
        Ok(Ok(page)) => Json(page).into_response(),
        Ok(Err(error)) => {
            if let Some(conflict) = error.downcast_ref::<Conflict>() {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error":conflict.0})),
                )
                    .into_response();
            }
            tracing::warn!(error=?error,"native sync export failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"native export unavailable"})),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(error=?error,"native sync export worker failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error":"unauthorized"})),
    )
        .into_response()
}

/// Read-only relay of the same-host Viz broker funds for an exported source.
async fn export_funds(
    State(service): State<ExportService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !service.authorized(&headers) {
        return unauthorized();
    }
    let state = &service.0;
    let (Some((base, client)), Some(source)) = (
        state.viz.as_ref(),
        state
            .journals
            .contains_key(&id)
            .then(|| state.config.sources.iter().find(|s| s.id == id))
            .flatten(),
    ) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let result = async {
        let url = base.join(&format!("account-funds/{id}"))?;
        let response = client.get(url).send().await?;
        // A counter without an explicit Manager binding has no funds.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return anyhow::Ok(None);
        }
        let observation: crate::native_futures::funds::Observation =
            response.error_for_status()?.json().await?;
        observation.validate(&source.id, &source.adapter)?;
        anyhow::Ok(Some(observation))
    }
    .await;
    match result {
        Ok(Some(observation)) => Json(observation).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::warn!(source_id=%id,error=?error,"native broker funds relay failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"broker funds unavailable"})),
            )
                .into_response()
        }
    }
}
