//! The operator's memory: pages the platform's curator writes from what the
//! operator stated, indexed one line each.
//!
//! Memory is a set of markdown entries under a scope's data directory —
//! `<root>/general/memory/` for general memory (the profile, interests,
//! areas, people), `<root>/topics/<slug>/memory/` for a topic's own. An
//! entry's path is its subject (`people/alice`, `conventions`); its file
//! opens with one `summary:` line the curator keeps current, then a blank
//! line, then the body. The gateway assembles the index from those summary
//! lines on every turn and serves an entry's body on demand; nothing here
//! reads a transcript or decides what to remember — that is the curator's.
//!
//! References name an entry across scopes as the index spells it:
//! `general/people/alice`, `topic/conventions`. A session without a topic
//! sees general memory only.

use std::path::{Path, PathBuf};

use swage_core::ScopeLocation;

/// The one agent definition that may write memory: the platform's curator.
/// Every other definition is offered reading alone.
pub const MEMORY_CURATOR_AGENT_ID: &str = "memory-curator";

/// The general-memory entry carried whole into every turn: the operator's
/// profile and preferences.
pub const PROFILE_PATH: &str = "profile";

const SUMMARY_PREFIX: &str = "summary:";

/// Which store a reference names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryScope {
    /// General memory: the operator's own, visible in every session.
    General,
    /// The session's topic's memory; absent for a session without a topic.
    Topic,
}

impl MemoryScope {
    /// The reference prefix the index spells.
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Topic => "topic",
        }
    }
}

/// An entry's reference as the index spells it: scope label, then the
/// entry's path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemoryReference {
    pub scope: MemoryScope,
    pub path: String,
}

impl MemoryReference {
    /// Parse `general/<path>` or `topic/<path>`.
    pub fn parse(reference: &str) -> Result<Self, MemoryPathError> {
        let reference = reference.trim();
        let (scope, path) = reference.split_once('/').ok_or_else(|| {
            MemoryPathError(format!(
                "'{reference}' is not a memory reference: expected general/<entry> or topic/<entry>"
            ))
        })?;
        let scope = match scope {
            "general" => MemoryScope::General,
            "topic" => MemoryScope::Topic,
            other => {
                return Err(MemoryPathError(format!(
                    "'{other}' is not a memory scope: expected general or topic"
                )));
            }
        };
        validate_entry_path(path)?;
        Ok(Self {
            scope,
            path: path.to_string(),
        })
    }
}

impl std::fmt::Display for MemoryReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.scope.label(), self.path)
    }
}

/// A reference or path the store refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPathError(pub String);

impl std::fmt::Display for MemoryPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MemoryPathError {}

/// An entry path is one or more `/`-separated segments of letters, digits,
/// hyphens and underscores — never `.`, `..`, an empty segment or a leading
/// slash — so a reference can never leave the memory directory.
pub fn validate_entry_path(path: &str) -> Result<(), MemoryPathError> {
    if path.is_empty() {
        return Err(MemoryPathError(
            "a memory entry path must not be empty".into(),
        ));
    }
    for segment in path.split('/') {
        if segment.is_empty() {
            return Err(MemoryPathError(format!(
                "'{path}' is not a memory entry path: empty segment"
            )));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(MemoryPathError(format!(
                "'{path}' is not a memory entry path: segments use letters, digits, hyphens and underscores only"
            )));
        }
    }
    Ok(())
}

/// One entry as stored: its reference, the curator's summary line, its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    pub reference: MemoryReference,
    /// Absent when the file carries no `summary:` line.
    pub summary: Option<String>,
    pub body: String,
}

