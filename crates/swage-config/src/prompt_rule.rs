//! Prompt rule loading — priority-ordered markdown files with YAML frontmatter.
//!
//! Prompt rules are individual `.md` files discovered at each config hierarchy
//! level. Each file may have YAML frontmatter specifying a priority value.
//! Rules from all levels are merged additively and sorted by priority.

use std::path::{Path, PathBuf};

use crate::model::HierarchyLevel;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// A single prompt rule loaded from a markdown file at a config hierarchy level.
///
/// Rules are assembled in priority order: higher `priority` values appear
/// earlier in the context (as system instruction content). Rules with equal
/// priority are ordered by hierarchy level specificity (Agent > Project >
/// Global) and then by filename.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptRule {
    /// Ordering priority (higher = earlier). Defaults to 0 when not specified.
    pub priority: i32,
    /// Markdown content with YAML frontmatter stripped.
    pub content: String,
    /// Which hierarchy level this rule was discovered at.
    pub source_level: HierarchyLevel,
    /// Originating file path — used for diagnostics and the assembly report.
    pub file_path: PathBuf,
}

/// Describes a prompt rule file that was skipped during loading.
///
/// Warnings are non-fatal: they indicate a file was found but could not be
/// parsed. The load continues with the remaining files.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptRuleWarning {
    /// Path of the file that was skipped.
    pub file_path: PathBuf,
    /// Human-readable reason the file was skipped.
    pub reason: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// Parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a single prompt rule file.
