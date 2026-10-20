//! The one rule for a tag, wherever tags are read: on a topic, on a library
//! entry, and in the catalogue's filter. A tag is trimmed, lower-cased,
//! dropped when empty, and kept once — so a tag saved as " Research " is
//! the tag `research`, matches a filter for `RESEARCH`, and is never stored
//! twice because it was typed twice. Annotation tags on a surface keep
//! their own semantics and do not pass through here.

/// The canonical form of `tags`, in first-seen order.
pub fn canonical_tags<I, S>(tags: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = std::collections::HashSet::new();
    tags.into_iter()
        .map(|tag| tag.as_ref().trim().to_lowercase())
        .filter(|tag| !tag.is_empty() && seen.insert(tag.clone()))
        .collect()
}

/// The canonical form of one tag, or `None` when it is empty.
pub fn canonical_tag(tag: &str) -> Option<String> {
    let tag = tag.trim().to_lowercase();
    (!tag.is_empty()).then_some(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_is_trimmed_lower_cased_and_kept_once() {
        assert_eq!(
            canonical_tags([
                " Research ",
                "RESEARCH",
                "",
                "  ",
                "notes",
                "Notes",
                "research"
            ]),
            vec!["research", "notes"]
        );
        assert_eq!(canonical_tags(Vec::<String>::new()), Vec::<String>::new());
    }

    #[test]
    fn one_tag_canonicalises_or_is_nothing() {
        assert_eq!(canonical_tag("  Daily "), Some("daily".into()));
        assert_eq!(canonical_tag("   "), None);
    }

    #[test]
    fn a_filter_and_a_saved_tag_meet_in_canonical_form() {
        let saved = canonical_tags(["Standup", " planning"]);
        let wanted = canonical_tags(["PLANNING "]);
        assert!(saved.iter().any(|tag| wanted.contains(tag)));
    }
}
