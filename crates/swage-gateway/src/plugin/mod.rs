//! Installed plugins as the gateway runs them.
//!
//! The kernel (`swage-plugin`) reads a package and runs its script; this
//! module is everything around that which belongs to the gateway: the
//! install root under the data root, the revision and enablement records,
//! the audit events, the isolate kept live per plugin, the tools a plugin
//! contributes to the dispatcher, and the per-scope view that withholds a
//! plugin's tools from a topic that does not enable it.
//!
//! A plugin's tools are ordinary tools: they register in the same registry
//! as the built-ins, carry the same grants, approvals and record, and are
//! withheld before permissions are consulted where the scope does not
//! enable the plugin — as a platform agent's held tool is withheld from
//! other agents.

pub mod tool;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use swage_core::StorageError;
use swage_plugin::{
    HostGrant, PluginManifest, PluginRuntime, PluginRuntimeState, install_dir, install_package,
    read_package,
};
use swage_protocol::client::plugin::{
    PluginListResponse, PluginProvides, PluginRevision, PluginSecret, PluginServiceState,
    PluginSummary, ProvidedTool,
};
use swage_storage::model::{NewAuditEvent, NewPluginRevision, PluginRevisionRecord};
use swage_storage::store::Storage;
use uuid::Uuid;

use crate::state::AppState;
use crate::tool::Tool;

/// The directory under the data root that holds installed packages, one
/// subdirectory per plugin and one per revision beneath it.
pub const PLUGINS_DIR: &str = "plugins";

#[derive(Debug, thiserror::Error)]
pub enum PluginServiceError {
    /// The package or the request does not conform; nothing was changed.
    #[error("{0}")]
    Refused(String),
    #[error("no plugin named '{0}' is installed")]
    NotFound(String),
    #[error("{0}")]
    Storage(#[from] StorageError),
    /// The change was applied but the record of it could not be written.
    #[error("{0}")]
    Unrecorded(String),
}

/// A plugin as the gateway holds it: its active revision, the manifest that
/// revision declares, its isolate (or why it could not be loaded), every
/// revision recorded, and where it is enabled.
struct InstalledPlugin {
    active: PluginRevisionRecord,
    manifest: PluginManifest,
    runtime: Result<Arc<PluginRuntime>, String>,
    revisions: Vec<PluginRevisionRecord>,
    enabled_for_install: bool,
    enabled_topics: HashSet<Uuid>,
    disabled_topics: HashSet<Uuid>,
}

impl InstalledPlugin {
    /// Whether a session in `topic_id` (none: the general scope) may see
    /// the plugin's tools: the topic's own answer where it gave one, the
    /// install's otherwise.
    fn enabled_for(&self, topic_id: Option<Uuid>) -> bool {
        match topic_id {
            Some(topic) if self.enabled_topics.contains(&topic) => true,
            Some(topic) if self.disabled_topics.contains(&topic) => false,
            _ => self.enabled_for_install,
        }
    }

