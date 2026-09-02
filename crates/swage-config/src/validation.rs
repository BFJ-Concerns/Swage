use crate::error::{ConfigProblem, ConfigProblemKind};
use crate::merge::PartialConfig;
use crate::model::ResolvedConfig;
use std::path::Path;
use toml::{Table, Value};

// ---------------------------------------------------------------------------
// Schema constants
// ---------------------------------------------------------------------------

/// Top-level TOML sections recognised by the config schema.
const KNOWN_SECTIONS: &[&str] = &[
    "server",
    "providers",
    "models",
    "sandbox",
    "logging",
    "tools",
    "context",
    "pricing",
    "generation",
    "observability",
    "experimentation",
    "output",
    "hitl",
    "communication",
    "scheduling",
    "channels",
];

/// Top-level scalar keys (i.e. fields that live above any [section] header).
/// These are recognised alongside `KNOWN_SECTIONS` during validation.
const KNOWN_TOP_LEVEL_SCALARS: &[(&str, &str)] = &[("dangerously_skip_permissions", "boolean")];

/// Valid field names for each known section.
fn known_fields_for(section: &str) -> &'static [&'static str] {
    match section {
        "server" => &["host", "port"],
        "providers" => &["default", "entries"],
        "models" => &["default", "default_model_id", "entries"],
        "sandbox" => &["enabled", "allow_degraded_execution"],
        "logging" => &["level"],
        // Scalar tool settings plus flattened permission fields (allow, deny,
        // bundles, require_approval) which live at the [tools] level in TOML.
        "tools" => &[
            "shell_timeout_secs",
            "python_execute",
            "max_output_bytes",
            "plugin_directory",
            "search",
            "browser",
            "allow",
            "deny",
            "bundles",
            "require_approval",
        ],
        "context" => &["assembly", "alternatives", "timeout", "budget"],
        // pricing.models is a dynamic table of model-name → pricing entry;
        // field-level validation is skipped for the per-model sub-tables.
        "pricing" => &["models"],
        "generation" => &["presets"],
        "observability" => &["force_on"],
        // `locations` is a dynamic table of location-id → controls; nested
        // fields are validated by `validate_experimentation_locations_table`.
        "experimentation" => &["enabled", "locations"],
        "output" => &["schema", "max_retries"],
        "hitl" => &["timeout", "timeout_policy"],
        // context_transfer is a dynamic sub-table — field-level validation
        // skipped for its contents, same as pricing.models.
        "communication" => &[
            "context_transfer",
            "message_ttl_secs",
            "expiry_interval_secs",
        ],
        "scheduling" => &["run_history_max_age_days", "run_history_max_per_schedule"],
        // telegram has a fixed nested schema validated by
        // `validate_channels_telegram_table`.
        "channels" => &["telegram"],
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
        ("providers", "default") => ("string", matches!(value, Value::String(_))),
        ("providers", "entries") => ("table", matches!(value, Value::Table(_))),
        ("models", "default") => ("string", matches!(value, Value::String(_))),
        ("models", "default_model_id") => ("string", matches!(value, Value::String(_))),
        ("models", "entries") => ("table", matches!(value, Value::Table(_))),
        ("sandbox", "enabled" | "allow_degraded_execution") => {
            ("boolean", matches!(value, Value::Boolean(_)))
        }
        ("logging", "level") => ("string", matches!(value, Value::String(_))),
        ("tools", "shell_timeout_secs") => ("integer", matches!(value, Value::Integer(_))),
        ("tools", "python_execute") => ("table", matches!(value, Value::Table(_))),
        ("tools", "max_output_bytes") => ("integer", matches!(value, Value::Integer(_))),
        ("tools", "plugin_directory") => ("string", matches!(value, Value::String(_))),
        ("tools", "search") => ("table", matches!(value, Value::Table(_))),
        ("tools", "browser") => ("table", matches!(value, Value::Table(_))),
        ("context", "assembly") => ("string", matches!(value, Value::String(_))),
        ("context", "alternatives") => ("array", matches!(value, Value::Array(_))),
        ("context", "timeout") => ("integer", matches!(value, Value::Integer(_))),
        ("context", "budget") => ("integer", matches!(value, Value::Integer(_))),
        // pricing.models is a dynamic map — validate only that it is a table.
        ("pricing", "models") => ("table", matches!(value, Value::Table(_))),
        // generation.presets is a dynamic map of preset name → preset config.
        ("generation", "presets") => ("table", matches!(value, Value::Table(_))),
        ("observability", "force_on") => ("boolean", matches!(value, Value::Boolean(_))),
        ("experimentation", "enabled") => ("boolean", matches!(value, Value::Boolean(_))),
        ("experimentation", "locations") => ("table", matches!(value, Value::Table(_))),
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
        ("scheduling", "run_history_max_age_days") => {
            ("integer", matches!(value, Value::Integer(_)))
        }
        ("scheduling", "run_history_max_per_schedule") => {
            ("integer", matches!(value, Value::Integer(_)))
        }
        ("channels", "telegram") => ("table", matches!(value, Value::Table(_))),
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
            if let Value::Integer(port) = value
                && (*port < 1 || *port > 65535)
            {
                problems.push(make_problem(
                    file_path,
                    Some(field_path.to_string()),
                    ConfigProblemKind::SemanticError {
                        message: format!("port must be between 1 and 65535, got {port}"),
                    },
                ));
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
        ("models", "default_model_id") => {
            if let Value::String(model_id) = value
                && model_id.trim().is_empty()
            {
                problems.push(make_problem(
                    file_path,
                    Some(field_path.to_string()),
                    ConfigProblemKind::SemanticError {
                        message: "models.default_model_id must not be empty".to_string(),
                    },
                ));
            }
        }
        ("context", "timeout") => {
            if let Value::Integer(t) = value
                && (*t < 1 || *t > 60)
            {
                problems.push(make_problem(
                    file_path,
                    Some(field_path.to_string()),
                    ConfigProblemKind::SemanticError {
                        message: format!("context.timeout must be between 1 and 60, got {t}"),
                    },
                ));
            }
        }
        ("context", "budget") => {
            if let Value::Integer(b) = value
                && *b < 1
            {
                problems.push(make_problem(
                    file_path,
                    Some(field_path.to_string()),
                    ConfigProblemKind::SemanticError {
                        message: format!("context.budget must be a positive integer, got {b}"),
                    },
                ));
            }
        }
        ("hitl", "timeout") => {
            if let Value::Integer(t) = value
                && *t < 1
            {
                problems.push(make_problem(
                    file_path,
                    Some(field_path.to_string()),
                    ConfigProblemKind::SemanticError {
                        message: format!("hitl.timeout must be a positive integer, got {t}"),
                    },
                ));
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
            if let Value::Integer(r) = value
                && *r < 0
            {
                problems.push(make_problem(
                    file_path,
                    Some(field_path.to_string()),
                    ConfigProblemKind::SemanticError {
                        message: format!("output.max_retries must be non-negative, got {r}"),
                    },
                ));
            }
        }
        ("communication", "message_ttl_secs") => {
            if let Value::Integer(t) = value
                && *t < 1
            {
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
        ("communication", "expiry_interval_secs") => {
            if let Value::Integer(t) = value
                && *t < 1
            {
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
        ("communication", "context_transfer") => {
            if let Value::Table(table) = value {
                problems.extend(validate_context_transfer_table(
                    table, file_path, field_path,
                ));
            }
        }
        ("scheduling", "run_history_max_age_days") => {
            if let Value::Integer(t) = value
                && *t < 1
            {
                problems.push(make_problem(
                    file_path,
                    Some(field_path.to_string()),
                    ConfigProblemKind::SemanticError {
                        message: format!(
                            "scheduling.run_history_max_age_days must be a positive integer, got {t}"
                        ),
                    },
                ));
            }
        }
        ("scheduling", "run_history_max_per_schedule") => {
            if let Value::Integer(t) = value
                && *t < 1
            {
                problems.push(make_problem(
                    file_path,
                    Some(field_path.to_string()),
                    ConfigProblemKind::SemanticError {
                        message: format!(
                            "scheduling.run_history_max_per_schedule must be a positive integer, got {t}"
                        ),
                    },
                ));
            }
        }
        ("experimentation", "locations") => {
            if let Value::Table(table) = value {
                problems.extend(validate_experimentation_locations_table(
                    table, file_path, field_path,
                ));
            }
        }
        ("tools", "python_execute") => {
            if let Value::Table(table) = value {
                problems.extend(validate_python_execute_table(table, file_path, field_path));
            }
        }
        ("channels", "telegram") => {
            if let Value::Table(table) = value {
                problems.extend(validate_channels_telegram_table(
                    table, file_path, field_path,
                ));
            }
        }
        _ => {}
    }

    problems
}

fn validate_channels_telegram_table(
    table: &toml::map::Map<String, Value>,
    file_path: &Path,
    field_path: &str,
) -> Vec<ConfigProblem> {
    const KNOWN_FIELDS: &[&str] = &[
        "bot_token_secret_ref",
        "bot_token_env",
        "api_base_url",
        "poll_timeout_secs",
    ];

    let mut problems = Vec::new();

    for (name, value) in table {
        let nested_path = format!("{field_path}.{name}");
        if !KNOWN_FIELDS.contains(&name.as_str()) {
            problems.push(make_problem(
                file_path,
                Some(nested_path),
                ConfigProblemKind::UnknownField {
                    section: field_path.to_string(),
                    name: name.clone(),
                },
            ));
            continue;
        }

        let (expected, type_ok) = match name.as_str() {
            "bot_token_secret_ref" | "bot_token_env" | "api_base_url" => {
                ("string", matches!(value, Value::String(_)))
            }
            "poll_timeout_secs" => ("integer", matches!(value, Value::Integer(_))),
            _ => continue,
        };

        if !type_ok {
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

        if let ("poll_timeout_secs", Value::Integer(timeout)) = (name.as_str(), value)
            && *timeout < 0
        {
            problems.push(make_problem(
                file_path,
                Some(nested_path),
                ConfigProblemKind::SemanticError {
                    message: format!("poll_timeout_secs must be non-negative, got {timeout}"),
                },
            ));
        }
    }

    problems
}

fn validate_experimentation_locations_table(
    table: &toml::map::Map<String, Value>,
    file_path: &Path,
    field_path: &str,
) -> Vec<ConfigProblem> {
    const LOCATION_FIELDS: &[&str] = &["enabled", "forced_variant"];

    let mut problems = Vec::new();

    for (location_id, value) in table {
        let location_path = format!("{field_path}.{location_id}");
        let Value::Table(location_table) = value else {
            problems.push(make_problem(
                file_path,
                Some(location_path),
                ConfigProblemKind::TypeError {
                    expected: "table".to_string(),
                    found: toml_type_name(value).to_string(),
                },
            ));
            continue;
        };

        for (name, value) in location_table {
            let nested_path = format!("{location_path}.{name}");
            if !LOCATION_FIELDS.contains(&name.as_str()) {
                problems.push(make_problem(
                    file_path,
                    Some(nested_path),
                    ConfigProblemKind::UnknownField {
                        section: location_path.clone(),
                        name: name.clone(),
                    },
                ));
                continue;
            }

            match (name.as_str(), value) {
                ("enabled", Value::Boolean(_)) => {}
                ("enabled", other) => problems.push(make_problem(
                    file_path,
                    Some(nested_path),
                    ConfigProblemKind::TypeError {
                        expected: "boolean".to_string(),
                        found: toml_type_name(other).to_string(),
                    },
                )),
                ("forced_variant", Value::String(variant)) if variant.trim().is_empty() => {
                    problems.push(make_problem(
                        file_path,
                        Some(nested_path),
                        ConfigProblemKind::SemanticError {
                            message: "forced_variant must not be empty".to_string(),
                        },
                    ));
                }
                ("forced_variant", Value::String(_)) => {}
                ("forced_variant", other) => problems.push(make_problem(
                    file_path,
                    Some(nested_path),
                    ConfigProblemKind::TypeError {
                        expected: "string".to_string(),
                        found: toml_type_name(other).to_string(),
                    },
                )),
                _ => {}
            }
        }
    }

    problems
}

fn validate_context_transfer_table(
    table: &toml::map::Map<String, Value>,
    file_path: &Path,
    field_path: &str,
) -> Vec<ConfigProblem> {
    let mut problems = validate_context_transfer_policy(table, file_path, field_path, true);

    if let Some(Value::Table(targets)) = table.get("targets") {
        for (target, value) in targets {
            let target_path = format!("{field_path}.targets.{target}");
            match value {
                Value::Table(target_table) => {
                    problems.extend(validate_context_transfer_policy(
                        target_table,
                        file_path,
                        &target_path,
                        false,
                    ));
                }
                other => problems.push(make_problem(
                    file_path,
                    Some(target_path),
                    ConfigProblemKind::TypeError {
                        expected: "table".to_string(),
                        found: toml_type_name(other).to_string(),
                    },
                )),
            }
        }
    }

    problems
}

fn validate_context_transfer_policy(
    table: &toml::map::Map<String, Value>,
    file_path: &Path,
    field_path: &str,
    allow_targets: bool,
) -> Vec<ConfigProblem> {
    const POLICY_FIELDS: &[&str] = &["mode", "allowed_fields", "summary_budget"];
    const VALID_MODES: &[&str] = &["none", "explicit", "summary"];
    const VALID_FIELDS: &[&str] = &["system", "user", "assistant"];

    let mut problems = Vec::new();

    for (name, value) in table {
        let is_known_field =
            POLICY_FIELDS.contains(&name.as_str()) || allow_targets && name == "targets";
        if !is_known_field {
            problems.push(make_problem(
                file_path,
                Some(format!("{field_path}.{name}")),
                ConfigProblemKind::UnknownField {
                    section: field_path.to_string(),
                    name: name.clone(),
                },
            ));
        }

        match (name.as_str(), value) {
            ("mode", Value::String(mode)) if !VALID_MODES.contains(&mode.as_str()) => {
                problems.push(make_problem(
                    file_path,
                    Some(format!("{field_path}.mode")),
                    ConfigProblemKind::SemanticError {
                        message: format!(
                            "unknown context transfer mode '{mode}'; expected one of: none, explicit, summary"
                        ),
                    },
                ));
            }
            ("mode", Value::String(_)) => {}
            ("mode", other) => problems.push(make_problem(
                file_path,
                Some(format!("{field_path}.mode")),
                ConfigProblemKind::TypeError {
                    expected: "string".to_string(),
                    found: toml_type_name(other).to_string(),
                },
            )),
            ("allowed_fields", Value::Array(fields)) => {
                for (index, field) in fields.iter().enumerate() {
                    match field {
                        Value::String(field) if VALID_FIELDS.contains(&field.as_str()) => {}
                        Value::String(field) => problems.push(make_problem(
                            file_path,
                            Some(format!("{field_path}.allowed_fields[{index}]")),
                            ConfigProblemKind::SemanticError {
                                message: format!(
                                    "unknown context transfer field '{field}'; expected one of: system, user, assistant"
                                ),
                            },
                        )),
                        other => problems.push(make_problem(
                            file_path,
                            Some(format!("{field_path}.allowed_fields[{index}]")),
                            ConfigProblemKind::TypeError {
                                expected: "string".to_string(),
                                found: toml_type_name(other).to_string(),
                            },
                        )),
                    }
                }
            }
            ("allowed_fields", other) => problems.push(make_problem(
                file_path,
                Some(format!("{field_path}.allowed_fields")),
                ConfigProblemKind::TypeError {
                    expected: "array".to_string(),
                    found: toml_type_name(other).to_string(),
                },
            )),
            ("summary_budget", Value::Integer(raw)) if *raw < 1 => {
                problems.push(make_problem(
                    file_path,
                    Some(format!("{field_path}.summary_budget")),
                    ConfigProblemKind::SemanticError {
                        message: format!("summary_budget must be a positive integer, got {raw}"),
                    },
                ));
            }
            ("summary_budget", Value::Integer(_)) => {}
            ("summary_budget", other) => problems.push(make_problem(
                file_path,
                Some(format!("{field_path}.summary_budget")),
                ConfigProblemKind::TypeError {
                    expected: "integer".to_string(),
                    found: toml_type_name(other).to_string(),
                },
            )),
            ("targets", Value::Table(_)) if allow_targets => {}
            ("targets", other) if allow_targets => problems.push(make_problem(
                file_path,
                Some(format!("{field_path}.targets")),
                ConfigProblemKind::TypeError {
                    expected: "table".to_string(),
                    found: toml_type_name(other).to_string(),
                },
            )),
            _ => {}
        }
    }

    let mode = table.get("mode").and_then(Value::as_str).unwrap_or("none");

    if mode != "explicit"
        && let Some(Value::Array(fields)) = table.get("allowed_fields")
        && !fields.is_empty()
    {
        problems.push(make_problem(
            file_path,
            Some(format!("{field_path}.allowed_fields")),
            ConfigProblemKind::SemanticError {
                message: "allowed_fields is only valid when mode is explicit".to_string(),
            },
        ));
    }

    if mode != "summary" && table.contains_key("summary_budget") {
        problems.push(make_problem(
            file_path,
            Some(format!("{field_path}.summary_budget")),
            ConfigProblemKind::SemanticError {
                message: "summary_budget is only valid when mode is summary".to_string(),
            },
        ));
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

fn validate_catalog_references(table: &Table, file_path: &Path) -> Vec<ConfigProblem> {
    let mut problems = Vec::new();

    if let Some(providers) = table.get("providers").and_then(Value::as_table)
        && let Some(default_provider) = providers.get("default").and_then(Value::as_str)
        && let Some(provider_entries) = providers.get("entries").and_then(Value::as_table)
        && !default_provider.is_empty()
        && !provider_entries.contains_key(default_provider)
    {
        problems.push(make_problem(
            file_path,
            Some("providers.default".to_string()),
            ConfigProblemKind::SemanticError {
                message: format!(
                    "providers.default references missing provider entry '{default_provider}'"
                ),
            },
        ));
    }

    let Some(models) = table.get("models").and_then(Value::as_table) else {
        return problems;
    };
    let Some(default_model) = models.get("default").and_then(Value::as_str) else {
        return problems;
    };
    if default_model.is_empty() {
        return problems;
    }
    let Some(model_entries) = models.get("entries").and_then(Value::as_table) else {
        return problems;
    };

    let Some(default_model_entry) = model_entries.get(default_model) else {
        problems.push(make_problem(
            file_path,
            Some("models.default".to_string()),
            ConfigProblemKind::SemanticError {
                message: format!("models.default references missing model entry '{default_model}'"),
            },
        ));
        return problems;
    };

    if let Some(provider_id) = default_model_entry
        .as_table()
        .and_then(|entry| entry.get("provider_id"))
        .and_then(Value::as_str)
        && let Some(providers) = table.get("providers").and_then(Value::as_table)
        && let Some(provider_entries) = providers.get("entries").and_then(Value::as_table)
        && !provider_entries.contains_key(provider_id)
    {
        problems.push(make_problem(
            file_path,
            Some(format!("models.entries.{default_model}.provider_id")),
            ConfigProblemKind::SemanticError {
                message: format!(
                    "default model '{default_model}' references missing provider entry '{provider_id}'"
                ),
            },
        ));
    }

    problems
}

/// Validate references that can only be known after all config layers merge.
pub fn validate_resolved_catalog_references(
    config: &ResolvedConfig,
    file_path: &Path,
) -> Vec<ConfigProblem> {
    let mut problems = Vec::new();

    if !config.providers.default.is_empty()
        && !config
            .providers
            .entries
            .contains_key(&config.providers.default)
    {
        problems.push(make_problem(
            file_path,
            Some("providers.default".to_string()),
            ConfigProblemKind::SemanticError {
                message: format!(
                    "providers.default references missing provider entry '{}'",
                    config.providers.default
                ),
            },
        ));
    }

    for (model_id, model) in &config.models.entries {
        if let Some(effort) = model.capabilities.effort.as_ref()
            && let Err(message) = effort.validate()
        {
            problems.push(make_problem(
                file_path,
                Some(format!("models.entries.{model_id}.capabilities.effort")),
                ConfigProblemKind::SemanticError {
                    message: format!("model '{model_id}' declares an effort capability that cannot be honoured: {message}"),
                },
            ));
        }
    }

    if config.models.default.is_empty() && config.models.entries.is_empty() {
        return problems;
    }

    let Some(default_model) = config.models.entries.get(&config.models.default) else {
        problems.push(make_problem(
            file_path,
            Some("models.default".to_string()),
            ConfigProblemKind::SemanticError {
                message: format!(
                    "models.default references missing model entry '{}'",
                    config.models.default
                ),
            },
        ));
        return problems;
    };

    if !config
        .providers
        .entries
        .contains_key(&default_model.provider_id)
        && !default_model.provider_id.is_empty()
    {
        problems.push(make_problem(
            file_path,
            Some(format!(
                "models.entries.{}.provider_id",
                config.models.default
            )),
            ConfigProblemKind::SemanticError {
                message: format!(
                    "default model '{}' references missing provider entry '{}'",
                    config.models.default, default_model.provider_id
                ),
            },
        ));
    }

    problems
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Validate TOML content against the closed config schema, collecting all
/// problems rather than stopping at the first.
///
/// Step 1 — syntax: a parse error is returned immediately (can't walk
/// unparseable TOML).
/// Step 2 — schema + semantics: unknown sections, unknown fields, type
/// mismatches, and constraint violations are all collected.
pub fn validate_toml_string(content: &str, file_path: &Path) -> Vec<ConfigProblem> {
    // Step 1: syntax
    let table = match content.parse::<Table>() {
        Ok(table) => table,
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

    let mut problems = Vec::new();

    // Step 2: schema and semantics
    for (section_name, section_value) in &table {
        // Top-level scalar keys (above any [section] header) live in the
        // same iteration as section tables. Recognise them first so they
        // don't trip the unknown-section check.
        if let Some((_, expected_type)) = KNOWN_TOP_LEVEL_SCALARS
            .iter()
            .find(|(name, _)| name == section_name)
        {
            let actual_type = toml_type_name(section_value);
            if actual_type != *expected_type {
                problems.push(make_problem(
                    file_path,
                    Some(section_name.clone()),
                    ConfigProblemKind::TypeError {
                        expected: (*expected_type).to_string(),
                        found: actual_type.to_string(),
                    },
                ));
            }
            continue;
        }

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

    problems.extend(validate_catalog_references(&table, file_path));

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
    fn generation_presets_section_is_known() {
        let toml = r#"
[generation.presets.fast]
max_tokens = 1024
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "generation presets should pass schema validation: {problems:?}"
        );
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
    fn workspace_path_is_rejected_as_an_unknown_section() {
        let problems =
            validate_toml_string("[workspace]\npath = \"/tmp/project\"", &path("config.toml"));
        assert!(
            problems.iter().any(|problem| {
                matches!(
                    &problem.kind,
                    ConfigProblemKind::UnknownSection { name } if name == "workspace"
                )
            }),
            "removed [workspace] configuration must be rejected: {problems:?}"
        );
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

[providers]
default = "anthropic"

[providers.entries.anthropic]
protocol = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-4-20250514"
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
    fn empty_default_model_id_produces_semantic_error() {
        let toml = "[models]\ndefault_model_id = \"\"";
        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.iter().any(|problem| {
                problem.field_path.as_deref() == Some("models.default_model_id")
                    && matches!(&problem.kind, ConfigProblemKind::SemanticError { .. })
            }),
            "empty default_model_id should produce SemanticError, got: {problems:?}"
        );
    }

    #[test]
    fn missing_default_model_entry_produces_semantic_error() {
        let toml = r#"
[models]
default = "missing"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-4-20250514"
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.iter().any(|problem| {
                problem.field_path.as_deref() == Some("models.default")
                    && matches!(&problem.kind, ConfigProblemKind::SemanticError { .. })
            }),
            "missing default model entry should produce SemanticError, got: {problems:?}"
        );
    }

    #[test]
    fn empty_catalogue_defaults_are_treated_as_unset() {
        let toml = r#"
[providers]
default = ""

[providers.entries.openrouter]
protocol = "openrouter"
api_key_env = "OPENROUTER_API_KEY"

[models]
default = ""

[models.entries]
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.iter().all(|problem| {
                problem.field_path.as_deref() != Some("providers.default")
                    && problem.field_path.as_deref() != Some("models.default")
            }),
            "empty catalogue defaults should be treated as unset, got: {problems:?}"
        );
    }

    #[test]
    fn default_model_with_missing_provider_entry_produces_semantic_error() {
        let toml = r#"
[providers.entries.anthropic]
protocol = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "missing-provider"
model_id = "claude-sonnet-4-20250514"
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.iter().any(|problem| {
                problem.field_path.as_deref() == Some("models.entries.sonnet.provider_id")
                    && matches!(&problem.kind, ConfigProblemKind::SemanticError { .. })
            }),
            "missing provider entry should produce SemanticError, got: {problems:?}"
        );
    }

    #[test]
    fn legacy_provider_section_is_rejected() {
        let toml = "[provider]\nprotocol = \"anthropic\"";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::UnknownSection { .. })),
            "legacy [provider] should be rejected"
        );
    }

    // --- parse_and_validate ---

    #[test]
    fn parse_and_validate_returns_partial_config_on_valid_input() {
        let toml = r#"
[server]
host = "10.0.0.1"
port = 8080

[models]
default_model_id = "claude-opus"
"#;
        let partial =
            parse_and_validate(toml, &path("config.toml")).expect("valid TOML should return Ok");
        let server = partial.server.expect("server section should be present");
        assert_eq!(server.host.as_deref(), Some("10.0.0.1"));
        assert_eq!(server.port, Some(8080));
        let models = partial.models.expect("models section should be present");
        assert_eq!(models.default_model_id.as_deref(), Some("claude-opus"));
    }

    #[test]
    fn parse_and_validate_preserves_telegram_channel_config() {
        let toml = r#"
[channels.telegram]
bot_token_secret_ref = "channel-token:telegram-main"
bot_token_env = "SWAGE_TELEGRAM_TOKEN"
api_base_url = "http://127.0.0.1:9876"
poll_timeout_secs = 7
"#;

        let partial =
            parse_and_validate(toml, &path("config.toml")).expect("Telegram config should pass");
        let telegram = partial
            .channels
            .expect("channels section should be present")
            .telegram
            .expect("Telegram section should be present");

        assert_eq!(
            telegram.bot_token_secret_ref,
            Some(Some("channel-token:telegram-main".to_string()))
        );
        assert_eq!(
            telegram.bot_token_env,
            Some(Some("SWAGE_TELEGRAM_TOKEN".to_string()))
        );
        assert_eq!(
            telegram.api_base_url.as_deref(),
            Some("http://127.0.0.1:9876")
        );
        assert_eq!(telegram.poll_timeout_secs, Some(7));
    }

    #[test]
    fn telegram_channel_rejects_unknown_nested_field() {
        let toml = r#"
[channels.telegram]
bot_token_en = "SWAGE_TELEGRAM_TOKEN"
"#;

        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(problems.iter().any(|problem| {
            problem.field_path.as_deref() == Some("channels.telegram.bot_token_en")
                && matches!(
                    &problem.kind,
                    ConfigProblemKind::UnknownField { section, name }
                        if section == "channels.telegram" && name == "bot_token_en"
                )
        }));
    }

    #[test]
    fn telegram_channel_rejects_wrong_nested_field_types() {
        let toml = r#"
[channels.telegram]
bot_token_secret_ref = 1
bot_token_env = false
api_base_url = 2
poll_timeout_secs = "seven"
"#;

        let problems = validate_toml_string(toml, &path("config.toml"));

        for field in [
            "bot_token_secret_ref",
            "bot_token_env",
            "api_base_url",
            "poll_timeout_secs",
        ] {
            let field_path = format!("channels.telegram.{field}");
            assert!(
                problems.iter().any(|problem| {
                    problem.field_path.as_deref() == Some(field_path.as_str())
                        && matches!(&problem.kind, ConfigProblemKind::TypeError { .. })
                }),
                "expected a type error for {field_path}, got {problems:?}"
            );
        }
    }

    #[test]
    fn telegram_channel_rejects_negative_poll_timeout() {
        let toml = r#"
[channels.telegram]
poll_timeout_secs = -1
"#;

        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(problems.iter().any(|problem| {
            problem.field_path.as_deref() == Some("channels.telegram.poll_timeout_secs")
                && matches!(&problem.kind, ConfigProblemKind::SemanticError { .. })
        }));
    }

    #[test]
    fn telegram_channel_accepts_zero_poll_timeout() {
        let toml = r#"
[channels.telegram]
poll_timeout_secs = 0
"#;

        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.is_empty(),
            "expected no problems, got {problems:?}"
        );
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
    fn context_transfer_rejects_fields_for_none_mode() {
        let toml = r#"
[communication.context_transfer]
mode = "none"
allowed_fields = ["system"]
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.iter().any(|problem| {
                problem.field_path.as_deref()
                    == Some("communication.context_transfer.allowed_fields")
                    && matches!(&problem.kind, ConfigProblemKind::SemanticError { .. })
            }),
            "allowed_fields should be rejected outside explicit mode, got: {problems:?}"
        );
    }

    #[test]
    fn context_transfer_rejects_summary_budget_for_explicit_mode() {
        let toml = r#"
[communication.context_transfer]
mode = "explicit"
allowed_fields = ["system"]
summary_budget = 512
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.iter().any(|problem| {
                problem.field_path.as_deref()
                    == Some("communication.context_transfer.summary_budget")
                    && matches!(&problem.kind, ConfigProblemKind::SemanticError { .. })
            }),
            "summary_budget should be rejected outside summary mode, got: {problems:?}"
        );
    }

    #[test]
    fn context_transfer_validates_target_overrides_by_mode() {
        let toml = r#"
[communication.context_transfer]
mode = "none"

[communication.context_transfer.targets.reviewer]
mode = "summary"
allowed_fields = ["user"]
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.iter().any(|problem| {
                problem.field_path.as_deref()
                    == Some("communication.context_transfer.targets.reviewer.allowed_fields")
                    && matches!(&problem.kind, ConfigProblemKind::SemanticError { .. })
            }),
            "target allowed_fields should be rejected outside explicit mode, got: {problems:?}"
        );
    }

    #[test]
    fn context_transfer_rejects_nested_target_tables() {
        let toml = r#"
[communication.context_transfer]
mode = "none"

[communication.context_transfer.targets.reviewer]
mode = "summary"

[communication.context_transfer.targets.reviewer.targets.writer]
mode = "explicit"
allowed_fields = ["user"]
"#;
        let problems = validate_toml_string(toml, &path("config.toml"));

        assert!(
            problems.iter().any(|problem| {
                problem.field_path.as_deref()
                    == Some("communication.context_transfer.targets.reviewer.targets")
                    && matches!(&problem.kind, ConfigProblemKind::UnknownField { name, .. } if name == "targets")
            }),
            "nested target tables should be rejected as unknown, got: {problems:?}"
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
    fn scheduling_section_validates_without_warnings() {
        let toml =
            "[scheduling]\nrun_history_max_age_days = 90\nrun_history_max_per_schedule = 500\n";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems.is_empty(),
            "valid [scheduling] should produce no errors, got: {problems:?}"
        );
    }

    #[test]
    fn scheduling_retention_zero_produces_semantic_error() {
        let toml = "[scheduling]\nrun_history_max_age_days = 0\nrun_history_max_per_schedule = 0\n";
        let problems = validate_toml_string(toml, &path("config.toml"));
        assert!(
            problems
                .iter()
                .any(|p| matches!(&p.kind, ConfigProblemKind::SemanticError { .. })),
            "zero retention caps should produce SemanticError"
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

[providers]
default = "anthropic"

[providers.entries.anthropic]
protocol = "anthropic"

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-4-20250514"

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

    #[test]
    fn an_effort_capability_that_cannot_be_honoured_is_a_semantic_problem() {
        let mut config = ResolvedConfig::default();
        config.models.default = "m".into();
        config.models.entries.insert(
            "m".into(),
            crate::ModelEndpointConfig {
                provider_id: "p".into(),
                model_id: "m".into(),
                capabilities: crate::ModelCapabilitiesConfig {
                    reasoning_replay: false,
                    effort: Some(swage_protocol::client::settings::EffortCapability::Budget {
                        min_tokens: 100,
                        max_tokens: 10,
                        can_disable: false,
                    }),
                },
                ..Default::default()
            },
        );
        config.providers.entries.insert("p".into(), crate::ProviderEndpointConfig::default());
        let problems = validate_resolved_catalog_references(&config, Path::new("config.toml"));
        assert!(
            problems.iter().any(|problem| {
                problem.field_path.as_deref() == Some("models.entries.m.capabilities.effort")
                    && problem.to_string().contains("inverted")
            }),
            "{problems:?}"
        );
    }
}
