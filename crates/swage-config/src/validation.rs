use std::path::Path;
use toml::Value;
use crate::error::{ConfigProblem, ConfigProblemKind};
use crate::merge::PartialConfig;

// ---------------------------------------------------------------------------
// Schema constants
// ---------------------------------------------------------------------------

/// Top-level TOML sections recognised by the config schema.
const KNOWN_SECTIONS: &[&str] = &[
    "server", "provider", "workspace", "sandbox", "lifecycle", "logging",
];

/// Valid field names for each known section.
fn known_fields_for(section: &str) -> &'static [&'static str] {
    match section {
        "server"    => &["host", "port"],
        "provider"  => &["protocol", "model", "api_key_env"],
        "workspace" => &["path"],
        "sandbox"   => &["enabled"],
        "lifecycle" => &["idle_timeout"],
        "logging"   => &["level"],
        _           => &[],
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a byte offset to a 1-based line number.
fn byte_to_line(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())]
        .chars()
        .filter(|&c| c == '\n')
        .count()
        + 1
}

fn make_problem(file_path: &Path, field_path: Option<String>, kind: ConfigProblemKind) -> ConfigProblem {
    ConfigProblem {
        file_path: file_path.to_path_buf(),
        field_path,
        kind,
    }
}

fn toml_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_)   => "string",
        Value::Integer(_)  => "integer",
        Value::Float(_)    => "float",
        Value::Boolean(_)  => "boolean",
        Value::Datetime(_) => "datetime",
        Value::Array(_)    => "array",
        Value::Table(_)    => "table",
    }
}

// ---------------------------------------------------------------------------
// Type checking
// ---------------------------------------------------------------------------

/// Check that `value` has the expected primitive type for `section.field`.
/// Returns `Some(problem)` on mismatch, `None` if the type is correct.
fn check_field_type(
    section: &str,
    field: &str,
    value: &Value,
    file_path: &Path,
    field_path: &str,
) -> Option<ConfigProblem> {
    let (expected, is_ok): (&str, bool) = match (section, field) {
        ("server",    "host")        => ("string",  matches!(value, Value::String(_))),
        ("server",    "port")        => ("integer", matches!(value, Value::Integer(_))),
        ("provider",  "protocol")    => ("string",  matches!(value, Value::String(_))),
        ("provider",  "model")       => ("string",  matches!(value, Value::String(_))),
        ("provider",  "api_key_env") => ("string",  matches!(value, Value::String(_))),
        ("workspace", "path")        => ("string",  matches!(value, Value::String(_))),
        ("sandbox",   "enabled")     => ("boolean", matches!(value, Value::Boolean(_))),
        ("lifecycle", "idle_timeout")=> ("integer", matches!(value, Value::Integer(_))),
        ("logging",   "level")       => ("string",  matches!(value, Value::String(_))),
        // Unknown field — already caught before this call, so no match needed.
        _ => return None,
    };

    if is_ok {
        None
    } else {
        Some(make_problem(
            file_path,
            Some(field_path.to_string()),
            ConfigProblemKind::TypeError {
                expected: expected.to_string(),
                found: toml_type_name(value).to_string(),
            },
        ))
    }
}

// ---------------------------------------------------------------------------
// Semantic checking
// ---------------------------------------------------------------------------

