//! The plugin manifest: what a plugin provides and what it needs.
//!
//! `plugin.toml` at a package's root is the one declaration the kernel reads.
//! Everything the settings surface lists — provided tools, grants, secrets,
//! the function the plugin is a variant of — comes from here, and a manifest
//! that asks for something the kernel does not offer (an unknown grant, a
//! kind of provision the kernel cannot load) is refused at parse, never
//! accepted and ignored.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use swage_isolate::IsolateLimits;

/// The manifest's file name at a package's root.
pub const MANIFEST_FILE: &str = "plugin.toml";

/// Why a manifest was refused.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The TOML could not be read as a manifest: a syntax error, a missing
    /// field, an unknown field, or a grant or provision kind the kernel does
    /// not offer.
    #[error("plugin manifest could not be read: {0}")]
    Parse(String),
    /// A field held a value the kernel refuses.
    #[error("plugin manifest refused: {0}")]
    Invalid(String),
}

/// A host capability a plugin asks for. The shim exposes a host function only
/// under its grant, and the host refuses a call for anything else, so a grant
/// is a capability in two places and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostGrant {
    /// Call the session's tools through the host, under the session's own
    /// grants, approvals and record: `swage.callTool(name, arguments)`.
    Tools,
}

impl HostGrant {
    /// The grant as the manifest and the settings surface name it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
        }
    }
}

/// A secret the plugin holds. The kernel records the declaration; delivering
/// the value is the host's business and is never done here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretDeclaration {
    pub name: String,
    pub description: String,
}

/// A tool the plugin provides, as the model will see it. The script supplies
/// the handler under the same name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDeclaration {
    pub name: String,
    pub description: String,
    /// The JSON Schema object of the tool's parameters.
    pub parameters: serde_json::Value,
}

/// What the plugin provides, by kind. A kind the kernel cannot load is an
/// unknown field here, refused at parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Provides {
    pub tools: Vec<ToolDeclaration>,
}

impl Provides {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// The isolate's ceilings as the manifest states them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IsolateLimitsDeclaration {
    /// The most heap the plugin's isolate may hold, in mebibytes.
    pub heap_mib: u64,
    /// The longest a handler may run without yielding to the host, in
    /// seconds.
    pub continuous_execution_secs: u64,
}

impl Default for IsolateLimitsDeclaration {
    fn default() -> Self {
        Self {
            heap_mib: 64,
            continuous_execution_secs: 5,
        }
    }
}

impl IsolateLimitsDeclaration {
    /// The heap limit in bytes, or `None` when the declared size does not
    /// fit this host's address space; validation refuses such a manifest.
    fn heap_bytes(&self) -> Option<usize> {
        self.heap_mib
            .checked_mul(1024 * 1024)
            .and_then(|bytes| usize::try_from(bytes).ok())
    }

    pub fn limits(self) -> IsolateLimits {
        IsolateLimits {
            heap_bytes: self.heap_bytes().unwrap_or(usize::MAX),
            continuous_execution: Duration::from_secs(self.continuous_execution_secs),
        }
    }
}

/// A process the gateway starts for a session on its first call and stops
/// when the session closes, after `idle_secs` without a call, or when the
/// gateway stops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessDeclaration {
    /// The executable, relative to the package root.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// How long the process may go without a call before it is stopped.
    #[serde(default = "default_idle_secs")]
    pub idle_secs: u64,
}

fn default_idle_secs() -> u64 {
    300
}

impl ProcessDeclaration {
    pub fn idle(&self) -> Duration {
        Duration::from_secs(self.idle_secs)
    }
}

/// How a plugin runs: in an isolate the kernel builds, or as a process the
/// gateway brokers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Isolate,
    Process,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Isolate => "isolate",
            Profile::Process => "process",
        }
    }
}

