//! Skill discovery — SKILL.md loading with YAML frontmatter and hierarchy
//! resolution.
//!
//! Skills are directories containing a `SKILL.md` file discovered at three
//! hierarchy levels (global, space, agent). Same-name skills at more
//! specific levels shadow less specific ones.

use std::path::{Path, PathBuf};

use crate::model::HierarchyLevel;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// A single skill loaded from a `SKILL.md` file at a config hierarchy level.
///
/// Skills are discovered as subdirectories of a `skills/` folder at each
/// hierarchy level. The directory name is the canonical skill identifier and
/// must match `[a-z0-9-]+`. More specific levels (Agent > Space > Global)
/// shadow less specific ones when the same name appears at multiple levels.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillDefinition {
    /// Canonical identifier — the directory name (`[a-z0-9-]+`).
    pub name: String,
    /// Optional human-readable display name from frontmatter `name` field.
    pub display_name: Option<String>,
    /// Required short description from frontmatter `description` field.
    pub description: String,
    /// Full markdown body after frontmatter is stripped — the skill instructions.
    pub instructions: String,
    /// Tool names the skill requests access to (from frontmatter `tools` list).
    /// Defaults to empty if the field is absent.
    pub tools: Vec<String>,
    /// Which hierarchy level this skill was discovered at.
    pub level: HierarchyLevel,
    /// Path to the `SKILL.md` file — used for diagnostics and the assembly report.
    pub source_path: PathBuf,
    /// Whether this definition is compiled into the product rather than authored
    /// in the operator's configuration hierarchy.
    pub shipped: bool,
}

/// Describes a skill directory or file that was skipped during discovery.
///
/// Warnings are non-fatal: they indicate a directory or file was found but
/// could not be used. Discovery continues with the remaining items.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillWarning {
    /// Path of the directory or file that was skipped.
    pub file_path: PathBuf,
    /// Human-readable reason the item was skipped.
    pub reason: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a single `SKILL.md` file at the given path.
///
/// `skill_name` is the canonical name derived from the containing directory —
/// it is never read from the file itself. The `source_level` identifies which
/// hierarchy level the skill belongs to.
///
/// # Frontmatter format
///
/// ```markdown
/// ---
/// name: My Skill            # optional — becomes display_name
/// description: Short blurb. # required — file is skipped without this
/// tools:                    # optional list
///   - bash
///   - read
/// ---
///
/// Full skill instructions here.
/// ```
///
/// Returns a [`SkillWarning`] if the file cannot be read, has no frontmatter,
/// is missing the required `description` field, or contains malformed YAML.
pub fn parse_skill_file(
    path: &Path,
    skill_name: &str,
    source_level: HierarchyLevel,
) -> Result<SkillDefinition, SkillWarning> {
    let raw = std::fs::read_to_string(path).map_err(|e| SkillWarning {
        file_path: path.to_path_buf(),
        reason: format!("failed to read file: {e}"),
    })?;

    parse_skill_source(&raw, path, skill_name, source_level)
}

