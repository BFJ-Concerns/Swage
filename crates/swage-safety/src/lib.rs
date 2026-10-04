//! Safety infrastructure for the Swage framework.
//!
//! Provides credential leak detection with configurable pattern matching
//! and automatic redaction. Uses Aho-Corasick for literal pattern matching
//! and regex for structured format detection, both operating in linear time.

use std::fmt::Write as _;

use aho_corasick::AhoCorasick;
use regex::Regex;
use tracing::instrument;

/// Describes the matching strategy for a leak pattern.
#[derive(Debug, Clone)]
pub enum PatternKind {
    /// Exact literal match, handled by Aho-Corasick for multi-pattern speed.
    Literal(String),
    /// Regular expression for structured format detection.
    Regex(String),
}

/// A single credential pattern with its redaction label.
#[derive(Debug, Clone)]
pub struct LeakPattern {
    /// Human-readable name for diagnostics.
    pub name: String,
    /// The matching strategy (literal or regex).
    pub kind: PatternKind,
    /// Label used in redaction output, e.g. "api_key" produces `[REDACTED:api_key]`.
    pub redaction_label: String,
}

/// A compiled regex pattern paired with its redaction label and optional
/// capture group index for partial redaction.
struct CompiledRegex {
    regex: Regex,
    redaction_label: String,
    /// When set, only the specified capture group is redacted rather than
    /// the full match. Used for patterns like high-entropy credential
    /// values where the keyword context should be preserved.
    redact_group: Option<usize>,
}

/// Credential leak detector with Aho-Corasick and regex pattern matching.
///
/// Scans text for known credential patterns and replaces matches with
/// `[REDACTED:<type>]` labels. Built-in patterns cover common API keys,
/// AWS credentials, PEM private keys, bearer tokens, and high-entropy strings
/// near credential keywords.
pub struct LeakDetector {
    /// Aho-Corasick automaton for literal patterns. `None` when no literals
    /// are configured (avoids building an empty automaton).
    literal_automaton: Option<AhoCorasick>,
    /// Redaction labels indexed by the Aho-Corasick pattern index.
    literal_labels: Vec<String>,
    /// Compiled regex patterns with their redaction labels.
    regex_patterns: Vec<CompiledRegex>,
}

impl LeakDetector {
    /// Creates a detector with custom patterns (plus built-in defaults).
    ///
    /// Returns an error if any custom regex pattern is invalid. This
    /// validates at construction time so `scan()` never fails.
    pub fn new(custom_patterns: Vec<LeakPattern>) -> Result<Self, String> {
        let mut all_patterns = builtin_patterns();
        all_patterns.extend(custom_patterns);

        let mut literal_strings = Vec::new();
        let mut literal_labels = Vec::new();
        let mut regex_patterns = Vec::new();

        for pattern in all_patterns {
            match pattern.kind {
                PatternKind::Literal(literal) => {
                    literal_strings.push(literal);
                    literal_labels.push(pattern.redaction_label);
                }
                PatternKind::Regex(regex_str) => {
                    let regex = Regex::new(&regex_str).map_err(|e| {
                        format!("invalid regex for pattern '{}': {}", pattern.name, e)
                    })?;
                    regex_patterns.push(CompiledRegex {
                        regex,
                        redaction_label: pattern.redaction_label,
                        redact_group: None,
                    });
                }
            }
        }

        // Add the high-entropy credential context pattern separately,
        // since it needs group-based redaction (group 2 = the value).
        let high_entropy_regex = Regex::new(
            r#"(?i)(password|secret|token|api_key)\s*[=:]\s*["']?([A-Za-z0-9+/=_\-]{16,})"#,
        )
        .expect("built-in high_entropy regex is valid");
        regex_patterns.push(CompiledRegex {
            regex: high_entropy_regex,
            redaction_label: "high_entropy".into(),
            redact_group: Some(2),
        });

        let literal_automaton = if literal_strings.is_empty() {
            None
        } else {
            Some(
                AhoCorasick::new(&literal_strings)
                    .map_err(|e| format!("failed to build Aho-Corasick automaton: {e}"))?,
            )
        };

        Ok(Self {
            literal_automaton,
            literal_labels,
            regex_patterns,
        })
    }

    /// Scans text and replaces detected credentials with redaction labels.
    ///
    /// Returns the input unchanged if no credentials are found.
    #[instrument(skip(self, text), fields(text_len = text.len()))]
    pub fn scan(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }

        // Track which byte ranges to redact and with what label.
        // Collect all matches first, then apply in reverse order to
        // preserve byte offsets.
        let mut redactions: Vec<(usize, usize, &str)> = Vec::new();

        // Literal matches via Aho-Corasick
        if let Some(automaton) = &self.literal_automaton {
            for mat in automaton.find_iter(text) {
                let label = &self.literal_labels[mat.pattern().as_usize()];
                redactions.push((mat.start(), mat.end(), label));
            }
        }

        // Regex matches
        for compiled in &self.regex_patterns {
            if let Some(redact_group) = compiled.redact_group {
                // Redact only the specified capture group
                for caps in compiled.regex.captures_iter(text) {
                    if let Some(group_match) = caps.get(redact_group) {
                        redactions.push((
                            group_match.start(),
                            group_match.end(),
                            &compiled.redaction_label,
                        ));
                    }
                }
            } else {
                // Redact the entire match
                for mat in compiled.regex.find_iter(text) {
                    redactions.push((mat.start(), mat.end(), &compiled.redaction_label));
                }
            }
        }

        if redactions.is_empty() {
            return text.to_string();
        }

        // Sort by start position, then prefer longer matches (more specific)
        redactions.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

        // Merge overlapping ranges, retaining the first match's label.
        let mut merged: Vec<(usize, usize, &str)> = Vec::new();
        for redaction in redactions {
            if let Some(last) = merged.last_mut()
                && redaction.0 < last.1
            {
                last.1 = last.1.max(redaction.1);
                continue;
            }
            merged.push(redaction);
        }

        // Apply redactions by building the output string
        let mut result = String::with_capacity(text.len());
        let mut last_end = 0;

        for (start, end, label) in merged {
            result.push_str(&text[last_end..start]);
            let _ = write!(result, "[REDACTED:{label}]");
            last_end = end;
        }
        result.push_str(&text[last_end..]);

        result
    }
}

impl Default for LeakDetector {
    fn default() -> Self {
        Self::new(Vec::new()).expect("built-in patterns are valid")
    }
}

/// Returns the built-in pattern library.
fn builtin_patterns() -> Vec<LeakPattern> {
    vec![
        LeakPattern {
            name: "openai_api_key".into(),
            kind: PatternKind::Regex(r"sk-[A-Za-z0-9_\-]{20,}".into()),
            redaction_label: "api_key".into(),
        },
        LeakPattern {
            name: "aws_access_key".into(),
            kind: PatternKind::Regex(r"AKIA[A-Z0-9]{16}".into()),
            redaction_label: "aws_key".into(),
        },
        LeakPattern {
            name: "pem_private_key".into(),
            // A truncated block is sensitive through the end of the input.
            kind: PatternKind::Regex(
                r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)"
                    .into(),
            ),
            redaction_label: "pem_key".into(),
        },
        LeakPattern {
            name: "bearer_token".into(),
            kind: PatternKind::Regex(r"[Bb]earer\s+[A-Za-z0-9_\-.~+/]+=*".into()),
            redaction_label: "bearer_token".into(),
        },
        // Note: high_entropy is added separately in new() because it
        // uses group-based redaction (only the value part is redacted).
    ]
}