/// What a store operation can fail on beyond a bad path.
#[derive(Debug)]
pub enum MemoryError {
    Path(MemoryPathError),
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(error) => error.fmt(f),
            Self::Io { path, error } => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<MemoryPathError> for MemoryError {
    fn from(error: MemoryPathError) -> Self {
        Self::Path(error)
    }
}

fn memory_dir(root: &Path, scope_location: &ScopeLocation) -> PathBuf {
    scope_location.data_dir(root).join("memory")
}

fn entry_file(root: &Path, scope_location: &ScopeLocation, path: &str) -> PathBuf {
    memory_dir(root, scope_location).join(format!("{path}.md"))
}

/// The scope location a reference resolves against for a session in
/// `session_scope`: general memory is its own directory everywhere; topic
/// memory exists only inside a topic.
pub fn scope_location_for(
    reference: &MemoryReference,
    session_scope: &ScopeLocation,
) -> Result<ScopeLocation, MemoryPathError> {
    match reference.scope {
        MemoryScope::General => Ok(ScopeLocation::General),
        MemoryScope::Topic => match session_scope {
            ScopeLocation::General => Err(MemoryPathError(
                "this session has no topic, so it has no topic memory: use a general/ reference"
                    .into(),
            )),
            topic => Ok(topic.clone()),
        },
    }
}

/// Split a stored file into its summary line and body.
fn parse_entry(text: &str) -> (Option<String>, String) {
    let mut lines = text.lines();
    match lines.next() {
        Some(first) if first.starts_with(SUMMARY_PREFIX) => {
            let summary = first[SUMMARY_PREFIX.len()..].trim().to_string();
            let rest: Vec<&str> = lines.collect();
            let body = rest.join("\n");
            let body = body.strip_prefix('\n').unwrap_or(&body).to_string();
            (
                if summary.is_empty() {
                    None
                } else {
                    Some(summary)
                },
                body,
            )
        }
        _ => (None, text.to_string()),
    }
}

fn render_entry(summary: &str, body: &str) -> String {
    format!("{SUMMARY_PREFIX} {}\n\n{}", summary.trim(), body.trim_end())
}

/// Read one entry; `None` when nothing is recorded under that path.
pub async fn read_entry(
    root: &Path,
    scope_location: &ScopeLocation,
    reference: &MemoryReference,
) -> Result<Option<MemoryEntry>, MemoryError> {
    validate_entry_path(&reference.path)?;
    let file = entry_file(root, scope_location, &reference.path);
    match tokio::fs::read_to_string(&file).await {
        Ok(text) => {
            let (summary, body) = parse_entry(&text);
            Ok(Some(MemoryEntry {
                reference: reference.clone(),
                summary,
                body,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(MemoryError::Io { path: file, error }),
    }
}

/// Write an entry whole — its summary line and body — creating it or
/// replacing what was there. The curator's route; nothing else writes.
pub async fn write_entry(
    root: &Path,
    scope_location: &ScopeLocation,
    reference: &MemoryReference,
    summary: &str,
    body: &str,
) -> Result<(), MemoryError> {
    validate_entry_path(&reference.path)?;
    let file = entry_file(root, scope_location, &reference.path);
    if let Some(parent) = file.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| MemoryError::Io {
                path: parent.to_path_buf(),
                error,
            })?;
    }
    tokio::fs::write(&file, render_entry(summary, body))
        .await
        .map_err(|error| MemoryError::Io { path: file, error })
}

/// One line of the index: the reference and the curator's summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryIndexLine {
    pub reference: MemoryReference,
    pub summary: Option<String>,
}

/// Every entry under a scope's memory directory, by path, with its summary
/// line. A missing directory is an empty store.
pub async fn list_entries(
    root: &Path,
    scope_location: &ScopeLocation,
    scope: MemoryScope,
) -> Result<Vec<MemoryIndexLine>, MemoryError> {
    let dir = memory_dir(root, scope_location);
    let mut files = Vec::new();
    collect_entry_files(&dir, &dir, &mut files).await?;
    files.sort();
    let mut lines = Vec::with_capacity(files.len());
    for (path, file) in files {
        let text = tokio::fs::read_to_string(&file)
            .await
            .map_err(|error| MemoryError::Io {
                path: file.clone(),
                error,
            })?;
        let (summary, _) = parse_entry(&text);
        lines.push(MemoryIndexLine {
            reference: MemoryReference { scope, path },
            summary,
        });
    }
    Ok(lines)
}

/// Walk the memory directory for `.md` files, yielding each entry's path
/// relative to the directory, slash-separated and without the extension.
async fn collect_entry_files(
    base: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), MemoryError> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(MemoryError::Io {
                path: dir.to_path_buf(),
                error,
            });
        }
    };
    let mut subdirs = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| MemoryError::Io {
            path: dir.to_path_buf(),
            error,
        })?
    {
        let file = entry.path();
        if file.is_dir() {
            subdirs.push(file);
            continue;
        }
        if file.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let relative = file.strip_prefix(base).unwrap_or(&file).with_extension("");
        let path = relative
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/");
        if validate_entry_path(&path).is_ok() {
            out.push((path, file));
        }
    }
    for subdir in subdirs {
        Box::pin(collect_entry_files(base, &subdir, out)).await?;
    }
    Ok(())
}

