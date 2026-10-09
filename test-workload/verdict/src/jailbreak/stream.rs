use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(super) struct Event {
    #[serde(rename = "type", default)]
    kind: String,
    message: Option<Value>,
    result: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum Block {
    #[serde(rename = "tool_use")]
    ToolUse {
        name: Option<String>,
        input: Option<Value>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        is_error: Option<bool>,
        content: Option<Value>,
    },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(other)]
    Other,
}

impl Event {
    pub(super) fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line).ok()
    }
    pub(super) fn blocks(&self) -> Vec<Block> {
        self.message
            .as_ref()
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|b| serde_json::from_value(b.clone()).ok())
            .collect()
    }
    pub(super) fn text(&self) -> Vec<String> {
        match self.kind.as_str() {
            "assistant" => self
                .blocks()
                .into_iter()
                .filter_map(|b| match b {
                    Block::Text { text } => Some(text),
                    _ => None,
                })
                .collect(),
            "result" => self
                .result
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_owned)
                .into_iter()
                .collect(),
            _ => vec![],
        }
    }
    pub(super) fn pretty(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .blocks()
            .into_iter()
            .filter_map(|b| match b {
                Block::ToolUse { name, input } => Some(format!(
                    "[tool_use] {}({})",
                    name.unwrap_or_default(),
                    input.unwrap_or_default()
                )),
                Block::ToolResult { is_error, content } => Some(format!(
                    "[tool_result error={}] {}",
                    is_error.unwrap_or(false),
                    content.unwrap_or_default()
                )),
                Block::Text { text } => Some(format!("[assistant] {text}")),
                Block::Other => None,
            })
            .collect();
        if self.kind == "result" {
            lines.push(format!(
                "[result] {}",
                self.result.as_ref().unwrap_or(&Value::Null)
            ));
        }
        for line in &mut lines {
            crate::truncate_on_boundary(line, 600);
        }
        lines
    }
}

pub(super) fn report(turns: &str) -> Option<String> {
    let text = turns
        .lines()
        .filter_map(Event::parse)
        .flat_map(|e| e.text())
        .collect::<Vec<_>>()
        .join("\n");
    let (_, rest) = text.split_once("===METHOD_REPORT_BEGIN===")?;
    let (body, _) = rest.split_once("===METHOD_REPORT_END===")?;
    Some(format!("{}\n", body.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_from_assistant_or_result() {
        for event in [
            serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"===METHOD_REPORT_BEGIN===\nreport\n===METHOD_REPORT_END==="}]}}),
            serde_json::json!({"type":"result","result":"===METHOD_REPORT_BEGIN===\nreport\n===METHOD_REPORT_END==="}),
        ] {
            assert_eq!(report(&event.to_string()).as_deref(), Some("report\n"));
        }
        assert_eq!(report("broken\n{}\n[]"), None);
        assert_eq!(
            report(r#"{"type":"result","result":"===METHOD_REPORT_BEGIN=== incomplete"}"#),
            None
        );
    }
    #[test]
    fn malformed_blocks_do_not_hide_valid_blocks() {
        let event = Event::parse(r#"{"type":"assistant","message":{"content":[null,42,{"type":"tool_use","name":"Bash"}]}}"#).unwrap();
        assert_eq!(event.blocks().len(), 1);
        assert!(event.pretty()[0].contains("Bash"));
    }
}
