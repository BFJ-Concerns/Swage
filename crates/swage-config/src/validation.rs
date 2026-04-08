use crate::error::{ConfigProblem, ConfigProblemKind};
use crate::merge::PartialConfig;
use std::path::Path;
use toml::Value;

// ---------------------------------------------------------------------------
// Schema constants
// ---------------------------------------------------------------------------

/// Top-level TOML sections recognised by the config schema.
const KNOWN_SECTIONS: &[&str] = &[
    "server",
    "provider",
    "workspace",
    "sandbox",
    "lifecycle",
    "logging",
    "tools",
    "context",
    "pricing",
    "observability",
    "output",
    "hitl",
    "communication",
];

/// Valid field names for each known section.
fn known_fields_for(section: &str) -> &'static [&'static str] {
    match section {
        "server" => &["host", "port"],
        "provider" => &["protocol", "model", "api_key_env"],
        "workspace" => &["path"],
        "sandbox" => &["enabled"],
        "lifecycle" => &["idle_timeout"],
        "logging" => &["level"],
        // Scalar tool settings plus flattened permission fields (allow, deny,
        // bundles, require_approval) which live at the [tools] level in TOML.
        "tools" => &[
            "shell_timeout_secs",
            "python_execute",
            "max_output_bytes",
            "plugin_directory",
            "search",
            "allow",
            "deny",
            "bundles",
            "require_approval",
        ],
        "context" => &["assembly", "timeout", "budget"],
        // pricing.models is a dynamic table of model-name → pricing entry;
        // field-level validation is skipped for the per-model sub-tables.
        "pricing" => &["models"],
        "observability" => &["force_on"],
        "output" => &["schema", "max_retries"],
        "hitl" => &["timeout", "timeout_policy"],
        // context_transfer is a dynamic sub-table — field-level validation
        // skipped for its contents, same as pricing.models.
        "communication" => &[
            "context_transfer",
            "message_ttl_secs",
            "expiry_interval_secs",
        ],
        _ => &[],
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

fn make_problem(
    file_path: &Path,
    field_path: Option<String>,
    kind: ConfigProblemKind,
) -> ConfigProblem {
    ConfigProblem {
        file_path: file_path.to_path_buf(),
        field_path,
        kind,
    }
}

fn toml_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "boolean",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::Table(_) => "table",
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
        ("server", "host") => ("string", matches!(value, Value::String(_))),
        ("server", "port") => ("integer", matches!(value, Value::Integer(_))),
        ("provider", "protocol") => ("string", matches!(value, Value::String(_))),
        ("provider", "model") => ("string", matches!(value, Value::String(_))),
        ("provider", "api_key_env") => ("string", matches!(value, Value::String(_))),
        ("workspace", "path") => ("string", matches!(value, Value::String(_))),
        ("sandbox", "enabled") => ("boolean", matches!(value, Value::Boolean(_))),
        ("lifecycle", "idle_timeout") => ("integer", matches!(value, Value::Integer(_))),
        ("logging", "level") => ("string", matches!(value, Value::String(_))),
        ("tools", "shell_timeout_secs") => ("integer", matches!(value, Value::Integer(_))),
        ("tools", "python_execute") => ("table", matches!(value, Value::Table(_))),
        ("tools", "max_output_bytes") => ("integer", matches!(value, Value::Integer(_))),
        ("tools", "plugin_directory") => ("string", matches!(value, Value::String(_))),
        ("tools", "search") => ("table", matches!(value, Value::Table(_))),
        ("context", "assembly") => ("string", matches!(value, Value::String(_))),
        ("context", "timeout") => ("integer", matches!(value, Value::Integer(_))),
        ("context", "budget") => ("integer", matches!(value, Value::Integer(_))),
        // pricing.models is a dynamic map — validate only that it is a table.
        ("pricing", "models") => ("table", matches!(value, Value::Table(_))),
        ("observability", "force_on") => ("boolean", matches!(value, Value::Boolean(_))),
        // [tools] permission fields — flat under [tools] in TOML
        ("tools", "allow") => ("array", matches!(value, Value::Array(_))),
        ("tools", "deny") => ("array", matches!(value, Value::Array(_))),
        ("tools", "bundles") => ("table", matches!(value, Value::Table(_))),
        ("tools", "require_approval") => ("array", matches!(value, Value::Array(_))),
        // [output] — schema validation settings
        ("output", "schema") => ("table", matches!(value, Value::Table(_))),
        ("output", "max_retries") => ("integer", matches!(value, Value::Integer(_))),
        // [hitl] — human-in-the-loop settings
        ("hitl", "timeout") => ("integer", matches!(value, Value::Integer(_))),
        ("hitl", "timeout_policy") => ("string", matches!(value, Value::String(_))),
        // [communication] — inter-agent messaging settings
        ("communication", "context_transfer") => ("table", matches!(value, Value::Table(_))),
        ("communication", "message_ttl_secs") => ("integer", matches!(value, Value::Integer(_))),
        ("communication", "expiry_interval_secs") => {
            ("integer", matches!(value, Value::Integer(_)))
        }
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
        ("context", "timeout") => {
            if let Value::Integer(t) = value {
                if *t < 1 || *t > 60 {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!("context.timeout must be between 1 and 60, got {t}"),
                        },
                    ));
                }
            }
        }
        ("context", "budget") => {
            if let Value::Integer(b) = value {
                if *b < 1 {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!("context.budget must be a positive integer, got {b}"),
                        },
                    ));
                }
            }
        }
        ("hitl", "timeout") => {
            if let Value::Integer(t) = value {
                if *t < 1 {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!("hitl.timeout must be a positive integer, got {t}"),
                        },
                    ));
                }
            }
        }
        ("hitl", "timeout_policy") => {
            if let Value::String(policy) = value {
                const VALID: &[&str] = &["reject", "interrupt"];
                if !VALID.contains(&policy.to_lowercase().as_str()) {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!(
                                "unknown timeout policy '{policy}'; expected one of: reject, interrupt"
                            ),
                        },
                    ));
                }
            }
        }
        ("output", "max_retries") => {
            if let Value::Integer(r) = value {
                if *r < 0 {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!("output.max_retries must be non-negative, got {r}"),
                        },
                    ));
                }
            }
        }
        ("communication", "message_ttl_secs") => {
            if let Value::Integer(t) = value {
                if *t < 1 {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!(
                                "communication.message_ttl_secs must be a positive integer, got {t}"
                            ),
                        },
                    ));
                }
            }
        }
        ("communication", "expiry_interval_secs") => {
            if let Value::Integer(t) = value {
                if *t < 1 {
                    problems.push(make_problem(
                        file_path,
                        Some(field_path.to_string()),
                        ConfigProblemKind::SemanticError {
                            message: format!(
                                "communication.expiry_interval_secs must be a positive integer, got {t}"
                            ),
                        },
                    ));
                }
            }
        }
        ("tools", "python_execute") => {
            if let Value::Table(table) = value {
                problems.extend(validate_python_execute_table(table, file_path, field_path));
            }
        }
        _ => {}
    }

    problems
}

