use super::{store::*, *};
use crate::native_futures::config::{Config, Source, resolved};
use rocksdb::{DB, IteratorMode, WriteBatch};
use std::{
    io::Read,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) fn decode_page(bytes: Vec<u8>, gzip: bool) -> Result<Page> {
    let bytes = if gzip {
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .take(MAX_RESPONSE_BYTES as u64 + 1)
            .read_to_end(&mut decoded)
            .context("decompress native sync response")?;
        decoded
    } else {
        bytes
    };
    ensure!(
        bytes.len() <= MAX_RESPONSE_BYTES,
        "decoded native sync response exceeds 16 MiB"
    );
    serde_json::from_slice(&bytes).context("decode native sync response")
}

pub struct Replica {
    pub(super) db: DB,
    source: Source,
    pub(super) path: PathBuf,
    poll_lock: tokio::sync::Mutex<()>,
}
impl Replica {
    pub fn open(config: &Config, source: &Source) -> Result<Self> {
        config.validate()?;
        let settings = config.sync.as_ref().context("configure [sync]")?;
        ensure!(
            settings.enabled && source.enabled && source.sync.is_some(),
            "native replica synchronization is disabled for this source"
        );
        ensure!(
            config.sources.iter().any(|s| s.enabled
                && s.id == source.id
                && s.rocksdb_path == source.rocksdb_path
                && s.sync.is_some()),
            "replica source is not configured"
        );
        let root = resolved(&settings.replica_root)?;
        let path = resolved(&source.rocksdb_path)?;
        ensure!(
            path == resolved(&root.join(&source.id))?,
            "replica path is outside its source directory"
        );
        std::fs::create_dir_all(&root)?;
        let cfs = [NativeCf::Orders.name(), NativeCf::Trades.name(), STAGING];
        let db = open_owned(&path, "cn_futures_replica", &source.id, &cfs)?;
        if get::<ReplicaStatus>(&db, STATE)?.is_none() {
            ensure!(
                NativeCf::ALL.iter().all(|cf| db
                    .iterator_cf(db.cf_handle(cf.name()).unwrap(), IteratorMode::Start)
                    .next()
                    .is_none()),
                "refusing to initialize missing replica state over existing native records"
            );
            let mut batch = WriteBatch::default();
            batch.put(STATE, serde_json::to_vec(&ReplicaStatus::default())?);
            write(&db, batch)?;
        }
        drop(writer_lock(&path)?);
        Ok(Self {
            db,
            source: source.clone(),
            path,
            poll_lock: tokio::sync::Mutex::new(()),
        })
    }
    pub fn status(&self) -> Result<ReplicaStatus> {
        replica_status(&self.db, &self.source.id)
    }

    pub(super) fn accept(&self, page: &Page) -> Result<()> {
        let mut state = self.status()?;
        ensure!(
            page.mode == crate::runtime_mode::RuntimeMode::CnFutures
                && !state.applying
                && page.source_id == self.source.id
                && page.store_id.len() == 64
                && page.store_id.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid replica source identity or application state"
        );
        ensure!(
            state
                .store_id
                .as_ref()
                .is_none_or(|id| *id == page.store_id),
            "source store identity changed; preserve history and provision a new source"
        );
        ensure!(
            page.after == state.received_cursor
                && page.after <= page.next
                && page.next <= page.until
                && state
                    .target_cursor
                    .is_none_or(|target| target == page.until),
            "replication page cursor or target mismatch"
        );
        ensure!(
            page.captured_at_us >= 0
                && page.after_sha256 == state.received_sha256.as_deref().unwrap_or(GENESIS)
                && state
                    .target_sha256
                    .as_ref()
                    .is_none_or(|hash| *hash == page.until_sha256),
            "replication checkpoint or target hash mismatch"
        );
        ensure!(
            page.changes.len() <= MAX_PAGE_RECORDS
                && (page.next == page.until || !page.changes.is_empty()),
            "empty or oversized unfinished replication page"
        );
        let mut batch = WriteBatch::default();
        let mut cursor = page.after;
        let mut previous = page.after_sha256.clone();
        for change in &page.changes {
            cursor = cursor
                .checked_add(1)
                .context("replication cursor exhausted")?;
            ensure!(
                change.sequence == cursor && change.previous_sha256 == previous,
                "replication page has a gap or duplicate sequence"
            );
            change.decode()?;
            previous = change.record_sha256.clone();
            batch.put_cf(
                self.db.cf_handle(STAGING).context("replica staging")?,
                change.sequence.to_be_bytes(),
                serde_json::to_vec(change)?,
            );
        }
        ensure!(
            cursor == page.next
                && (cursor > page.after || cursor == page.until)
                && previous == page.next_sha256
                && (cursor < page.until || previous == page.until_sha256),
            "replication page does not advance to advertised cursor"
        );
        state.store_id = Some(page.store_id.clone());
        if state.target_cursor.is_none() {
            state.target_captured_at_us = Some(page.captured_at_us);
        }
        state.target_cursor = Some(page.until);
        state.target_sha256 = Some(page.until_sha256.clone());
        state.received_cursor = page.next;
        state.received_sha256 = Some(page.next_sha256.clone());
        state.last_attempt_at_us = Some(crate::native_futures::now_us());
        state.last_error = None;
        batch.put(STATE, serde_json::to_vec(&state)?);
        write(&self.db, batch)
    }

