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
    let count = content
        .as_bytes()
        .windows(old.len())
        .filter(|part| *part == old.as_bytes())
        .count();
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
}
