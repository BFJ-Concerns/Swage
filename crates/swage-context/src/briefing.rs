//! Briefing element — one resolved operator-authored context file.

use std::collections::BTreeMap;

use swage_core::{ChatMessage, Role};

use crate::element::ContextElement;
use crate::named_element::{ResolvedNamedElement, validate_element_name};
use crate::tokeniser::Tokeniser;

/// Apply the session-value substitution contract to Markdown content.
pub fn substitute_variables(template: &str, variables: &BTreeMap<String, String>) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let candidate = &remaining[start + 2..];
        let Some(end) = candidate.find("}}") else {
            rendered.push_str(&remaining[start..]);
            return rendered;
        };
        let name = &candidate[..end];
        if validate_element_name(name).is_ok() {
            if let Some(value) = variables.get(name) {
                rendered.push_str(value);
            } else {
                rendered.push_str(&format!("⟦unset:{name}⟧"));
            }
        } else {
            rendered.push_str("{{");
            rendered.push_str(name);
            rendered.push_str("}}");
        }
        remaining = &candidate[end + 2..];
    }
    rendered.push_str(remaining);
    rendered
}

/// One resolved briefing at ordinary operator-prompt standing.
pub struct BriefingElement {
    name: String,
    source: String,
    content: String,
}

impl BriefingElement {
    pub fn new(resolved: ResolvedNamedElement, variables: &BTreeMap<String, String>) -> Self {
        Self {
            source: format!(
                "briefing '{}' ({}, {})",
                resolved.name,
                resolved.level,
                resolved.path.display()
            ),
            name: resolved.name,
            content: substitute_variables(&resolved.content, variables),
        }
    }
}

impl ContextElement for BriefingElement {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> i32 {
        100
    }

    fn content(&self) -> Vec<ChatMessage> {
        vec![ChatMessage::new(Role::System, &self.content)]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        tokeniser.count_messages(&self.content())
    }

    fn source(&self) -> String {
        self.source.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokeniser::TiktokenTokeniser;
    use swage_config::HierarchyLevel;

    #[test]
    fn identity_priority_source_and_live_substitution_are_visible() {
        let element = BriefingElement::new(
            ResolvedNamedElement {
                name: "current-work".into(),
                level: HierarchyLevel::Space,
                path: "elements/current-work.md".into(),
                content: "Task: {{task}}; owner: {{owner}}".into(),
            },
            &BTreeMap::from([("task".to_owned(), "audit storage".to_owned())]),
        );
        assert_eq!(element.name(), "current-work");
        assert_eq!(element.priority(), 100);
        assert!(element.source().contains("briefing 'current-work' (Space"));
        assert_eq!(
            element.content()[0].text_content(),
            "Task: audit storage; owner: ⟦unset:owner⟧"
        );
        assert!(element.token_count(&TiktokenTokeniser::new("gpt-4o")) > 0);
    }
}
