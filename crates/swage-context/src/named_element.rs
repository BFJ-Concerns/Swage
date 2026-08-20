//! Named operator context — reusable Markdown briefings from the config hierarchy.
//!
//! Files live at `elements/<name>.md` under global, space, or agent config
//! roots. Resolution is deterministic and most-specific-wins. Filesystem work
//! happens before construction; model-facing rendering lives separately in
//! the function-named briefing element.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use swage_config::HierarchyLevel;

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
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!(
            "invalid context element name '{name}': use lowercase letters, digits, and hyphens"
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
        space: &'a Path,
        agent: &'a Path,
    ) -> Vec<(HierarchyLevel, &'a Path)> {
        vec![
            (HierarchyLevel::Global, global),
            (HierarchyLevel::Space, space),
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
    }

    #[test]
    fn agent_shadows_space_which_shadows_global() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path();
        let space = global.join("spaces/demo");
        let agent = space.join("agents/worker");
        for (base, text) in [
            (&global.to_path_buf(), "global"),
            (&space, "space"),
            (&agent, "agent"),
        ] {
            std::fs::create_dir_all(base.join("elements")).unwrap();
            std::fs::write(base.join("elements/layout.md"), text).unwrap();
        }
        let hierarchy = levels(global, &space, &agent);
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
            "space"
        );
        std::fs::remove_file(space.join("elements/layout.md")).unwrap();
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
        let space = global.join("spaces/demo");
        let agent = space.join("agents/worker");
        for base in [global, &space, &agent] {
            std::fs::create_dir_all(base.join("elements")).unwrap();
        }
        std::fs::write(global.join("elements/zeta.md"), "global zeta").unwrap();
        std::fs::write(space.join("elements/alpha.md"), "space alpha").unwrap();
        std::fs::write(agent.join("elements/zeta.md"), "agent zeta").unwrap();
        std::fs::write(agent.join("elements/Not-Valid.md"), "ignored").unwrap();

        let resolved = discover_named_elements(&levels(global, &space, &agent)).unwrap();
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