///
/// Reads the file at `path`, extracts optional YAML frontmatter between
/// `---` delimiters, and returns a [`PromptRule`] with the frontmatter
/// stripped from the content.
///
/// # Frontmatter format
///
/// ```markdown
/// ---
/// priority: 100
/// ---
///
/// Rule content here.
/// ```
///
/// Only the `priority` key is used; other keys are silently ignored.
/// If no frontmatter is present, `priority` defaults to 0.
/// If frontmatter is present but malformed (unclosed block, non-integer
/// priority, or invalid syntax), a [`PromptRuleWarning`] is returned and
/// the file is skipped.
pub fn parse_prompt_rule_file(
    path: &Path,
    source_level: HierarchyLevel,
) -> Result<PromptRule, PromptRuleWarning> {
    let raw = std::fs::read_to_string(path).map_err(|e| PromptRuleWarning {
        file_path: path.to_path_buf(),
        reason: format!("failed to read file: {e}"),
    })?;

    let (priority, content) = parse_frontmatter(&raw).map_err(|reason| PromptRuleWarning {
        file_path: path.to_path_buf(),
        reason,
    })?;

    // Trim leading/trailing blank lines from the body, but keep internal structure.
    let content = content.trim().to_string();

    Ok(PromptRule {
        priority,
        content,
        source_level,
        file_path: path.to_path_buf(),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Frontmatter parsing (internal)
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a markdown string's YAML frontmatter and return `(priority, body)`.
///
/// Returns an error string (for embedding in a warning) if the frontmatter
/// block is malformed. Missing frontmatter is not an error — it returns
/// `(0, raw)`.
fn parse_frontmatter(raw: &str) -> Result<(i32, String), String> {
    // No frontmatter — return the entire content with default priority.
    if !raw.starts_with("---\n") && !raw.starts_with("---\r\n") {
        return Ok((0, raw.to_string()));
    }

    // Split by lines so we can find the closing --- as a standalone line.
    // This correctly handles empty frontmatter blocks (---\n---\n) and
    // avoids the ambiguity of searching for "\n---" in the raw bytes.
    let lines: Vec<&str> = raw.lines().collect();

    // lines[0] is the opening "---"; find the next line containing only "---".
    let close_idx = lines.iter().skip(1)
        .position(|line| *line == "---")
        .ok_or_else(|| "frontmatter block is not closed (missing closing `---`)".to_string())?;

    // Adjust index back to the full lines array (skip(1) offset).
    let close_idx = close_idx + 1;

    let frontmatter_block = lines[1..close_idx].join("\n");
    let body = lines[close_idx + 1..].join("\n");

    // Parse the frontmatter lines as simple `key: value` pairs.
    // We only care about `priority`; other keys are accepted but ignored.
    let priority = extract_priority(&frontmatter_block)?;

    Ok((priority, body))
}

/// Extract the `priority` integer from a YAML frontmatter block string.
///
/// Returns `0` if no `priority` key is present. Returns an error if the
/// frontmatter contains lines that are not valid `key: value` pairs
/// (i.e. clearly malformed YAML), or if `priority` is not an integer.
fn extract_priority(block: &str) -> Result<i32, String> {
    let mut priority = 0i32;

    for line in block.lines() {
        let line = line.trim();

        // Skip blank lines and YAML comments.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Every non-comment line must be a `key: value` pair.
        let colon = line.find(':').ok_or_else(|| {
            format!("malformed frontmatter line (expected `key: value`): {line:?}")
        })?;

        let key = line[..colon].trim();
        let value = line[colon + 1..].trim();

        if key == "priority" {
            priority = value.parse::<i32>().map_err(|_| {
                format!("priority must be an integer, got: {value:?}")
            })?;
        }
        // Other keys are silently ignored.
    }

    Ok(priority)
}

// ─────────────────────────────────────────────────────────────────────────────
// Discovery
// ─────────────────────────────────────────────────────────────────────────────

/// Specificity rank for sorting (lower = more specific = earlier in output).
///
/// Agent is the most specific level, so it receives rank 0 and sorts first
/// among rules at equal priority.
fn level_specificity(level: &HierarchyLevel) -> i32 {
    match level {
        HierarchyLevel::Agent   => 0,
        HierarchyLevel::Team    => 1,
        HierarchyLevel::Project => 2,
        HierarchyLevel::Lens    => 3,
        HierarchyLevel::Global  => 4,
    }
}

/// Discover and load prompt rules from a set of hierarchy levels.
///
/// For each `(level, base_dir)` pair, scans `base_dir/prompts/` for `*.md`
/// files and parses each via [`parse_prompt_rule_file`]. A fallback to
/// `base_dir/instructions/` is supported for backward compatibility.
///
/// Returned rules are sorted by:
/// 1. **Priority descending** — higher `priority` value first
/// 2. **Level specificity** — Agent > Team > Project > Lens > Global
/// 3. **Filename alphabetically** — deterministic tie-break
///
/// Missing directories are silently skipped. If both `prompts/` and
/// `instructions/` exist at the same level, `prompts/` takes precedence and
/// a warning is added pointing at the `instructions/` directory.
pub fn load_prompt_rules(levels: &[(HierarchyLevel, &Path)]) -> (Vec<PromptRule>, Vec<PromptRuleWarning>) {
    let mut rules    = Vec::new();
    let mut warnings = Vec::new();

    for (level, base_dir) in levels {
        let prompts_dir      = base_dir.join("prompts");
        let instructions_dir = base_dir.join("instructions");

        let prompts_exists      = prompts_dir.is_dir();
        let instructions_exists = instructions_dir.is_dir();

        // Determine which directory to scan, handling the backward-compat case.
        let scan_dir: Option<PathBuf> = if prompts_exists {
            if instructions_exists {
                // Both exist: use prompts/, warn that instructions/ is shadowed.
                warnings.push(PromptRuleWarning {
                    file_path: instructions_dir,
                    reason: format!(
                        "both prompts/ and instructions/ directories exist at {}; \
                         prompts/ takes precedence",
                        base_dir.display(),
                    ),
                });
            }
            Some(prompts_dir)
        } else if instructions_exists {
            // Only instructions/ exists — use it as a fallback, no warning.
            Some(instructions_dir)
        } else {
            None
        };

        let Some(dir) = scan_dir else { continue };

        // Collect .md files alphabetically so the filename sort is stable
        // regardless of the order the OS returns directory entries.
        let mut md_files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
                .collect(),
            Err(_) => continue,
        };
        md_files.sort();

        for file_path in md_files {
            match parse_prompt_rule_file(&file_path, level.clone()) {
                Ok(rule)    => rules.push(rule),
                Err(warning) => warnings.push(warning),
            }
        }
    }

    // Sort: priority desc → level specificity asc → filename asc.
    rules.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| level_specificity(&a.source_level).cmp(&level_specificity(&b.source_level)))
            .then_with(|| a.file_path.cmp(&b.file_path))
    });

    (rules, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── parse_prompt_rule_file tests ─────────────────────────────────────────

    /// File with valid frontmatter — priority extracted and content stripped.
    #[test]
    fn file_with_valid_frontmatter_parses_correctly() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rule.md");
        fs::write(&path, "---\npriority: 100\n---\n\nYou are a helpful assistant.").unwrap();

        let rule = parse_prompt_rule_file(&path, HierarchyLevel::Global).unwrap();

        assert_eq!(rule.priority, 100);
        assert_eq!(rule.content, "You are a helpful assistant.");
        assert_eq!(rule.source_level, HierarchyLevel::Global);
        assert_eq!(rule.file_path, path);
    }

    /// File with no frontmatter — default priority 0, full content returned.
    #[test]
    fn file_with_no_frontmatter_gets_default_priority_zero() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rule.md");
        fs::write(&path, "You are a coding assistant.").unwrap();

        let rule = parse_prompt_rule_file(&path, HierarchyLevel::Agent).unwrap();

        assert_eq!(rule.priority, 0);
        assert_eq!(rule.content, "You are a coding assistant.");
    }

    /// Frontmatter with unknown keys — only priority is used; other keys ignored.
    #[test]
    fn frontmatter_with_unknown_keys_ignores_them_silently() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rule.md");
        fs::write(&path, "---\npriority: 50\ntitle: My Rule\nauthor: test\n---\n\nContent.").unwrap();

        let rule = parse_prompt_rule_file(&path, HierarchyLevel::Project).unwrap();

        assert_eq!(rule.priority, 50);
        assert_eq!(rule.content, "Content.");
    }

    /// Frontmatter that is malformed (non-integer priority) returns a warning.
    #[test]
    fn malformed_frontmatter_priority_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rule.md");
        fs::write(&path, "---\npriority: high\n---\n\nContent.").unwrap();

        let result = parse_prompt_rule_file(&path, HierarchyLevel::Global);

        assert!(result.is_err(), "malformed priority should produce a warning");
        let warning = result.unwrap_err();
        assert_eq!(warning.file_path, path);
        assert!(warning.reason.contains("priority"), "reason should mention priority");
    }

    /// Frontmatter that is malformed (unclosed block) returns a warning.
    #[test]
    fn unclosed_frontmatter_block_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rule.md");
        fs::write(&path, "---\npriority: 10\n\nContent without closing delimiter.").unwrap();

        let result = parse_prompt_rule_file(&path, HierarchyLevel::Global);

        assert!(result.is_err(), "unclosed frontmatter should produce a warning");
        let warning = result.unwrap_err();
        assert!(warning.reason.contains("not closed") || warning.reason.contains("missing"),
            "reason should describe the unclosed block: {:?}", warning.reason);
    }

    /// Frontmatter with an invalid line (no colon) returns a warning.
    #[test]
    fn frontmatter_with_invalid_line_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rule.md");
        fs::write(&path, "---\ninvalid line without colon\n---\n\nContent.").unwrap();

        let result = parse_prompt_rule_file(&path, HierarchyLevel::Global);

        assert!(result.is_err(), "invalid frontmatter line should produce a warning");
    }

    /// Frontmatter is stripped and body content is trimmed.
    #[test]
    fn frontmatter_is_stripped_from_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rule.md");
        fs::write(&path,
            "---\npriority: 200\n---\n\nAlways be safe and helpful.\n\nSecond paragraph.\n",
        ).unwrap();

        let rule = parse_prompt_rule_file(&path, HierarchyLevel::Global).unwrap();

        // Frontmatter must not appear in content.
        assert!(!rule.content.contains("priority:"),
            "content should not contain frontmatter");
        assert!(!rule.content.contains("---"),
            "content should not contain frontmatter delimiters");
        assert!(rule.content.contains("Always be safe"),
            "content should contain the rule body");
    }

    /// Non-existent file returns a warning (not a panic).
    #[test]
    fn missing_file_returns_warning() {
        let path = PathBuf::from("/nonexistent/path/rule.md");
        let result = parse_prompt_rule_file(&path, HierarchyLevel::Global);
        assert!(result.is_err(), "missing file should produce a warning");
    }

    // ── parse_frontmatter internal tests ────────────────────────────────────

    /// Empty frontmatter block returns priority 0.
    #[test]
    fn empty_frontmatter_block_returns_default_priority() {
        let (priority, body) = parse_frontmatter("---\n---\n\nContent.").unwrap();
        assert_eq!(priority, 0);
        assert!(body.contains("Content."));
    }

    /// YAML comments in frontmatter are ignored.
    #[test]
    fn frontmatter_yaml_comments_are_ignored() {
        let raw = "---\n# This is a comment\npriority: 42\n---\n\nContent.";
        let (priority, _) = parse_frontmatter(raw).unwrap();
        assert_eq!(priority, 42);
    }

    // ── load_prompt_rules tests ──────────────────────────────────────────────

    fn write_rule(dir: &Path, filename: &str, content: &str) {
        fs::write(dir.join(filename), content).unwrap();
    }

    fn make_prompts_dir(base: &Path) -> PathBuf {
        let p = base.join("prompts");
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_instructions_dir(base: &Path) -> PathBuf {
        let i = base.join("instructions");
        fs::create_dir_all(&i).unwrap();
        i
    }

    /// Single level with one rule file — rule is discovered and returned.
    #[test]
    fn single_level_single_rule_discovered() {
        let dir = TempDir::new().unwrap();
        let prompts = make_prompts_dir(dir.path());
        write_rule(&prompts, "a.md", "---\npriority: 10\n---\n\nHello.");

        let (rules, warnings) = load_prompt_rules(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(rules.len(), 1, "one rule should be found");
        assert_eq!(rules[0].priority, 10);
        assert_eq!(rules[0].content, "Hello.");
        assert!(warnings.is_empty(), "no warnings expected");
    }

    /// Missing prompts/ directory is silently skipped — no rules, no warnings.
    #[test]
    fn missing_directory_silently_skipped() {
        let dir = TempDir::new().unwrap();
        // No prompts/ subdirectory created.
        let (rules, warnings) = load_prompt_rules(&[(HierarchyLevel::Global, dir.path())]);
        assert!(rules.is_empty(), "no rules from missing dir");
        assert!(warnings.is_empty(), "no warnings from missing dir");
    }

    /// Non-.md files in prompts/ are ignored.
    #[test]
    fn non_md_files_ignored() {
        let dir = TempDir::new().unwrap();
        let prompts = make_prompts_dir(dir.path());
        write_rule(&prompts, "rule.md", "Content.");
        fs::write(prompts.join("readme.txt"), "not a rule").unwrap();
        fs::write(prompts.join("script.sh"), "#!/bin/bash").unwrap();

        let (rules, warnings) = load_prompt_rules(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(rules.len(), 1, "only .md files should be loaded");
        assert!(warnings.is_empty());
    }

    /// Rules from multiple levels are all returned.
    #[test]
    fn rules_from_multiple_levels_merged() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();

        make_prompts_dir(global_dir.path());
        write_rule(&global_dir.path().join("prompts"), "g.md", "Global.");
        make_prompts_dir(project_dir.path());
        write_rule(&project_dir.path().join("prompts"), "p.md", "Project.");

        let levels = [
            (HierarchyLevel::Global, global_dir.path()),
            (HierarchyLevel::Project, project_dir.path()),
        ];
        let (rules, warnings) = load_prompt_rules(&levels);

        assert_eq!(rules.len(), 2, "rules from both levels should be present");
        assert!(warnings.is_empty());
    }

    /// Sorting: higher priority comes first.
    #[test]
    fn sorting_priority_descending() {
        let dir = TempDir::new().unwrap();
        let prompts = make_prompts_dir(dir.path());
        write_rule(&prompts, "low.md",  "---\npriority: 5\n---\n\nLow.");
        write_rule(&prompts, "high.md", "---\npriority: 100\n---\n\nHigh.");
        write_rule(&prompts, "mid.md",  "---\npriority: 50\n---\n\nMid.");

        let (rules, _) = load_prompt_rules(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].priority, 100, "highest priority first");
        assert_eq!(rules[1].priority, 50);
        assert_eq!(rules[2].priority, 5, "lowest priority last");
    }

    /// Sorting: at equal priority, Agent-level rules precede Project-level rules.
    #[test]
    fn sorting_level_specificity_agent_before_global() {
        let global_dir = TempDir::new().unwrap();
        let agent_dir  = TempDir::new().unwrap();

        make_prompts_dir(global_dir.path());
        write_rule(&global_dir.path().join("prompts"), "rule.md", "Global rule.");
        make_prompts_dir(agent_dir.path());
        write_rule(&agent_dir.path().join("prompts"), "rule.md", "Agent rule.");

        let levels = [
            (HierarchyLevel::Global, global_dir.path()),
            (HierarchyLevel::Agent,  agent_dir.path()),
        ];
        let (rules, _) = load_prompt_rules(&levels);

        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].source_level, HierarchyLevel::Agent,
            "Agent rule should come before Global at equal priority");
        assert_eq!(rules[1].source_level, HierarchyLevel::Global);
    }

    /// Sorting: at equal priority and level, rules are sorted alphabetically by filename.
    #[test]
    fn sorting_filename_alphabetical_as_tiebreaker() {
        let dir = TempDir::new().unwrap();
        let prompts = make_prompts_dir(dir.path());
        write_rule(&prompts, "c.md", "C.");
        write_rule(&prompts, "a.md", "A.");
        write_rule(&prompts, "b.md", "B.");

        let (rules, _) = load_prompt_rules(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(rules.len(), 3);
        assert!(rules[0].file_path.ends_with("a.md"), "a.md should be first alphabetically");
        assert!(rules[1].file_path.ends_with("b.md"));
        assert!(rules[2].file_path.ends_with("c.md"));
    }

    /// Malformed file generates a warning; other files in the same dir still load.
    #[test]
    fn malformed_file_generates_warning_others_still_load() {
        let dir = TempDir::new().unwrap();
        let prompts = make_prompts_dir(dir.path());
        write_rule(&prompts, "good.md",    "Good content.");
        write_rule(&prompts, "bad.md",     "---\npriority: high\n---\n\nBad.");

        let (rules, warnings) = load_prompt_rules(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(rules.len(), 1, "valid file should still load");
        assert_eq!(warnings.len(), 1, "one warning for the bad file");
        assert!(warnings[0].file_path.ends_with("bad.md"));
    }

    /// Both prompts/ and instructions/ at the same level: prompts/ wins, warning issued.
    #[test]
    fn both_dirs_uses_prompts_and_warns() {
        let dir = TempDir::new().unwrap();
        let prompts = make_prompts_dir(dir.path());
        let instructions = make_instructions_dir(dir.path());

        write_rule(&prompts,      "from_prompts.md",      "From prompts.");
        write_rule(&instructions, "from_instructions.md", "From instructions.");

        let (rules, warnings) = load_prompt_rules(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(rules.len(), 1, "only prompts/ content should load");
        assert_eq!(rules[0].content, "From prompts.");
        assert_eq!(warnings.len(), 1, "one warning about both dirs existing");
        assert!(warnings[0].reason.contains("instructions"),
            "warning should mention instructions/");
    }

    /// Only instructions/ (no prompts/): backward compat — rules loaded from instructions/.
    #[test]
    fn only_instructions_dir_loads_as_fallback() {
        let dir = TempDir::new().unwrap();
        let instructions = make_instructions_dir(dir.path());
        write_rule(&instructions, "rule.md", "From instructions.");

        let (rules, warnings) = load_prompt_rules(&[(HierarchyLevel::Global, dir.path())]);

        assert_eq!(rules.len(), 1, "instructions/ should be used as fallback");
        assert_eq!(rules[0].content, "From instructions.");
        assert!(warnings.is_empty(), "no warnings when only instructions/ exists");
    }
}
