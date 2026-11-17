//! Reusable source readers. Only the secondary's private directory is writable.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use rocksdb::{BlockBasedOptions, Cache, ColumnFamilyDescriptor, DB, Options};

const MAX_READERS: usize = 32;
const IDLE_RETENTION: Duration = Duration::from_secs(300);

#[derive(Debug, PartialEq, Eq)]
struct SourceIdentity {
    device: u64,
    inode: u64,
    database: Vec<u8>,
    manifest: Vec<u8>,
    options: (String, u64, u64, i64, i64),
}

impl SourceIdentity {
    fn same_database(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode && self.database == other.database
    }

    fn read(path: &Path) -> Result<Self> {
        let metadata = path.metadata()?;
        ensure!(metadata.is_dir(), "source is not a directory");
        let database = std::fs::read(path.join("IDENTITY"))?;
        ensure!(!database.is_empty(), "source identity is empty");
        let manifest = std::fs::read(path.join("CURRENT"))?;
        // A secondary deliberately retains handles to dropped CFs. Names alone
        // cannot detect drop+recreate of the same name. RocksDB publishes a new
        // OPTIONS file for CF changes and primary reopen; treat any such change
        // (or MANIFEST rollover) as a boundary requiring a fresh local reader.
        let mut latest = None;
        for entry in std::fs::read_dir(path)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(number) = name
                .strip_prefix("OPTIONS-")
                .and_then(|n| n.parse::<u64>().ok())
            {
                if latest.as_ref().is_none_or(|(old, _)| number > *old) {
                    latest = Some((number, name.to_owned()));
                }
            }
        }
        let options_name = latest.context("source has no completed OPTIONS file")?.1;
        let options = path.join(&options_name).metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            database,
            manifest,
            options: (
                options_name,
                options.ino(),
                options.len(),
                options.mtime(),
                options.mtime_nsec(),
            ),
        })
    }
}

struct Secondary {
    // Drop RocksDB before its pinned source directory and private scratch.
    db: DB,
    _primary_directory: File,
    _scratch: tempfile::TempDir,
    identity: SourceIdentity,
    families: BTreeSet<String>,
}

fn pinned_path(directory: &File) -> PathBuf {
    // rocksdb-rs calls create_dir_all even for secondary opens. Holding the
    // original directory prevents a concurrent source rename/removal from
    // causing that wrapper to recreate a missing live source pathname.
    PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
}

type ReaderSlot = Arc<Mutex<Option<Secondary>>>;

struct Entry {
    reader: ReaderSlot,
    used: Instant,
}

#[derive(Default)]
struct Registry {
    root: Option<PathBuf>,
    entries: BTreeMap<PathBuf, Entry>,
    closed: bool,
}

#[derive(Default)]
struct ReaderPool {
    registry: Mutex<Registry>,
    #[cfg(test)]
    opens: std::sync::atomic::AtomicUsize,
}

static READERS: OnceLock<ReaderPool> = OnceLock::new();

fn readers() -> &'static ReaderPool {
    READERS.get_or_init(ReaderPool::default)
}

/// Owns the web process's configured reader lifetime, including error exits.
pub struct ReaderSession;

impl Drop for ReaderSession {
    fn drop(&mut self) {
        let entries = {
            let mut registry = readers().registry.lock().unwrap_or_else(|e| e.into_inner());
            registry.closed = true;
            std::mem::take(&mut registry.entries)
        };
        drop(entries);
    }
}

/// Configure scratch before starting analysis; no source DB is opened here.
pub fn configure(root: &Path, protected: &[&Path]) -> Result<ReaderSession> {
    let root = crate::config::resolve_storage_path(root)?;
    for path in protected {
        let path = crate::config::resolve_storage_path(path)?;
        ensure!(
            !root.starts_with(&path) && !path.starts_with(&root),
            "source reader scratch overlaps a live database"
        );
    }
    let mut registry = readers().registry.lock().unwrap_or_else(|e| e.into_inner());
    ensure!(
        registry.entries.is_empty(),
        "configure source readers before use"
    );
    registry.root = Some(root);
    registry.closed = false;
    Ok(ReaderSession)
}

impl ReaderPool {
    fn acquire(&self, primary: &Path) -> Result<(ReaderSlot, PathBuf)> {
        let now = Instant::now();
        let mut retired = Vec::new();
        let (reader, root) = {
            let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            ensure!(!registry.closed, "source readers are shutting down");
            registry.entries.retain(|path, entry| {
                let keep = path == primary
                    || now.duration_since(entry.used) < IDLE_RETENTION
                    || Arc::strong_count(&entry.reader) > 1;
                if !keep {
                    retired.push(entry.reader.clone());
                }
                keep
            });
            if !registry.entries.contains_key(primary) {
                if registry.entries.len() >= MAX_READERS {
                    let oldest = registry
                        .entries
                        .iter()
                        .filter(|(_, e)| Arc::strong_count(&e.reader) == 1)
                        .min_by_key(|(_, e)| e.used)
                        .map(|(path, _)| path.clone())
                        .context("all source readers are busy")?;
                    retired.push(registry.entries.remove(&oldest).unwrap().reader);
                }
                registry.entries.insert(
                    primary.to_owned(),
                    Entry {
                        reader: Arc::new(Mutex::new(None)),
                        used: now,
                    },
                );
            }
            let entry = registry.entries.get_mut(primary).unwrap();
            entry.used = now;
            let reader = entry.reader.clone();
            let root = registry.root.clone().unwrap_or_else(std::env::temp_dir);
            (reader, root)
        };
        // Closing another source must not hold the process-wide registry lock.
        drop(retired);
        Ok((reader, root))
    }

