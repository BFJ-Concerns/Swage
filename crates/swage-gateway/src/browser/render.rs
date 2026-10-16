use std::collections::{HashMap, HashSet};

use chromiumoxide::cdp::browser_protocol::accessibility::{
    AxNode, AxProperty, AxPropertyName, AxValue,
};
use chromiumoxide::cdp::browser_protocol::dom::BackendNodeId;

pub type RefTable = HashMap<String, RefEntry>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    pub reference: String,
    pub backend_node_id: BackendNodeId,
    pub selector: Option<String>,
    pub role: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct RenderedSnapshot {
    pub output: String,
    pub full_output: Option<String>,
    pub refs: RefTable,
}

#[derive(Debug, Clone)]
struct RenderLine {
    text: String,
    depth: usize,
    meta: Option<RenderLineMeta>,
}

#[derive(Debug, Clone)]
struct RenderLineMeta {
    subtree_end: usize,
    subtree_nodes: usize,
    subtree_weight: usize,
    has_interactive: bool,
    has_opening: bool,
}

pub fn render_snapshot(
    nodes: &[AxNode],
    url: &str,
    title: &str,
    snapshot_seq: u64,
    max_bytes: usize,
    masked_backend_nodes: &HashSet<BackendNodeId>,
) -> RenderedSnapshot {
    let header = snapshot_header(url, title, snapshot_seq);
    let mut renderer = Renderer::new(header.clone(), snapshot_seq, masked_backend_nodes);
    let roots = root_nodes(nodes);
    for node in &roots {
        renderer.render_node(nodes, node, 0, false, false);
    }
    if renderer.only_rendered_root_refs() && nodes.len() > roots.len() {
        renderer = Renderer::new(header, snapshot_seq, masked_backend_nodes);
        for node in nodes.iter().filter(|node| node.parent_id.is_some()) {
            renderer.render_node(nodes, node, 0, false, false);
        }
    }
    let full_output = join_render_lines(&renderer.lines);
    let output = truncate_snapshot(&renderer.lines, max_bytes);
    let full_spill = (output.len() < full_output.len()).then_some(full_output);
    RenderedSnapshot {
        output,
        full_output: full_spill,
        refs: renderer.refs,
    }
}

struct Renderer {
    refs: RefTable,
    next_ref: usize,
    snapshot_seq: u64,
    masked_backend_nodes: HashSet<BackendNodeId>,
    lines: Vec<RenderLine>,
    opening_started: bool,
}

impl Renderer {
    fn new(
        lines: Vec<RenderLine>,
        snapshot_seq: u64,
        masked_backend_nodes: &HashSet<BackendNodeId>,
    ) -> Self {
        Self {
            refs: HashMap::new(),
            next_ref: 1,
            snapshot_seq,
            masked_backend_nodes: masked_backend_nodes.clone(),
            lines,
            opening_started: false,
        }
    }

    fn only_rendered_root_refs(&self) -> bool {
        self.refs.is_empty()
            || self
                .refs
                .values()
                .all(|entry| entry.role.eq_ignore_ascii_case("RootWebArea"))
    }