/// `plugin.toml`, parsed and validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// The plugin's identity: `[a-z0-9-]+`, the directory it installs under.
    pub name: String,
    pub version: String,
    pub description: String,
    /// The script file, relative to the package root: the plugin runs in an
    /// isolate. Exactly one of `script` and `process` is declared.
    #[serde(default)]
    pub script: Option<String>,
    /// A process the gateway brokers per session: the plugin runs as that
    /// process, which speaks the plugin-process contract over a socket.
    /// Exactly one of `script` and `process` is declared.
    #[serde(default)]
    pub process: Option<ProcessDeclaration>,
    #[serde(default)]
    pub grants: Vec<HostGrant>,
    #[serde(default)]
    pub secrets: Vec<SecretDeclaration>,
    /// The function this plugin is a variant of — a location in the
    /// experimentation registry's terms — when two plugins provide one
    /// function and are drawn against each other.
    #[serde(default)]
    pub variant_of: Option<String>,
    #[serde(default)]
    pub limits: IsolateLimitsDeclaration,
    #[serde(default)]
    pub provides: Provides,
}

impl PluginManifest {
    /// Parse `plugin.toml` text and validate it.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(text)
            .map_err(|error| ManifestError::Parse(error.message().to_owned()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Whether the manifest asks for `grant`.
    pub fn grants(&self, grant: HostGrant) -> bool {
        self.grants.contains(&grant)
    }

    /// How the plugin runs; validation guarantees exactly one declaration.
    pub fn profile(&self) -> Profile {
        if self.process.is_some() {
            Profile::Process
        } else {
            Profile::Isolate
        }
    }

    fn validate(&self) -> Result<(), ManifestError> {
        validate_plugin_name(&self.name)?;
        if self.version.trim().is_empty() {
            return Err(ManifestError::Invalid("version must not be empty".into()));
        }
        if self.description.trim().is_empty() {
            return Err(ManifestError::Invalid(
                "description must not be empty".into(),
            ));
        }
        match (&self.script, &self.process) {
            (Some(script), None) => validate_relative_file(script, "script")?,
            (None, Some(process)) => {
                validate_relative_file(&process.command, "process.command")?;
                if process.idle_secs == 0 {
                    return Err(ManifestError::Invalid(
                        "process.idle_secs must be positive".into(),
                    ));
                }
            }
            (Some(_), Some(_)) => {
                return Err(ManifestError::Invalid(
                    "declare either script (an isolate) or [process] (a brokered process), not both"
                        .into(),
                ));
            }
            (None, None) => {
                return Err(ManifestError::Invalid(
                    "declare how the plugin runs: script (an isolate) or [process] (a brokered process)"
                        .into(),
                ));
            }
        }
        if self.provides.is_empty() {
            return Err(ManifestError::Invalid(
                "the plugin provides nothing: declare at least one tool under [[provides.tools]]"
                    .into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for grant in &self.grants {
            if !seen.insert(*grant) {
                return Err(ManifestError::Invalid(format!(
                    "grant '{}' is declared twice",
                    grant.as_str()
                )));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for secret in &self.secrets {
            if secret.name.trim().is_empty() {
                return Err(ManifestError::Invalid(
                    "a secret declaration must carry a name".into(),
                ));
            }
            if !seen.insert(secret.name.as_str()) {
                return Err(ManifestError::Invalid(format!(
                    "secret '{}' is declared twice",
                    secret.name
                )));
            }
        }
        if self
            .variant_of
            .as_deref()
            .is_some_and(|location| location.trim().is_empty())
        {
            return Err(ManifestError::Invalid(
                "variant_of must name a function, or be omitted".into(),
            ));
        }
        if self.limits.heap_mib == 0 || self.limits.continuous_execution_secs == 0 {
            return Err(ManifestError::Invalid(
                "limits.heap_mib and limits.continuous_execution_secs must be positive".into(),
            ));
        }
        if self.limits.heap_bytes().is_none() {
            return Err(ManifestError::Invalid(
                "limits.heap_mib is larger than this host can address".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for tool in &self.provides.tools {
            validate_tool_name(&tool.name)?;
            if !seen.insert(tool.name.as_str()) {
                return Err(ManifestError::Invalid(format!(
                    "tool '{}' is declared twice",
                    tool.name
                )));
            }
            if tool.description.trim().is_empty() {
                return Err(ManifestError::Invalid(format!(
                    "tool '{}' must carry a description",
                    tool.name
                )));
            }
            if !tool.parameters.is_object() {
                return Err(ManifestError::Invalid(format!(
                    "tool '{}' parameters must be a JSON Schema object",
                    tool.name
                )));
            }
        }
        Ok(())
    }
}

/// A plugin name is its install directory and its audit identity: lower-case
/// letters, digits and hyphens.
pub fn validate_plugin_name(name: &str) -> Result<(), ManifestError> {
    if name.is_empty() {
        return Err(ManifestError::Invalid("name must not be empty".into()));
    }
    if !name
        .chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
    {
        return Err(ManifestError::Invalid(format!(
            "name '{name}' must match [a-z0-9-]+"
        )));
    }
    Ok(())
}

/// A tool name is what the model calls: a lower-case identifier.
fn validate_tool_name(name: &str) -> Result<(), ManifestError> {
    let mut chars = name.chars();
    let well_formed = chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'));
    if !well_formed {
        return Err(ManifestError::Invalid(format!(
            "tool name '{name}' must match [a-z][a-z0-9_]*"
        )));
    }
    Ok(())
}

fn validate_relative_file(path: &str, field: &str) -> Result<(), ManifestError> {
    let candidate = std::path::Path::new(path);
    if path.trim().is_empty() {
        return Err(ManifestError::Invalid(format!("{field} must name a file")));
    }
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(ManifestError::Invalid(format!(
            "{field} '{path}' must be a plain path inside the package"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECHO: &str = r#"
        name = "echo"
        version = "1.0.0"
        description = "Echoes its input."
        script = "plugin.js"
        grants = ["tools"]
        secrets = [{ name = "api_key", description = "The upstream key." }]
        variant_of = "tools.echo"

        [limits]
        heap_mib = 8
        continuous_execution_secs = 2

        [[provides.tools]]
        name = "echo"
        description = "Echo the text back."
        parameters = { type = "object", properties = { text = { type = "string" } }, required = ["text"] }
    "#;

    #[test]
    fn a_conforming_manifest_reads_every_declaration() {
        let manifest = PluginManifest::parse(ECHO).expect("parses");
        assert_eq!(manifest.name, "echo");
        assert!(manifest.grants(HostGrant::Tools));
        assert_eq!(manifest.secrets[0].name, "api_key");
        assert_eq!(manifest.variant_of.as_deref(), Some("tools.echo"));
        assert_eq!(manifest.limits.limits().heap_bytes, 8 * 1024 * 1024);
        assert_eq!(
            manifest.limits.limits().continuous_execution,
            Duration::from_secs(2)
        );
        let tool = &manifest.provides.tools[0];
        assert_eq!(tool.name, "echo");
        assert_eq!(tool.parameters["required"][0], "text");
    }

    #[test]
    fn defaults_fill_what_the_manifest_leaves_out() {
        let manifest = PluginManifest::parse(
            r#"
            name = "quiet"
            version = "0.1"
            description = "Says nothing."
            script = "main.js"
            [[provides.tools]]
            name = "nothing"
            description = "Does nothing."
            parameters = { type = "object" }
            "#,
        )
        .expect("parses");
        assert!(manifest.grants.is_empty());
        assert!(manifest.secrets.is_empty());
        assert_eq!(manifest.variant_of, None);
        assert_eq!(manifest.limits, IsolateLimitsDeclaration::default());
    }

    #[test]
    fn an_unknown_grant_is_refused_by_name() {
        let error = PluginManifest::parse(
            &ECHO.replace(r#"grants = ["tools"]"#, r#"grants = ["network"]"#),
        )
        .expect_err("refused");
        assert!(
            matches!(&error, ManifestError::Parse(message) if message.contains("network")),
            "{error}"
        );
    }

    #[test]
    fn a_kind_the_kernel_cannot_load_is_refused() {
        let error =
            PluginManifest::parse(&format!("{ECHO}\n[[provides.channels]]\nname = \"irc\"\n"))
                .expect_err("refused");
        assert!(
            matches!(&error, ManifestError::Parse(message) if message.contains("channels")),
            "{error}"
        );
    }

    #[test]
    fn a_manifest_declares_exactly_one_way_to_run() {
        let neither = PluginManifest::parse(&ECHO.replace(r#"script = "plugin.js""#, ""))
            .expect_err("refused");
        assert!(
            matches!(&neither, ManifestError::Invalid(message) if message.contains("script") && message.contains("[process]")),
            "{neither}"
        );
        let both = PluginManifest::parse(&format!("{ECHO}\n[process]\ncommand = \"server\"\n"))
            .expect_err("refused");
        assert!(
            matches!(&both, ManifestError::Invalid(message) if message.contains("not both")),
            "{both}"
        );
    }

    #[test]
    fn a_process_plugin_names_its_command_and_defaults_its_idle_period() {
        let manifest = PluginManifest::parse(&format!(
            "{}\n[process]\ncommand = \"server.py\"\nargs = [\"--serve\"]\n",
            ECHO.replace(r#"script = "plugin.js""#, "")
        ))
        .expect("parses");
        assert_eq!(manifest.profile(), Profile::Process);
        let process = manifest.process.as_ref().expect("declared");
        assert_eq!(process.command, "server.py");
        assert_eq!(process.args, vec!["--serve"]);
        assert_eq!(process.idle(), Duration::from_secs(300));
        let absolute = PluginManifest::parse(&format!(
            "{}\n[process]\ncommand = \"/usr/bin/server\"\n",
            ECHO.replace(r#"script = "plugin.js""#, "")
        ))
        .expect_err("refused");
        assert!(
            matches!(&absolute, ManifestError::Invalid(message) if message.contains("process.command")),
            "{absolute}"
        );
    }

    #[test]
    fn a_heap_limit_beyond_the_address_space_is_refused() {
        let error =
            PluginManifest::parse(&ECHO.replace("heap_mib = 8", "heap_mib = 17592186044416"))
                .expect_err("refused");
        assert!(
            matches!(&error, ManifestError::Invalid(message) if message.contains("heap_mib")),
            "{error}"
        );
    }

    #[test]
    fn the_rules_each_name_the_field_they_refuse() {
        let cases = [
            (r#"name = "echo""#, r#"name = "Echo Plugin""#, "[a-z0-9-]+"),
            (
                r#"script = "plugin.js""#,
                r#"script = "../plugin.js""#,
                "inside the package",
            ),
            (
                r#"name = "echo"
        description = "Echo the text back.""#,
                r#"name = "Echo"
        description = "Echo the text back.""#,
                "[a-z][a-z0-9_]*",
            ),
            (
                r#"parameters = { type = "object", properties = { text = { type = "string" } }, required = ["text"] }"#,
                r#"parameters = "string""#,
                "JSON Schema object",
            ),
            (r#"heap_mib = 8"#, r#"heap_mib = 0"#, "positive"),
        ];
        for (from, to, expected) in cases {
            let error = PluginManifest::parse(&ECHO.replace(from, to)).expect_err(expected);
            assert!(error.to_string().contains(expected), "{error}");
        }
        let error = PluginManifest::parse(
            r#"
            name = "empty"
            version = "0.1"
            description = "Provides nothing."
            script = "main.js"
            "#,
        )
        .expect_err("refused");
        assert!(error.to_string().contains("provides nothing"), "{error}");
    }
}
