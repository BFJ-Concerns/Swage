//! Scripted fake App Server transport used by protocol and gateway tests.

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Run ordered JSON-RPC receive/send steps and return every received frame.
pub async fn run_scenario<R, W>(
    scenario: &Value,
    mut input: R,
    mut output: W,
) -> Result<Vec<Value>, String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let steps = scenario
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "scenario.steps must be an array".to_string())?;
    let mut line = String::new();
    let mut received = Vec::with_capacity(steps.len());
    for (index, step) in steps.iter().enumerate() {
        line.clear();
        if input
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?
            == 0
        {
            return Err(format!("scenario step {index} expected a client frame"));
        }
        let frame: Value = serde_json::from_str(&line)
            .map_err(|error| format!("scenario step {index} received malformed JSON: {error}"))?;
        let expected = step
            .get("receive")
            .ok_or_else(|| format!("scenario step {index} omitted receive"))?;
        let matches = if step
            .get("receiveExact")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            expected == &frame
        } else {
            contains(expected, &frame)
        };
        if !matches {
            return Err(format!(
                "scenario step {index} mismatch\nexpected: {expected}\nactual: {frame}"
            ));
        }
        let request_id = frame.get("id").cloned();
        received.push(frame);
        for scripted in step
            .get("send")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(raw) = scripted.get("raw").and_then(Value::as_str) {
                output
                    .write_all(raw.as_bytes())
                    .await
                    .map_err(|error| error.to_string())?;
                output
                    .write_all(b"\n")
                    .await
                    .map_err(|error| error.to_string())?;
                continue;
            }
            let mut frame = scripted.clone();
            if frame.get("id").and_then(Value::as_str) == Some("$request_id") {
                frame["id"] = request_id.clone().unwrap_or(Value::Null);
            }
            output
                .write_all(frame.to_string().as_bytes())
                .await
                .map_err(|error| error.to_string())?;
            output
                .write_all(b"\n")
                .await
                .map_err(|error| error.to_string())?;
        }
        output.flush().await.map_err(|error| error.to_string())?;
        if step.get("close").and_then(Value::as_bool).unwrap_or(false) {
            output.shutdown().await.map_err(|error| error.to_string())?;
            break;
        }
    }
    if scenario
        .get("keepOpen")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        std::future::pending::<()>().await;
    }
    Ok(received)
}

fn contains(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => expected.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|actual| contains(value, actual))
        }),
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected
                    .iter()
                    .zip(actual)
                    .all(|(expected, actual)| contains(expected, actual))
        }
        _ => expected == actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