    pub(super) fn apply(&self) -> Result<()> {
        let mut state = self.status()?;
        let Some(target) = state.target_cursor else {
            return Ok(());
        };
        if state.received_cursor != target {
            return Ok(());
        }
        let _barrier = writer_lock(&self.path)?;
        state.applying = true;
        let mut batch = WriteBatch::default();
        batch.put(STATE, serde_json::to_vec(&state)?);
        write(&self.db, batch)?;
        let mut cursor = state.committed_cursor;
        let mut previous = state
            .committed_sha256
            .clone()
            .unwrap_or_else(|| GENESIS.into());
        let mut batch = WriteBatch::default();
        let mut bytes = 0;
        for item in self.db.iterator_cf(
            self.db.cf_handle(STAGING).context("replica staging")?,
            IteratorMode::Start,
        ) {
            let (key, value) = item?;
            if cursor == target {
                break;
            }
            let change: Change = serde_json::from_slice(&value)?;
            ensure!(
                change.sequence == cursor + 1
                    && key.as_ref() == change.sequence.to_be_bytes()
                    && change.previous_sha256 == previous,
                "replica staging sequence gap"
            );
            let (key, value) = change.decode()?;
            previous = change.record_sha256.clone();
            let cf = self
                .db
                .cf_handle(change.column_family.name())
                .context("native replica column family")?;
            ensure!(
                self.db.get_cf(cf, &key)?.is_none_or(|old| old == value),
                "append-only native record conflict in local replica"
            );
            bytes += key.len() + value.len();
            batch.put_cf(cf, key, value);
            cursor = change.sequence;
            if bytes >= 4 * 1024 * 1024 {
                write(&self.db, batch)?;
                batch = WriteBatch::default();
                bytes = 0;
            }
        }
        ensure!(
            cursor == target && state.target_sha256.as_deref() == Some(previous.as_str()),
            "replica target not fully staged or hash mismatch"
        );
        state.initialized = true;
        state.applying = false;
        state.committed_cursor = target;
        state.committed_sha256 = state.target_sha256.take();
        state.target_cursor = None;
        state.source_captured_at_us = state.target_captured_at_us.take();
        state.last_synced_at_us = Some(crate::native_futures::now_us());
        state.last_error = None;
        batch.put(STATE, serde_json::to_vec(&state)?);
        batch.delete_range_cf(
            self.db.cf_handle(STAGING).unwrap(),
            &b""[..],
            &[255_u8; 9][..],
        );
        write(&self.db, batch)
    }