/// Parse a skill definition from an in-memory source using the same grammar as
/// [`parse_skill_file`].
///
/// `source_path` identifies where sibling files resolve for diagnostics and
/// context assembly; parsing never reads it.
pub fn parse_skill_source(
    raw: &str,
    source_path: &Path,
    skill_name: &str,
    source_level: HierarchyLevel,
) -> Result<SkillDefinition, SkillWarning> {
    let (display_name, description, tools, body) =
        parse_skill_frontmatter(raw).map_err(|reason| SkillWarning {
            file_path: source_path.to_path_buf(),
            reason,
        })?;

    // description is required — a skill without one is not usable.
    let description = description.ok_or_else(|| SkillWarning {
        file_path: source_path.to_path_buf(),
        reason: "missing required `description` field in frontmatter".to_string(),
    })?;

    Ok(SkillDefinition {
        name: skill_name.to_string(),
        display_name,
        description,
        // Trim leading/trailing blank lines from the body; internal structure preserved.
        instructions: body.trim().to_string(),
        tools,
        level: source_level,
        source_path: source_path.to_path_buf(),
        shipped: false,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Frontmatter parsing (internal)
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a SKILL.md string's YAML frontmatter.
///
/// Returns `(display_name, description, tools, body)` on success, or an
/// error string (for embedding in a [`SkillWarning`]) on failure.
///
/// Unlike prompt rule files, skills *must* have frontmatter — there is no
/// sensible default for `description`. Missing frontmatter is therefore an
/// error here rather than a tolerated absence.
type SkillFrontmatterParts = (Option<String>, Option<String>, Vec<String>, String);

fn parse_skill_frontmatter(raw: &str) -> Result<SkillFrontmatterParts, String> {
    // Skills always require a frontmatter block.
    if !raw.starts_with("---\n") && !raw.starts_with("---\r\n") {
        return Err(
            "SKILL.md must begin with a YAML frontmatter block containing `description`"
                .to_string(),
        );
    }

    // Split into lines so we can find the closing `---` unambiguously.
    let lines: Vec<&str> = raw.lines().collect();

    // lines[0] is the opening "---"; find the next standalone "---".
    let close_offset = lines
        .iter()
        .skip(1)
        .position(|line| *line == "---")
        .ok_or_else(|| "frontmatter block is not closed (missing closing `---`)".to_string())?;

    // Adjust back to the full array (skip(1) offset).
    let close_idx = close_offset + 1;

    let frontmatter_lines = &lines[1..close_idx];
    let body = lines[close_idx + 1..].join("\n");

    let mut display_name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut tools: Vec<String> = Vec::new();
    // When true, subsequent `  - item` lines are collected as tool names.
    let mut in_tools_list = false;

    for line in frontmatter_lines {
        let trimmed = line.trim();

        // Skip blank lines and YAML comments; a blank line also ends a list block.
        if trimmed.is_empty() {
            in_tools_list = false;
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }

        // Collect tool list items while inside a `tools:` block.
        if in_tools_list && line.starts_with("  - ") {
            tools.push(line[4..].trim().to_string());
            continue;
        }

        // Any non-list line ends the tools block (encountered a new key).
        in_tools_list = false;

        // Every remaining line must be a `key: value` pair.
        let colon = line.find(':').ok_or_else(|| {
            format!("malformed frontmatter line (expected `key: value`): {trimmed:?}")
        })?;

        let key = line[..colon].trim();
        let value = line[colon + 1..].trim();

        match key {
            "name" if !value.is_empty() => {
                display_name = Some(value.to_string());
            }
            "description" if !value.is_empty() => {
                description = Some(value.to_string());
            }
            // `tools:` with no inline value begins a list block.
            "tools" if value.is_empty() => {
                in_tools_list = true;
                // Inline `tools: value` is not a list — silently ignored.
            }
            "tools" => {}
            _ => {
                // Unknown keys are silently ignored (forward compatibility).
            }
        }
    }

    Ok((display_name, description, tools, body))
}

// ─────────────────────────────────────────────────────────────────────────────
// Discovery
// ─────────────────────────────────────────────────────────────────────────────

/// Specificity rank for override resolution and output sorting.
///
/// Lower return value = more specific = wins when names collide and sorts first.
pub fn level_specificity(level: &HierarchyLevel) -> i32 {
    match level {
        HierarchyLevel::Agent => 0,
        HierarchyLevel::Space => 1,
        HierarchyLevel::Global => 2,
    }
}

/// Discover skills across a set of hierarchy levels, applying override resolution.
///
/// For each `(level, base_dir)` pair:
/// 1. Looks for `base_dir/skills/` — silently skips if absent
/// 2. Scans subdirectories and validates names against `[a-z0-9-]+`
/// 3. Checks each valid directory for a `SKILL.md` — silently skips if absent
/// 4. Parses found `SKILL.md` files via [`parse_skill_file`]
///
/// **Override resolution:** When the same skill name appears at more than one
/// level, only the most specific version survives (Agent > Space > Global).
///
/// **Sort order:** Agent → Space → Global, then alphabetically
/// by name within each level.
///
/// Invalid directory names and malformed `SKILL.md` files produce
/// [`SkillWarning`]s but do not halt discovery.
pub fn discover_skills(
    levels: &[(HierarchyLevel, &Path)],
) -> (Vec<SkillDefinition>, Vec<SkillWarning>) {
    let mut candidates: Vec<SkillDefinition> = Vec::new();
    let mut warnings: Vec<SkillWarning> = Vec::new();

    for (level, base_dir) in levels {
        let skills_dir = base_dir.join("skills");
        if !skills_dir.is_dir() {
            continue;
        }

        // Collect subdirectory entries; ignore read errors for individual entries.
        let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(&skills_dir) {
            Ok(iter) => iter
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect(),
            Err(_) => continue,
        };
        // Sort entries so discovery is deterministic regardless of OS ordering.
        entries.sort();

        for entry in entries {
            // The directory name must be a valid `[a-z0-9-]+` identifier.
            let dir_name = match entry.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => {
                    warnings.push(SkillWarning {
                        file_path: entry.clone(),
                        reason: "directory name is not valid UTF-8".to_string(),
                    });
                    continue;
                }
            };

            if !is_valid_skill_name(&dir_name) {
                warnings.push(SkillWarning {
                    file_path: entry.clone(),
                    reason: format!(
                        "invalid skill directory name {dir_name:?}; names must match [a-z0-9-]+"
                    ),
                });
                continue;
            }

            // No SKILL.md → not a skill directory, silently skip.
            let skill_md = entry.join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }

            match parse_skill_file(&skill_md, &dir_name, level.clone()) {
                Ok(skill) => candidates.push(skill),
                Err(warning) => warnings.push(warning),
            }
        }
    }

    // Override resolution: for each skill name, keep only the most specific level.
    // Process candidates in reverse specificity order so that the most specific
    // entry for a given name is the last one inserted (and therefore the keeper).
    let mut resolved: std::collections::HashMap<String, SkillDefinition> =
        std::collections::HashMap::new();

    for skill in candidates {
        let entry = resolved.entry(skill.name.clone());
        match entry {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(skill);
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                // Replace if the new skill is more specific (lower rank).
                if level_specificity(&skill.level) < level_specificity(&o.get().level) {
                    o.insert(skill);
                }
            }
        }
    }

    // Collect and sort: level specificity ascending (Agent first), then name.
    let mut skills: Vec<SkillDefinition> = resolved.into_values().collect();
    skills.sort_by(|a, b| {
        level_specificity(&a.level)
            .cmp(&level_specificity(&b.level))
            .then_with(|| a.name.cmp(&b.name))
    });

    (skills, warnings)
}

