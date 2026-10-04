//! Tests for LeakDetector credential scanning and redaction.

use swage_safety::{LeakDetector, LeakPattern, PatternKind};

// -- Construction -----------------------------------------------------------

#[test]
fn default_creates_detector_with_builtin_patterns() {
    let detector = LeakDetector::default();
    // Should not panic; builtins are valid
    drop(detector);
}

#[test]
fn new_with_valid_custom_patterns_succeeds() {
    let patterns = vec![LeakPattern {
        name: "custom".into(),
        kind: PatternKind::Regex(r"CUSTOM_[A-Z]{10}".into()),
        redaction_label: "custom_key".into(),
    }];
    let result = LeakDetector::new(patterns);
    assert!(result.is_ok());
}

#[test]
fn new_with_invalid_regex_returns_error() {
    let patterns = vec![LeakPattern {
        name: "broken".into(),
        kind: PatternKind::Regex(r"[invalid".into()),
        redaction_label: "broken".into(),
    }];
    let result = LeakDetector::new(patterns);
    assert!(result.is_err());
}

#[test]
fn new_with_literal_pattern_succeeds() {
    let patterns = vec![LeakPattern {
        name: "literal".into(),
        kind: PatternKind::Literal("SECRET_VALUE_123".into()),
        redaction_label: "literal_secret".into(),
    }];
    let result = LeakDetector::new(patterns);
    assert!(result.is_ok());
}

// -- Clean text passes through unchanged ------------------------------------

#[test]
fn clean_text_passes_through_unchanged() {
    let detector = LeakDetector::default();
    let input = "This is a normal message with no credentials.";
    assert_eq!(detector.scan(input), input);
}

#[test]
fn empty_string_passes_through() {
    let detector = LeakDetector::default();
    assert_eq!(detector.scan(""), "");
}

// -- OpenAI API key detection -----------------------------------------------

#[test]
fn detects_openai_api_key() {
    let detector = LeakDetector::default();
    let input = "My key is sk-abc123def456ghi789jklmnopqrstuvwx";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:api_key]"));
    assert!(!result.contains("sk-abc123"));
}

#[test]
fn openai_key_in_json() {
    let detector = LeakDetector::default();
    let input = r#"{"api_key": "sk-proj-abcdefghijklmnopqrstuvwxyz1234567890"}"#;
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:api_key]"));
}

#[test]
fn sk_prefix_alone_does_not_match() {
    let detector = LeakDetector::default();
    // "sk-" followed by fewer than 20 characters should not match
    let input = "The prefix sk-short is not a key.";
    assert_eq!(detector.scan(input), input);
}

// -- AWS access key detection -----------------------------------------------

#[test]
fn detects_aws_access_key() {
    let detector = LeakDetector::default();
    let input = "AWS key: AKIAIOSFODNN7EXAMPLE";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:aws_key]"));
    assert!(!result.contains("AKIAIOSFODNN7EXAMPLE"));
}

#[test]
fn akia_prefix_alone_does_not_match() {
    let detector = LeakDetector::default();
    let input = "AKIA is just a prefix without enough chars.";
    assert_eq!(detector.scan(input), input);
}

// -- PEM private key detection ----------------------------------------------

#[test]
fn detects_pem_private_key_header() {
    let detector = LeakDetector::default();
    let input = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:pem_key]"));
}

#[test]
fn detects_ec_private_key_header() {
    let detector = LeakDetector::default();
    let input = "-----BEGIN EC PRIVATE KEY-----";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:pem_key]"));
}

#[test]
fn pem_private_key_blocks_are_redacted_whole() {
    let detector = LeakDetector::default();
    for kind in [
        "PRIVATE KEY",
        "RSA PRIVATE KEY",
        "EC PRIVATE KEY",
        "ENCRYPTED PRIVATE KEY",
    ] {
        for newline in ["\n", "\r\n"] {
            let block = format!(
                "-----BEGIN {kind}-----{newline}MIIEpAIBAAKCAQEA{newline}c2VjcmV0a2V5Ym9keQ=={newline}-----END {kind}-----"
            );
            let input = format!("before\n{block}\nbetween\n{block}\nafter");
            assert_eq!(
                detector.scan(&input),
                "before\n[REDACTED:pem_key]\nbetween\n[REDACTED:pem_key]\nafter",
                "whole blocks must disappear for {kind} with {newline:?}"
            );
        }
    }
}

