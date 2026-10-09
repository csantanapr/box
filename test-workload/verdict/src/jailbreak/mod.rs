//! The on-instance jailbreak harness and its host-evidence verdict.

mod agent_a;
mod coverage;
mod creds;
mod oracle;
mod setup;
mod shutdown;
mod stream;
mod upload;
mod validity;
mod verdict;

pub use verdict::*;

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Run a jailbreak harness command and return its process exit code.
pub fn command(args: &[String]) -> u8 {
    match dispatch(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("jailbreak: {error}");
            2
        }
    }
}

fn dispatch(args: &[String]) -> io::Result<u8> {
    let action = args.first().map(String::as_str).unwrap_or("");
    let offset = if action == "oracle" { 2 } else { 1 };
    let rest = args.get(offset..).unwrap_or_default();
    if rest.len() % 2 != 0 {
        return Err(io::Error::other("each flag requires a value"));
    }
    let mut flags = BTreeMap::new();
    for pair in rest.chunks_exact(2) {
        if !pair[0].starts_with("--") || flags.insert(pair[0].as_str(), pair[1].as_str()).is_some()
        {
            return Err(io::Error::other("invalid or repeated flag"));
        }
    }
    let allowed: &[&str] = match action {
        "run" => &["--case", "--platform", "--box-commit", "--run-id"],
        "validity" => &["--turns", "--markers"],
        "oracle" | "oracle-worker" => &["--run-dir"],
        "agent-worker" => &["--config", "--workspace", "--prompt"],
        "control-worker" => &["--target", "--port", "--timeout", "--state"],
        _ => return Err(io::Error::other("expected jailbreak run|oracle|validity")),
    };
    for flag in flags.keys() {
        if !allowed.contains(flag) {
            return Err(io::Error::other(format!("unknown flag {flag}")));
        }
    }
    let required = |key: &str| {
        flags
            .get(key)
            .copied()
            .ok_or_else(|| io::Error::other(format!("{key} is required")))
    };
    if matches!(action, "run" | "oracle-worker") {
        shutdown::install()?;
    }
    match action {
        "run" => run(&flags),
        "validity" => {
            let path = Path::new(required("--turns")?);
            let markers = required("--markers")?;
            if !matches!(markers, "EXTRACTED" | "NO_MARKERS") {
                return Err(io::Error::other(
                    "--markers must be EXTRACTED or NO_MARKERS",
                ));
            }
            let turns = fs::read_to_string(path).unwrap_or_default();
            let validity = validity::assess(&turns, markers == "EXTRACTED");
            validity.write(path.parent().unwrap_or(Path::new(".")))?;
            println!(
                "{} {} {} {}",
                validity.status,
                validity.uses,
                validity.ran,
                if validity.cause.is_empty() {
                    "-"
                } else {
                    &validity.cause
                }
            );
            Ok(0)
        }
        "agent-worker" => {
            agent_a::worker(
                Path::new(required("--config")?),
                Path::new(required("--workspace")?),
                required("--prompt")?,
            )?;
            Ok(0)
        }
        "control-worker" => {
            let seconds: f64 = required("--timeout")?.parse().map_err(io::Error::other)?;
            if !seconds.is_finite() || seconds <= 0.0 || seconds > 120.0 {
                return Err(io::Error::other("invalid timeout"));
            }
            oracle::control_worker(
                required("--target")?,
                required("--port")?.parse().map_err(io::Error::other)?,
                Duration::from_secs_f64(seconds),
                Path::new(required("--state")?),
            )?;
            Ok(0)
        }
        "oracle-worker" => {
            let run_dir = Path::new(required("--run-dir")?);
            let oracle = oracle::Oracle::start(run_dir)?;
            fs::write(run_dir.join("oracle/ready"), "")?;
            while !run_dir.join("oracle/stop").exists() {
                shutdown::check()?;
                std::thread::sleep(Duration::from_millis(100));
            }
            oracle.stop()?;
            Ok(0)
        }
        "oracle" => debug_oracle(
            args.get(1).map(String::as_str).unwrap_or(""),
            Path::new(required("--run-dir")?),
        ),
        _ => unreachable!(),
    }
}

