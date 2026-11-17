use super::*;
#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Status,
    Export { before_us: i64 },
    Commit { receipt: Receipt },
    Compact { before_us: i64 },
}
#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default)]
    pub empty: bool,
    pub error: Option<String>,
    pub status: Option<Status>,
}
impl Reply {
    fn success(status: Option<Status>) -> Self {
        Self {
            ok: true,
            empty: false,
            error: None,
            status,
        }
    }
}
fn reply(writer: &mut impl Write, value: &Reply) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Bind before accepting HTTP traffic. Existing active sockets and non-sockets
/// are never replaced. The private parent prevents another user substituting it.
pub fn start_socket(
    archive: Arc<PositionArchive>,
    path: &Path,
    can_seal: impl Fn(Cursor) -> Result<()> + Send + 'static,
) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    ensure!(path.is_absolute(), "archive.socket must be absolute");
    let parent = path.parent().context("archive socket has no parent")?;
    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let metadata = std::fs::symlink_metadata(parent)?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.mode() & 0o077 == 0
            && metadata.uid() == std::fs::metadata(archive.db.path())?.uid(),
        "archive socket parent must be a private Manager-owned directory (0700)"
    );
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_socket()
                    && metadata.uid() == std::fs::metadata(parent)?.uid(),
                "refusing to replace non-socket/foreign archive control path"
            );
            match UnixStream::connect(path) {
                Ok(_) => bail!("archive control socket already active"),
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(path)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::Builder::new()
        .name("signal-archive".into())
        .spawn(move || {
            if let Ok(thread) = std::fs::read_link("/proc/thread-self") {
                if let Some(tid) = thread.file_name() {
                    let _ = std::process::Command::new("renice")
                        .args(["-n", "19", "-p"])
                        .arg(tid)
                        .output();
                }
            }
            for connection in listener.incoming() {
                let Ok(mut stream) = connection else {
                    continue;
                };
                let result = serve_connection(&archive, &mut stream, &can_seal);
                if let Err(error) = result {
                    tracing::warn!(error = %error, "position archive maintenance failed");
                    let _ = reply(
                        &mut stream,
                        &Reply {
                            ok: false,
                            empty: false,
                            error: Some(error.to_string()),
                            status: None,
                        },
                    );
                }
            }
        })?;
    Ok(())
}

pub(super) fn serve_connection(
    archive: &PositionArchive,
    stream: &mut std::os::unix::net::UnixStream,
    can_seal: &impl Fn(Cursor) -> Result<()>,
) -> Result<()> {
    use std::io::BufRead;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(120)))?;
    let mut request = Vec::new();
    std::io::BufReader::new(&*stream)
        .take(64 * 1024)
        .read_until(b'\n', &mut request)?;
    ensure!(
        request.last() == Some(&b'\n'),
        "incomplete/oversized maintenance request"
    );
    let command: Command = serde_json::from_slice(&request)?;
    match command {
        Command::Status => reply(stream, &Reply::success(Some(archive.retention_status())))?,
        Command::Export { before_us } => {
            ensure!(
                archive.cold.is_none(),
                "cold archival is enabled; use the dedicated cold pull instead of direct hot reclamation"
            );
            let now = crate::kline::now_us();
            let before = (before_us, 0);
            validate_before(before, now)?;
            can_seal(before)?;
            if archive.retention_floor() >= before {
                let mut response = Reply::success(None);
                response.empty = true;
                reply(stream, &response)?;
            } else {
                reply(stream, &Reply::success(None))?;
                archive.export_prefix(stream, before, now)?;
            }
        }
        Command::Commit { receipt } => {
            ensure!(
                archive.cold.is_none(),
                "cold archival is enabled; direct hot-prefix commits are disabled"
            );
            archive.commit_receipt(&receipt, crate::kline::now_us())?;
            reply(stream, &Reply::success(Some(archive.retention_status())))?;
        }
        Command::Compact { before_us } => {
            ensure!(
                archive.cold.is_none(),
                "cold archival is enabled; forced hot-prefix compaction is disabled"
            );
            archive.compact_archived((before_us, 0))?;
            reply(stream, &Reply::success(Some(archive.retention_status())))?;
        }
    }
    Ok(())
}

pub fn client(path: &Path, command: &Command, output: &mut impl Write) -> Result<bool> {
    use std::io::BufRead;
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path).context("connect Manager archive owner socket")?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(3600)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(30)))?;
    serde_json::to_writer(&mut stream, command)?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reader = std::io::BufReader::new(stream);
    let mut line = Vec::new();
    (&mut reader).take(64 * 1024).read_until(b'\n', &mut line)?;
    let response: Reply = serde_json::from_slice(&line)?;
    ensure!(
        response.ok,
        "{}",
        response.error.as_deref().unwrap_or("maintenance rejected")
    );
    if response.empty {
        return Ok(false);
    }
    if matches!(command, Command::Export { .. }) {
        std::io::copy(&mut reader, output)?;
    } else {
        serde_json::to_writer(&mut *output, &response)?;
        output.write_all(b"\n")?;
    }
    Ok(true)
}
