use super::Run;
use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
};

const ARTIFACTS: &[(&str, &str)] = &[
    ("verdict.json", "verdict.json"),
    ("validation_report.md", "validation_report.md"),
    ("agent-a/method_report.md", "method_report.md"),
    ("oracle/verdict.json", "oracle-verdict.json"),
    ("agent-a.log", "agent-a.log"),
    ("agent-a/turns.jsonl", "turns.jsonl"),
    (
        "agent-b/deterministic_checks.json",
        "deterministic_checks.json",
    ),
    ("finding.json", "finding.json"),
    ("agent-a/coverage.md", "coverage.md"),
];

pub(super) fn upload(dir: &Path, run: &Run, bucket: &str) -> io::Result<()> {
    if !dir.join("verdict.json").is_file() {
        fs::write(
            dir.join("verdict.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "mode":"indeterministic","platform":run.platform,"dimension":run.dimension,
                "verdict":"UNCERTAIN","risk_score":0,"run_status":"INVALID","note":"no verdict produced"
            }))?,
        )?;
    }
    let destination = format!(
        "s3://{bucket}/reports/{}/{}/indeterministic/{}/{}",
        run.box_commit.as_deref().unwrap_or("unknown"),
        run.run_id.as_deref().unwrap_or("unknown"),
        run.platform,
        run.dimension
    );
    let mut failed = vec![];
    for (source, key) in ARTIFACTS
        .iter()
        .copied()
        .chain([("/tmp/box-build.log", "box-build.log")])
    {
        let source = dir.join(source);
        if !source.is_file() {
            continue;
        }
        let result = Command::new("aws")
            .args(["s3", "cp"])
            .arg(&source)
            .arg(format!("{destination}/{key}"))
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .stdout(Stdio::null())
            .status();
        if !result.is_ok_and(|s| s.success()) {
            failed.push(key);
        }
    }
    println!("artifacts: {destination}");
    if failed.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "artifact upload failed: {}",
            failed.join(", ")
        )))
    }
}
