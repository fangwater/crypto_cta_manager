//! File transfer for an immutable, confirmed cold segment. No remote DB mount.
use super::*;
use std::io::{BufReader, BufWriter};

const MAGIC: &[u8; 16] = b"CTA-COLD-ROCKS\0\0";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleFile {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bundle {
    pub segment: Segment,
    pub files: Vec<BundleFile>,
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "bundle member must be a regular file"
    );
    let mut input = BufReader::new(File::open(path)?);
    let mut h = Sha256::new();
    let mut count = 0;
    let mut buf = [0u8; 65536];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        count += n as u64;
    }
    Ok((count, hex::encode(h.finalize())))
}
fn include_file(name: &str) -> bool {
    name != "LOCK" && name != "LOG" && !name.starts_with("LOG.old")
}

pub fn export(root: &ColdRoot, id: &str, output: &mut impl Write, now_us: i64) -> Result<Bundle> {
    let path = root.segment_path(id)?;
    let _lease = lock(&root.path.join(format!(".lease-{id}")), true)?;
    let s = verify_segment(&path)?;
    ensure!(
        s.source_identity == root.identity
            && s.last < retention::cutoff(now_us)
            && path.join(READY).is_file(),
        "cold segment is unconfirmed or within retained 33 days"
    );
    let mut files = Vec::new();
    for e in std::fs::read_dir(&path)? {
        let e = e?;
        let name = e
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-UTF8 cold filename"))?;
        if !include_file(&name) {
            continue;
        }
        leaf(&name)?;
        ensure!(
            e.file_type()?.is_file() && !e.file_type()?.is_symlink(),
            "unsafe bundle member"
        );
        let (bytes, sha256) = hash_file(&e.path())?;
        files.push(BundleFile {
            name,
            bytes,
            sha256,
        });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    let bundle = Bundle { segment: s, files };
    let header = serde_json::to_vec(&bundle)?;
    ensure!(
        header.len() as u64 <= MAX_MANIFEST_BYTES,
        "bundle manifest exceeds limit"
    );
    output.write_all(MAGIC)?;
    output.write_all(&(header.len() as u64).to_be_bytes())?;
    output.write_all(&header)?;
    for f in &bundle.files {
        let mut input = BufReader::new(File::open(path.join(&f.name))?);
        let copied = std::io::copy(&mut input, output)?;
        ensure!(copied == f.bytes, "source cold file changed during export");
    }
    output.flush()?;
    Ok(bundle)
}

/// Decode one bounded bundle to a NEW directory, verifying bytes before DB use.
pub fn receive(
    input: &mut impl Read,
    destination: &Path,
    expected: &Segment,
    max_bytes: u64,
) -> Result<Bundle> {
    ensure!(!destination.exists(), "bundle destination already exists");
    let mut magic = [0; 16];
    input.read_exact(&mut magic)?;
    ensure!(&magic == MAGIC, "invalid cold bundle magic");
    let mut n = [0; 8];
    input.read_exact(&mut n)?;
    let length = u64::from_be_bytes(n);
    ensure!(
        length <= MAX_MANIFEST_BYTES && length <= max_bytes,
        "bundle manifest exceeds limit"
    );
    let mut header = vec![0; length as usize];
    input.read_exact(&mut header)?;
    let bundle: Bundle = serde_json::from_slice(&header)?;
    ensure!(
        bundle.segment == *expected,
        "bundle does not match selected cold segment"
    );
    validate_segment(&bundle.segment)?;
    ensure!(bundle.files.len() <= 100_000, "too many bundle files");
    let mut total = length + 24;
    let mut names = BTreeSet::new();
    for f in &bundle.files {
        leaf(&f.name)?;
        ensure!(
            include_file(&f.name) && names.insert(f.name.clone()),
            "forbidden/duplicate bundle member"
        );
        ensure!(
            f.sha256.len() == 64 && f.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid file checksum"
        );
        total = total.checked_add(f.bytes).context("bundle size overflow")?;
        ensure!(total <= max_bytes, "cold bundle exceeds byte limit");
    }
    ensure!(
        names.contains(SEGMENT_MARKER)
            && names.contains(READY)
            && names.contains("CURRENT")
            && names.contains("IDENTITY"),
        "incomplete cold DB bundle"
    );
    private_dir(destination)?;
    let mut buf = [0u8; 65536];
    for f in &bundle.files {
        let mut output = BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination.join(&f.name))?,
        );
        let mut remaining = f.bytes;
        let mut h = Sha256::new();
        while remaining > 0 {
            let size = usize::try_from(remaining.min(buf.len() as u64))?;
            input.read_exact(&mut buf[..size])?;
            output.write_all(&buf[..size])?;
            h.update(&buf[..size]);
            remaining -= size as u64;
        }
        ensure!(
            hex::encode(h.finalize()) == f.sha256,
            "cold bundle file checksum mismatch"
        );
        output.flush()?;
        output.get_ref().sync_all()?;
    }
    let mut extra = [0; 1];
    ensure!(input.read(&mut extra)? == 0, "trailing cold bundle bytes");
    sync_dir(destination)?;
    ensure!(
        verify_segment(destination)? == *expected,
        "restored cold DB differs"
    );
    Ok(bundle)
}

use std::collections::BTreeSet;