fn validate_python_execute_table(
    table: &toml::map::Map<String, Value>,
    file_path: &Path,
    field_path: &str,
) -> Vec<ConfigProblem> {
    const KNOWN_FIELDS: &[&str] = &[
        "enabled",
        "timeout_secs",
        "startup_timeout_secs",
        "idle_ttl_secs",
        "warm_target",
        "max_workers",
        "runtime_policy",
        "runtime_path",
        "require_sandbox",
    ];

    let mut problems = Vec::new();

    for (name, value) in table {
        let nested_path = format!("{field_path}.{name}");
        if !KNOWN_FIELDS.contains(&name.as_str()) {
            problems.push(make_problem(
                file_path,
                Some(nested_path),
                ConfigProblemKind::UnknownField {
                    section: "tools.python_execute".to_string(),
                    name: name.clone(),
                },
            ));
            continue;
        }

        let type_ok = match name.as_str() {
            "enabled" | "require_sandbox" => matches!(value, Value::Boolean(_)),
            "timeout_secs" | "startup_timeout_secs" | "idle_ttl_secs" => {
                matches!(value, Value::Integer(_))
            }
            "warm_target" | "max_workers" => matches!(value, Value::Integer(_)),
            "runtime_policy" | "runtime_path" => matches!(value, Value::String(_)),
            _ => true,
        };

        if !type_ok {
            let expected = match name.as_str() {
                "enabled" | "require_sandbox" => "boolean",
                "runtime_policy" | "runtime_path" => "string",
                _ => "integer",
            };
            problems.push(make_problem(
                file_path,
                Some(nested_path),
                ConfigProblemKind::TypeError {
                    expected: expected.to_string(),
                    found: toml_type_name(value).to_string(),
                },
            ));
            continue;
        }

        match (name.as_str(), value) {
            ("timeout_secs" | "startup_timeout_secs" | "idle_ttl_secs", Value::Integer(raw))
                if *raw < 1 =>
            {
                problems.push(make_problem(
                    file_path,
                    Some(nested_path),
                    ConfigProblemKind::SemanticError {
                        message: format!("{name} must be at least 1, got {raw}"),
                    },
                ));
            }
            ("warm_target" | "max_workers", Value::Integer(raw)) if *raw < 1 => {
                problems.push(make_problem(
                    file_path,
                    Some(nested_path),
                    ConfigProblemKind::SemanticError {
                        message: format!("{name} must be at least 1, got {raw}"),
                    },
                ));
            }
            ("runtime_policy", Value::String(policy))
                if policy.to_lowercase() != "bundled_then_system" =>
            {
                problems.push(make_problem(
                    file_path,
                    Some(nested_path),
                    ConfigProblemKind::SemanticError {
                        message: format!(
                            "unknown runtime_policy '{policy}'; expected bundled_then_system"
                        ),
                    },
                ));
            }
            _ => {}
        }
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
                ConfigProblemKind::UnknownSection {
                    name: section_name.clone(),
                },
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
            if let Some(type_problem) = check_field_type(
                section_name,
                field_name,
                field_value,
                file_path,
                &field_path_str,
            ) {
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
pub fn parse_and_validate(
    content: &str,
    file_path: &Path,
) -> Result<PartialConfig, Vec<ConfigProblem>> {
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

    fn path(s: &str) -> PathBuf {
        s.into()
    }

    // --- validate_toml_string ---

    #[test]
    fn malformed_toml_produces_parse_error() {
        let problems = validate_toml_string("this is not valid toml ===", &path("config.toml"));
        assert!(!problems.is_empty(), "should produce at least one error");
        assert!(
            matches!(problems[0].kind, ConfigProblemKind::ParseError { .. }),
            "should be a ParseError, got {:?}",
            problems[0].kind
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
        let found = problems.iter().any(
            |p| matches!(&p.kind, ConfigProblemKind::UnknownSection { name } if name == "databse"),
        );
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
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "should report TypeError for port as string"
        );
    }

    #[test]
    fn multiple_errors_all_collected() {
        let toml = "[server]\nport = \"bad\"\n\n[unknown_section]\nfoo = 1";
        let problems = validate_toml_string(toml, &path("config.toml"));
        let has_type_err = problems
            .iter()
            .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. }));
        let has_unknown = problems
            .iter()
            .any(|p| matches!(&p.kind, ConfigProblemKind::UnknownSection { .. }));
        assert!(has_type_err, "should report TypeError");
        assert!(has_unknown, "should report UnknownSection");
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
        assert!(
            problems.is_empty(),
            "valid TOML should produce no errors, got: {problems:?}"
        );
    }

    #[test]
    fn port_out_of_range_produces_semantic_error() {
        let toml = "[server]\nport = 0";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "port=0 should produce SemanticError"
        );
    }

    #[test]
    fn invalid_log_level_produces_semantic_error() {
        let toml = "[logging]\nlevel = \"verbose\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "'verbose' is not a valid log level"
        );
    }

    #[test]
    fn invalid_protocol_produces_semantic_error() {
        let toml = "[provider]\nprotocol = \"unknown-llm\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
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
        let partial =
            parse_and_validate(toml, &path("config.toml")).expect("valid TOML should return Ok");
        let server = partial.server.expect("server section should be present");
        assert_eq!(server.host.as_deref(), Some("10.0.0.1"));
        assert_eq!(server.port, Some(8080));
        let provider = partial
            .provider
            .expect("provider section should be present");
        assert_eq!(provider.model.as_deref(), Some("claude-opus"));
    }

    #[test]
    fn parse_and_validate_returns_err_on_invalid_input() {
        let toml = "[server]\nport = \"bad\"";
        let result = parse_and_validate(toml, &path("config.toml"));
        assert!(result.is_err(), "invalid TOML should return Err");
        assert!(!result.unwrap_err().is_empty());
    }

    // --- [tools] and [context] section tests ---

    #[test]
    fn tools_section_validates_without_warnings() {
        let toml = r#"
[tools]
shell_timeout_secs = 60
max_output_bytes = 50000
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "valid [tools] config should produce no errors, got: {problems:?}"
        );
    }

    #[test]
    fn context_section_validates_without_warnings() {
        let toml = r#"
[context]
assembly = "default"
timeout = 10
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "valid [context] config should produce no errors, got: {problems:?}"
        );
    }

    #[test]
    fn tools_shell_timeout_wrong_type_produces_error() {
        let toml = "[tools]\nshell_timeout_secs = \"sixty\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "string value for shell_timeout_secs should produce TypeError"
        );
    }

    #[test]
    fn context_assembly_wrong_type_produces_error() {
        let toml = "[context]\nassembly = 42";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "integer value for context.assembly should produce TypeError"
        );
    }

    #[test]
    fn context_timeout_zero_produces_semantic_error() {
        // timeout range is 1-60; 0 is out of range
        let toml = "[context]\ntimeout = 0";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "timeout=0 should produce SemanticError"
        );
    }

    #[test]
    fn context_timeout_above_max_produces_semantic_error() {
        let toml = "[context]\ntimeout = 61";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "timeout=61 should produce SemanticError"
        );
    }

    #[test]
    fn context_timeout_boundary_valid() {
        // 1 and 60 are the inclusive bounds — should produce no errors
        for timeout in [1_i64, 60] {
            let toml = format!("[context]\ntimeout = {timeout}");
            let problems = validate_toml_string(&toml, &path("config.toml"));
            assert!(
                problems.is_empty(),
                "timeout={timeout} is a valid boundary value, got: {problems:?}"
            );
        }
    }

    #[test]
    fn context_budget_zero_produces_semantic_error() {
        // budget must be a positive integer (> 0)
        let toml = "[context]\nbudget = 0";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "budget=0 should produce SemanticError"
        );
    }

    #[test]
    fn context_budget_negative_produces_semantic_error() {
        let toml = "[context]\nbudget = -1";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "budget=-1 should produce SemanticError"
        );
    }

    #[test]
    fn context_budget_positive_is_valid() {
        let toml = "[context]\nbudget = 4096";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "budget=4096 is valid, got: {problems:?}"
        );
    }

    // --- [output] section tests ---

    #[test]
    fn output_section_validates_without_warnings() {
        let toml = "[output]\nmax_retries = 5\n";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "valid [output] should produce no errors, got: {problems:?}"
        );
    }

    #[test]
    fn output_max_retries_wrong_type_produces_error() {
        let toml = "[output]\nmax_retries = \"five\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "string value for output.max_retries should produce TypeError"
        );
    }

    #[test]
    fn output_max_retries_negative_produces_semantic_error() {
        let toml = "[output]\nmax_retries = -1";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "negative max_retries should produce SemanticError"
        );
    }

    #[test]
    fn output_schema_must_be_table() {
        let toml = "[output]\nschema = \"not a table\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "string value for output.schema should produce TypeError"
        );
    }

    #[test]
    fn output_unknown_field_produces_error() {
        let toml = "[output]\nfoo = 42";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::UnknownField { .. })),
            "unknown field in [output] should produce error"
        );
    }

    // --- [hitl] section tests ---

    #[test]
    fn hitl_section_validates_without_warnings() {
        let toml = "[hitl]\ntimeout = 300\ntimeout_policy = \"reject\"\n";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "valid [hitl] should produce no errors, got: {problems:?}"
        );
    }

    #[test]
    fn hitl_timeout_wrong_type_produces_error() {
        let toml = "[hitl]\ntimeout = \"long\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "string value for hitl.timeout should produce TypeError"
        );
    }

    #[test]
    fn hitl_timeout_zero_produces_semantic_error() {
        let toml = "[hitl]\ntimeout = 0";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "hitl.timeout=0 should produce SemanticError"
        );
    }

    #[test]
    fn hitl_timeout_policy_wrong_type_produces_error() {
        let toml = "[hitl]\ntimeout_policy = 42";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "integer value for hitl.timeout_policy should produce TypeError"
        );
    }

    #[test]
    fn hitl_timeout_policy_invalid_produces_semantic_error() {
        let toml = "[hitl]\ntimeout_policy = \"explode\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "'explode' is not a valid timeout policy"
        );
    }

    #[test]
    fn hitl_timeout_policy_interrupt_is_valid() {
        let toml = "[hitl]\ntimeout_policy = \"interrupt\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "interrupt is a valid policy, got: {problems:?}"
        );
    }

    // --- [communication] section tests ---

    #[test]
    fn communication_section_validates_without_warnings() {
        let toml = "[communication]\nmessage_ttl_secs = 86400\nexpiry_interval_secs = 60\n";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "valid [communication] should produce no errors, got: {problems:?}"
        );
    }

    #[test]
    fn communication_message_ttl_wrong_type_produces_error() {
        let toml = "[communication]\nmessage_ttl_secs = \"one day\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "string value for message_ttl_secs should produce TypeError"
        );
    }

    #[test]
    fn communication_message_ttl_zero_produces_semantic_error() {
        let toml = "[communication]\nmessage_ttl_secs = 0";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "message_ttl_secs=0 should produce SemanticError"
        );
    }

    #[test]
    fn communication_context_transfer_must_be_table() {
        let toml = "[communication]\ncontext_transfer = \"not a table\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "string value for context_transfer should produce TypeError"
        );
    }

    #[test]
    fn communication_unknown_field_produces_error() {
        let toml = "[communication]\nbogus = 42";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::UnknownField { .. })),
            "unknown field in [communication] should produce error"
        );
    }

    // --- [tools] permission fields ---

    #[test]
    fn tools_allow_field_validates() {
        let toml = "[tools]\nallow = [\"*\"]";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "tools.allow should be a valid field, got: {problems:?}"
        );
    }

    #[test]
    fn tools_deny_field_validates() {
        let toml = "[tools]\ndeny = [\"shell\"]";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "tools.deny should be a valid field, got: {problems:?}"
        );
    }

    #[test]
    fn tools_allow_wrong_type_produces_error() {
        let toml = "[tools]\nallow = \"not an array\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "string value for tools.allow should produce TypeError"
        );
    }

    // --- combined section test ---

    #[test]
    fn all_sections_validate_together() {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[provider]
protocol = "anthropic"
model = "claude-sonnet-4-20250514"

[output]
max_retries = 3

[hitl]
timeout = 300
timeout_policy = "reject"

[communication]
message_ttl_secs = 86400
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "valid config with all sections should produce no errors, got: {problems:?}"
        );
    }

    #[test]
    fn tools_search_must_be_table() {
        let toml = "[tools]\nsearch = \"http://localhost\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::TypeError { .. })),
            "string value for tools.search should produce TypeError"
        );
    }
}