    fn open(
        &self,
        primary: &Path,
        directory: File,
        root: &Path,
        identity: SourceIdentity,
        families: BTreeSet<String>,
    ) -> Result<Secondary> {
        let root = crate::config::resolve_storage_path(root)?;
        ensure!(
            !root.starts_with(primary),
            "secondary scratch is inside source"
        );
        std::fs::create_dir_all(&root)?;
        let scratch = tempfile::Builder::new()
            .prefix(&format!("cta-source-reader-{}-", std::process::id()))
            .tempdir_in(root)?;
        let scratch_path = scratch.path().canonicalize()?;
        ensure!(
            !scratch_path.starts_with(primary) && !primary.starts_with(&scratch_path),
            "secondary scratch overlaps source"
        );
        let mut options = Options::default();
        options.create_if_missing(false);
        options.create_missing_column_families(false);
        // Pin the selected CFs' SSTs while the live primary flushes/compacts.
        // A bounded file cache could reopen a file already deleted by primary.
        options.set_max_open_files(-1);
        options.set_max_file_opening_threads(1);
        options.set_max_log_file_size(1024 * 1024);
        options.set_keep_log_file_num(2);
        let cache = Cache::new_lru_cache(8 * 1024 * 1024);
        let descriptors = families.iter().map(|name| {
            let mut cf = Options::default();
            let mut table = BlockBasedOptions::default();
            table.set_block_cache(&cache);
            cf.set_block_based_table_factory(&table);
            ColumnFamilyDescriptor::new(name, cf)
        });
        let started = Instant::now();
        let db = DB::open_cf_descriptors_as_secondary(
            &options,
            &pinned_path(&directory),
            &scratch_path,
            descriptors,
        )
        .with_context(|| format!("failed to open source secondary {}", primary.display()))?;
        ensure!(
            SourceIdentity::read(primary)? == identity,
            "source changed while opening secondary"
        );
        #[cfg(test)]
        self.opens
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            source_path = %primary.display(),
            column_families = ?families,
            duration_ms = started.elapsed().as_millis(),
            "opened reusable source secondary"
        );
        Ok(Secondary {
            db,
            _primary_directory: directory,
            _scratch: scratch,
            identity,
            families,
        })
    }

    fn read<T>(
        &self,
        path: &Path,
        requested: &[String],
        scan: impl FnOnce(&DB) -> Result<T>,
    ) -> Result<T> {
        let primary = path
            .canonicalize()
            .with_context(|| format!("source path is unavailable: {}", path.display()))?;
        let (entry, root) = self.acquire(&primary)?;
        // Catch-up and every CF iterator belong to one serialized view. Secondary
        // mode has no RocksDB snapshots, so another reader must not catch up
        // while this callback is scanning its frozen multi-CF generation.
        let mut slot = entry.lock().unwrap_or_else(|e| e.into_inner());
        let identity = SourceIdentity::read(&primary)?;
        let previous_families = slot
            .as_ref()
            .filter(|s| s.identity.same_database(&identity))
            .map(|s| s.families.clone())
            .unwrap_or_default();
        if slot.as_ref().is_some_and(|s| s.identity != identity) {
            *slot = None;
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(&primary)?;
        let pinned = pinned_path(&directory);
        ensure!(
            SourceIdentity::read(&pinned)? == identity,
            "source changed before read"
        );
        let available: BTreeSet<_> = DB::list_cf(&Options::default(), &pinned)?
            .into_iter()
            .collect();
        let mut selected: BTreeSet<String> = previous_families
            .intersection(&available)
            .cloned()
            .collect();
        selected.insert("default".into());
        selected.extend(requested.iter().filter(|n| available.contains(*n)).cloned());
        let reopen = slot.as_ref().is_none_or(|s| s.families != selected);
        if reopen {
            // Release obsolete SST pins before acquiring a replacement.
            *slot = None;
            *slot = Some(self.open(&primary, directory, &root, identity, selected)?);
        } else if let Err(error) = slot.as_ref().unwrap().db.try_catch_up_with_primary() {
            *slot = None;
            return Err(error).context("source secondary catch-up failed");
        }
        let reader = slot.as_ref().unwrap();
        if SourceIdentity::read(&primary)? != reader.identity {
            *slot = None;
            anyhow::bail!("source changed during secondary catch-up");
        }
        let result = scan(&reader.db);
        let unchanged = SourceIdentity::read(&primary)
            .is_ok_and(|current| current == reader.identity)
            && path.canonicalize().is_ok_and(|current| current == primary);
        let storage_error = result
            .as_ref()
            .err()
            .is_some_and(|error| error.chain().any(|cause| cause.is::<rocksdb::Error>()));
        if !unchanged || storage_error {
            *slot = None;
        }
        ensure!(unchanged, "source changed during secondary scan");
        result
    }
}

pub(super) fn read<T>(
    path: &Path,
    requested: &[String],
    scan: impl FnOnce(&DB) -> Result<T>,
) -> Result<T> {
    readers().read(path, requested, scan)
}

#[cfg(test)]
mod tests;