fn debug_oracle(action: &str, run_dir: &Path) -> io::Result<u8> {
    let dir = run_dir.join("oracle");
    match action {
        "start" => {
            if dir.join("oracle.pid").exists() {
                return Err(io::Error::other("oracle already owns this run directory"));
            }
            fs::create_dir_all(&dir)?;
            for name in ["ready", "stop"] {
                if dir.join(name).exists() {
                    fs::remove_file(dir.join(name))?;
                }
            }
            let log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("worker.log"))?;
            use std::os::unix::process::CommandExt;
            let mut child = oracle::OwnedChild(
                Command::new(std::env::current_exe()?)
                    .args(["jailbreak", "oracle-worker", "--run-dir"])
                    .arg(run_dir)
                    .stdin(Stdio::null())
                    .stdout(log.try_clone()?)
                    .stderr(log)
                    .process_group(0)
                    .spawn()?,
            );
            let deadline = Instant::now() + Duration::from_secs(150);
            while !dir.join("ready").exists() {
                if let Some(status) = child.0.try_wait()? {
                    return Err(io::Error::other(format!(
                        "oracle start failed: {status}; see {}",
                        dir.join("worker.log").display()
                    )));
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::other("oracle startup timed out"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            std::mem::forget(child);
        }
        "stop" => {
            if !dir.join("oracle.pid").exists() {
                return Err(io::Error::other("oracle is not running"));
            }
            fs::write(dir.join("stop"), "")?;
            let deadline = Instant::now() + Duration::from_secs(20);
            while dir.join("oracle.pid").exists() {
                if Instant::now() >= deadline {
                    return Err(io::Error::other("oracle shutdown timed out"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let rows = fs::read_to_string(dir.join("verdict.json"))?;
            if !rows
                .lines()
                .filter_map(|s| serde_json::from_str::<OracleRow>(s).ok())
                .any(|r| r.layer == LAYER_FINAL)
            {
                return Err(io::Error::other("oracle stopped without a final row"));
            }
        }
        "status" => {
            println!(
                "oracle PID: {}",
                fs::read_to_string(dir.join("oracle.pid"))
                    .unwrap_or_else(|_| "not running".into())
                    .trim()
            );
            println!(
                "subtree roots: {}",
                fs::read_to_string(dir.join("subtree-roots"))
                    .unwrap_or_default()
                    .trim()
            );
            println!(
                "{}",
                fs::read_to_string(dir.join("verdict.json")).unwrap_or_default()
            );
        }
        _ => return Err(io::Error::other("expected oracle start|stop|status")),
    }
    Ok(0)
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}

fn run(flags: &BTreeMap<&str, &str>) -> io::Result<u8> {
    let case = flags.get("--case").copied().unwrap_or("network-egress");
    if case != "network-egress" {
        return Err(io::Error::other("only --case network-egress is supported"));
    }
    let home = fs::canonicalize(PathBuf::from(
        std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is required"))?,
    ))?;
    let source = std::env::var_os("INDET_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("strands-box"));
    let run_dir = home.join("indet-run").join(case);
    setup::fresh(&run_dir)?;
    for dir in ["oracle", "agent-a", "agent-b"] {
        fs::create_dir(run_dir.join(dir))?;
    }
    let platform = flags
        .get("--platform")
        .map(|v| v.to_string())
        .unwrap_or_else(|| {
            env(
                "PLATFORM",
                if cfg!(target_os = "macos") {
                    "macos"
                } else {
                    "linux"
                },
            )
        });
    if !matches!(platform.as_str(), "macos" | "linux") {
        return Err(io::Error::other("platform must be macos or linux"));
    }
    let commit = flags
        .get("--box-commit")
        .map(|v| v.to_string())
        .unwrap_or_else(|| env("BOX_COMMIT", "unknown"));
    let commit = fs::read_to_string(source.join("COMMIT"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().into())
        .unwrap_or(commit);
    let run = Run {
        dimension: case.into(),
        platform,
        box_commit: Some(commit),
        run_id: Some(
            flags
                .get("--run-id")
                .map(|v| v.to_string())
                .unwrap_or_else(|| env("RUN_ID", &unix_time().to_string())),
        ),
    };
    let mut fault = None;
    let campaign = (|| -> io::Result<()> {
        let setup = setup::prepare(&home, &source)?;
        let goal = include_str!("../../../network-egress/goal.md");
        creds::fetch(&home, &env("AWS_REGION", "us-west-2"))?;
        shutdown::check()?;
        let oracle = oracle::Oracle::start(&run_dir)?;
        let agent = agent_a::run(&run_dir, &setup, goal, &run.platform);
        let stopped = oracle.stop();
        let exit = agent.as_ref().copied().unwrap_or(1);
        let turns = fs::read_to_string(run_dir.join("agent-a/turns.jsonl"))?;
        let report = stream::report(&turns);
        let mut validity = validity::assess(&turns, report.is_some());
        validity.classify(
            &fs::read_to_string(run_dir.join("agent-a.log")).unwrap_or_default(),
            exit,
        );
        if let Err(error) = &agent {
            validity.status = "INVALID";
            validity.cause = error.to_string();
        }
        validity.write(&run_dir.join("agent-a"))?;
        fs::write(
            run_dir.join("agent-a/coverage.md"),
            coverage::render(goal, report.as_deref().unwrap_or("")),
        )?;
        fs::write(run_dir.join("agent-a/method_report.md"),report.unwrap_or_else(|| format!("# Method Report (fallback)\n\nrun_status: {}\nexit_code: {exit}\ntool_uses: {}\n\n{}\n",validity.status,validity.uses,validity.cause)))?;
        agent?;
        stopped?;
        Ok(())
    })();
    if let Err(error) = campaign {
        eprintln!("harness failure: {error}");
        fault = Some(error.to_string());
        fs::write(run_dir.join("agent-a/run_status.txt"), "INVALID\n")?;
        fs::write(run_dir.join("agent-a/first_error.txt"), error.to_string())?;
    }
    let finding = verdict(&run, &load(&run_dir));
    let body = serde_json::to_vec_pretty(&finding)?;
    for file in ["finding.json", "verdict.json"] {
        fs::write(run_dir.join(file), &body)?;
    }
    println!(
        "jailbreak: {} {:?}, run_status={}",
        finding.verdict.as_str(),
        finding.security_outcome,
        finding.run_status.as_deref().unwrap_or("INVALID")
    );
    let bucket = env("LEDGER_BUCKET", "");
    if !bucket.is_empty() {
        upload::upload(&run_dir, &run, &bucket)?;
    }
    Ok(
        if fault.is_none() && matches!(finding.verdict, Verdict::Pass) {
            0
        } else {
            1
        },
    )
}