    fn summary(&self) -> PluginSummary {
        let mut enabled_topics: Vec<Uuid> = self.enabled_topics.iter().copied().collect();
        enabled_topics.sort();
        let mut disabled_topics: Vec<Uuid> = self.disabled_topics.iter().copied().collect();
        disabled_topics.sort();
        PluginSummary {
            name: self.manifest.name.clone(),
            description: self.manifest.description.clone(),
            version: self.manifest.version.clone(),
            revision: self.active.revision,
            provides: PluginProvides {
                tools: self
                    .manifest
                    .provides
                    .tools
                    .iter()
                    .map(|tool| ProvidedTool {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                    })
                    .collect(),
            },
            grants: self
                .manifest
                .grants
                .iter()
                .map(|grant| grant.as_str().to_owned())
                .collect(),
            secrets: self
                .manifest
                .secrets
                .iter()
                .map(|secret| PluginSecret {
                    name: secret.name.clone(),
                    description: secret.description.clone(),
                })
                .collect(),
            variant_of: self.manifest.variant_of.clone(),
            revisions: self
                .revisions
                .iter()
                .map(|revision| PluginRevision {
                    revision: revision.revision,
                    version: revision.version.clone(),
                    installed_from: revision.installed_from.clone(),
                    installed_at: revision.installed_at.to_rfc3339(),
                    active: revision.active,
                })
                .collect(),
            enabled_for_install: self.enabled_for_install,
            enabled_topics,
            disabled_topics,
            service: match &self.runtime {
                Ok(runtime) => match runtime.state() {
                    PluginRuntimeState::Serving => PluginServiceState::Serving,
                    PluginRuntimeState::Terminated(termination) => PluginServiceState::Terminated {
                        reason: termination.reason().to_owned(),
                    },
                },
                Err(reason) => PluginServiceState::Failed {
                    reason: reason.clone(),
                },
            },
        }
    }
}

/// The installed plugins, live in the gateway process.
pub struct PluginService {
    plugins_root: PathBuf,
    storage: Arc<dyn Storage>,
    installed: RwLock<BTreeMap<String, InstalledPlugin>>,
    /// Installs run one at a time: the revision number, the name checks,
    /// the copy, the load and the record are one decision, and two installs
    /// deciding together would each claim the other's revision.
    install_lock: tokio::sync::Mutex<()>,
}

impl PluginService {
    /// An empty service over `plugins_root`; [`Self::load`] brings the
    /// recorded installs up.
    pub fn new(storage: Arc<dyn Storage>, plugins_root: PathBuf) -> Self {
        Self {
            plugins_root,
            storage,
            installed: RwLock::new(BTreeMap::new()),
            install_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn plugins_root(&self) -> &Path {
        &self.plugins_root
    }

    /// Bring every recorded plugin up from its active revision's install
    /// directory. A plugin whose package no longer reads or whose script no
    /// longer loads stays listed as failed, with the reason, and serves no
    /// tool; the others are unaffected.
    pub async fn load(&self) -> Result<(), StorageError> {
        let revisions = self.storage.list_plugin_revisions().await?;
        let enablement = self.storage.list_plugin_enablement().await?;
        let mut installed = BTreeMap::new();
        let mut by_name: BTreeMap<String, Vec<PluginRevisionRecord>> = BTreeMap::new();
        for revision in revisions {
            by_name
                .entry(revision.plugin_name.clone())
                .or_default()
                .push(revision);
        }
        for (name, revisions) in by_name {
            let Some(active) = revisions.iter().find(|revision| revision.active).cloned() else {
                tracing::warn!(plugin = %name, "plugin has revisions recorded but none active; it serves nothing");
                continue;
            };
            let (manifest, runtime) = match read_package(Path::new(&active.install_dir)) {
                Ok(package) => {
                    let manifest = package.manifest.clone();
                    let runtime = PluginRuntime::start(package.manifest, package.script)
                        .await
                        .map_err(|error| error.to_string());
                    (manifest, runtime)
                }
                Err(error) => {
                    let manifest = match serde_json::from_value::<PluginManifest>(
                        active.manifest.clone(),
                    ) {
                        Ok(manifest) => manifest,
                        Err(error) => {
                            tracing::warn!(plugin = %name, %error, "plugin's recorded manifest does not read; it serves nothing");
                            continue;
                        }
                    };
                    (manifest, Err(error.to_string()))
                }
            };
            if let Err(reason) = &runtime {
                tracing::warn!(plugin = %name, reason, "plugin failed to load; it serves nothing until a revision that loads is installed");
            }
            let mut plugin = InstalledPlugin {
                active,
                manifest,
                runtime,
                revisions,
                enabled_for_install: false,
                enabled_topics: HashSet::new(),
                disabled_topics: HashSet::new(),
            };
            for row in enablement.iter().filter(|row| row.plugin_name == name) {
                match (row.topic_id, row.enabled) {
                    (None, enabled) => plugin.enabled_for_install = enabled,
                    (Some(topic), true) => {
                        plugin.enabled_topics.insert(topic);
                    }
                    (Some(topic), false) => {
                        plugin.disabled_topics.insert(topic);
                    }
                }
            }
            installed.insert(name, plugin);
        }
        *self.write() = installed;
        Ok(())
    }

    /// Install the package at `source` as a new revision of its plugin: copy
    /// it under the install root, load its isolate, record the revision and
    /// make it the one serving. `reserved_tool_names` are the names the
    /// dispatcher already holds for built-ins; a package whose tools collide
    /// with those, or with another plugin's, is refused before anything is
    /// copied.
    pub async fn install(
        &self,
        source: &Path,
        reserved_tool_names: &[String],
        session_id: Option<Uuid>,
    ) -> Result<PluginSummary, PluginServiceError> {
        let _installing = self.install_lock.lock().await;
        let package =
            read_package(source).map_err(|error| PluginServiceError::Refused(error.to_string()))?;
        let name = package.manifest.name.clone();
        let next_revision = {
            let installed = self.read();
            for tool in &package.manifest.provides.tools {
                if reserved_tool_names.contains(&tool.name) {
                    return Err(PluginServiceError::Refused(format!(
                        "tool '{}' is the name of a built-in tool; a plugin cannot provide it",
                        tool.name
                    )));
                }
                if let Some((other, _)) = installed.iter().find(|(other, plugin)| {
                    **other != name
                        && plugin
                            .manifest
                            .provides
                            .tools
                            .iter()
                            .any(|provided| provided.name == tool.name)
                }) {
                    return Err(PluginServiceError::Refused(format!(
                        "tool '{}' is already provided by plugin '{other}'",
                        tool.name
                    )));
                }
            }
            installed
                .get(&name)
                .and_then(|plugin| {
                    plugin
                        .revisions
                        .iter()
                        .map(|revision| revision.revision)
                        .max()
                })
                .map_or(1, |last| last + 1)
        };
        let installed_package = install_package(&package, &self.plugins_root, next_revision)
            .map_err(|error| PluginServiceError::Refused(error.to_string()))?;
        let target = install_dir(&self.plugins_root, &name, next_revision);
        let runtime = match PluginRuntime::start(
            installed_package.manifest.clone(),
            installed_package.script.clone(),
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&target);
                return Err(PluginServiceError::Refused(format!(
                    "the plugin's script failed to load: {error}"
                )));
            }
        };
        let manifest_json = serde_json::to_value(&installed_package.manifest)
            .map_err(|error| PluginServiceError::Refused(error.to_string()))?;
        let record = match self
            .storage
            .record_plugin_revision(NewPluginRevision {
                plugin_name: name.clone(),
                revision: next_revision,
                version: installed_package.manifest.version.clone(),
                description: installed_package.manifest.description.clone(),
                manifest: manifest_json.clone(),
                installed_from: source.display().to_string(),
                install_dir: target.display().to_string(),
            })
            .await
        {
            Ok(record) => record,
            Err(error) => {
                drop(runtime);
                let _ = std::fs::remove_dir_all(&target);
                return Err(PluginServiceError::Storage(error));
            }
        };
        let summary = {
            let mut installed = self.write();
            let plugin = installed
                .entry(name.clone())
                .or_insert_with(|| InstalledPlugin {
                    active: record.clone(),
                    manifest: installed_package.manifest.clone(),
                    runtime: Err(String::new()),
                    revisions: Vec::new(),
                    enabled_for_install: false,
                    enabled_topics: HashSet::new(),
                    disabled_topics: HashSet::new(),
                });
            for earlier in &mut plugin.revisions {
                earlier.active = false;
            }
            plugin.revisions.push(record.clone());
            plugin.active = record.clone();
            plugin.manifest = installed_package.manifest.clone();
            plugin.runtime = Ok(runtime);
            plugin.summary()
        };
        self.record_event(
            session_id,
            &name,
            "plugin_installed",
            format!("Installed plugin {name} revision {next_revision}"),
            serde_json::json!({
                "revision": next_revision,
                "version": installed_package.manifest.version,
                "installed_from": source.display().to_string(),
                "provides": manifest_json["provides"],
                "grants": manifest_json["grants"],
                "secrets": manifest_json["secrets"],
                "variant_of": manifest_json["variant_of"],
            }),
        )
        .await?;
        Ok(summary)
    }