    async fn run(
        self: &Arc<Self>,
        client: &reqwest::Client,
        token: &str,
        stop: &AtomicBool,
    ) -> Result<ReplicaStatus> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.apply()).await??;
        let mut progress_at = Instant::now();
        loop {
            let state = self.status()?;
            if stop.load(Ordering::Relaxed) {
                return Ok(state);
            }
            let request = Request {
                mode: crate::runtime_mode::RuntimeMode::CnFutures,
                store_id: state.store_id,
                after: state.received_cursor,
                after_sha256: state.received_sha256.unwrap_or_else(|| GENESIS.into()),
                until: state.target_cursor,
            };
            let remote = self.source.sync.as_ref().context("native remote source")?;
            let url = format!(
                "{}/api/native/sync/{}",
                remote.url.trim_end_matches('/'),
                self.source.id
            );
            let mut authorization =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?;
            authorization.set_sensitive(true);
            let mut response = client
                .post(url)
                .header(reqwest::header::AUTHORIZATION, authorization)
                .header(reqwest::header::ACCEPT_ENCODING, "gzip")
                .json(&request)
                .send()
                .await?
                .error_for_status()?;
            let gzip = match response.headers().get(reqwest::header::CONTENT_ENCODING) {
                None => false,
                Some(value) if value == "identity" => false,
                Some(value) if value == "gzip" => true,
                _ => anyhow::bail!("unsupported native sync content encoding"),
            };
            let mut bytes = vec![];
            while let Some(chunk) = response.chunk().await? {
                ensure!(
                    bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES,
                    "native sync response exceeds 16 MiB"
                );
                bytes.extend_from_slice(&chunk);
            }
            let me = self.clone();
            let done = tokio::task::spawn_blocking(move || -> Result<bool> {
                let page = decode_page(bytes, gzip)?;
                let done = page.next == page.until;
                me.accept(&page)?;
                me.apply()?;
                Ok(done)
            })
            .await??;
            if done || progress_at.elapsed() >= Duration::from_secs(15) {
                let progress = self.status()?;
                tracing::info!(source_id=%self.source.id, received=progress.received_cursor,
                    committed=progress.committed_cursor, target=?progress.target_cursor,
                    "native synchronization progress");
                progress_at = Instant::now();
            }
            if done {
                return self.status();
            }
        }
    }

    pub async fn sync_once(
        self: &Arc<Self>,
        client: &reqwest::Client,
        token: &str,
    ) -> Result<ReplicaStatus> {
        self.sync_with_stop(client, token, &AtomicBool::new(false))
            .await
    }
    async fn sync_with_stop(
        self: &Arc<Self>,
        client: &reqwest::Client,
        token: &str,
        stop: &AtomicBool,
    ) -> Result<ReplicaStatus> {
        let _guard = self.poll_lock.lock().await;
        let result = self.run(client, token, stop).await;
        if result.is_err() {
            let me = self.clone();
            let recorded = tokio::task::spawn_blocking(move || -> Result<()> {
                let mut state = me.status()?;
                state.last_attempt_at_us = Some(crate::native_futures::now_us());
                state.last_error = Some("远端同步失败，请检查同步进程日志".into());
                let mut batch = WriteBatch::default();
                batch.put(STATE, serde_json::to_vec(&state)?);
                write(&me.db, batch)
            })
            .await?;
            if let Err(error) = recorded {
                tracing::warn!(error=?error,"failed to persist native sync error");
            }
        }
        result
    }
}

pub async fn pull(config: Config, once: bool) -> Result<()> {
    config.validate()?;
    let settings = config
        .sync
        .as_ref()
        .filter(|s| s.enabled)
        .context("enable [sync] before starting the collector")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(settings.timeout_secs))
        .build()?;
    let mut workers = vec![];
    let mut startup_failed = false;
    for source in config
        .sources
        .iter()
        .filter(|s| s.enabled && s.sync.is_some())
    {
        let prepared = (|| -> Result<_> {
            Ok((
                Arc::new(Replica::open(&config, source)?),
                super::settings::token(&source.sync.as_ref().unwrap().token_env)?,
            ))
        })();
        match prepared {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                startup_failed = true;
                tracing::warn!(source_id=%source.id,error=?error,"native replica unavailable");
            }
        }
    }
    ensure!(
        !workers.is_empty(),
        "no native replica sources are available"
    );
    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = stop.clone();
    let signal = tokio::spawn(async move {
        super::shutdown_signal().await;
        signal_stop.store(true, Ordering::Relaxed);
    });
    let permits = Arc::new(tokio::sync::Semaphore::new(settings.concurrent_sources));
    let mut tasks = tokio::task::JoinSet::new();
    for (replica, token) in workers {
        let client = client.clone();
        let stop = stop.clone();
        let permits = permits.clone();
        let poll_secs = settings.poll_secs;
        tasks.spawn(async move {
            let mut failures=0_u32;
            loop {
                if stop.load(Ordering::Relaxed){return false;}
                let permit=permits.acquire().await.expect("collector permits");
                let result=replica.sync_with_stop(&client,&token,&stop).await;drop(permit);
                let failed=result.is_err();
                match result {
                    Ok(state)=>{failures=0;if once {println!("{}",serde_json::json!({"source_id":replica.source.id,"replication":state}));}},
                    Err(error)=>{failures=failures.saturating_add(1);tracing::warn!(source_id=%replica.source.id,error=?error,"native synchronization failed");},
                }
                if once {return failed;}
                let delay=poll_secs.saturating_mul(1_u64<<failures.min(5)).min(poll_secs.max(300));
                let until=tokio::time::Instant::now()+Duration::from_secs(delay);
                while tokio::time::Instant::now()<until && !stop.load(Ordering::Relaxed){tokio::time::sleep(Duration::from_millis(200)).await;}
            }
        });
    }
    let mut failed = startup_failed;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(source_failed) => failed |= source_failed,
            Err(error) => {
                failed = true;
                tracing::error!(error=?error,"native synchronization task failed");
            }
        }
    }
    signal.abort();
    ensure!(
        !once || !failed,
        "one or more native sources failed to synchronize"
    );
    Ok(())
}