/// Returns `true` if `name` matches the pattern `[a-z0-9-]+`.
fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn embedded_source_uses_the_file_parser_grammar() {
        let directory = TempDir::new().expect("temporary skill directory");
        let path = directory.path().join("SKILL.md");
        let source = "---\nname: Example\ndescription: Embedded parser proof\ntools:\n  - shell\n---\n\nRead references/proof.md.\n";
        fs::write(&path, source).expect("write skill source");

        let from_file = parse_skill_file(&path, "example", HierarchyLevel::Global)
            .expect("file-backed skill parses");
        let from_embedded = parse_skill_source(source, &path, "example", HierarchyLevel::Global)
            .expect("embedded skill parses");

        assert_eq!(from_embedded, from_file);
        assert_eq!(from_embedded.instructions, "Read references/proof.md.");
    }

    // ── SkillDefinition tests ────────────────────────────────────────────────

    /// SkillDefinition can be constructed with all required fields.
    #[test]
    fn skill_definition_fields_are_accessible() {
        let skill = SkillDefinition {
            name: "my-skill".to_string(),
            display_name: Some("My Skill".to_string()),
            description: "Does something useful.".to_string(),
            instructions: "## Usage\n\nRun this skill to do the thing.".to_string(),
            tools: vec!["bash".to_string(), "read".to_string()],
            level: HierarchyLevel::Global,
            source_path: PathBuf::from("/global/skills/my-skill/SKILL.md"),
            shipped: false,
        };

        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.display_name, Some("My Skill".to_string()));
        assert_eq!(skill.description, "Does something useful.");
        assert!(skill.instructions.contains("## Usage"));
        assert_eq!(skill.tools, vec!["bash", "read"]);
        assert_eq!(skill.level, HierarchyLevel::Global);
        assert_eq!(
            skill.source_path,
            PathBuf::from("/global/skills/my-skill/SKILL.md")
        );
    }

    /// SkillDefinition with no display_name (optional field can be None).
    #[test]
    fn skill_definition_display_name_is_optional() {
        let skill = SkillDefinition {
            name: "unnamed-skill".to_string(),
            display_name: None,
            description: "A skill with no display name.".to_string(),
            instructions: String::new(),
            tools: vec![],
            level: HierarchyLevel::Space,
            source_path: PathBuf::from("/space/skills/unnamed-skill/SKILL.md"),
            shipped: false,
        };

        assert!(skill.display_name.is_none());
        assert!(skill.tools.is_empty());
    }

    /// SkillDefinition implements Clone — clone produces an equal value.
    #[test]
    fn skill_definition_clone_produces_equal_value() {
        let original = SkillDefinition {
            name: "clonable".to_string(),
            display_name: None,
            description: "Clone me.".to_string(),
            instructions: "Instructions here.".to_string(),
            tools: vec!["glob".to_string()],
            level: HierarchyLevel::Agent,
            source_path: PathBuf::from("/agent/skills/clonable/SKILL.md"),
            shipped: false,
        };

        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    /// SkillDefinition implements Debug — debug output contains the name.
    #[test]
    fn skill_definition_debug_contains_name() {
        let skill = SkillDefinition {
            name: "debuggable".to_string(),
            display_name: None,
            description: "Debug me.".to_string(),
            instructions: String::new(),
            tools: vec![],
            level: HierarchyLevel::Global,
            source_path: PathBuf::from("/skills/debuggable/SKILL.md"),
            shipped: false,
        };

        let debug_str = format!("{skill:?}");
        assert!(
            debug_str.contains("debuggable"),
            "debug output should contain the skill name"
        );
    }

    // ── SkillWarning tests ───────────────────────────────────────────────────

    /// SkillWarning can be constructed with all required fields.
    #[test]
    fn skill_warning_fields_are_accessible() {
        let warning = SkillWarning {
            file_path: PathBuf::from("/skills/bad-skill/SKILL.md"),
            reason: "missing required `description` field".to_string(),
        };

        assert_eq!(
            warning.file_path,
            PathBuf::from("/skills/bad-skill/SKILL.md")
        );
        assert!(warning.reason.contains("description"));
    }

    /// SkillWarning implements Clone — clone produces an equal value.
    #[test]
    fn skill_warning_clone_produces_equal_value() {
        let original = SkillWarning {
            file_path: PathBuf::from("/skills/broken/SKILL.md"),
            reason: "malformed frontmatter".to_string(),
        };

        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    /// SkillWarning implements Debug — debug output contains the reason.
    #[test]
    fn skill_warning_debug_contains_reason() {
        let warning = SkillWarning {
            file_path: PathBuf::from("/skills/broken/SKILL.md"),
            reason: "invalid directory name".to_string(),
        };

        let debug_str = format!("{warning:?}");
        assert!(debug_str.contains("invalid directory name"));
    }

    /// Two SkillWarnings with different reasons are not equal.
    #[test]
    fn skill_warnings_with_different_reasons_are_not_equal() {
        let w1 = SkillWarning {
            file_path: PathBuf::from("/skills/foo/SKILL.md"),
            reason: "reason one".to_string(),
        };
        let w2 = SkillWarning {
            file_path: PathBuf::from("/skills/foo/SKILL.md"),
            reason: "reason two".to_string(),
        };

        assert_ne!(w1, w2);
    }

    // ── parse_skill_file tests ───────────────────────────────────────────────

    fn write_skill(dir: &TempDir, content: &str) -> PathBuf {
        let path = dir.path().join("SKILL.md");
        fs::write(&path, content).unwrap();
        path
    }

    /// Valid SKILL.md with all frontmatter fields returns a complete SkillDefinition.
    #[test]
    fn valid_skill_file_with_all_fields_parses_correctly() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
name: My Skill
description: Does something useful.
tools:
  - bash
  - read
---

## Usage

Run this to do the thing.
",
        );

        let result = parse_skill_file(&path, "my-skill", HierarchyLevel::Global);

        let skill = result.expect("valid SKILL.md should parse without warning");
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.display_name, Some("My Skill".to_string()));
        assert_eq!(skill.description, "Does something useful.");
        assert_eq!(skill.tools, vec!["bash", "read"]);
        assert!(skill.instructions.contains("## Usage"));
        assert_eq!(skill.level, HierarchyLevel::Global);
        assert_eq!(skill.source_path, path);
    }

    /// SKILL.md with only required description — display_name and tools default to empty.
    #[test]
    fn skill_file_with_only_description_uses_defaults() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