    fn render_node(
        &mut self,
        nodes: &[AxNode],
        node: &AxNode,
        depth: usize,
        in_main: bool,
        in_opening: bool,
    ) {
        if should_drop(node) {
            return;
        }
        let role = ax_value_string(node.role.as_ref()).unwrap_or_else(|| "node".to_string());
        let in_main = in_main || matches!(role.as_str(), "main" | "article");
        let starts_opening = in_main && role == "paragraph" && !self.opening_started;
        self.opening_started |= starts_opening;
        let in_opening = in_opening || starts_opening;
        let name = ax_value_string(node.name.as_ref())
            .map(|name| name.trim().to_string())
            .unwrap_or_default();
        if role == "generic" && name.is_empty() && !has_interesting_state(node) {
            for child in child_nodes(nodes, node) {
                self.render_node(nodes, child, depth, in_main, in_opening);
            }
            return;
        }
        if role == "StaticText"
            && node
                .parent_id
                .as_ref()
                .and_then(|parent_id| {
                    nodes
                        .iter()
                        .find(|candidate| &candidate.node_id == parent_id)
                })
                .and_then(|parent| ax_value_string(parent.name.as_ref()))
                .is_some_and(|parent_name| parent_name == name)
        {
            return;
        }

        let mut line = format!("{}- {}", "  ".repeat(depth), role);
        if !name.is_empty() {
            line.push_str(&format!(" \"{}\"", escape_text(&name)));
        }
        let masked = self.is_masked(node);
        let state_text = state_text(node, masked);
        if !state_text.is_empty() {
            line.push(' ');
            line.push_str(&state_text);
        }
        let actionable = is_actionable(node, &role);
        if actionable && let Some(backend_node_id) = node.backend_dom_node_id {
            // The reference token carries the snapshot that issued it, so the
            // action path can reject a ref from a superseded snapshot instead
            // of resolving whatever the same position means now.
            let reference = element_reference(self.snapshot_seq, self.next_ref);
            self.next_ref += 1;
            line.push_str(&format!(" ref={reference}"));
            self.refs.insert(
                reference.clone(),
                RefEntry {
                    reference,
                    backend_node_id,
                    selector: None,
                    role: role.clone(),
                    name: name.clone(),
                },
            );
        }
        let line_index = self.lines.len();
        self.lines.push(RenderLine {
            text: line,
            depth,
            meta: None,
        });
        if masked {
            self.finish_render_line(line_index, actionable, in_opening);
            return;
        }
        for child in child_nodes(nodes, node) {
            self.render_node(nodes, child, depth + 1, in_main, in_opening);
        }
        self.finish_render_line(line_index, actionable, in_opening);
    }

    fn is_masked(&self, node: &AxNode) -> bool {
        node.backend_dom_node_id
            .is_some_and(|id| self.masked_backend_nodes.contains(&id))
    }

    fn finish_render_line(&mut self, line_index: usize, is_interactive: bool, in_opening: bool) {
        let subtree_end = self.lines.len();
        let mut subtree_nodes = 1;
        let mut subtree_weight = self.lines[line_index].text.len();
        let mut has_interactive = is_interactive;
        let mut has_opening = in_opening;
        for child in &self.lines[line_index + 1..subtree_end] {
            if child.meta.is_some() {
                subtree_nodes += 1;
                subtree_weight += child.text.len();
            }
            has_opening |= child.meta.as_ref().is_some_and(|meta| meta.has_opening);
            if child.meta.as_ref().is_some_and(|meta| meta.has_interactive) {
                has_interactive = true;
            }
        }
        self.lines[line_index].meta = Some(RenderLineMeta {
            subtree_end,
            subtree_nodes,
            subtree_weight,
            has_interactive,
            has_opening,
        });
    }
}

/// Format an element reference: snapshot identity, then index. The token is
/// the model-facing handle — parsing it back is `parse_reference_snapshot`.
pub fn element_reference(snapshot_seq: u64, index: usize) -> String {
    format!("s{snapshot_seq}e{index}")
}

/// The snapshot sequence a reference token was issued from, when the token
/// carries one. A malformed token reads as None — callers treat it as an
/// unknown reference, never as current.
pub fn parse_reference_snapshot(reference: &str) -> Option<u64> {
    let rest = reference.strip_prefix('s')?;
    let split = rest.find('e')?;
    let (seq, index) = rest.split_at(split);
    let digits = &index[1..];
    if seq.is_empty() || digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    seq.parse().ok()
}

fn snapshot_header(url: &str, title: &str, snapshot_seq: u64) -> Vec<RenderLine> {
    [
        format!("url: {url}"),
        format!("title: {title}"),
        format!("snapshot: {snapshot_seq}"),
        String::new(),
    ]
    .into_iter()
    .map(|text| RenderLine {
        text,
        depth: 0,
        meta: None,
    })
    .collect()
}

fn root_nodes(nodes: &[AxNode]) -> Vec<&AxNode> {
    nodes
        .iter()
        .filter(|node| node.parent_id.is_none())
        .collect()
}