    /// Set whether `name` is enabled for the install (`topic_id` absent) or
    /// for one topic.
    pub async fn set_enablement(
        &self,
        name: &str,
        topic_id: Option<Uuid>,
        enabled: bool,
        session_id: Option<Uuid>,
    ) -> Result<PluginSummary, PluginServiceError> {
        if !self.read().contains_key(name) {
            return Err(PluginServiceError::NotFound(name.to_owned()));
        }
        self.storage
            .set_plugin_enablement(name, topic_id, enabled)
            .await?;
        let summary = {
            let mut installed = self.write();
            let plugin = installed
                .get_mut(name)
                .ok_or_else(|| PluginServiceError::NotFound(name.to_owned()))?;
            match topic_id {
                None => plugin.enabled_for_install = enabled,
                Some(topic) => {
                    plugin.enabled_topics.remove(&topic);
                    plugin.disabled_topics.remove(&topic);
                    if enabled {
                        plugin.enabled_topics.insert(topic);
                    } else {
                        plugin.disabled_topics.insert(topic);
                    }
                }
            }
            plugin.summary()
        };
        let scope = topic_id.map_or_else(
            || "the install".to_owned(),
            |topic| format!("topic {topic}"),
        );
        self.record_event(
            session_id,
            name,
            if enabled {
                "plugin_enabled"
            } else {
                "plugin_disabled"
            },
            format!(
                "{} plugin {name} for {scope}",
                if enabled { "Enabled" } else { "Disabled" }
            ),
            serde_json::json!({ "topic_id": topic_id, "enabled": enabled }),
        )
        .await?;
        Ok(summary)
    }

