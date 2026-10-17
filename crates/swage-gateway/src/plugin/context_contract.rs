//! Run the existing assembly contract assertions through the shipped plugin.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde_json::Value;
use swage_config::model::ContextConfig;
use swage_context::{element::ContextElement, report::AssemblyOutput, tokeniser::Tokeniser};
use swage_core::ChatMessage;
use swage_plugin::{HostCallFailure, PluginHost, PluginManifest, PluginRuntime};

struct Host;

#[async_trait]
impl PluginHost for Host {
    async fn call_tool(&self, _: &str, _: Value) -> Result<Value, HostCallFailure> {
        unreachable!("context assembly calls no tools")
    }
    fn log(&self, _: &str) {}
}

pub(crate) fn default_assembly(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
) -> AssemblyOutput {
    default_assembly_with_variables(elements, config, tokeniser, &BTreeMap::new())
}

fn default_assembly_with_variables(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    variables: &BTreeMap<String, String>,
) -> AssemblyOutput {
    assemble(elements, config, tokeniser, variables, "default", || {
        panic!("the shipped default must serve without running the kernel")
    })
}

pub(crate) fn assemble(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    variables: &BTreeMap<String, String>,
    name: &str,
    named: impl FnOnce() -> AssemblyOutput,
) -> AssemblyOutput {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let runtime = PluginRuntime::start(
                PluginManifest::parse(include_str!(
                    "../../../../shipped/plugins/context-program/plugin.toml"
                ))
                .unwrap(),
                include_str!("../../../../shipped/plugins/context-program/plugin.js").to_owned(),
            )
            .await
            .unwrap();
            let slot = super::slot::PluginSlot::new(
                "context-program".into(),
                runtime,
                Arc::new(Host),
                swage_plugin::InvocationContext {
                    session_id: "contract".into(),
                    agent_id: "contract".into(),
                    call_id: None,
                    working_directory: "/".into(),
                },
            );
            slot.context_program(elements, variables, tokeniser, name, config, named)
                .expect("the shipped context program serves")
        })
}

#[path = "../../../swage-context/src/assembly/contract.rs"]
mod tests;