#[test]
fn truncated_pem_private_key_redacts_to_end_of_input() {
    let detector = LeakDetector::default();
    for body in [
        "",
        "\nMIIEpAIBAAKCAQEA\nc2VjcmV0a2V5Ym9keQ==\ntrailing text",
    ] {
        let input = format!("before\n-----BEGIN RSA PRIVATE KEY-----{body}");
        assert_eq!(detector.scan(&input), "before\n[REDACTED:pem_key]");
    }
}

#[test]
fn overlapping_matches_redact_the_extending_tail() {
    let detector = LeakDetector::new(vec![
        LeakPattern {
            name: "first".into(),
            kind: PatternKind::Regex("ABCDEF".into()),
            redaction_label: "first".into(),
        },
        LeakPattern {
            name: "contained".into(),
            kind: PatternKind::Regex("BCDE".into()),
            redaction_label: "contained".into(),
        },
        LeakPattern {
            name: "extending".into(),
            kind: PatternKind::Regex("DEFGHI".into()),
            redaction_label: "extending".into(),
        },
        LeakPattern {
            name: "chained".into(),
            kind: PatternKind::Regex("HIJKL".into()),
            redaction_label: "chained".into(),
        },
    ])
    .unwrap();
    assert_eq!(
        detector.scan("before ABCDEFGHIJKL after"),
        "before [REDACTED:first] after"
    );
}

// -- Bearer token detection -------------------------------------------------

#[test]
fn detects_bearer_token() {
    let detector = LeakDetector::default();
    let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:bearer_token]"));
}

#[test]
fn detects_lowercase_bearer() {
    let detector = LeakDetector::default();
    let input = "bearer abc123def456ghi789jklmnop";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:bearer_token]"));
}

// -- High-entropy credential context detection ------------------------------

#[test]
fn detects_password_assignment() {
    let detector = LeakDetector::default();
    let input = r#"password = "SuperSecretPassword123456""#;
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:high_entropy]"));
}

#[test]
fn detects_secret_with_colon() {
    let detector = LeakDetector::default();
    let input = "SECRET: abcdefghijklmnop1234";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:high_entropy]"));
}

#[test]
fn detects_api_key_assignment() {
    let detector = LeakDetector::default();
    let input = r#"api_key = "my_secret_api_key_value_1234""#;
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:"));
}

#[test]
fn short_value_after_keyword_does_not_match() {
    let detector = LeakDetector::default();
    // Value is under 16 characters — should not match high_entropy
    let input = "password=short";
    assert_eq!(detector.scan(input), input);
}

// -- Custom literal pattern -------------------------------------------------

#[test]
fn custom_literal_pattern_redacts() {
    let patterns = vec![LeakPattern {
        name: "internal-token".into(),
        kind: PatternKind::Literal("INTERNAL_SECRET_TOKEN_12345".into()),
        redaction_label: "internal_token".into(),
    }];
    let detector = LeakDetector::new(patterns).unwrap();
    let input = "Token is INTERNAL_SECRET_TOKEN_12345 here.";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:internal_token]"));
    assert!(!result.contains("INTERNAL_SECRET_TOKEN_12345"));
}

// -- Custom regex pattern ---------------------------------------------------

#[test]
fn custom_regex_pattern_redacts() {
    let patterns = vec![LeakPattern {
        name: "github-pat".into(),
        kind: PatternKind::Regex(r"ghp_[A-Za-z0-9]{36}".into()),
        redaction_label: "github_pat".into(),
    }];
    let detector = LeakDetector::new(patterns).unwrap();
    let input = "Token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef1234";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:github_pat]"));
}

// -- Multiple credentials in same text --------------------------------------

#[test]
fn multiple_credentials_all_redacted() {
    let detector = LeakDetector::default();
    let input = "Key: sk-abcdefghijklmnopqrstuvwxyz1234 and AWS: AKIAIOSFODNN7EXAMPLE";
    let result = detector.scan(input);
    assert!(result.contains("[REDACTED:api_key]"));
    assert!(result.contains("[REDACTED:aws_key]"));
    assert!(!result.contains("sk-abcdefghijklmnopqrstuvwxyz1234"));
    assert!(!result.contains("AKIAIOSFODNN7EXAMPLE"));
}

// -- Redaction format -------------------------------------------------------

#[test]
fn redaction_format_is_bracketed_with_type() {
    let detector = LeakDetector::default();
    let input = "AKIAIOSFODNN7EXAMPLE";
    let result = detector.scan(input);
    // Should be exactly [REDACTED:aws_key] format
    assert!(result.contains("[REDACTED:aws_key]"));
}