/// Semantic constraints for a field that has already passed type-checking.
fn check_field_semantics(
    section: &str,
    field: &str,
    value: &Value,
    file_path: &Path,
    field_path: &str,
) -> Vec<ConfigProblem> {
    let mut problems = Vec::new();

    match (section, field) {
        ("server", "port") => {
            if let Value::Integer(port) = value {
                if *port < 1 || *port > 65535 {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!("port must be between 1 and 65535, got {port}"),
                        },
                    ));
                }
            }
        }
        ("logging", "level") => {
            if let Value::String(level) = value {
                const VALID: &[&str] = &["trace", "debug", "info", "warn", "error"];
                if !VALID.contains(&level.to_lowercase().as_str()) {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!(
                                "unknown log level '{level}'; expected one of: trace, debug, info, warn, error"
                            ),
                        },
                    ));
                }
            }
        }
        ("provider", "protocol") => {
            if let Value::String(proto) = value {
                const VALID: &[&str] = &["openai", "anthropic", "google", "openrouter"];
                if !VALID.contains(&proto.to_lowercase().as_str()) {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!(
                                "unknown protocol '{proto}'; expected one of: openai, anthropic, google, openrouter"
                            ),
                        },
                    ));
                }
            }
        }
        ("provider", "model") => {
            // A model name that is present but empty is a constraint violation.
            if let Value::String(model) = value {
                if model.trim().is_empty() {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: "provider.model must not be empty".to_string(),
                        },
                    ));
                }
            }
        }
        _ => {}
    }

    problems
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Validate TOML content against the closed config schema, collecting all
/// problems rather than stopping at the first.
///
/// Phase 1 — syntax: a parse error is returned immediately (can't walk
/// unparseable TOML).
/// Phase 2 — schema + semantics: unknown sections, unknown fields, type
/// mismatches, and constraint violations are all collected.
pub fn validate_toml_string(content: &str, file_path: &Path) -> Vec<ConfigProblem> {
    // Phase 1: syntax
    let value = match content.parse::<Value>() {
        Ok(v) => v,
        Err(e) => {
            let line = e
                .span()
                .map(|span| byte_to_line(content, span.start))
                .unwrap_or(1);
            return vec![make_problem(
                file_path,
                None,
                ConfigProblemKind::ParseError {
                    line,
                    message: e.message().to_string(),
                },
            )];
        }
    };

    let table = match value {
        Value::Table(t) => t,
        // Valid TOML that isn't a table is unusual; nothing to validate.
        _ => return vec![],
    };

    let mut problems = Vec::new();

    // Phase 2: schema and semantics
    for (section_name, section_value) in &table {
        if !KNOWN_SECTIONS.contains(&section_name.as_str()) {
            problems.push(make_problem(
                file_path,
                None,
                ConfigProblemKind::UnknownSection { name: section_name.clone() },
            ));
            // No point inspecting fields in an unknown section.
            continue;
        }

        let section_table = match section_value {
            Value::Table(t) => t,
            other => {
                problems.push(make_problem(
                    file_path,
                    None,
                    ConfigProblemKind::TypeError {
                        expected: "table".to_string(),
                        found: toml_type_name(other).to_string(),
                    },
                ));
                continue;
            }
        };

        for (field_name, field_value) in section_table {
            let field_path_str = format!("{section_name}.{field_name}");
            let known = known_fields_for(section_name);

            if !known.contains(&field_name.as_str()) {
                problems.push(make_problem(
                    file_path,
                    Some(field_path_str.clone()),
                    ConfigProblemKind::UnknownField {
                        section: section_name.clone(),
                        name: field_name.clone(),
                    },
                ));
                continue;
            }

            // Type check — skip semantic check if the type is wrong.
            if let Some(type_problem) =
                check_field_type(section_name, field_name, field_value, file_path, &field_path_str)
            {
                problems.push(type_problem);
                continue;
            }

            // Semantic check.
            problems.extend(check_field_semantics(
                section_name,
                field_name,
                field_value,
                file_path,
                &field_path_str,
            ));
        }
    }

    problems
}

