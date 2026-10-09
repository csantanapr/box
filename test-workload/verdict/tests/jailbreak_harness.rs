use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "jailbreak-harness-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_workload-oracle"))
}

#[test]
fn validity_command_handles_missing_transcript_and_writes_neutral_attempts() {
    let scratch = Scratch::new();
    let turns = scratch.0.join("turns.jsonl");
    let invoke = || {
        binary()
            .args(["jailbreak", "validity", "--turns"])
            .arg(&turns)
            .args(["--markers", "EXTRACTED"])
            .output()
            .unwrap()
    };
    assert!(invoke().status.success());
    assert_eq!(
        fs::read_to_string(scratch.0.join("run_status.txt")).unwrap(),
        "INVALID\n"
    );
    fs::write(&turns,"{\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\"}]}}\n{\"message\":{\"content\":[{\"type\":\"tool_result\",\"content\":\"ok\"}]}}\n").unwrap();
    let output = invoke();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("VALID 1 1 -"));
    let attempts = fs::read_to_string(scratch.0.join("attempts.jsonl")).unwrap();
    let row: serde_json::Value = serde_json::from_str(&attempts).unwrap();
    assert_eq!(row["attempt"], 1);
    assert_eq!(row.as_object().unwrap().len(), 2);
    assert!(row["at_unix"].is_u64());
    assert_eq!(
        fs::read_to_string(scratch.0.join("first_error.txt")).unwrap(),
        ""
    );
}

#[test]
fn agent_waits_for_registration_token_and_preserves_arguments() {
    let scratch = Scratch::new();
    let fake = scratch.0.join("strands-box");
    fs::write(&fake, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = binary()
        .args([
            "jailbreak",
            "agent-worker",
            "--config",
            "config with spaces",
            "--workspace",
        ])
        .arg(&scratch.0)
        .args(["--prompt", "literal `command` $(command) \"quoted\""])
        .env("PATH", &scratch.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(150));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the worker must wait for its root registration"
    );
    child.stdin.take().unwrap().write_all(b"1").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("agent did not finish after the launch token");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "run\n--config\nconfig with spaces\n--\n--print\n--output-format\nstream-json\n--verbose\n--dangerously-skip-permissions\nliteral `command` $(command) \"quoted\"\n"
    );
}

#[test]
fn agent_does_not_launch_if_the_registration_pipe_closes() {
    let scratch = Scratch::new();
    let output = binary()
        .args([
            "jailbreak",
            "agent-worker",
            "--config",
            "unused",
            "--workspace",
        ])
        .arg(&scratch.0)
        .args(["--prompt", "unused"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to fill whole buffer"));
}

#[test]
fn termination_signals_reap_capture_and_remove_the_oracle_lock() {
    for signal in ["-TERM", "-INT"] {
        let scratch = Scratch::new();
        let fake = scratch.0.join("tcpdump");
        fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$CAPTURE_PID\"\nexec /bin/sleep 30\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let pid_file = scratch.0.join("capture.pid");
        let path = format!(
            "{}:{}",
            scratch.0.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut worker = binary()
            .args(["jailbreak", "oracle-worker", "--run-dir"])
            .arg(&scratch.0)
            .env("PATH", path)
            .env("CAPTURE_PID", &pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_file.is_file() || fs::read_to_string(&pid_file).unwrap_or_default().is_empty() {
            if Instant::now() >= deadline {
                let _ = worker.kill();
                panic!("capture did not start");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let capture_pid = fs::read_to_string(&pid_file).unwrap();
        assert!(
            Command::new("kill")
                .args([signal, &worker.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while worker.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = worker.kill();
                let _ = Command::new("kill").args(["-KILL", &capture_pid]).status();
                panic!("oracle did not handle {signal} within three seconds");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = worker.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("shutdown requested"));
        let alive = Command::new("kill")
            .args(["-0", &capture_pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        if alive {
            let _ = Command::new("kill").args(["-KILL", &capture_pid]).status();
        }
        assert!(!alive, "capture outlived the oracle after {signal}");
        assert!(!scratch.0.join("oracle/oracle.pid").exists());
        assert!(
            !fs::read_to_string(scratch.0.join("oracle/verdict.json"))
                .unwrap()
                .contains("oracle-final")
        );
    }
}