description: Minimal skill.
---

Minimal instructions.
",
        );

        let skill = parse_skill_file(&path, "minimal", HierarchyLevel::Space).unwrap();

        assert_eq!(skill.name, "minimal");
        assert_eq!(
            skill.display_name, None,
            "no name in frontmatter → display_name is None"
        );
        assert_eq!(skill.description, "Minimal skill.");
        assert!(
            skill.tools.is_empty(),
            "no tools in frontmatter → empty list"
        );
        assert_eq!(skill.instructions, "Minimal instructions.");
    }

    /// Frontmatter `name` field maps to display_name; canonical name comes from skill_name param.
    #[test]
    fn frontmatter_name_maps_to_display_name_not_canonical_name() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
name: Pretty Name
description: A skill.
---
",
        );

        let skill = parse_skill_file(&path, "canonical-dir", HierarchyLevel::Agent).unwrap();

        assert_eq!(
            skill.name, "canonical-dir",
            "canonical name comes from skill_name param"
        );
        assert_eq!(skill.display_name, Some("Pretty Name".to_string()));
    }

    /// Empty body is valid — instructions is an empty string.
    #[test]
    fn empty_body_is_valid() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
description: A skill with no instructions.
---
",
        );

        let skill = parse_skill_file(&path, "empty-body", HierarchyLevel::Global).unwrap();

        assert_eq!(skill.instructions, "");
    }

    /// Missing description in frontmatter returns a SkillWarning.
    #[test]
    fn missing_description_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