/// What a turn carries of memory without the agent reading anything: the
/// profile whole, and one line per entry of each scope the session can see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryIndex {
    pub profile: Option<String>,
    pub lines: Vec<MemoryIndexLine>,
}

/// Assemble the index for a session in `session_scope`: general memory
/// everywhere, the topic's memory inside a topic. A store that cannot be
/// read is omitted with a warning — the turn runs with what could be read,
/// never with text nobody recorded.
pub async fn load_memory_index(root: &Path, session_scope: &ScopeLocation) -> MemoryIndex {
    let mut index = MemoryIndex::default();
    let profile_reference = MemoryReference {
        scope: MemoryScope::General,
        path: PROFILE_PATH.to_string(),
    };
    match read_entry(root, &ScopeLocation::General, &profile_reference).await {
        Ok(entry) => {
            index.profile = entry
                .map(|entry| entry.body)
                .filter(|body| !body.trim().is_empty());
        }
        Err(error) => tracing::warn!(%error, "the operator profile could not be read; omitted"),
    }
    let mut visible = vec![(ScopeLocation::General, MemoryScope::General)];
    if !session_scope.is_general() {
        visible.push((session_scope.clone(), MemoryScope::Topic));
    }
    for (location, scope) in visible {
        match list_entries(root, &location, scope).await {
            Ok(lines) => index.lines.extend(lines),
            Err(error) => tracing::warn!(
                scope = scope.label(),
                %error,
                "a memory store could not be listed; its index is omitted"
            ),
        }
    }
    index
}