fn child_nodes<'a>(nodes: &'a [AxNode], node: &AxNode) -> Vec<&'a AxNode> {
    if let Some(ids) = &node.child_ids {
        return ids
            .iter()
            .filter_map(|id| nodes.iter().find(|candidate| candidate.node_id == *id))
            .collect();
    }
    nodes
        .iter()
        .filter(|candidate| candidate.parent_id.as_ref() == Some(&node.node_id))
        .collect()
}

fn should_drop(node: &AxNode) -> bool {
    node.ignored || ax_value_string(node.role.as_ref()).is_some_and(|role| role == "InlineTextBox")
}

fn is_actionable(node: &AxNode, role: &str) -> bool {
    const ROLES: &[&str] = &[
        "button",
        "link",
        "textbox",
        "checkbox",
        "radio",
        "combobox",
        "option",
        "slider",
        "tab",
        "menuitem",
        "switch",
        "searchbox",
        "spinbutton",
    ];
    ROLES.contains(&role) || property_bool(node, "focusable").unwrap_or(false)
}

fn has_interesting_state(node: &AxNode) -> bool {
    [
        "checked",
        "disabled",
        "selected",
        "expanded",
        "required",
        "invalid",
        "focusable",
    ]
    .iter()
    .any(|name| property_value(node, name).is_some())
}

fn state_text(node: &AxNode, mask_value: bool) -> String {
    let mut states = Vec::new();
    for name in [
        "checked", "disabled", "selected", "expanded", "required", "invalid",
    ] {
        if let Some(value) = property_value(node, name) {
            match value.as_bool() {
                Some(true) => states.push(name.to_string()),
                Some(false) => {}
                None => states.push(format!("{name}={}", short_json(value))),
            }
        }
    }
    if mask_value {
        states.push("value=\"[masked]\"".to_string());
    } else if let Some(value) = ax_value_string(node.value.as_ref())
        && !value.is_empty()
    {
        states.push(format!("value=\"{}\"", escape_text(&value)));
    }
    if states.is_empty() {
        String::new()
    } else {
        format!("[{}]", states.join(", "))
    }
}

fn property_bool(node: &AxNode, name: &str) -> Option<bool> {
    property_value(node, name).and_then(serde_json::Value::as_bool)
}

fn property_value<'a>(node: &'a AxNode, name: &str) -> Option<&'a serde_json::Value> {
    node.properties
        .as_ref()?
        .iter()
        .find(|property| property_name(property) == name)
        .and_then(|property| property.value.value.as_ref())
}

fn property_name(property: &AxProperty) -> &'static str {
    match property.name {
        AxPropertyName::Checked => "checked",
        AxPropertyName::Disabled => "disabled",
        AxPropertyName::Selected => "selected",
        AxPropertyName::Expanded => "expanded",
        AxPropertyName::Required => "required",
        AxPropertyName::Invalid => "invalid",
        AxPropertyName::Focusable => "focusable",
        _ => "",
    }
}

fn ax_value_string(value: Option<&AxValue>) -> Option<String> {
    match value?.value.as_ref()? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        other => Some(short_json(other)),
    }
}

fn short_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => escape_text(text),
        _ => value.to_string(),
    }
}

fn escape_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn join_render_lines(lines: &[RenderLine]) -> String {
    lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_snapshot(lines: &[RenderLine], max_bytes: usize) -> String {
    let full = join_render_lines(lines);
    if full.len() <= max_bytes {
        return full;
    }
    let mut rendered = lines
        .iter()
        .map(|line| TruncLine {
            text: line.text.clone(),
            depth: line.depth,
            meta: line.meta.clone(),
            removed: false,
        })
        .collect::<Vec<_>>();
    let mut candidates = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let meta = line.meta.as_ref()?;
            (meta.subtree_nodes > 1 && !meta.has_opening).then_some(ElisionCandidate {
                index,
                depth: line.depth,
                subtree_weight: meta.subtree_weight,
                has_interactive: meta.has_interactive,
            })
        })
        .collect::<Vec<_>>();

    // Keep the opening paragraph of main content and its ancestors. Prefer
    // large leaf-ish prose subtrees elsewhere. Interactive subtrees are a fallback
    // only for pathological pages where text-only elision cannot reach budget.
    candidates.sort_by(|left, right| {
        left.has_interactive
            .cmp(&right.has_interactive)
            .then_with(|| right.depth.cmp(&left.depth))
            .then_with(|| right.subtree_weight.cmp(&left.subtree_weight))
    });

    for candidate in candidates {
        if current_render_len(&rendered) <= max_bytes {
            break;
        }
        if rendered[candidate.index].removed {
            continue;
        }
        let Some(meta) = rendered[candidate.index].meta.clone() else {
            continue;
        };
        if rendered[candidate.index + 1..meta.subtree_end]
            .iter()
            .any(|line| line.removed)
        {
            continue;
        }
        for line in &mut rendered[candidate.index + 1..meta.subtree_end] {
            line.removed = true;
        }
        rendered[candidate.index].text = format!(
            "{}... ({} nodes elided - full snapshot saved)",
            "  ".repeat(rendered[candidate.index].depth),
            meta.subtree_nodes.saturating_sub(1)
        );
    }

    let output = join_truncated_lines(&rendered);
    if output.len() <= max_bytes {
        return output;
    }
    tail_truncate(&output, max_bytes)
}