/// Parse and validate TOML content, converting to `PartialConfig` on success.
///
/// Returns `Err` with all collected problems if any validation fails.
/// Returns `Ok(PartialConfig)` when the content is syntactically and
/// semantically valid.
pub fn parse_and_validate(content: &str, file_path: &Path) -> Result<PartialConfig, Vec<ConfigProblem>> {
    let problems = validate_toml_string(content, file_path);
    if !problems.is_empty() {
        return Err(problems);
    }

    // Validation passed — safe to deserialize. All types and sections are
    // schema-conformant so serde deserialization should not fail.
    toml::from_str(content).map_err(|e| {
        // This branch should never be reached after successful validation;
        // guard it anyway so errors surface rather than panic.
        vec![make_problem(
            file_path,
            None,
            ConfigProblemKind::ParseError {
                line: 1,
                message: format!("unexpected deserialization error: {e}"),
            },
        )]
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path(s: &str) -> PathBuf { s.into() }

    // --- validate_toml_string ---

    #[test]
    fn malformed_toml_produces_parse_error() {
        let problems = validate_toml_string("this is not valid toml ===", &path("config.toml"));
        assert!(!problems.is_empty(), "should produce at least one error");
        assert!(
            matches!(problems[0].kind, ConfigProblemKind::ParseError { .. }),
            "should be a ParseError, got {:?}", problems[0].kind
        );
    }

    #[test]
    fn parse_error_includes_line_number() {
        let toml = "key = \n===broken";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(!problems.is_empty());
        if let ConfigProblemKind::ParseError { line, .. } = &problems[0].kind {
            assert!(*line > 0, "line number should be positive, got {line}");
        } else {
            panic!("expected ParseError");
        }
    }

    #[test]
    fn unknown_section_produces_error() {
        let toml = "[server]\nport = 3000\n\n[databse]\nurl = \"test\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        let found = problems.iter().any(|p| {
            matches!(&p.kind, ConfigProblemKind::UnknownSection { name } if name == "databse")
        });
        assert!(found, "should report unknown section 'databse'");
    }

    #[test]
    fn unknown_field_produces_error() {
        let toml = "[server]\nport = 3000\nprotocol = \"tcp\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        let found = problems.iter().any(|p| {
            matches!(&p.kind, ConfigProblemKind::UnknownField { section, name }
                if section == "server" && name == "protocol")
        });
        assert!(found, "should report unknown field 'protocol' in [server]");
    }

    #[test]
    fn wrong_type_produces_type_error() {
        let toml = "[server]\nport = \"not a number\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.iter().any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "should report TypeError for port as string"
        );
    }

    #[test]
    fn multiple_errors_all_collected() {
        let toml = "[server]\nport = \"bad\"\n\n[unknown_section]\nfoo = 1";
        let problems = validate_toml_string(toml, &path("config.toml"));
        let has_type_err = problems.iter().any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. }));
        let has_unknown  = problems.iter().any(|p| matches!(&p.kind, ConfigProblemKind::UnknownSection { .. }));
        assert!(has_type_err, "should report TypeError");
        assert!(has_unknown,  "should report UnknownSection");
    }

    #[test]
    fn valid_toml_produces_no_errors() {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[provider]
protocol = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(problems.is_empty(), "valid TOML should produce no errors, got: {problems:?}");
    }

    #[test]
    fn port_out_of_range_produces_semantic_error() {
        let toml = "[server]\nport = 0";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.iter().any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "port=0 should produce SemanticError"
        );
    }

    #[test]
    fn invalid_log_level_produces_semantic_error() {
        let toml = "[logging]\nlevel = \"verbose\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.iter().any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "'verbose' is not a valid log level"
        );
    }

    #[test]
    fn invalid_protocol_produces_semantic_error() {
        let toml = "[provider]\nprotocol = \"unknown-llm\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.iter().any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "'unknown-llm' is not a valid protocol"
        );
    }

    // --- parse_and_validate ---

    #[test]
    fn parse_and_validate_returns_partial_config_on_valid_input() {
        let toml = r#"
[server]
host = "10.0.0.1"
port = 8080

[provider]
model = "claude-opus"
"#;
        let partial = parse_and_validate(toml, &path("config.toml"))
            .expect("valid TOML should return Ok");
        let server = partial.server.expect("server section should be present");
        assert_eq!(server.host.as_deref(), Some("10.0.0.1"));
        assert_eq!(server.port, Some(8080));
        let provider = partial.provider.expect("provider section should be present");
        assert_eq!(provider.model.as_deref(), Some("claude-opus"));
    }

    #[test]
    fn parse_and_validate_returns_err_on_invalid_input() {
        let toml = "[server]\nport = \"bad\"";
        let result = parse_and_validate(toml, &path("config.toml"));
        assert!(result.is_err(), "invalid TOML should return Err");
        assert!(!result.unwrap_err().is_empty());
    }
}
