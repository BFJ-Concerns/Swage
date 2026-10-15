//! ToolOutput — the framework-assembled result of a tool invocation.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Duration serde helper — serialise as milliseconds (u64)
// ---------------------------------------------------------------------------

/// Serde helper module that serialises `Duration` as a `u64` millisecond count.
///
/// Using milliseconds gives sub-second resolution while remaining human-readable
/// in JSON. Nanosecond counts overflow u32 quickly, and f64 seconds introduce
/// floating-point ambiguity — millis strike the right balance for tool timing.
mod duration_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        // Truncate to millis; sub-millisecond precision is not meaningful for
        // tool execution timings logged in audit records.
        (d.as_millis() as u64).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let millis = u64::deserialize(d)?;
        Ok(Duration::from_millis(millis))
    }
}

// ---------------------------------------------------------------------------
// ToolOutput
// ---------------------------------------------------------------------------

/// The framework-assembled result of a completed tool invocation.
///
/// The dispatch pipeline constructs this after executing a tool: it measures
/// wall-clock time, runs leak detection on the raw output, and populates `raw`
/// only when debug mode is enabled at dispatch time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub execution: swage_protocol::client::conversation::ToolExecution,
    /// Tool output after leak-detection redaction applied by the dispatch pipeline.
    pub result: Value,

    /// Resource cost incurred by this invocation, if the tool reports one.
    /// None for tools that do not track cost (e.g., local filesystem tools).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,

    /// Wall-clock execution time, measured by the framework around the `execute()`
    /// call. Serialised as milliseconds.
    #[serde(with = "duration_millis")]
    pub duration: Duration,

    /// Original output before leak-detection redaction. Populated only when the
    /// debug flag is enabled at dispatch time; always None in normal operation.
    /// Enables post-hoc inspection of what was redacted without re-running the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::ToolOutput;

    #[test]
    fn tool_output_constructs_with_all_fields() {
        let output = ToolOutput {
            execution: swage_protocol::client::conversation::ToolExecution::succeeded(),
            result: json!({"answer": 42}),
            cost: Some(0.01),
            duration: Duration::from_millis(150),
            raw: None,
        };
        assert_eq!(output.cost, Some(0.01));
        assert_eq!(output.duration, Duration::from_millis(150));
        assert!(output.raw.is_none());
    }

    #[test]
    fn tool_output_raw_is_none_by_default() {
        // Constructing without debug mode should leave raw as None.
        let output = ToolOutput {
            execution: swage_protocol::client::conversation::ToolExecution::succeeded(),
            result: json!(null),
            cost: None,
            duration: Duration::from_secs(0),
            raw: None,
        };
        assert!(output.raw.is_none());
    }

    #[test]
    fn tool_output_serde_round_trip_no_raw() {
        let output = ToolOutput {
            execution: swage_protocol::client::conversation::ToolExecution::succeeded(),
            result: json!({"key": "value"}),
            cost: Some(0.05),
            duration: Duration::from_millis(250),
            raw: None,
        };
        let json_str = serde_json::to_string(&output).unwrap();
        let decoded: ToolOutput = serde_json::from_str(&json_str).unwrap();
        assert_eq!(output.result, decoded.result);
        assert_eq!(output.cost, decoded.cost);
        assert_eq!(output.duration, decoded.duration);
        assert!(decoded.raw.is_none());
    }

    #[test]
    fn tool_output_serde_round_trip_with_raw() {
        let output = ToolOutput {
            execution: swage_protocol::client::conversation::ToolExecution::succeeded(),
            result: json!({"redacted": "[REDACTED]"}),
            cost: None,
            duration: Duration::from_millis(100),
            raw: Some(json!({"secret": "sk-12345"})),
        };
        let json_str = serde_json::to_string(&output).unwrap();
        let decoded: ToolOutput = serde_json::from_str(&json_str).unwrap();
        assert_eq!(output.result, decoded.result);
        assert_eq!(output.duration, decoded.duration);
        assert_eq!(output.raw, decoded.raw);
    }

    #[test]
    fn tool_output_duration_serialises_as_millis() {
        // Duration must survive the serde round-trip with sub-second precision.
        let output = ToolOutput {
            execution: swage_protocol::client::conversation::ToolExecution::succeeded(),
            result: json!({}),
            cost: None,
            duration: Duration::from_millis(1234),
            raw: None,
        };
        let json_val: serde_json::Value = serde_json::to_value(&output).unwrap();
        // The serialised duration must be recoverable as 1234 millis.
        let decoded: ToolOutput = serde_json::from_value(json_val).unwrap();
        assert_eq!(decoded.duration, Duration::from_millis(1234));
    }
}
