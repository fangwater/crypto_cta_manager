//! Bound even a connected-but-stalled SSH session; reap children on all exits.
use anyhow::{Context, Result};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

fn terminate(child: &mut Child) {
    // SCP starts an SSH subprocess. Kill the isolated process group as well as
    // the leader so a stalled transfer cannot leave an orphaned SSH connection.
    if matches!(child.try_wait(), Ok(None)) {
        let _ = Command::new("kill")
            .args(["-KILL", "--"])
            .arg(format!("-{}", child.id()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = child.kill();
    }
}

pub(super) struct Operation {
    child: Arc<Mutex<Child>>,
    cancel: mpsc::Sender<()>,
    watchdog: Option<JoinHandle<()>>,
}
impl Operation {
    pub fn spawn(command: &mut Command, timeout: Duration) -> Result<Self> {
        command.process_group(0);
        let child = Arc::new(Mutex::new(
            command.spawn().context("SSH operation failed to start")?,
        ));
        let (cancel, receive) = mpsc::channel();
        let watched = child.clone();
        let watchdog = std::thread::Builder::new()
            .name("archive-ssh-deadline".into())
            .spawn(move || {
                if receive.recv_timeout(timeout).is_err() {
                    terminate(&mut watched.lock().unwrap());
                }
            });
        match watchdog {
            Ok(watchdog) => Ok(Self {
                child,
                cancel,
                watchdog: Some(watchdog),
            }),
            Err(error) => {
                let mut child = child.lock().unwrap();
                terminate(&mut child);
                let _ = child.wait();
                Err(error.into())
            }
        }
    }
    pub fn stdout(&self) -> Result<ChildStdout> {
        self.child
            .lock()
            .unwrap()
            .stdout
            .take()
            .context("missing SSH stdout")
    }
    pub fn stdin(&self) -> Result<ChildStdin> {
        self.child
            .lock()
            .unwrap()
            .stdin
            .take()
            .context("missing SSH stdin")
    }
    pub fn finish(&self) -> Result<ExitStatus> {
        loop {
            if let Some(status) = self.child.lock().unwrap().try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
        let mut child = self.child.lock().unwrap();
        terminate(&mut child);
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::process::Stdio;
    #[test]
    fn connected_but_stalled_child_is_killed_and_reaped() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]).stdout(Stdio::piped());
        let operation = Operation::spawn(&mut cmd, Duration::from_millis(40)).unwrap();
        let mut bytes = Vec::new();
        operation.stdout().unwrap().read_to_end(&mut bytes).unwrap();
        assert!(!operation.finish().unwrap().success());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn failed_reader_drops_and_reaps_child_without_waiting_for_deadline() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let operation = Operation::spawn(&mut cmd, Duration::from_secs(60)).unwrap();
        let child = operation.child.clone();
        drop(operation);
        assert!(child.lock().unwrap().try_wait().unwrap().is_some());
    }
}
