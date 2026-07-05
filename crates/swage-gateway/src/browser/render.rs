use std::collections::HashMap;

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

pub fn render_snapshot(
    nodes: &[AxNode],
    url: &str,
    title: &str,
    snapshot_seq: u64,
    max_bytes: usize,
) -> RenderedSnapshot {
    let mut renderer = Renderer {
        refs: HashMap::new(),
        next_ref: 1,
        lines: vec![
            format!("url: {url}"),
            format!("title: {title}"),
            format!("snapshot: {snapshot_seq}"),
            String::new(),
        ],
    };
    for node in root_nodes(nodes) {
        renderer.render_node(nodes, node, 0);
    }
    let full_output = renderer.lines.join("\n");
    let output = truncate_snapshot(&full_output, max_bytes);
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
    lines: Vec<String>,
}

impl Renderer {
    fn render_node(&mut self, nodes: &[AxNode], node: &AxNode, depth: usize) {
        if should_drop(node) {
            return;
        }
        let role = ax_value_string(node.role.as_ref()).unwrap_or_else(|| "node".to_string());
        let name = ax_value_string(node.name.as_ref()).unwrap_or_default();
        if role == "generic" && name.is_empty() && !has_interesting_state(node) {
            for child in child_nodes(nodes, node) {
                self.render_node(nodes, child, depth);
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
        let state_text = state_text(node);
        if !state_text.is_empty() {
            line.push(' ');
            line.push_str(&state_text);
        }
        if is_actionable(node, &role)
            && let Some(backend_node_id) = node.backend_dom_node_id
        {
            let reference = format!("e{}", self.next_ref);
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
        self.lines.push(line);
        for child in child_nodes(nodes, node) {
            self.render_node(nodes, child, depth + 1);
        }
    }
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

fn state_text(node: &AxNode) -> String {
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
    if let Some(value) = ax_value_string(node.value.as_ref())
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

fn truncate_snapshot(full: &str, max_bytes: usize) -> String {
    if full.len() <= max_bytes {
        return full.to_string();
    }
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
            node("1", None, "RootWebArea", "Example", None),
            node("2", Some("1"), "heading", "Sign in", None),
            node("3", Some("1"), "textbox", "Username", Some(10)),
            node("4", Some("1"), "button", "Submit", Some(11)),
        ];
        let rendered = render_snapshot(&nodes, "http://fixture/", "Fixture", 1, 10_000);
        assert!(rendered.output.contains("- textbox \"Username\" ref=e1"));
        assert!(rendered.output.contains("- button \"Submit\" ref=e2"));
        assert_eq!(rendered.refs.len(), 2);
    }

    fn node(
        id: &str,
        parent: Option<&str>,
        role: &str,
        name: &str,
        backend: Option<i64>,
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
        if let Some(parent_id) = parent {
            // The generated type stores child ids on the parent. Tests keep the
            // fixture tiny enough that patching the parent after construction is clearer.
            let _ = parent_id;
        }
        node.child_ids = match id {
            "1" => Some(vec![
                "2".to_string().into(),
                "3".to_string().into(),
                "4".to_string().into(),
            ]),
            _ => None,
        };
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