impl MemoryIndex {
    /// The element the assembly carries: the same lines, spelled for the
    /// model.
    pub fn into_element(self) -> swage_context::memory::MemoryIndexElement {
        swage_context::memory::MemoryIndexElement::new(
            self.profile,
            self.lines
                .into_iter()
                .map(|line| swage_context::memory::MemoryIndexLine {
                    reference: line.reference.to_string(),
                    summary: line.summary,
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic() -> ScopeLocation {
        ScopeLocation::topic("alloys")
    }

    fn general(path: &str) -> MemoryReference {
        MemoryReference {
            scope: MemoryScope::General,
            path: path.into(),
        }
    }

    #[tokio::test]
    async fn an_entry_round_trips_with_its_summary_line_first() {
        let root = tempfile::tempdir().unwrap();
        write_entry(
            root.path(),
            &ScopeLocation::General,
            &general("people/alice"),
            "Alice is the operator's sister.",
            "Alice lives in Leeds.\n\nShe keeps bees.",
        )
        .await
        .unwrap();

        let stored = tokio::fs::read_to_string(root.path().join("general/memory/people/alice.md"))
            .await
            .unwrap();
        assert!(stored.starts_with("summary: Alice is the operator's sister.\n\n"));

        let entry = read_entry(
            root.path(),
            &ScopeLocation::General,
            &general("people/alice"),
        )
        .await
        .unwrap()
        .expect("entry");
        assert_eq!(
            entry.summary.as_deref(),
            Some("Alice is the operator's sister.")
        );
        assert_eq!(entry.body, "Alice lives in Leeds.\n\nShe keeps bees.");
        assert!(
            read_entry(root.path(), &ScopeLocation::General, &general("people/bob"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_file_without_a_summary_line_indexes_by_path_alone() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("general/memory");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("notes.md"), "Just a body.\n")
            .await
            .unwrap();
        let lines = list_entries(root.path(), &ScopeLocation::General, MemoryScope::General)
            .await
            .unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].reference.to_string(), "general/notes");
        assert_eq!(lines[0].summary, None);
        let entry = read_entry(root.path(), &ScopeLocation::General, &general("notes"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.body, "Just a body.\n");
    }

    #[tokio::test]
    async fn the_index_carries_the_profile_whole_and_every_visible_scope_one_line_each() {
        let root = tempfile::tempdir().unwrap();
        write_entry(
            root.path(),
            &ScopeLocation::General,
            &general(PROFILE_PATH),
            "Who the operator is.",
            "Ben runs a small workshop. Prefers British spelling.",
        )
        .await
        .unwrap();
        write_entry(
            root.path(),
            &ScopeLocation::General,
            &general("interests/cycling"),
            "Cycles most weekends.",
            "Rides a steel frame.",
        )
        .await
        .unwrap();
        write_entry(
            root.path(),
            &topic(),
            &MemoryReference {
                scope: MemoryScope::Topic,
                path: "conventions".into(),
            },
            "This topic uses metric units only.",
            "Metric only, stated 2026-10-04.",
        )
        .await
        .unwrap();

        // Inside the topic: profile, both general lines, the topic's line.
        let inside = load_memory_index(root.path(), &topic()).await;
        assert_eq!(
            inside.profile.as_deref(),
            Some("Ben runs a small workshop. Prefers British spelling.")
        );
        let refs: Vec<String> = inside
            .lines
            .iter()
            .map(|line| line.reference.to_string())
            .collect();
        assert_eq!(
            refs,
            vec![
                "general/interests/cycling",
                "general/profile",
                "topic/conventions"
            ]
        );
        assert_eq!(
            inside.lines[2].summary.as_deref(),
            Some("This topic uses metric units only.")
        );

        // Outside any topic: general memory alone, the topic's line absent.
        let outside = load_memory_index(root.path(), &ScopeLocation::General).await;
        assert!(outside.profile.is_some());
        assert!(
            outside
                .lines
                .iter()
                .all(|line| line.reference.scope == MemoryScope::General),
            "a session without a topic sees no topic memory"
        );

        // A session in another topic sees that topic's (empty) store.
        let elsewhere = load_memory_index(root.path(), &ScopeLocation::topic("ferns")).await;
        assert!(
            elsewhere
                .lines
                .iter()
                .all(|line| line.reference.scope == MemoryScope::General)
        );
    }

    #[tokio::test]
    async fn an_empty_store_is_an_empty_index_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        let index = load_memory_index(root.path(), &topic()).await;
        assert_eq!(index, MemoryIndex::default());
    }

    #[test]
    fn references_parse_as_the_index_spells_them_and_refuse_escapes() {
        let reference = MemoryReference::parse("topic/decisions/units").unwrap();
        assert_eq!(reference.scope, MemoryScope::Topic);
        assert_eq!(reference.path, "decisions/units");
        assert_eq!(reference.to_string(), "topic/decisions/units");

        for bad in [
            "profile",
            "shared/profile",
            "general/",
            "general//x",
            "general/../secrets",
            "general/a.b",
            "general/people/al ice",
        ] {
            assert!(
                MemoryReference::parse(bad).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn a_topic_reference_has_no_home_in_a_session_without_a_topic() {
        let reference = MemoryReference::parse("topic/conventions").unwrap();
        assert!(scope_location_for(&reference, &ScopeLocation::General).is_err());
        assert_eq!(scope_location_for(&reference, &topic()).unwrap(), topic());
        let general = MemoryReference::parse("general/profile").unwrap();
        assert_eq!(
            scope_location_for(&general, &topic()).unwrap(),
            ScopeLocation::General
        );
    }
}
