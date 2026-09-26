//! The platform's own documentation, compiled into the binary and served
//! read-only.
//!
//! The agent-facing documentation set under `agent-docs/` is the terse,
//! canonical rendering of how Swage behaves. Embedding it makes the gateway
//! the place any client or agent reads it from — a session with no
//! repository attached included — and the byte-identity test below keeps
//! the embedded set exactly the repository's, so the hosted docs never lag
//! the tree the binary was built from. Documents are addressed by the file
//! stem of their source (`agents-and-permissions` for
//! `agent-docs/agents-and-permissions.md`).

use axum::extract::State;

use crate::error::{ApiError, Json, Path};
use crate::state::AppState;

pub use swage_protocol::client::docs::{DocumentListResponse, DocumentResponse, DocumentSummary};

/// One embedded document: its address and the file's bytes as committed.
pub(crate) struct ShippedDocument {
    pub(crate) name: &'static str,
    pub(crate) contents: &'static str,
}

macro_rules! shipped_document {
    ($name:literal) => {
        ShippedDocument {
            name: $name,
            contents: include_str!(concat!("../../../agent-docs/", $name, ".md")),
        }
    };
}

/// Every hosted document, in the order the listing presents them. A new
/// document under `agent-docs/` is one entry here; the byte-identity test
/// fails until it is added.
pub(crate) const SHIPPED_DOCUMENTS: &[ShippedDocument] = &[
    shipped_document!("first-session"),
    shipped_document!("agents-and-permissions"),
    shipped_document!("interfaces-and-tooling"),
    shipped_document!("workflow-engine"),
];

pub(crate) fn find_by_name(name: &str) -> Option<&'static ShippedDocument> {
    SHIPPED_DOCUMENTS
        .iter()
        .find(|document| document.name == name)
}

/// The names a reader may ask for, in listing order.
pub(crate) fn document_names() -> impl Iterator<Item = &'static str> {
    SHIPPED_DOCUMENTS.iter().map(|document| document.name)
}

/// A document's listing entry, read from the document itself: the
/// `description` line of its front matter and its first heading. Neither is
/// stored beside the file, so the listing cannot drift from the content.
pub(crate) fn summary(document: &ShippedDocument) -> DocumentSummary {
    let (front_matter, body) = split_front_matter(document.contents);
    let description = front_matter_description(front_matter);
    let title = body
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .unwrap_or(document.name)
        .to_owned();
    DocumentSummary {
        name: document.name.to_owned(),
        title,
        description,
    }
}

pub(crate) fn response(document: &ShippedDocument) -> DocumentResponse {
    let DocumentSummary {
        name,
        title,
        description,
    } = summary(document);
    DocumentResponse {
        name,
        title,
        description,
        content: document.contents.to_owned(),
    }
}

pub(crate) fn list() -> DocumentListResponse {
    DocumentListResponse {
        documents: SHIPPED_DOCUMENTS.iter().map(summary).collect(),
    }
}

