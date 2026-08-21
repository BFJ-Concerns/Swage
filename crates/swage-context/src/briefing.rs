//! Briefing element — one resolved operator-authored context file.

use std::collections::BTreeMap;

use swage_core::{ChatMessage, Role};

use crate::element::ContextElement;
use crate::named_element::{ResolvedNamedElement, has_valid_element_name_shape};
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
        if has_valid_element_name_shape(name) {
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
    pub fn new(resolved: ResolvedNamedElement) -> Self {
        Self {
            source: format!(
                "briefing '{}' ({}, {})",
                resolved.name,
                resolved.level,
                resolved.path.display()
            ),
            name: resolved.name,
            content: resolved.content,
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

    fn renders_session_values(&self) -> bool {
        true
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
    use crate::assembly::default_assembly_with_variables;
    use crate::js_assembly::assemble_named;
    use crate::tokeniser::TiktokenTokeniser;
    use swage_config::HierarchyLevel;

    #[test]
    fn identity_priority_source_and_live_substitution_are_visible() {
        let element = BriefingElement::new(ResolvedNamedElement {
            name: "current-work".into(),
            level: HierarchyLevel::Space,
            path: "elements/current-work.md".into(),
            content: "Task: {{task}}; owner: {{owner}}".into(),
        });
        assert_eq!(element.name(), "current-work");
        assert_eq!(element.priority(), 100);
        assert_eq!(
            element.source(),
            "briefing 'current-work' (Space, elements/current-work.md)"
        );
        assert_eq!(
            element.content()[0].text_content(),
            "Task: {{task}}; owner: {{owner}}"
        );
        assert_eq!(
            substitute_variables(
                &element.content()[0].text_content(),
                &BTreeMap::from([("task".to_owned(), "audit storage".to_owned())]),
            ),
            "Task: audit storage; owner: ⟦unset:owner⟧"
        );
        assert!(element.token_count(&TiktokenTokeniser::new("gpt-4o")) > 0);

        let output = default_assembly_with_variables(
            &[Box::new(element)],
            &swage_config::model::ContextConfig::default(),
            &TiktokenTokeniser::new("gpt-4o"),
            &BTreeMap::new(),
        );
        assert_eq!(
            output.report.elements[0].source,
            "briefing 'current-work' (Space, elements/current-work.md)"
        );
    }

    #[test]
    fn stored_slot_shaped_values_render_once_on_both_composition_paths() {
        let resolved = ResolvedNamedElement {
            name: "current-work".into(),
            level: HierarchyLevel::Space,
            path: "elements/current-work.md".into(),
            content: "Task: {{task}}".into(),
        };
        let variables = BTreeMap::from([
            ("task".to_owned(), "{{other}}".to_owned()),
            ("other".to_owned(), "must not expand".to_owned()),
        ]);
        let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(BriefingElement::new(resolved))];
        let tokeniser = TiktokenTokeniser::new("gpt-4o");

        let default = default_assembly_with_variables(
            &elements,
            &swage_config::model::ContextConfig::default(),
            &tokeniser,
            &variables,
        );
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("briefing.js");
        std::fs::write(&script, "add(elements[0].render());").unwrap();
        let scripted = assemble_named(
            "briefing",
            Some(&script),
            &elements,
            &swage_config::model::ContextConfig::default(),
            &tokeniser,
            &variables,
        );

        assert_eq!(default.messages[0].text_content(), "Task: {{other}}");
        assert_eq!(scripted.messages[0].text_content(), "Task: {{other}}");
    }
}
