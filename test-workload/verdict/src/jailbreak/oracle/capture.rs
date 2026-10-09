use super::{OwnedChild, sockets::FORBIDDEN};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) struct Capture {
    child: OwnedChild,
    path: PathBuf,
}

impl Capture {
    pub(super) fn start(dir: &Path, name: &str) -> io::Result<Self> {
        let path = dir.join(name);
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("tcpdump.log"))?;
        let child = Command::new("tcpdump")
            .args(["-i", "any", "-n", "-U", "-w"])
            .arg(&path)
            .arg(FORBIDDEN.filter())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        let mut capture = Self {
            child: OwnedChild(child),
            path,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            super::super::shutdown::check()?;
            capture.check()?;
            if fs::metadata(&capture.path).is_ok_and(|m| m.len() >= 24) {
                return Ok(capture);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other(
                    "tcpdump did not initialize its capture file",
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    pub(super) fn check(&mut self) -> io::Result<()> {
        if let Some(status) = self.child.0.try_wait()? {
            return Err(io::Error::other(format!(
                "tcpdump exited during observation: {status}"
            )));
        }
        Ok(())
    }
    pub(super) fn count(&self) -> io::Result<usize> {
        count(&self.path)
    }
    pub(super) fn stop(mut self) -> io::Result<usize> {
        self.check()?;
        let status = Command::new("kill")
            .args(["-INT", &self.child.0.id().to_string()])
            .status()?;
        if !status.success() {
            return Err(io::Error::other("could not stop tcpdump"));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.0.try_wait()? {
                if !status.success() {
                    return Err(io::Error::other(format!(
                        "tcpdump shutdown failed: {status}"
                    )));
                }
                return self.count();
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("tcpdump did not stop"));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn count(path: &Path) -> io::Result<usize> {
    let output = Command::new("tcpdump")
        .args(["-n", "-r"])
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "cannot read packet capture: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count())
}