/// The `description` value of a front matter block: a plain one-line
/// scalar, or a folded (`>`) or literal (`|`) block whose text follows on
/// indented lines, joined on one line.
fn front_matter_description(front_matter: &str) -> String {
    let mut lines = front_matter.lines();
    let Some(first) = lines.find_map(|line| line.strip_prefix("description:")) else {
        return String::new();
    };
    let first = first.trim();
    if !matches!(first, ">" | "|" | ">-" | "|-") {
        return first.to_owned();
    }
    lines
        .take_while(|line| line.starts_with(' ') || line.starts_with('\t'))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split a `---`-fenced YAML front matter block from the body. A document
/// without one yields an empty front matter and the whole text as body.
fn split_front_matter(contents: &str) -> (&str, &str) {
    let Some(after_open) = contents.strip_prefix("---\n") else {
        return ("", contents);
    };
    match after_open.find("\n---\n") {
        Some(end) => (&after_open[..end], &after_open[end + "\n---\n".len()..]),
        None => ("", contents),
    }
}

/// `GET /api/docs` — the hosted documents, without bodies.
pub async fn list_documents(State(_state): State<AppState>) -> Json<DocumentListResponse> {
    Json(list())
}

/// `GET /api/docs/{name}` — one hosted document with its body.
pub async fn get_document(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<DocumentResponse>, ApiError> {
    find_by_name(&name)
        .map(response)
        .map(Json)
        .ok_or_else(|| ApiError::not_found_str("document", &name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, path::Path};

    fn repository_documents() -> BTreeMap<String, Vec<u8>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agent-docs");
        let mut documents = BTreeMap::new();
        for entry in fs::read_dir(&root).expect("read agent-docs directory") {
            let path = entry.expect("read agent-docs entry").path();
            assert!(
                path.is_file(),
                "agent-docs holds documents only; {} is not a file",
                path.display()
            );
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("UTF-8 document name")
                .to_owned();
            assert_eq!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("md"),
                "agent-docs holds Markdown only"
            );
            documents.insert(name, fs::read(&path).expect("read document"));
        }
        documents
    }

    #[test]
    fn catalogue_embeds_every_document_byte_for_byte() {
        let repository = repository_documents();
        let mut embedded = BTreeMap::new();
        for document in SHIPPED_DOCUMENTS {
            let previous = embedded.insert(document.name.to_owned(), document.contents.as_bytes());
            assert!(previous.is_none(), "duplicate embedded document name");
        }

        assert_eq!(
            embedded.keys().collect::<Vec<_>>(),
            repository.keys().collect::<Vec<_>>(),
            "every agent-docs file is one SHIPPED_DOCUMENTS entry"
        );
        for (name, repository_bytes) in &repository {
            assert_eq!(
                embedded.get(name).expect("repository document embedded"),
                &repository_bytes.as_slice(),
                "embedded bytes differ for {name}"
            );
        }
    }

    #[test]
    fn a_summary_reads_its_title_and_description_from_the_document() {
        let document = ShippedDocument {
            name: "example",
            contents: "---\ndescription: What the example covers\n---\n\n# The example\n\nBody.\n",
        };
        assert_eq!(
            summary(&document),
            DocumentSummary {
                name: "example".into(),
                title: "The example".into(),
                description: "What the example covers".into(),
            }
        );
    }

    #[test]
    fn a_folded_or_literal_description_block_lists_on_one_line() {
        let folded = ShippedDocument {
            name: "folded",
            contents: "---\ndescription: >\n  A long description\n  across two lines\nunpaired: true\n---\n\n# Folded\n",
        };
        assert_eq!(
            summary(&folded).description,
            "A long description across two lines"
        );
        let literal = ShippedDocument {
            name: "literal",
            contents: "---\ndescription: |-\n  One\n  Two\n---\n# Literal\n",
        };
        assert_eq!(summary(&literal).description, "One Two");
    }

    #[test]
    fn a_document_without_front_matter_lists_under_its_name() {
        let document = ShippedDocument {
            name: "bare",
            contents: "No heading, no front matter.\n",
        };
        assert_eq!(
            summary(&document),
            DocumentSummary {
                name: "bare".into(),
                title: "bare".into(),
                description: String::new(),
            }
        );
    }

    #[test]
    fn every_shipped_document_carries_a_title_and_description() {
        for entry in list().documents {
            assert!(!entry.title.is_empty(), "{} has no title", entry.name);
            assert!(
                !entry.description.is_empty(),
                "{} has no description",
                entry.name
            );
            assert_ne!(entry.title, entry.name, "{} has no heading", entry.name);
        }
    }

    #[test]
    fn an_unknown_name_finds_nothing() {
        assert!(find_by_name("not-a-document").is_none());
        assert!(find_by_name("../Cargo.toml").is_none());
    }
}