name: Some Skill
---

Instructions here.
",
        );

        let result = parse_skill_file(&path, "missing-desc", HierarchyLevel::Global);

        assert!(
            result.is_err(),
            "missing description should produce a warning"
        );
        let warning = result.unwrap_err();
        assert_eq!(warning.file_path, path);
        assert!(
            warning.reason.to_lowercase().contains("description"),
            "reason should mention description: {:?}",
            warning.reason,
        );
    }

    /// No frontmatter at all returns a SkillWarning (description is required).
    #[test]
    fn no_frontmatter_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(&dir, "Just plain instructions, no frontmatter.");

        let result = parse_skill_file(&path, "no-frontmatter", HierarchyLevel::Global);

        assert!(
            result.is_err(),
            "missing frontmatter should produce a warning"
        );
    }

    /// Malformed frontmatter (unclosed block) returns a SkillWarning.
    #[test]
    fn malformed_frontmatter_unclosed_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
description: Skill without closing delimiter.
instructions here with no closing ---
",
        );

        let result = parse_skill_file(&path, "bad-frontmatter", HierarchyLevel::Global);

        assert!(
            result.is_err(),
            "unclosed frontmatter should produce a warning"
        );
    }

    /// Non-existent file returns a SkillWarning.
    #[test]
    fn nonexistent_file_returns_warning() {
        let path = PathBuf::from("/nonexistent/path/SKILL.md");
        let result = parse_skill_file(&path, "ghost", HierarchyLevel::Global);
        assert!(result.is_err(), "missing file should produce a warning");
    }

    /// Tools list is correctly parsed from YAML list syntax.
    #[test]
    fn tools_list_parsed_from_yaml_list() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
description: A tool-rich skill.
tools:
  - glob
  - grep
  - bash
---
",
        );

        let skill = parse_skill_file(&path, "tool-skill", HierarchyLevel::Global).unwrap();

        assert_eq!(skill.tools, vec!["glob", "grep", "bash"]);
    }

    /// source_path in the returned SkillDefinition matches the path argument.
    #[test]
    fn source_path_matches_file_path_argument() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
