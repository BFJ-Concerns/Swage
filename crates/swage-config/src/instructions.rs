use crate::error::{ConfigProblem, ConfigProblemKind};
use crate::model::HierarchyLevel;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// File collection
// ---------------------------------------------------------------------------

/// Returns sorted paths of all `.md` files in `dir`.
///
/// Returns `Ok(empty vec)` when the directory does not exist — a missing
/// instructions directory is not an error, the level simply contributes nothing.
pub fn collect_instruction_files(dir: &Path) -> Result<Vec<PathBuf>, ConfigProblem> {
    if !dir.exists() {
        return Ok(vec![]);
    }

    let read = fs::read_dir(dir).map_err(|e| ConfigProblem {
        file_path: dir.to_path_buf(),
        field_path: None,
        kind: ConfigProblemKind::InstructionError {
            message: format!("failed to read instructions directory: {e}"),
        },
    })?;

    let mut paths: Vec<PathBuf> = read
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|ext| ext.to_str()) == Some("md"))
        .collect();

    // Alphabetical sort by filename so ordering is deterministic and
    // operators can control composition by naming files (01-base.md, etc.).
    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    Ok(paths)
}

// ---------------------------------------------------------------------------
// File reading
// ---------------------------------------------------------------------------

/// Reads and returns the text content of all `.md` files in `dir`,
/// sorted alphabetically by filename.
///
/// Non-UTF-8 content produces an `InstructionError` naming the offending file.
pub fn read_instruction_files(dir: &Path) -> Result<Vec<String>, ConfigProblem> {
    let paths = collect_instruction_files(dir)?;
    let mut contents = Vec::with_capacity(paths.len());

    for path in &paths {
        let text = fs::read_to_string(path).map_err(|e| ConfigProblem {
            file_path: path.clone(),
            field_path: None,
            kind: ConfigProblemKind::InstructionError {
                message: format!("failed to read instruction file: {e}"),
            },
        })?;
        contents.push(text);
    }

    Ok(contents)
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// Assembles a single instruction string from multiple hierarchy levels.
///
/// Each `(level, instructions_dir)` pair contributes its `.md` files,
/// preceded by an HTML attribution comment so the source level is visible
/// when inspecting the assembled prompt without consuming meaningful tokens.
///
/// Levels that have no `.md` files (or a missing directory) contribute
/// nothing — no attribution header is emitted for empty levels.
pub fn assemble_instructions(levels: &[(HierarchyLevel, &Path)]) -> Result<String, ConfigProblem> {
    let mut parts: Vec<String> = Vec::new();

    for (level, dir) in levels {
        let contents = read_instruction_files(dir)?;
        if contents.is_empty() {
            // Empty or missing level — omit attribution header entirely.
            continue;
        }

        // Attribution marker. HTML comment keeps it out of the rendered
        // markdown while remaining visible in the raw source.
        parts.push(format!("<!-- level: {level} -->"));
        for content in contents {
            parts.push(content);
        }
    }

    Ok(parts.join("\n\n"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn collect_returns_sorted_md_files() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "02-safety.md", "safety");
        write_file(tmp.path(), "01-base.md", "base");
        write_file(tmp.path(), "notes.txt", "ignored");

        let files = collect_instruction_files(tmp.path()).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["01-base.md", "02-safety.md"],
            "should return sorted .md files only"
        );
    }

    #[test]
    fn collect_returns_empty_when_dir_missing() {
        let files = collect_instruction_files(Path::new("/no/such/dir")).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn collect_ignores_non_md_files() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "a.md", "yes");
        write_file(tmp.path(), "b.txt", "no");
        write_file(tmp.path(), "c.json", "no");

        let files = collect_instruction_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name().unwrap(), "a.md");
    }

    #[test]
    fn read_instruction_files_returns_content_in_order() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "02-b.md", "second");
        write_file(tmp.path(), "01-a.md", "first");

        let contents = read_instruction_files(tmp.path()).unwrap();
        assert_eq!(contents, vec!["first", "second"]);
    }

    #[test]
    fn read_instruction_files_empty_when_dir_missing() {
        let contents = read_instruction_files(Path::new("/no/such/dir")).unwrap();
        assert!(contents.is_empty());
    }

    #[test]
    fn assemble_includes_content_from_all_levels() {
        let global_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();
        let agent_dir = TempDir::new().unwrap();

        write_file(global_dir.path(), "base.md", "global content");
        write_file(space_dir.path(), "context.md", "space content");
        write_file(agent_dir.path(), "persona.md", "agent content");

        let levels: Vec<(HierarchyLevel, &Path)> = vec![
            (HierarchyLevel::Global, global_dir.path()),
            (HierarchyLevel::Space, space_dir.path()),
            (HierarchyLevel::Agent, agent_dir.path()),
        ];

        let result = assemble_instructions(&levels).unwrap();
        assert!(
            result.contains("global content"),
            "should include global content"
        );
        assert!(
            result.contains("space content"),
            "should include space content"
        );
        assert!(
            result.contains("agent content"),
            "should include agent content"
        );
    }

    #[test]
    fn assemble_uses_attribution_markers() {
        let global_dir = TempDir::new().unwrap();
        write_file(global_dir.path(), "a.md", "some text");

        let levels: Vec<(HierarchyLevel, &Path)> =
            vec![(HierarchyLevel::Global, global_dir.path())];

        let result = assemble_instructions(&levels).unwrap();
        assert!(
            result.contains("Global"),
            "should include level name in attribution"
        );
    }

    #[test]
    fn assemble_orders_global_before_space_before_agent() {
        let global_dir = TempDir::new().unwrap();
        let space_dir = TempDir::new().unwrap();
        let agent_dir = TempDir::new().unwrap();

        write_file(global_dir.path(), "a.md", "GLOBAL");
        write_file(space_dir.path(), "a.md", "SPACE");
        write_file(agent_dir.path(), "a.md", "AGENT");

        let levels: Vec<(HierarchyLevel, &Path)> = vec![
            (HierarchyLevel::Global, global_dir.path()),
            (HierarchyLevel::Space, space_dir.path()),
            (HierarchyLevel::Agent, agent_dir.path()),
        ];

        let result = assemble_instructions(&levels).unwrap();
        let global_pos = result.find("GLOBAL").unwrap();
        let space_pos = result.find("SPACE").unwrap();
        let agent_pos = result.find("AGENT").unwrap();
        assert!(global_pos < space_pos, "Global should come before Space");
        assert!(space_pos < agent_pos, "Space should come before Agent");
    }

    #[test]
    fn assemble_skips_missing_directory_silently() {
        let existing_dir = TempDir::new().unwrap();
        write_file(existing_dir.path(), "a.md", "present");

        let levels: Vec<(HierarchyLevel, &Path)> = vec![
            (HierarchyLevel::Global, existing_dir.path()),
            (HierarchyLevel::Agent, Path::new("/no/such/agent/dir")),
        ];

        let result = assemble_instructions(&levels).unwrap();
        assert!(result.contains("present"), "existing content should appear");
    }

    #[test]
    fn assemble_returns_empty_when_all_levels_empty() {
        let dir = TempDir::new().unwrap(); // no .md files

        let levels: Vec<(HierarchyLevel, &Path)> = vec![(HierarchyLevel::Global, dir.path())];

        let result = assemble_instructions(&levels).unwrap();
        // No markdown files — assembled string should contain no meaningful content
        let meaningful = result
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with("<!--"))
            .count();
        assert_eq!(
            meaningful, 0,
            "no content should appear when no .md files exist"
        );
    }
}