#[derive(Debug, Clone)]
struct TruncLine {
    text: String,
    depth: usize,
    meta: Option<RenderLineMeta>,
    removed: bool,
}

#[derive(Debug, Clone)]
struct ElisionCandidate {
    index: usize,
    depth: usize,
    subtree_weight: usize,
    has_interactive: bool,
}

fn current_render_len(lines: &[TruncLine]) -> usize {
    join_truncated_lines(lines).len()
}

fn join_truncated_lines(lines: &[TruncLine]) -> String {
    lines
        .iter()
        .filter(|line| !line.removed)
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The one over-budget truncation contract both snapshot paths share — the
/// accessibility renderer's final stage and the DOM fallback alike: reserve
/// the marker, cut at a UTF-8 boundary, and say plainly that the full
/// snapshot was saved. Keeping a second inline truncation beside this one is
/// how the two contracts drift apart.
pub(crate) fn tail_truncate(full: &str, max_bytes: usize) -> String {
    let marker = "\n... (snapshot truncated - full snapshot saved)";
    let budget = max_bytes.saturating_sub(marker.len());
    let mut end = budget.min(full.len());
    while !full.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &full[..end], marker)
}

#[cfg(test)]
mod tests {
    use chromiumoxide::cdp::browser_protocol::accessibility::{
        AxNode, AxProperty, AxPropertyName, AxValue, AxValueType,
    };
    use chromiumoxide::cdp::browser_protocol::dom::BackendNodeId;

    use super::*;

    #[test]
    fn renders_refs_for_actionable_nodes_only() {
        let nodes = vec![
            node("1", None, "RootWebArea", "Example", None, &["2", "3", "4"]),
            node("2", Some("1"), "heading", "Sign in", None, &[]),
            node("3", Some("1"), "textbox", "Username", Some(10), &[]),
            node("4", Some("1"), "button", "Submit", Some(11), &[]),
        ];
        let rendered = render_snapshot(
            &nodes,
            "http://fixture/",
            "Fixture",
            1,
            10_000,
            &HashSet::new(),
        );
        assert!(rendered.output.contains("- textbox \"Username\" ref=s1e1"));
        assert!(rendered.output.contains("- button \"Submit\" ref=s1e2"));
        assert_eq!(rendered.refs.len(), 2);
        // The token round-trips its issuing snapshot.
        assert_eq!(parse_reference_snapshot("s1e1"), Some(1));
    }

