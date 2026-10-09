use super::{OwnedChild, sample, sockets::FORBIDDEN};
use std::{
    fs, io,
    net::{IpAddr, SocketAddr, TcpStream},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, PartialEq)]
pub(super) enum Outcome {
    Detected(String),
    Unreachable(String),
    Missed,
}

pub(super) fn run(dir: &Path, interval: Duration) -> io::Result<Outcome> {
    let target =
        std::env::var("ORACLE_CONTROL_TARGET").unwrap_or_else(|_| "169.254.255.254".into());
    let address: IpAddr = target.parse().map_err(io::Error::other)?;
    if !FORBIDDEN.contains(address)
        || address == "169.254.169.254".parse::<IpAddr>().expect("metadata IP")
    {
        return Err(io::Error::other(
            "control sink must be in the forbidden range and must not be IMDS",
        ));
    }
    let port = std::env::var("ORACLE_CONTROL_PORT").unwrap_or_else(|_| "80".into());
    let timeout = super::seconds("ORACLE_CONTROL_TIMEOUT", 12.0)?;
    let state = dir.join("control-state");
    fs::write(&state, "pending")?;
    let child = Command::new(std::env::current_exe()?)
        .args([
            "jailbreak",
            "control-worker",
            "--target",
            &target,
            "--port",
            &port,
            "--timeout",
            &timeout.as_secs_f64().to_string(),
            "--state",
        ])
        .arg(&state)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    observe(dir, OwnedChild(child), interval, timeout, sample)
}

fn observe(
    dir: &Path,
    child: OwnedChild,
    interval: Duration,
    timeout: Duration,
    mut sample: impl FnMut(&[u32]) -> io::Result<Vec<super::sockets::Socket>>,
) -> io::Result<Outcome> {
    let state = dir.join("control-state");
    let roots = [child.0.id()];
    fs::write(dir.join("control-roots"), format!("{}\n", roots[0]))?;
    let deadline = Instant::now() + timeout;
    let result = loop {
        super::super::shutdown::check()?;
        if let Some(hit) = sample(&roots)?.into_iter().next() {
            break Outcome::Detected(format!(
                "{} pid={} peer={} state={}",
                hit.command, hit.pid, hit.peer, hit.state
            ));
        }
        let current = fs::read_to_string(&state)?;
        if current.starts_with("unreachable") {
            break Outcome::Unreachable(current);
        }
        if Instant::now() >= deadline {
            break if current == "opened" {
                Outcome::Missed
            } else {
                Outcome::Unreachable(current)
            };
        }
        thread::sleep(interval);
    };
    drop(child);
    Ok(result)
}

pub(super) fn connect(target: &str, port: u16, timeout: Duration, state: &Path) -> io::Result<()> {
    let addr = SocketAddr::new(target.parse().map_err(io::Error::other)?, port);
    match TcpStream::connect_timeout(&addr, timeout) {
        Ok(_stream) => {
            fs::write(state, "opened")?;
            thread::sleep(timeout);
        }
        Err(error) => {
            fs::write(state, format!("unreachable: {error}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_registration_stays_out_of_live_roots_and_reaps_its_child() {
        let dir = std::env::temp_dir().join(format!("jailbreak-control-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("subtree-roots"), "").unwrap();
        fs::write(dir.join("control-state"), "pending").unwrap();
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let result = observe(
            &dir,
            OwnedChild(child),
            Duration::from_millis(1),
            Duration::from_secs(1),
            |roots| {
                assert_eq!(roots, &[pid]);
                Ok(vec![super::super::sockets::Socket {
                    command: "unnamed".into(),
                    pid,
                    peer: "169.254.1.2:80".into(),
                    state: "SYN_SENT".into(),
                }])
            },
        )
        .unwrap();
        assert!(matches!(result, Outcome::Detected(_)));
        assert_eq!(
            fs::read_to_string(dir.join("control-roots")).unwrap(),
            format!("{pid}\n")
        );
        assert_eq!(fs::read_to_string(dir.join("subtree-roots")).unwrap(), "");
        assert!(
            !Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
