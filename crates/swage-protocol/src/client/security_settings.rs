//! Security settings as they apply in one scope — the whole install, or one
//! topic — read and written through the settings surface (C69).
//!
//! Every shipped security check is on by default. A scope may relax a check
//! by setting it in its own configuration; what a session in that scope
//! actually runs under is the cascade's result (`effective`), the scope's own
//! settings are `overrides`, and `relaxed` names each check standing below
//! the shipped default and the scope that relaxed it, so the settings page
//! and a session's view can show the same thing.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use super::access::AccessGateMode;
use super::settings::RequireApprovalEntry;

/// The scope a set of security settings belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(optional_fields = nullable)]
pub enum SettingsScope {
    /// The install: the global configuration every session starts from.
    Install,
    /// One topic: its configuration overlays the install's for its sessions.
    Topic { topic_id: Uuid },
}

/// The shipped security checks a scope can relax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum SecurityCheck {
    /// The per-turn sandbox around shell, Python and file tools.
    Sandbox,
    /// The sandbox's confinement: with degraded execution allowed, a turn
    /// whose sandbox cannot start runs unconfined instead of refusing.
    SandboxConfinement,
    /// Approval prompts before the high-risk tools run in attended sessions.
    ApprovalPrompts,
    /// The token a client must carry to reach a gateway bound beyond
    /// loopback. Install-wide: it admits a client before any topic is chosen.
    AccessGate,
}

/// One check standing below the shipped default, and where it was relaxed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct RelaxedCheck {
    pub check: SecurityCheck,
    /// The scope whose settings relaxed it: a topic's own override, or the
    /// install's configuration the topic inherits.
    pub relaxed_by: SettingsScope,
    /// What the relaxation is, in the settings page's own words.
    pub detail: String,
}

/// The security checks as a session in the scope runs under them.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct EffectiveSecurityChecks {
    pub sandbox_enabled: bool,
    pub allow_degraded_execution: bool,
    /// The tools an attended session is asked about before they run.
    pub require_approval: Vec<RequireApprovalEntry>,
    /// How a binding beyond loopback gates requests; loopback is never gated.
    pub access_gate: AccessGateMode,
}

/// What one scope's own configuration says. Each field is absent where the
/// scope leaves the setting to the scope beneath it (a topic to the install,
/// the install to the shipped default).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct SecurityOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub allow_degraded_execution: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub require_approval: Option<Vec<RequireApprovalEntry>>,
    /// Install scope only; a topic's request carrying it is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub access_gate: Option<AccessGateMode>,
}

/// A scope's security settings as the settings surface reads them.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[ts(optional_fields = nullable)]
pub struct SecuritySettingsResponse {
    pub scope: SettingsScope,
    pub effective: EffectiveSecurityChecks,
    pub overrides: SecurityOverrides,
    pub relaxed: Vec<RelaxedCheck>,
}

/// A scope's security settings as the settings surface writes them: the
/// overrides the scope should hold afterwards, whole. An absent field clears
/// the scope's own setting and the scope beneath decides again.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct SaveSecuritySettingsRequest {
    #[serde(flatten)]
    pub overrides: SecurityOverrides,
}
