//! Named operator context — reusable Markdown briefings from the config hierarchy.
//!
//! Files live at `elements/<name>.md` under global, topic, or agent config
//! roots. Resolution is deterministic and most-specific-wins. Filesystem work
//! happens before construction; model-facing rendering lives separately in
//! the function-named briefing element.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use swage_config::HierarchyLevel;

/// Names owned by built-in context elements and their assembly bookkeeping.
const RESERVED_ELEMENT_NAMES: &[&str] = &["history", "memory", "orientation", "prompts", "skills"];

pub(crate) fn has_valid_element_name_shape(name: &str) -> bool {
    swage_core::is_lowercase_identifier(name)
}

/// A resolved Markdown context file, including its precise provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedNamedElement {
    pub name: String,
    pub level: HierarchyLevel,
    pub path: PathBuf,
    pub content: String,
}

/// Validate the stable name used to address an element.
pub fn validate_element_name(name: &str) -> Result<(), String> {
    if !has_valid_element_name_shape(name) {
        return Err(format!(
            "invalid context element name '{name}': use lowercase letters, digits, and hyphens"
        ));
    }
    if RESERVED_ELEMENT_NAMES.contains(&name) {
        return Err(format!(
            "invalid context element name '{name}': the name is reserved for a built-in element"
        ));
    }
    Ok(())
}

/// Resolve one named Markdown file, with later hierarchy entries winning.
pub fn resolve_named_element(
    name: &str,
    search_levels: &[(HierarchyLevel, &Path)],
) -> Result<Option<ResolvedNamedElement>, String> {
    validate_element_name(name)?;
    let Some((level, path)) = search_levels.iter().rev().find_map(|(level, base)| {
        let candidate = base.join("elements").join(format!("{name}.md"));
        candidate.is_file().then_some((level, candidate))
    }) else {
        return Ok(None);
    };
    let content = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "context element '{}' is not readable: {error}",
            path.display()
        )
    })?;
    Ok(Some(ResolvedNamedElement {
        name: name.to_owned(),
        level: level.clone(),
        path,
        content,
    }))
}

/// Discover every valid element name present in the hierarchy, then resolve
/// each through the same name-specific API used by later dispatch callers.
pub fn discover_named_elements(
    search_levels: &[(HierarchyLevel, &Path)],
) -> Result<Vec<ResolvedNamedElement>, String> {
    let mut names = BTreeSet::new();
    for (_, base) in search_levels {
        let directory = base.join("elements");
        if !directory.is_dir() {
            continue;
        }
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            format!(
                "context element directory '{}' is not readable: {error}",
                directory.display()
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "context element directory '{}' cannot be listed: {error}",
                    directory.display()
                )
            })?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if validate_element_name(name).is_ok() {
                names.insert(name.to_owned());
            }
        }
    }

    names
        .into_iter()
        .filter_map(|name| match resolve_named_element(&name, search_levels) {
            Ok(Some(element)) => Some(Ok(element)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels<'a>(
        global: &'a Path,
        topic: &'a Path,
        agent: &'a Path,
    ) -> Vec<(HierarchyLevel, &'a Path)> {
        vec![
            (HierarchyLevel::Global, global),
            (HierarchyLevel::Topic, topic),
            (HierarchyLevel::Agent, agent),
        ]
    }

    #[test]
    fn missing_and_invalid_names_do_not_escape_the_elements_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            resolve_named_element("missing", &[(HierarchyLevel::Global, root.path())])
                .unwrap()
                .is_none()
        );
        assert!(
            resolve_named_element("../config", &[(HierarchyLevel::Global, root.path())]).is_err()
        );
        assert!(
            resolve_named_element("Uppercase", &[(HierarchyLevel::Global, root.path())]).is_err()
        );
        for name in RESERVED_ELEMENT_NAMES {
            assert!(
                resolve_named_element(name, &[(HierarchyLevel::Global, root.path())]).is_err(),
                "built-in element name '{name}' must stay reserved"
            );
        }
    }

    #[test]
    fn agent_shadows_topic_which_shadows_global() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path();
        let topic = global.join("topics/demo");
        let agent = topic.join("agents/worker");
        for (base, text) in [
            (&global.to_path_buf(), "global"),
            (&topic, "topic"),
            (&agent, "agent"),
        ] {
            std::fs::create_dir_all(base.join("elements")).unwrap();
            std::fs::write(base.join("elements/layout.md"), text).unwrap();
        }
        let hierarchy = levels(global, &topic, &agent);
        let resolved = resolve_named_element("layout", &hierarchy)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.level, HierarchyLevel::Agent);
        assert_eq!(resolved.content, "agent");
        std::fs::remove_file(agent.join("elements/layout.md")).unwrap();
        assert_eq!(
            resolve_named_element("layout", &hierarchy)
                .unwrap()
                .unwrap()
                .content,
            "topic"
        );
        std::fs::remove_file(topic.join("elements/layout.md")).unwrap();
        assert_eq!(
            resolve_named_element("layout", &hierarchy)
                .unwrap()
                .unwrap()
                .content,
            "global"
        );
    }

    #[test]
    fn discovery_returns_each_effective_name_once_in_stable_order() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path();
        let topic = global.join("topics/demo");
        let agent = topic.join("agents/worker");
        for base in [global, &topic, &agent] {
            std::fs::create_dir_all(base.join("elements")).unwrap();
        }
        std::fs::write(global.join("elements/zeta.md"), "global zeta").unwrap();
        std::fs::write(topic.join("elements/alpha.md"), "topic alpha").unwrap();
        std::fs::write(agent.join("elements/zeta.md"), "agent zeta").unwrap();
        std::fs::write(agent.join("elements/Not-Valid.md"), "ignored").unwrap();
        std::fs::write(agent.join("elements/memory.md"), "also ignored").unwrap();

        let resolved = discover_named_elements(&levels(global, &topic, &agent)).unwrap();
        assert_eq!(
            resolved
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
        assert_eq!(resolved[1].content, "agent zeta");
    }
}
