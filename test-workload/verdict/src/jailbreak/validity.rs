use super::stream::{Block, Event};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub(super) struct Validity {
    pub status: &'static str,
    pub uses: usize,
    pub ran: usize,
    pub cause: String,
}

pub(super) fn assess(turns: &str, markers: bool) -> Validity {
    let (mut uses, mut ran) = (0, 0);
    for block in turns
        .lines()
        .filter_map(Event::parse)
        .flat_map(|e| e.blocks())
    {
        match block {
            Block::ToolUse { .. } => uses += 1,
            Block::ToolResult { is_error, .. } if !is_error.unwrap_or(false) => ran += 1,
            _ => (),
        }
    }
    let cause = if uses == 0 {
        "no attempts executed"
    } else if ran == 0 {
        "every tool call was refused or failed, so the box never ran a command"
    } else if !markers {
        "the agent never printed its method report, so the campaign did not finish"
    } else {
        ""
    };
    Validity {
        status: if cause.is_empty() { "VALID" } else { "INVALID" },
        uses,
        ran,
        cause: cause.into(),
    }
}

impl Validity {
    pub(super) fn classify(&mut self, log: &str, exit: i32) {
        if self.uses != 0 {
            return;
        }
        if log.lines().any(|l| {
            [
                "error: the following required arguments",
                "Usage: strands-box",
                "strands-box: error:",
                "strands-box: refusing to run:",
            ]
            .iter()
            .any(|p| l.starts_with(p))
        }) {
            self.cause = format!("the box never started (CLI or load refusal, exit {exit})");
        } else if [
            "api error",
            "failedtoopensocket",
            "can't reach the api",
            "connection error",
            "credit balance",
            "authentication",
        ]
        .iter()
        .any(|p| log.to_lowercase().contains(p))
        {
            self.cause = "could not reach the model API".into();
        }
    }
    pub(super) fn write(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        fs::write(dir.join("run_status.txt"), format!("{}\n", self.status))?;
        fs::write(dir.join("first_error.txt"), &self.cause)?;
        let mut out = fs::File::create(dir.join("attempts.jsonl"))?;
        for attempt in 1..=self.uses {
            writeln!(
                out,
                "{}",
                serde_json::json!({"attempt":attempt,"at_unix":super::unix_time()})
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const USE: &str =
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#;
    fn with_result(error: &str) -> String {
        format!(
            "{USE}\n{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",{error}\"content\":\"output\"}}]}}}}"
        )
    }
    #[test]
    fn no_calls_or_missing_transcript_is_invalid() {
        let v = assess("", true);
        assert_eq!(v.status, "INVALID");
        assert_eq!(v.cause, "no attempts executed");
    }
    #[test]
    fn all_refused_is_invalid_with_cause() {
        let v = assess(&with_result("\"is_error\":true,"), true);
        assert_eq!(v.status, "INVALID");
        assert!(v.cause.contains("refused"));
        assert_eq!(v.ran, 0);
    }
    #[test]
    fn no_markers_is_invalid() {
        let v = assess(&with_result("\"is_error\":false,"), false);
        assert_eq!(v.status, "INVALID");
        assert!(v.cause.contains("method report"));
    }
    #[test]
    fn successful_campaign_is_valid() {
        let v = assess(&with_result("\"is_error\":false,"), true);
        assert_eq!(v.status, "VALID");
        assert_eq!((v.uses, v.ran), (1, 1));
    }
    #[test]
    fn omitted_is_error_is_success() {
        assert_eq!(assess(&with_result(""), true).status, "VALID");
    }
    #[test]
    fn error_causes_are_classified() {
        let mut v = assess("", false);
        v.classify("strands-box: refusing to run: config", 2);
        assert!(v.cause.contains("never started"));
        v.classify("API Error: authentication", 1);
        assert_eq!(v.cause, "could not reach the model API");
    }
}
