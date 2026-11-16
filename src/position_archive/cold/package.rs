//! Durable source-side daily files. Network retries never regenerate RocksDB exports.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Package {
    pub segment: Segment,
    pub archive_path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub bundle: wire::Bundle,
}
pub fn day(s: &Segment) -> Result<String> {
    let time =
        chrono::DateTime::from_timestamp_micros(s.first.0).context("cold date out of range")?;
    Ok(time
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .format("%Y-%m-%d")
        .to_string())
}
pub fn directory(root: &Path, s: &Segment) -> Result<PathBuf> {
    validate_segment(s)?;
    Ok(root.join(".packages").join(day(s)?).join(&s.id))
}
pub(crate) fn hash(path: &Path) -> Result<(u64, String)> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "archive must be a regular file"
    );
    let mut input = File::open(path)?;
    let mut h = Sha256::new();
    let mut total = 0;
    let mut buffer = [0; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        h.update(&buffer[..count]);
        total += count as u64;
    }
    Ok((total, hex::encode(h.finalize())))
}
pub fn prepare(root: &ColdRoot, id: &str, now_us: i64, max_bytes: u64) -> Result<Package> {
    ensure!(
        (4096..=2 * 1024 * 1024 * 1024).contains(&max_bytes),
        "invalid package byte limit"
    );
    let source = root.segment_path(id)?;
    let s: Segment = json_file(&source.join(SEGMENT_MARKER))?;
    validate_segment(&s)?;
    ensure!(
        s.id == id
            && s.source_identity == root.identity
            && s.last < retention::cutoff(now_us)
            && source.join(READY).is_file(),
        "source segment is foreign/unconfirmed/retained"
    );
    let _lease = lock(&root.path.join(format!(".lease-{id}")), true)?;
    let _packaging = lock(&root.path.join(format!(".package-{id}.lock")), false)?;
    let final_dir = directory(&root.path, &s)?;
    let archive_path = final_dir.join("original.rocks.gz");
    if final_dir.exists() {
        let previous: Package = json_file(&final_dir.join("package.json"))?;
        ensure!(
            previous.segment == s
                && previous.archive_path == archive_path
                && previous.bundle.segment == s
                && previous.bytes <= max_bytes
                && hash(&archive_path)? == (previous.bytes, previous.sha256.clone()),
            "cached source package differs; originals retained"
        );
        return Ok(previous);
    }
    let daily = final_dir.parent().context("daily package parent missing")?;
    private_dir(&root.path.join(".packages"))?;
    private_dir(daily)?;
    let stage = tempfile::Builder::new()
        .prefix(".packaging-")
        .tempdir_in(daily)?;
    let path = stage.path().join("original.rocks.gz");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut gzip = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let bundle = wire::export(root, id, &mut gzip, now_us)?;
    let raw_bytes = serde_json::to_vec(&bundle)?.len() as u64
        + 24
        + bundle.files.iter().map(|f| f.bytes).sum::<u64>();
    ensure!(
        raw_bytes <= max_bytes,
        "source package exceeds raw byte limit; originals retained"
    );
    let file = gzip.finish()?;
    file.sync_all()?;
    let (bytes, sha256) = hash(&path)?;
    ensure!(
        bytes <= max_bytes,
        "compressed source package exceeds byte limit"
    );
    let result = Package {
        segment: s,
        archive_path,
        bytes,
        sha256,
        bundle,
    };
    durable_json(&stage.path().join("package.json"), &result)?;
    sync_dir(stage.path())?;
    std::fs::rename(stage.path(), &final_dir)?;
    sync_dir(daily)?;
    Ok(result)
}
// Called only after exact receipt verification and durable cold retirement.
pub(super) fn remove(root: &ColdRoot, s: &Segment) -> Result<()> {
    let path = directory(&root.path, s)?;
    if path.exists() {
        for parent in [
            root.path.join(".packages"),
            path.parent().unwrap().to_owned(),
        ] {
            let metadata = std::fs::symlink_metadata(parent)?;
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "unsafe package parent"
            );
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "unsafe retired package path"
        );
        std::fs::remove_dir_all(&path)?;
        sync_dir(path.parent().unwrap())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_daily_package_is_reused_restorable_and_removed_only_with_verified_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = ColdRoot::create(&temp.path().join("cold"), "source").unwrap();
        let (rows, before, after) = super::super::tests::records(10 * 86_400_000_000, 3);
        let s = root
            .write_batch(&rows, &before, &after, BatchLimits::default())
            .unwrap();
        let now = 100 * 86_400_000_000;
        assert!(prepare(&root, &s.id, now, 16 * 1024 * 1024).is_err());
        root.confirm(&s).unwrap();
        let p = prepare(&root, &s.id, now, 16 * 1024 * 1024).unwrap();
        assert!(
            p.archive_path
                .to_str()
                .unwrap()
                .contains("/.packages/1970-01-11/")
        );
        let modified = p.archive_path.metadata().unwrap().modified().unwrap();
        assert_eq!(prepare(&root, &s.id, now, 16 * 1024 * 1024).unwrap(), p);
        assert_eq!(
            p.archive_path.metadata().unwrap().modified().unwrap(),
            modified
        );
        let mut input = flate2::read::GzDecoder::new(File::open(&p.archive_path).unwrap());
        assert_eq!(
            wire::receive(
                &mut input,
                &temp.path().join("restore"),
                &s,
                16 * 1024 * 1024
            )
            .unwrap(),
            p.bundle
        );
        let mut bad = s.clone();
        bad.count += 1;
        assert!(root.delete_verified(&bad, now).is_err());
        assert!(p.archive_path.is_file());
        root.delete_verified(&s, now).unwrap();
        assert!(!p.archive_path.exists());
        root.delete_verified(&s, now).unwrap();
    }
    #[test]
    fn corrupted_source_package_cannot_be_reused_or_authorize_cold_removal() {
        let temp = tempfile::tempdir().unwrap();
        let root = ColdRoot::create(&temp.path().join("cold"), "source").unwrap();
        let (rows, before, after) = super::super::tests::records(10 * 86_400_000_000, 3);
        let s = root
            .write_batch(&rows, &before, &after, BatchLimits::default())
            .unwrap();
        root.confirm(&s).unwrap();
        let p = prepare(&root, &s.id, 100 * 86_400_000_000, 16 * 1024 * 1024).unwrap();
        std::fs::write(&p.archive_path, b"truncated SCP source").unwrap();
        assert!(prepare(&root, &s.id, 100 * 86_400_000_000, 16 * 1024 * 1024).is_err());
        assert_eq!(verify_segment(&root.path.join(&s.id)).unwrap(), s);
    }
}
