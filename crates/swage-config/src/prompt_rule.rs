//! Prompt-rule parsing primitives used by context elements.

use std::path::{Path, PathBuf};

use crate::model::HierarchyLevel;

/// A parsed prompt rule.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptRule {
    pub priority: i32,
    pub content: String,
    pub source_level: HierarchyLevel,
    pub file_path: PathBuf,
}

/// A non-fatal parse failure for a prompt-rule file.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptRuleWarning {
    pub file_path: PathBuf,
    pub reason: String,
}

/// Parse one prompt-rule file and remove its optional YAML frontmatter.
pub fn parse_prompt_rule_file(
    path: &Path,
    source_level: HierarchyLevel,
) -> Result<PromptRule, PromptRuleWarning> {
    let raw = std::fs::read_to_string(path).map_err(|error| PromptRuleWarning {
        file_path: path.to_path_buf(),
        reason: format!("failed to read file: {error}"),
    })?;
    let (priority, content) = parse_frontmatter(&raw).map_err(|reason| PromptRuleWarning {
        file_path: path.to_path_buf(),
        reason,
    })?;
    Ok(PromptRule {
        priority,
        content: content.trim().to_string(),
        source_level,
        file_path: path.to_path_buf(),
    })
}

fn parse_frontmatter(raw: &str) -> Result<(i32, String), String> {
    if !raw.starts_with("---\n") && !raw.starts_with("---\r\n") {
        return Ok((0, raw.to_string()));
    }
    let lines: Vec<&str> = raw.lines().collect();
    let close = lines
        .iter()
        .skip(1)
        .position(|line| *line == "---")
        .map(|index| index + 1)
        .ok_or_else(|| "frontmatter block is not closed (missing closing `---`)".to_string())?;
    Ok((
        extract_priority(&lines[1..close].join("\n"))?,
        lines[close + 1..].join("\n"),
    ))
}

fn extract_priority(block: &str) -> Result<i32, String> {
    let mut priority = 0;
    for line in block.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let colon = line.find(':').ok_or_else(|| {
            format!("malformed frontmatter line (expected `key: value`): {line:?}")
        })?;
        if line[..colon].trim() == "priority" {
            priority = line[colon + 1..].trim().parse().map_err(|_| {
                format!(
                    "priority must be an integer, got: {:?}",
                    line[colon + 1..].trim()
                )
            })?;
        }
    }
    Ok(priority)
}