    /// Every installed plugin, by name.
    pub fn list(&self) -> PluginListResponse {
        PluginListResponse {
            plugins: self.read().values().map(InstalledPlugin::summary).collect(),
        }
    }

    pub fn summary(&self, name: &str) -> Option<PluginSummary> {
        self.read().get(name).map(InstalledPlugin::summary)
    }

    /// The tools every serving plugin provides, for the dispatcher's
    /// registry. Enablement does not apply here: a scope that does not
    /// enable a plugin withholds its tools at the turn
    /// ([`Self::withheld_tool_names`]).
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.read()
            .values()
            .filter_map(|plugin| {
                plugin
                    .runtime
                    .as_ref()
                    .ok()
                    .map(|runtime| (plugin, runtime))
            })
            .flat_map(|(plugin, runtime)| {
                plugin
                    .manifest
                    .provides
                    .tools
                    .iter()
                    .map(move |declaration| {
                        Arc::new(tool::PluginTool::new(
                            plugin.manifest.name.clone(),
                            declaration.clone(),
                            plugin.manifest.grants(HostGrant::Tools),
                            Arc::clone(runtime),
                        )) as Arc<dyn Tool>
                    })
            })
            .collect()
    }

    /// The plugin tools a session in `topic_id` (none: the general scope)
    /// must not see: every tool of a plugin the scope does not enable.
    pub fn withheld_tool_names(&self, topic_id: Option<Uuid>) -> Vec<String> {
        self.read()
            .values()
            .filter(|plugin| !plugin.enabled_for(topic_id))
            .flat_map(|plugin| {
                plugin
                    .manifest
                    .provides
                    .tools
                    .iter()
                    .map(|tool| tool.name.clone())
            })
            .collect()
    }

    /// Whether `tool_name` is provided by an installed plugin.
    pub fn provides_tool(&self, tool_name: &str) -> bool {
        self.read().values().any(|plugin| {
            plugin
                .manifest
                .provides
                .tools
                .iter()
                .any(|tool| tool.name == tool_name)
        })
    }

