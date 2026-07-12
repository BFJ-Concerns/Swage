use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    if let Some(path) = std::env::var_os("SWAGE_CODEX_FAKE_SCENARIO") {
        run_scenario(&std::path::PathBuf::from(path));
        return;
    }

    run_default_scenario();
}

fn run_scenario(path: &std::path::Path) {
    let scenario: Value =
        serde_json::from_slice(&std::fs::read(path).expect("read SWAGE_CODEX_FAKE_SCENARIO"))
            .expect("valid fake app-server scenario JSON");
    let steps = scenario
        .get("steps")
        .and_then(Value::as_array)
        .expect("scenario.steps array");
    let stdin = std::io::stdin();
    let mut input = stdin.lock().lines();
    let mut stdout = std::io::stdout().lock();
    for (index, step) in steps.iter().enumerate() {
        let frame: Value = serde_json::from_str(
            &input
                .next()
                .unwrap_or_else(|| panic!("scenario step {index} expected a client frame"))
                .expect("read client frame"),
        )
        .expect("valid client JSON-RPC frame");
        let expected = step
            .get("receive")
            .unwrap_or_else(|| panic!("scenario step {index} omitted receive"));
        assert!(
            contains(expected, &frame),
            "scenario step {index} mismatch\nexpected subset: {expected}\nactual: {frame}"
        );
        let request_id = frame.get("id").cloned();
        for output in step
            .get("send")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let mut output = output.clone();
            if output.get("id").and_then(Value::as_str) == Some("$request_id") {
                output["id"] = request_id.clone().unwrap_or(Value::Null);
            }
            writeln!(stdout, "{output}").expect("write scripted frame");
        }
        stdout.flush().expect("flush scripted frames");
    }
}

fn contains(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => expected.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|actual| contains(value, actual))
        }),
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() <= actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| contains(expected, actual))
        }
        _ => expected == actual,
    }
}

fn run_default_scenario() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("read JSON-RPC frame");
        let frame: Value = serde_json::from_str(&line).expect("valid JSON-RPC frame");
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            continue;
        };
        let Some(id) = frame.get("id").cloned() else {
            continue;
        };
        let result = match method {
            "initialize" => json!({"userAgent":"swage-fake-app-server/1"}),
            "account/read" => json!({
                "account":{"type":"chatgpt","email":"fake@example.test","planType":"plus"},
                "requiresOpenaiAuth":true
            }),
            "model/list" => {
                json!({"data":[{"id":"fake-codex","displayName":"Fake Codex","contextWindow":128000}]})
            }
            "thread/start" => {
                let params = frame.get("params").expect("thread/start params");
                assert!(
                    params.get("dynamicTools").is_some(),
                    "thread/start must carry dynamicTools"
                );
                assert!(
                    params.get("developerInstructions").is_some(),
                    "thread/start must carry developerInstructions"
                );
                assert!(
                    params.get("sandbox").is_some(),
                    "thread/start must carry sandbox mode"
                );
                json!({"thread":{"id":"fake-thread"}})
            }
            "thread/resume" => json!({"thread":{"id":"fake-thread"}}),
            "turn/start" => json!({"turn":{"id":"fake-turn","status":"inProgress"}}),
            "turn/interrupt" => json!({}),
            _ => json!({}),
        };
        writeln!(
            stdout,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"result":result})
        )
        .expect("write JSON-RPC frame");
        stdout.flush().expect("flush JSON-RPC frame");
        if method == "turn/start" {
            for notification in [
                json!({"jsonrpc":"2.0","method":"turn/started","params":{"turn":{"id":"fake-turn","status":"inProgress"}}}),
                json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"fake-thread","turnId":"fake-turn","itemId":"message-1","delta":"fake response"}}),
                json!({"jsonrpc":"2.0","method":"item/completed","params":{"item":{"id":"message-1","type":"agentMessage","text":"fake response"}}}),
                json!({"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"threadId":"fake-thread","turnId":"fake-turn","tokenUsage":{"last":{"inputTokens":12,"cachedInputTokens":0,"outputTokens":3,"reasoningOutputTokens":0,"totalTokens":15}}}}),
                json!({"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"fake-turn","status":"completed","error":null}}}),
            ] {
                writeln!(stdout, "{notification}").expect("write notification");
            }
            stdout.flush().expect("flush notifications");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_expectations_match_nested_subsets() {
        let expected = json!({"method":"thread/start","params":{"sandbox":"read-only"}});
        let actual = json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"thread/start",
            "params":{"sandbox":"read-only","dynamicTools":[]}
        });
        assert!(contains(&expected, &actual));
        assert!(!contains(&json!({"method":"turn/start"}), &actual));
    }
}
