use crate::error::{ConfigProblem, ConfigProblemKind};
use std::fs;
use std::path::Path;

/// File name of the standard agent-guidance file read from a guidance root.
pub const AGENT_GUIDANCE_FILE: &str = "AGENTS.md";

/// Reads the standard agent-guidance file (`AGENTS.md`) at the root of `dir`.
///
/// Returns `Ok(None)` when the file does not exist — a root without a
/// guidance file simply contributes nothing. A path that exists but is not a
/// regular file is treated the same way. Unreadable or non-UTF-8 content
/// produces an `InstructionError` naming the offending file.
pub fn read_agent_guidance(dir: &Path) -> Result<Option<String>, ConfigProblem> {
    let path = dir.join(AGENT_GUIDANCE_FILE);
    if !path.is_file() {
        return Ok(None);
    }

    let text = fs::read_to_string(&path).map_err(|e| ConfigProblem {
        file_path: path.clone(),
        field_path: None,
        kind: ConfigProblemKind::InstructionError {
            message: format!("failed to read agent guidance file: {e}"),
        },
    })?;

    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn reads_guidance_file_content() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(AGENT_GUIDANCE_FILE), "Prefer short replies.").unwrap();

        let content = read_agent_guidance(tmp.path()).unwrap();
        assert_eq!(content.as_deref(), Some("Prefer short replies."));
    }

    #[test]
    fn absent_file_contributes_nothing() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read_agent_guidance(tmp.path()).unwrap(), None);
    }

    #[test]
    fn missing_root_contributes_nothing() {
        assert_eq!(
            read_agent_guidance(Path::new("/no/such/root")).unwrap(),
            None
        );
    }

    #[test]
    fn directory_named_like_guidance_file_contributes_nothing() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join(AGENT_GUIDANCE_FILE)).unwrap();

        assert_eq!(read_agent_guidance(tmp.path()).unwrap(), None);
    }

    #[test]
    fn non_utf8_content_is_an_instruction_error() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(AGENT_GUIDANCE_FILE), [0xff, 0xfe, 0xfd]).unwrap();

        let problem = read_agent_guidance(tmp.path()).unwrap_err();
        assert_eq!(problem.file_path, tmp.path().join(AGENT_GUIDANCE_FILE));
        assert!(matches!(
            problem.kind,
            ConfigProblemKind::InstructionError { .. }
        ));
    }
}