    /// Record a plugin event in the audit trail, attributed to the session
    /// that caused it. A failed record is reported as the trail missing
    /// it: the change it describes has already been applied.
    async fn record_event(
        &self,
        session_id: Option<Uuid>,
        plugin: &str,
        event_kind: &str,
        summary: String,
        metadata: serde_json::Value,
    ) -> Result<(), PluginServiceError> {
        self.storage
            .create_audit_event(NewAuditEvent {
                id: Uuid::new_v4().to_string(),
                session_id,
                entity_kind: "plugin".to_owned(),
                entity_id: plugin.to_owned(),
                event_kind: event_kind.to_owned(),
                summary: summary.clone(),
                metadata,
            })
            .await
            .map(drop)
            .map_err(|error| {
                PluginServiceError::Unrecorded(format!(
                    "'{summary}' was applied but recording it failed ({error}); \
                     the change stands without a record — reconcile before further changes"
                ))
            })
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, InstalledPlugin>> {
        self.installed
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, InstalledPlugin>> {
        self.installed
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Rebuild the live tool dispatcher so the plugins' current tools are in
/// it: after an install, and after any change that adds or removes a
/// plugin tool. Enablement needs no rebuild — it is read at each turn.
pub(crate) fn refresh_tool_dispatcher(state: &AppState) {
    let config = state.active_config();
    let dispatcher = crate::startup::build_tool_dispatcher(
        &config,
        state.storage.clone(),
        &state.config_root,
        state.plugins.tools(),
    );
    state.replace_tool_dispatcher(dispatcher);
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_storage::SqliteStorage;
    use swage_storage::store::{AuditStore, PluginStore, SessionStore, TopicStore};

    const MANIFEST: &str = r#"
        name = "echo"
        version = "1.0.0"
        description = "Echoes its input."
        script = "plugin.js"
        grants = ["tools"]
        [[provides.tools]]
        name = "echo"
        description = "Echo the text back."
        parameters = { type = "object", properties = { text = { type = "string" } } }
    "#;

    fn write_package(root: &Path, script: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("plugin.toml"), MANIFEST).unwrap();
        std::fs::write(root.join("plugin.js"), script).unwrap();
    }

    /// A session an audit record can name: the audit table refers to the
    /// session row, so an invented id would not do.
    async fn session_in_a_topic(storage: &SqliteStorage) -> Uuid {
        let topic = TopicStore::create(
            storage,
            swage_storage::model::NewTopic {
                name: "plugins".into(),
                slug: "plugins".into(),
            },
        )
        .await
        .unwrap();
        storage
            .create_session(swage_storage::fixtures::new_session(topic.id))
            .await
            .unwrap()
            .id
    }

    fn service(root: &Path) -> (PluginService, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let service = PluginService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            root.join(PLUGINS_DIR),
        );
        (service, storage)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_install_records_a_revision_serves_the_tool_and_is_audited() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        write_package(&source, r#"swage.provide.tool("echo", (p) => p);"#);
        let (service, storage) = service(dir.path());
        let session = session_in_a_topic(&storage).await;

        let summary = service
            .install(&source, &["shell".to_owned()], Some(session))
            .await
            .expect("installs");
        assert_eq!(summary.name, "echo");
        assert_eq!(summary.revision, 1);
        assert_eq!(summary.grants, vec!["tools"]);
        assert_eq!(summary.provides.tools[0].name, "echo");
        assert_eq!(summary.service, PluginServiceState::Serving);
        assert!(!summary.enabled_for_install);
        assert!(dir.path().join("plugins/echo/1/plugin.js").is_file());

        let tools = service.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "echo");
        assert!(service.provides_tool("echo"));
        assert_eq!(service.withheld_tool_names(None), vec!["echo"]);

        let events = storage.list_audit_events_by_session(session).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_kind, "plugin_installed");
        assert_eq!(events[0].entity_id, "echo");
        assert_eq!(events[0].metadata["revision"], 1);
        assert_eq!(events[0].metadata["grants"][0], "tools");

        let again = service
            .install(&source, &[], Some(session))
            .await
            .expect("reinstalls as a new revision");
        assert_eq!(again.revision, 2);
        assert_eq!(again.revisions.len(), 2);
        assert!(!again.revisions[0].active);
        assert!(again.revisions[1].active);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_package_whose_tool_collides_with_a_built_in_is_refused_before_copying() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        write_package(&source, r#"swage.provide.tool("echo", (p) => p);"#);
        let (service, storage) = service(dir.path());
        let error = service
            .install(&source, &["echo".to_owned()], None)
            .await
            .expect_err("refused");
        assert!(
            matches!(&error, PluginServiceError::Refused(message) if message.contains("built-in")),
            "{error}"
        );
        assert!(!dir.path().join("plugins").exists());
        assert!(storage.list_plugin_revisions().await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_script_that_does_not_load_is_refused_and_leaves_no_install_behind() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        write_package(&source, "throw new Error('broken at load');");
        let (service, storage) = service(dir.path());
        let error = service
            .install(&source, &[], None)
            .await
            .expect_err("refused");
        assert!(error.to_string().contains("broken at load"), "{error}");
        assert!(!dir.path().join("plugins/echo/1").exists());
        assert!(storage.list_plugin_revisions().await.unwrap().is_empty());
        assert!(service.list().plugins.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enablement_answers_per_scope_and_withholds_tools_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        write_package(&source, r#"swage.provide.tool("echo", (p) => p);"#);
        let (service, storage) = service(dir.path());
        service.install(&source, &[], None).await.unwrap();
        let topic = Uuid::new_v4();
        let other = Uuid::new_v4();

        let summary = service
            .set_enablement("echo", Some(topic), true, None)
            .await
            .expect("enables for the topic");
        assert_eq!(summary.enabled_topics, vec![topic]);
        assert!(service.withheld_tool_names(Some(topic)).is_empty());
        assert_eq!(service.withheld_tool_names(Some(other)), vec!["echo"]);
        assert_eq!(service.withheld_tool_names(None), vec!["echo"]);

        service
            .set_enablement("echo", None, true, None)
            .await
            .unwrap();
        assert!(service.withheld_tool_names(Some(other)).is_empty());
        assert!(service.withheld_tool_names(None).is_empty());

        let summary = service
            .set_enablement("echo", Some(other), false, None)
            .await
            .expect("a topic opts out over the install");
        assert_eq!(summary.disabled_topics, vec![other]);
        assert_eq!(service.withheld_tool_names(Some(other)), vec!["echo"]);

        let kinds: Vec<String> = storage
            .list_audit_events()
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.event_kind)
            .collect();
        assert!(kinds.contains(&"plugin_enabled".to_owned()));
        assert!(kinds.contains(&"plugin_disabled".to_owned()));

        let error = service
            .set_enablement("missing", None, true, None)
            .await
            .expect_err("unknown");
        assert!(matches!(error, PluginServiceError::NotFound(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fresh_service_loads_the_recorded_installs_and_their_enablement() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        write_package(&source, r#"swage.provide.tool("echo", (p) => p);"#);
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let first = PluginService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            dir.path().join(PLUGINS_DIR),
        );
        first.install(&source, &[], None).await.unwrap();
        first
            .set_enablement("echo", None, true, None)
            .await
            .unwrap();
        drop(first);

        let second = PluginService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            dir.path().join(PLUGINS_DIR),
        );
        second.load().await.expect("loads");
        let listed = second.list().plugins;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].service, PluginServiceState::Serving);
        assert!(listed[0].enabled_for_install);
        assert_eq!(second.tools().len(), 1);

        std::fs::remove_dir_all(dir.path().join("plugins/echo/1")).unwrap();
        let third = PluginService::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            dir.path().join(PLUGINS_DIR),
        );
        third.load().await.expect("loads");
        let listed = third.list().plugins;
        assert!(
            matches!(&listed[0].service, PluginServiceState::Failed { reason } if reason.contains("not a directory")),
            "{:?}",
            listed[0].service
        );
        assert!(third.tools().is_empty(), "a failed plugin serves no tool");
    }
}
