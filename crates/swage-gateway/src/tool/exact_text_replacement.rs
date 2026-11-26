//! Replace a passage only when it occurs at exactly one position.

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReplacementError {
    Missing,
    Ambiguous(usize),
}

/// Count overlapping occurrences too: `aa` in `aaa` names two positions.
/// An empty passage cannot identify a replacement position.
pub(super) fn replace_exact_text(
    content: &str,
    old: &str,
    new: &str,
) -> Result<String, ReplacementError> {
    if old.is_empty() {
        return Err(ReplacementError::Missing);
    }
    let passage = old.as_bytes();
    // Prefix lengths let each mismatch resume without rescanning input bytes.
    let mut prefixes = vec![0; passage.len()];
    let mut matched = 0;
    for index in 1..passage.len() {
        while matched > 0 && passage[index] != passage[matched] {
            matched = prefixes[matched - 1];
        }
        if passage[index] == passage[matched] {
            matched += 1;
        }
        prefixes[index] = matched;
    }

    let mut count = 0;
    matched = 0;
    for &byte in content.as_bytes() {
        while matched > 0 && byte != passage[matched] {
            matched = prefixes[matched - 1];
        }
        if byte == passage[matched] {
            matched += 1;
        }
        if matched == passage.len() {
            count += 1;
            // Retain the matching suffix so overlapping positions count too.
            matched = prefixes[matched - 1];
        }
    }
    match count {
        0 => Err(ReplacementError::Missing),
        1 => Ok(content.replacen(old, new, 1)),
        count => Err(ReplacementError::Ambiguous(count)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_a_unique_passage() {
        assert_eq!(
            replace_exact_text("café 🌱 fin", "🌱", "sapling"),
            Ok("café sapling fin".to_owned())
        );
    }

    #[test]
    fn refuses_a_missing_passage() {
        assert_eq!(
            replace_exact_text("alpha", "beta", "gamma"),
            Err(ReplacementError::Missing)
        );
        assert_eq!(
            replace_exact_text("alpha", "", "gamma"),
            Err(ReplacementError::Missing)
        );
    }

    #[test]
    fn refuses_overlapping_occurrences() {
        assert_eq!(
            replace_exact_text("aaa", "aa", "b"),
            Err(ReplacementError::Ambiguous(2))
        );
    }

    #[test]
    fn refuses_repeated_occurrences() {
        assert_eq!(
            replace_exact_text("aa aa aa", "aa", "b"),
            Err(ReplacementError::Ambiguous(3))
        );
    }

    #[test]
    fn refuses_a_long_missing_passage_within_ten_seconds() {
        let content = "a".repeat(2 * 1024 * 1024);
        let old = format!("{}b", "a".repeat(1024 * 1024 - 1));
        let started = std::time::Instant::now();
        assert_eq!(
            replace_exact_text(&content, &old, "b"),
            Err(ReplacementError::Missing)
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn counts_long_overlapping_passages_within_ten_seconds() {
        let content = "a".repeat(2 * 1024 * 1024);
        let old = "a".repeat(1024 * 1024);
        let started = std::time::Instant::now();
        assert_eq!(
            replace_exact_text(&content, &old, "b"),
            Err(ReplacementError::Ambiguous(1024 * 1024 + 1))
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
}