    #[test]
    fn over_budget_snapshot_preserves_late_interactive_refs() {
        let mut nodes = vec![node(
            "1",
            None,
            "RootWebArea",
            "Example",
            None,
            &["2", "5", "6"],
        )];
        nodes.extend([
            node("2", Some("1"), "section", "Long article", None, &["3"]),
            node("3", Some("2"), "paragraph", "Intro", None, &["4"]),
            node(
                "4",
                Some("3"),
                "StaticText",
                &"Long non-interactive copy. ".repeat(80),
                None,
                &[],
            ),
            node("5", Some("1"), "link", "Useful late link", Some(50), &[]),
            node("6", Some("1"), "button", "Final action", Some(51), &[]),
        ]);

        let rendered = render_snapshot(
            &nodes,
            "http://fixture/",
            "Fixture",
            1,
            360,
            &HashSet::new(),
        );

        assert!(
            rendered.output.contains("Useful late link"),
            "late interactive link vanished from truncated snapshot:\n{}",
            rendered.output
        );
        assert!(
            rendered.output.contains("Final action"),
            "late interactive button vanished from truncated snapshot:\n{}",
            rendered.output
        );
        assert!(
            rendered.output.contains("ref=s1e1") && rendered.output.contains("ref=s1e2"),
            "interactive refs should remain visible after truncation:\n{}",
            rendered.output
        );
        assert!(
            rendered.output.contains("nodes elided"),
            "over-budget snapshot should mark elided subtrees in place:\n{}",
            rendered.output
        );
        assert!(
            rendered.full_output.is_some(),
            "full snapshot should still spill"
        );
    }

    #[test]
    fn over_budget_article_keeps_its_opening() {
        let nodes: Vec<AxNode> =
            serde_json::from_str(include_str!("../../tests/fixtures/browser/ax/article.json"))
                .expect("article AX fixture");
        let rendered = render_snapshot(
            &nodes,
            "https://fixture.local/article",
            "Heron",
            1,
            650,
            &HashSet::new(),
        );
        assert!(rendered.output.contains("Herons are long-legged, long-necked freshwater and coastal birds in the family Ardeidae."), "article opening was elided:\n{}", rendered.output);
        assert_eq!(
            rendered.output.trim_end(),
            include_str!("../../tests/fixtures/browser/ax/article-over-budget.rendered").trim_end()
        );
        assert!(rendered.output.len() <= 650);
        assert!(
            rendered
                .full_output
                .as_ref()
                .expect("full snapshot spill")
                .contains("Detailed discussion")
        );
        assert!(rendered.output.contains("ref=s1e3"), "article link lost");
    }

    #[test]
    fn shared_tail_truncation_reserves_its_marker_at_a_char_boundary() {
        // Both snapshot paths — the accessibility renderer's final stage and
        // the DOM fallback — cut through this one contract.
        let full = "héllo wörld ".repeat(100);
        let cut = tail_truncate(&full, 120);
        assert!(cut.len() <= 120);
        assert!(cut.ends_with("... (snapshot truncated - full snapshot saved)"));
        assert!(full.starts_with(cut.split('\n').next().unwrap()));

        let short = tail_truncate("tiny", 120);
        assert!(short.ends_with("... (snapshot truncated - full snapshot saved)"));
    }

    #[test]
    fn reference_tokens_carry_and_yield_their_issuing_snapshot() {
        assert_eq!(element_reference(7, 3), "s7e3");
        assert_eq!(parse_reference_snapshot("s7e3"), Some(7));
        // Malformed tokens read as unknown, never as current.
        assert_eq!(parse_reference_snapshot("e3"), None);
        assert_eq!(parse_reference_snapshot("s7e"), None);
        assert_eq!(parse_reference_snapshot("se3"), None);
        assert_eq!(parse_reference_snapshot("s7x3"), None);
    }

    fn node(
        id: &str,
        parent: Option<&str>,
        role: &str,
        name: &str,
        backend: Option<i64>,
        children: &[&str],
    ) -> AxNode {
        let mut node = AxNode::new(id.to_string(), false);
        node.parent_id = parent.map(|id| id.to_string().into());
        node.role = Some(value(role));
        node.name = Some(value(name));
        node.backend_dom_node_id = backend.map(BackendNodeId::new);
        node.properties = Some(vec![AxProperty::new(
            AxPropertyName::Focusable,
            AxValue {
                r#type: AxValueType::Boolean,
                value: Some(serde_json::json!(backend.is_some())),
                related_nodes: None,
                sources: None,
            },
        )]);
        if !children.is_empty() {
            node.child_ids = Some(children.iter().map(|id| (*id).to_string().into()).collect());
        }
        node
    }

    fn value(text: &str) -> AxValue {
        AxValue {
            r#type: AxValueType::String,
            value: Some(serde_json::json!(text)),
            related_nodes: None,
            sources: None,
        }
    }
}