description: Path check.
---
",
        );

        let skill = parse_skill_file(&path, "path-check", HierarchyLevel::Space).unwrap();

        assert_eq!(skill.source_path, path);
    }

    /// Frontmatter body is stripped from instructions — delimiter markers don't appear.
    #[test]
    fn frontmatter_stripped_from_instructions() {
        let dir = TempDir::new().unwrap();
        let path = write_skill(
            &dir,
            "\
---
description: Strip check.
---

The actual instructions.
",
        );

        let skill = parse_skill_file(&path, "strip", HierarchyLevel::Global).unwrap();

        assert!(
            !skill.instructions.contains("---"),
            "instructions must not contain frontmatter delimiters"
        );
        assert!(
            !skill.instructions.contains("description:"),
            "instructions must not contain frontmatter keys"
        );
        assert!(skill.instructions.contains("The actual instructions."));
    }

    // ── discover_skills tests ────────────────────────────────────────────────

    /// Helper: create a `skills/<name>/SKILL.md` under `base_dir`.
    fn make_skill(base_dir: &std::path::Path, name: &str, content: &str) {
        let skill_dir = base_dir.join("skills").join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    /// Helper: create `skills/` dir but no SKILL.md inside the skill subdir.
    fn make_skill_dir_no_file(base_dir: &std::path::Path, name: &str) {
        fs::create_dir_all(base_dir.join("skills").join(name)).unwrap();
    }

    /// Helper: minimal valid SKILL.md content with a given description.
    fn minimal_skill_md(description: &str) -> String {
        format!("---\ndescription: {description}\n---\n")
    }

    /// Single level with one valid skill — skill is discovered.
    #[test]
    fn single_level_single_skill_discovered() {
        let dir = TempDir::new().unwrap();
        make_skill(dir.path(), "my-skill", &minimal_skill_md("Does the thing."));

        let (skills, warnings) = discover_skills(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(skills.len(), 1, "one skill should be found");
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description, "Does the thing.");
        assert_eq!(skills[0].level, HierarchyLevel::Global);
        assert!(warnings.is_empty());
    }

    /// Missing `skills/` directory is silently skipped — no skills, no warnings.
    #[test]
    fn missing_skills_directory_silently_skipped() {
        let dir = TempDir::new().unwrap();
        // No skills/ subdirectory created.
        let (skills, warnings) = discover_skills(&[(HierarchyLevel::Global, dir.path())]);
        assert!(skills.is_empty(), "no skills from missing dir");
        assert!(warnings.is_empty(), "no warnings from missing dir");
    }

    /// Skill directory with no SKILL.md is silently skipped — not a warning.
    #[test]
    fn skill_dir_without_skill_md_silently_skipped() {
        let dir = TempDir::new().unwrap();
        make_skill_dir_no_file(dir.path(), "incomplete");

        let (skills, warnings) = discover_skills(&[(HierarchyLevel::Global, dir.path())]);

        assert!(skills.is_empty(), "no SKILL.md means no skill");
        assert!(warnings.is_empty(), "missing SKILL.md is silently skipped");
    }

    /// Invalid directory name (uppercase letter) produces a warning and is skipped.
    #[test]
    fn invalid_dir_name_uppercase_produces_warning() {
        let dir = TempDir::new().unwrap();
        // Create the skills/ dir and a subdirectory with an invalid name.
        fs::create_dir_all(dir.path().join("skills").join("MySkill")).unwrap();

        let (skills, warnings) = discover_skills(&[(HierarchyLevel::Global, dir.path())]);

        assert!(skills.is_empty());
        assert_eq!(warnings.len(), 1, "one warning for the invalid name");
        assert!(
            warnings[0].reason.to_lowercase().contains("name")
                || warnings[0].reason.to_lowercase().contains("invalid"),
            "warning should describe the invalid name: {:?}",
            warnings[0].reason,
        );
    }

    /// Invalid directory name (contains space) produces a warning.
    #[test]
    fn invalid_dir_name_with_space_produces_warning() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("skills").join("my skill")).unwrap();

        let (skills, warnings) = discover_skills(&[(HierarchyLevel::Global, dir.path())]);

        assert!(skills.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    /// Same-name skill at two levels — only the more specific (Agent) is kept.
    #[test]
    fn same_name_at_two_levels_keeps_most_specific() {
        let global_dir = TempDir::new().unwrap();
        let agent_dir = TempDir::new().unwrap();

        make_skill(
            global_dir.path(),
            "shared",
            &minimal_skill_md("Global version."),
        );
        make_skill(
            agent_dir.path(),
            "shared",
            &minimal_skill_md("Agent version."),
        );

        let levels = [
            (HierarchyLevel::Global, global_dir.path()),
            (HierarchyLevel::Agent, agent_dir.path()),
        ];
        let (skills, warnings) = discover_skills(&levels);

        assert_eq!(
            skills.len(),
            1,
            "only the more specific skill should survive"
        );
        assert_eq!(
            skills[0].level,
            HierarchyLevel::Agent,
            "agent-level skill should override global"
        );
        assert_eq!(skills[0].description, "Agent version.");
        assert!(warnings.is_empty());
    }

    /// Unique skills from multiple levels are all returned.
    #[test]
    fn unique_skills_from_multiple_levels_all_returned() {
        let global_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();

        make_skill(
            global_dir.path(),
            "global-only",
            &minimal_skill_md("Global skill."),
        );
        make_skill(
            space_dir.path(),
            "space-only",
            &minimal_skill_md("Space skill."),
        );

        let levels = [
            (HierarchyLevel::Global, global_dir.path()),
            (HierarchyLevel::Space, space_dir.path()),
        ];
        let (skills, warnings) = discover_skills(&levels);

        assert_eq!(skills.len(), 2, "both unique skills should be present");
        assert!(warnings.is_empty());
    }

    /// Malformed SKILL.md produces a warning; other skills in the same level still load.
    #[test]
    fn malformed_skill_md_warns_others_still_load() {
        let dir = TempDir::new().unwrap();
        make_skill(dir.path(), "good-skill", &minimal_skill_md("Works fine."));
        // No description → will produce a warning
        make_skill(dir.path(), "bad-skill", "---\nname: Bad\n---\n");

        let (skills, warnings) = discover_skills(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(skills.len(), 1, "only the valid skill should load");
        assert_eq!(skills[0].name, "good-skill");
        assert_eq!(warnings.len(), 1, "one warning for the bad skill");
    }

    /// Skills within the same level are sorted alphabetically by name.
    #[test]
    fn skills_within_level_sorted_alphabetically() {
        let dir = TempDir::new().unwrap();
        make_skill(dir.path(), "zebra", &minimal_skill_md("Z skill."));
        make_skill(dir.path(), "alpha", &minimal_skill_md("A skill."));
        make_skill(dir.path(), "middle", &minimal_skill_md("M skill."));

        let (skills, _) = discover_skills(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(skills.len(), 3);
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[1].name, "middle");
        assert_eq!(skills[2].name, "zebra");
    }

    /// Sorting across levels: Agent skills sort before Space, Space before Global.
    #[test]
    fn skills_sorted_agent_before_space_before_global() {
        let global_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();
        let agent_dir = TempDir::new().unwrap();

        make_skill(global_dir.path(), "g-skill", &minimal_skill_md("Global."));
        make_skill(space_dir.path(), "p-skill", &minimal_skill_md("Space."));
        make_skill(agent_dir.path(), "a-skill", &minimal_skill_md("Agent."));

        let levels = [
            (HierarchyLevel::Global, global_dir.path()),
            (HierarchyLevel::Space, space_dir.path()),
            (HierarchyLevel::Agent, agent_dir.path()),
        ];
        let (skills, _) = discover_skills(&levels);

        assert_eq!(skills.len(), 3);
        assert_eq!(skills[0].level, HierarchyLevel::Agent, "agent first");
        assert_eq!(skills[1].level, HierarchyLevel::Space, "space second");
        assert_eq!(skills[2].level, HierarchyLevel::Global, "global last");
    }
}
