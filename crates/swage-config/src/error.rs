use std::fmt;
use std::path::PathBuf;
use thiserror::Error;

// ---------------------------------------------------------------------------
// ConfigProblemKind — leaf error descriptions
// ---------------------------------------------------------------------------

/// Specific kind of configuration problem, describing what went wrong.
///
/// The Display output is the problem-specific part of the message.
/// The file path prefix is added by `ConfigProblem::Display`.
#[derive(Debug, Error)]
pub enum ConfigProblemKind {
    /// TOML syntax error at a specific line.
    #[error("line {line}: {message}")]
    ParseError { line: usize, message: String },

    /// Unrecognised top-level section in the TOML file.
    #[error("unknown section [{name}]")]
    UnknownSection { name: String },

    /// Unrecognised field within a known section.
    #[error("unknown field '{name}' in [{section}]")]
    UnknownField { section: String, name: String },

    /// Wrong TOML type for a known field.
    #[error("expected {expected}, found {found}")]
    TypeError { expected: String, found: String },

    /// Value constraint violation (e.g. port out of valid range).
    #[error("{message}")]
    SemanticError { message: String },

    /// Directory structure problems encountered during discovery.
    #[error("{message}")]
    DiscoveryError { message: String },

    /// Instruction file read failure (e.g. non-UTF-8 content).
    #[error("{message}")]
    InstructionError { message: String },
}

// ---------------------------------------------------------------------------
// ConfigProblem — single issue with location context
// ---------------------------------------------------------------------------

/// A single configuration problem, tied to a specific file and optional field.
pub struct ConfigProblem {
    /// The config file that contains the problem.
    pub file_path: PathBuf,
    /// Dotted field path within the file, if applicable (e.g. `"models.default"`).
    pub field_path: Option<String>,
    /// What went wrong.
    pub kind: ConfigProblemKind,
}

impl fmt::Display for ConfigProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.file_path.display();
        match &self.field_path {
            // When a field path is available, include it before the kind message.
            // e.g. "config.toml: models.default expected string, found integer"
            Some(field) => write!(f, "{path}: {field}: {}", self.kind),
            None => write!(f, "{path}: {}", self.kind),
        }
    }
}

impl fmt::Debug for ConfigProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConfigProblem({self})")
    }
}

// ---------------------------------------------------------------------------
// ConfigError — top-level error collecting multiple problems
// ---------------------------------------------------------------------------

/// Top-level configuration error, potentially wrapping multiple problems
/// collected across several files or validation passes.
#[derive(Debug)]
pub struct ConfigError {
    pub problems: Vec<ConfigProblem>,
}

impl ConfigError {
    pub fn new(problems: Vec<ConfigProblem>) -> Self {
        Self { problems }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Each problem on its own line, formatted with its file path prefix.
        let mut first = true;
        for problem in &self.problems {
            if !first {
                writeln!(f)?;
            }
            write!(f, "{problem}")?;
            first = false;
        }
        // Graceful output when the list is somehow empty.
        if self.problems.is_empty() {
            write!(f, "configuration error (no details)")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(file: &str, field: Option<&str>, kind: ConfigProblemKind) -> ConfigProblem {
        ConfigProblem {
            file_path: file.into(),
            field_path: field.map(|s| s.to_string()),
            kind,
        }
    }

    #[test]
    fn unknown_section_includes_file_and_name() {
        let p = problem(
            "config.toml",
            None,
            ConfigProblemKind::UnknownSection {
                name: "databse".to_string(),
            },
        );
        let s = p.to_string();
        assert!(s.contains("config.toml"), "should contain file path: {s}");
        assert!(s.contains("databse"), "should contain section name: {s}");
    }

    #[test]
    fn unknown_field_includes_file_section_and_field() {
        let p = problem(
            "bench/config.toml",
            None,
            ConfigProblemKind::UnknownField {
                section: "server".to_string(),
                name: "protokol".to_string(),
            },
        );
        let s = p.to_string();
        assert!(
            s.contains("bench/config.toml"),
            "should contain file path: {s}"
        );
        assert!(s.contains("protokol"), "should contain field name: {s}");
    }

    #[test]
    fn type_error_includes_file_and_types() {
        let p = problem(
            "config.toml",
            Some("server.port"),
            ConfigProblemKind::TypeError {
                expected: "integer".to_string(),
                found: "string".to_string(),
            },
        );
        let s = p.to_string();
        assert!(s.contains("config.toml"), "should contain file path: {s}");
        assert!(
            s.contains("integer") || s.contains("string"),
            "should describe type mismatch: {s}"
        );
    }

    #[test]
    fn parse_error_includes_file_and_line() {
        let p = problem(
            "config.toml",
            None,
            ConfigProblemKind::ParseError {
                line: 12,
                message: "expected value, found '='".to_string(),
            },
        );
        let s = p.to_string();
        assert!(s.contains("config.toml"), "should contain file path: {s}");
        assert!(s.contains("12"), "should contain line number: {s}");
    }

    #[test]
    fn config_error_display_contains_all_problems() {
        let problems = vec![
            problem(
                "config.toml",
                None,
                ConfigProblemKind::UnknownSection {
                    name: "databse".to_string(),
                },
            ),
            problem(
                "config.toml",
                Some("server.port"),
                ConfigProblemKind::TypeError {
                    expected: "integer".to_string(),
                    found: "string".to_string(),
                },
            ),
        ];
        let err = ConfigError::new(problems);
        let s = err.to_string();
        assert!(s.contains("config.toml"), "should include file path: {s}");
        assert!(
            s.contains("databse") || s.contains("integer"),
            "should include at least one problem detail: {s}"
        );
    }

    #[test]
    fn config_error_implements_std_error() {
        let err = ConfigError::new(vec![]);
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn semantic_error_includes_file_and_message() {
        let p = problem(
            "agent/config.toml",
            None,
            ConfigProblemKind::SemanticError {
                message: "port must be between 1 and 65535".to_string(),
            },
        );
        let s = p.to_string();
        assert!(
            s.contains("agent/config.toml"),
            "should contain file path: {s}"
        );
        assert!(s.contains("port must be"), "should contain message: {s}");
    }

    #[test]
    fn discovery_error_includes_file_and_message() {
        let p = problem(
            "/workspace",
            None,
            ConfigProblemKind::DiscoveryError {
                message: "benches directory not found".to_string(),
            },
        );
        let s = p.to_string();
        assert!(s.contains("/workspace"), "should contain path: {s}");
        assert!(
            s.contains("benches directory"),
            "should contain message: {s}"
        );
    }
}
