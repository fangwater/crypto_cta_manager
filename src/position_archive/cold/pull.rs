//! Powerleader pull coordinator. The production transport is direct public SSH.
use super::*;
use std::net::Ipv4Addr;
use std::process::{Command, Stdio};
use std::time::Duration;
mod process;
use process::Operation;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullConfig {
    pub backup_root: PathBuf,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_max_bytes")]
    pub max_bundle_bytes: u64,
    #[serde(default = "default_timeout")]
    pub operation_timeout_secs: u64,
    #[serde(default = "default_transfer_timeout")]
    pub transfer_timeout_secs: u64,
    pub hosts: Vec<PullHost>,
}
fn default_interval() -> u64 {
    3600
}
fn default_timeout() -> u64 {
    300
}
fn default_transfer_timeout() -> u64 {
    3600
}
fn default_max_bytes() -> u64 {
    512 * 1024 * 1024
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullHost {
    pub name: String,
    /// Host-only environment setting; numeric public address, never an alias.
    pub public_ip_env: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub identity_file: Option<PathBuf>,
    /// Optional pinned known-hosts file, populated from an already trusted connection.
    pub known_hosts_file: Option<PathBuf>,
    pub source_binary: PathBuf,
    pub cold_root: PathBuf,
    pub source_identity: String,
}
fn default_port() -> u16 {
    22
}

pub fn validate_public_ipv4(value: &str) -> Result<Ipv4Addr> {
    let ip: Ipv4Addr = value
        .parse()
        .context("pull public endpoint must be a numeric IPv4 address")?;
    let o = ip.octets();
    ensure!(
        !ip.is_private()
            && !ip.is_loopback()
            && !ip.is_link_local()
            && !ip.is_unspecified()
            && !ip.is_broadcast()
            && !ip.is_multicast()
            && !ip.is_documentation()
            && o[0] != 0
            && o[0] < 224
            && !(o[0] == 100 && (64..=127).contains(&o[1]))
            && !(o[0] == 198 && (18..=19).contains(&o[1]))
            && !(o[0] == 192 && o[1] == 0 && o[2] == 0),
        "pull requires public egress; private/reserved endpoints are forbidden"
    );
    Ok(ip)
}
impl PullConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let s: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        s.validate()?;
        Ok(s)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.backup_root.is_absolute()
                && (3600..=31 * 86400).contains(&self.interval_secs)
                && (30..=3600).contains(&self.operation_timeout_secs)
                && (30..=86400).contains(&self.transfer_timeout_secs)
                && (4096..=2 * 1024 * 1024 * 1024).contains(&self.max_bundle_bytes),
            "invalid pull path/hourly interval/bundle limit"
        );
        let mut names = std::collections::BTreeSet::new();
        for h in &self.hosts {
            leaf(&h.name)?;
            ensure!(
                names.insert(&h.name)
                    && h.source_binary.is_absolute()
                    && h.cold_root.is_absolute()
                    && !h.source_identity.is_empty()
                    && h.port > 0,
                "invalid/duplicate pull host"
            );
            ensure!(
                !h.user.is_empty()
                    && h.user
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
                "invalid SSH user"
            );
            ensure!(
                !h.public_ip_env.is_empty()
                    && h.public_ip_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "invalid public endpoint environment name"
            );
            if let Some(p) = &h.identity_file {
                ensure!(p.is_absolute(), "identity_file must be absolute");
            }
            if let Some(p) = &h.known_hosts_file {
                ensure!(p.is_absolute(), "known_hosts_file must be absolute");
            }
        }
        Ok(())
    }
    pub fn host(&self, name: &str) -> Result<&PullHost> {
        self.hosts
            .iter()
            .find(|h| h.name == name)
            .context("unknown pull host")
    }
}
pub fn shell_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}
impl PullHost {
    fn transport_command(&self, program: &str) -> Result<(Command, Ipv4Addr)> {
        let value = std::env::var(&self.public_ip_env)
            .context("required public endpoint environment is unset")?;
        let ip = validate_public_ipv4(&value)?;
        let mut c = Command::new(program);
        c.args([
            "-F",
            "/dev/null",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "ProxyJump=none",
            "-o",
            "ProxyCommand=none",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "LogLevel=ERROR",
            "-o",
            "IPQoS=none",
        ]);
        c.arg(if program == "scp" { "-P" } else { "-p" })
            .arg(self.port.to_string());
        if let Some(key) = &self.identity_file {
            c.arg("-i").arg(key);
        }
        if let Some(path) = &self.known_hosts_file {
            c.arg("-o")
                .arg(format!("UserKnownHostsFile={}", path.display()));
            c.args(["-o", "GlobalKnownHostsFile=/dev/null"]);
        }
        Ok((c, ip))
    }
    pub fn ssh_command(&self, args: &[&str]) -> Result<Command> {
        let (mut c, ip) = self.transport_command("ssh")?;
        c.arg("-l").arg(&self.user).arg(ip.to_string());
        let mut remote = vec![shell_quote(
            self.source_binary
                .to_str()
                .context("non-UTF8 binary path")?,
        )];
        remote.extend(args.iter().map(|s| shell_quote(s)));
        c.arg(remote.join(" "));
        Ok(c)
    }
    fn scp_command(&self, remote: &Path, local: &Path) -> Result<Command> {
        let (mut command, ip) = self.transport_command("scp")?;
        // Force SFTP transport: remote paths are data, never shell programs.
        command
            .args(["-q", "-s"])
            .arg(format!(
                "{}@{}:{}",
                self.user,
                ip,
                remote.to_str().context("non-UTF8 package path")?
            ))
            .arg(local);
        Ok(command)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PullReceipt {
    segment: Segment,
    archive_sha256: String,
    bundle: wire::Bundle,
}
#[derive(Serialize, Deserialize)]
struct Attempt {
    at_us: i64,
}
#[derive(Clone, Debug, Serialize)]
pub struct PullResult {
    pub host: String,
    pub read_only: bool,
    pub eligible_segments: usize,
    pub segment: Option<String>,
    pub backed_up: bool,
    pub source_removed: bool,
    pub deferred: bool,
}

fn remote_json<T: serde::de::DeserializeOwned>(
    host: &PullHost,
    args: &[&str],
    timeout: Duration,
) -> Result<T> {
    let mut command = host.ssh_command(args)?;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let operation = Operation::spawn(&mut command, timeout)?;
    let mut stdout = Vec::new();
    operation
        .stdout()?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut stdout)?;
    ensure!(
        stdout.len() as u64 <= MAX_MANIFEST_BYTES,
        "source inventory exceeds limit"
    );
    ensure!(
        operation.finish()?.success(),
        "source {} failed or timed out (no private/proxy fallback)",
        args.first().copied().unwrap_or("operation")
    );
    Ok(serde_json::from_slice(&stdout)?)
}
fn list(host: &PullHost, timeout: Duration, after: Option<&str>) -> Result<Vec<Segment>> {
    let root = host.cold_root.to_str().context("non-UTF8 cold root")?;
    let mut args = vec!["cold-list", "--root", root, "--limit", "100"];
    if let Some(after) = after {
        args.extend(["--after", after]);
    }
    let segments: Vec<Segment> = remote_json(host, &args, timeout)?;
    let mut previous = None;
    for s in &segments {
        validate_segment(s)?;
        ensure!(
            s.source_identity == host.source_identity && previous.is_none_or(|p| p < s.first),
            "foreign/unordered source cold inventory"
        );
        previous = Some(s.last);
    }
    Ok(segments)
}
fn archive_hash(path: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let mut input = File::open(path)?;
    let mut b = [0; 65536];
    loop {
        let n = input.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn verify_compressed_archive(path: &Path, receipt: &PullReceipt, max_bytes: u64) -> Result<()> {
    ensure!(
        archive_hash(path)? == receipt.archive_sha256,
        "compressed backup checksum mismatch"
    );
    let parent = path.parent().context("backup parent missing")?;
    let restored = tempfile::Builder::new()
        .prefix(".verify-gzip-")
        .tempdir_in(parent)?;
    let mut input = flate2::read::GzDecoder::new(std::io::BufReader::new(File::open(path)?));
    let bundle = wire::receive(
        &mut input,
        &restored.path().join("db"),
        &receipt.segment,
        max_bytes,
    )?;
    ensure!(
        bundle == receipt.bundle,
        "independent gzip restore differs from captured bundle"
    );
    Ok(())
}
/// One oldest eligible segment per attempt; a daemon's cadence is never below an hour.
pub fn once(
    config: &PullConfig,
    host: &PullHost,
    run: bool,
    reclaim: bool,
    now_us: i64,
) -> Result<PullResult> {
    config.validate()?;
    ensure!(
        !reclaim || run,
        "source reclamation requires an explicit run"
    );
    let mut result = PullResult {
        host: host.name.clone(),
        read_only: !run,
        eligible_segments: 0,
        segment: None,
        backed_up: false,
        source_removed: false,
        deferred: false,
    };
    let timeout = Duration::from_secs(config.operation_timeout_secs);
    let directory = config.backup_root.join(&host.name);
    let backup_cursor = directory.join("last-backed-up.json");
    let after = if run && !reclaim && backup_cursor.exists() {
        let s: Segment = json_file(&backup_cursor)?;
        validate_segment(&s)?;
        ensure!(
            s.source_identity == host.source_identity,
            "foreign backup cursor"
        );
        Some(s.id)
    } else {
        None
    };
    let segments = list(host, timeout, after.as_deref())?;
    result.eligible_segments = segments.len();
    if !run {
        result.segment = segments.first().map(|s| s.id.clone());
        return Ok(result);
    }
    private_dir(&config.backup_root)?;
    private_dir(&directory)?;
    let _job = lock(&directory.join(".pull.lock"), false)?;
    let identity_file = directory.join("identity.json");
    if identity_file.exists() {
        ensure!(
            json_file::<String>(&identity_file)? == host.source_identity,
            "local backup identity changed"
        );
    } else {
        durable_json(&identity_file, &host.source_identity)?;
    }
    let mut candidates = segments;
    if reclaim {
        // Recover a successful source deletion whose acknowledgement was lost.
        for e in std::fs::read_dir(&directory)? {
            let e = e?;
            if !e.file_type()?.is_dir() || e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if e.path().join("receipt.json").is_file()
                && !e.path().join("source-removed.json").exists()
            {
                let previous: PullReceipt = json_file(&e.path().join("receipt.json"))?;
                ensure!(
                    previous.segment.source_identity == host.source_identity,
                    "foreign pending backup"
                );
                if !candidates.iter().any(|s| s.id == previous.segment.id) {
                    candidates.push(previous.segment);
                }
            }
        }
    }
    candidates.sort_by_key(|s| s.first);
    if !reclaim {
        // Backup-only mode advances through inventory instead of repulling the
        // same oldest segment forever. Reclamation always retries oldest first.
        candidates.retain(|s| !directory.join(&s.id).join("receipt.json").exists());
    }
    let Some(s) = candidates.first() else {
        return Ok(result);
    };
    ensure!(
        s.last < retention::cutoff(now_us),
        "source inventory exceeds local 33-day retention boundary"
    );
    result.segment = Some(s.id.clone());
    let attempt = directory.join("last-attempt.json");
    if attempt.exists() {
        let old: Attempt = json_file(&attempt)?;
        if now_us.saturating_sub(old.at_us) < (config.interval_secs * 1_000_000) as i64 {
            result.deferred = true;
            return Ok(result);
        }
    }
    durable_json(&attempt, &Attempt { at_us: now_us })?;
    let final_dir = directory.join(&s.id);
    let receipt_path = final_dir.join("receipt.json");
    let gzip_path = final_dir.join("original.rocks.gz");
    let receipt = if receipt_path.exists() {
        let r: PullReceipt = json_file(&receipt_path)?;
        ensure!(
            r.segment == *s
                && archive_hash(&gzip_path)? == r.archive_sha256
                && verify_segment(&final_dir.join("db"))? == *s,
            "saved cold backup verification failed"
        );
        r
    } else {
        ensure!(
            !final_dir.exists() && !gzip_path.exists(),
            "incomplete prior pull requires recovery; source retained"
        );
        let stage = tempfile::Builder::new()
            .prefix(".receiving-")
            .tempdir_in(&directory)?;
        let limit = config.max_bundle_bytes.to_string();
        let package: package::Package = remote_json(
            host,
            &[
                "cold-package",
                "--root",
                host.cold_root.to_str().unwrap(),
                "--id",
                &s.id,
                "--max-bytes",
                &limit,
            ],
            timeout,
        )?;
        let expected = package::directory(&host.cold_root, s)?.join("original.rocks.gz");
        ensure!(
            package.segment == *s
                && package.bundle.segment == *s
                && package.archive_path == expected
                && package.bytes <= config.max_bundle_bytes
                && package.sha256.len() == 64,
            "source package does not match selected segment"
        );
        let incoming = directory.join(".downloads");
        private_dir(&incoming)?;
        let partial = incoming.join(format!("{}.rocks.gz.part", s.id));
        if partial.exists() {
            ensure!(
                std::fs::symlink_metadata(&partial)?.is_file(),
                "unsafe SCP partial path"
            );
        }
        if !partial.exists() || package::hash(&partial)? != (package.bytes, package.sha256.clone())
        {
            let mut command = host.scp_command(&package.archive_path, &partial)?;
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let operation = Operation::spawn(
                &mut command,
                Duration::from_secs(config.transfer_timeout_secs),
            )?;
            ensure!(
                operation.finish()?.success(),
                "SCP failed or timed out; source package retained for file retry"
            );
        }
        ensure!(
            package::hash(&partial)? == (package.bytes, package.sha256.clone()),
            "SCP file length/checksum mismatch; source retained"
        );
        File::open(&partial)?.sync_all()?;
        let mut input =
            flate2::read::GzDecoder::new(std::io::BufReader::new(File::open(&partial)?));
        let bundle = wire::receive(
            &mut input,
            &stage.path().join("db"),
            s,
            config.max_bundle_bytes,
        )?;
        ensure!(
            bundle == package.bundle,
            "downloaded package differs from source manifest"
        );
        let r = PullReceipt {
            segment: s.clone(),
            archive_sha256: package.sha256,
            bundle,
        };
        std::fs::copy(&partial, stage.path().join("original.rocks.gz"))?;
        File::open(stage.path().join("original.rocks.gz"))?.sync_all()?;
        verify_compressed_archive(
            &stage.path().join("original.rocks.gz"),
            &r,
            config.max_bundle_bytes,
        )?;
        durable_json(&stage.path().join("receipt.json"), &r)?;
        sync_dir(stage.path())?;
        std::fs::rename(stage.path(), &final_dir)?;
        sync_dir(&directory)?;
        std::fs::remove_file(&partial)?;
        sync_dir(&incoming)?;
        r
    };
    result.backed_up = true;
    if reclaim {
        // Reverify both independent representations immediately before asking for deletion.
        verify_compressed_archive(&gzip_path, &receipt, config.max_bundle_bytes)?;
        ensure!(
            archive_hash(&gzip_path)? == receipt.archive_sha256
                && verify_segment(&final_dir.join("db"))? == *s,
            "backup changed before source removal"
        );
        let mut command = host.ssh_command(&[
            "cold-delete",
            "--root",
            host.cold_root.to_str().unwrap(),
            "--receipt",
            "-",
        ])?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let operation = Operation::spawn(&mut command, timeout)?;
        {
            let mut input = operation.stdin()?;
            serde_json::to_writer(&mut input, s)?;
        }
        ensure!(
            operation.finish()?.success(),
            "source deletion not confirmed; retry later with verified backup"
        );
        durable_json(&final_dir.join("source-removed.json"), s)?;
        result.source_removed = true;
    } else {
        durable_json(&backup_cursor, s)?;
    }
    Ok(result)
}

pub async fn watch(config: PullConfig, host_name: String, reclaim: bool) -> Result<()> {
    loop {
        let c = config.clone();
        let h = c.host(&host_name)?.clone();
        match tokio::task::spawn_blocking(move || {
            once(&c, &h, true, reclaim, crate::kline::now_us())
        })
        .await?
        {
            Ok(result) => println!("{}", serde_json::to_string(&result)?),
            Err(error) => {
                eprintln!("cold pull host={host_name} failed: {error:#}")
            }
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_secs(config.interval_secs))=>{},_=tokio::signal::ctrl_c()=>return Ok(())}
    }
}
