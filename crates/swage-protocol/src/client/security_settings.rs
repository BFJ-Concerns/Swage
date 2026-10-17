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
    /// Tools currently registered with the gateway, available to the approval picker.
    pub available_tools: Vec<String>,
    pub scope: SettingsScope,
    pub effective: EffectiveSecurityChecks,
    pub overrides: SecurityOverrides,
    pub relaxed: Vec<RelaxedCheck>,
}

/// Changes to a scope's security overrides. Absent fields leave stored
/// overrides unchanged; explicit null clears an override so the scope
/// beneath decides again. A supplied approval list replaces that list.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(optional_fields = nullable)]
pub struct SaveSecuritySettingsRequest {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_override"
    )]
    #[ts(optional)]
    pub sandbox_enabled: Option<Option<bool>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_override"
    )]
    #[ts(optional)]
    pub allow_degraded_execution: Option<Option<bool>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_override"
    )]
    #[ts(optional)]
    pub require_approval: Option<Option<Vec<RequireApprovalEntry>>>,
    /// Install scope only; a topic's request carrying it is refused.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_override"
    )]
    #[ts(optional)]
    pub access_gate: Option<Option<AccessGateMode>>,
}

// Serde calls this only for a present field, preserving explicit null as
// Some(None) while the field's default represents omission as None.
#[expect(
    clippy::option_option,
    reason = "serde must distinguish omitted, null and supplied overrides"
)]
fn present_override<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_updates_distinguish_absent_null_and_value() {
        let empty: SaveSecuritySettingsRequest =
            serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(empty.sandbox_enabled.is_none());
        assert!(empty.allow_degraded_execution.is_none());
        assert!(empty.require_approval.is_none());
        assert!(empty.access_gate.is_none());
        assert_eq!(serde_json::to_value(empty).unwrap(), serde_json::json!({}));
        let cleared: SaveSecuritySettingsRequest = serde_json::from_value(serde_json::json!({
            "sandbox_enabled": null, "allow_degraded_execution": null,
            "require_approval": null, "access_gate": null
        }))
        .unwrap();
        assert_eq!(cleared.sandbox_enabled, Some(None));
        assert_eq!(cleared.allow_degraded_execution, Some(None));
        assert!(matches!(cleared.require_approval, Some(None)));
        assert_eq!(cleared.access_gate, Some(None));
        assert_eq!(
            serde_json::to_value(cleared).unwrap(),
            serde_json::json!({
                "sandbox_enabled": null, "allow_degraded_execution": null,
                "require_approval": null, "access_gate": null
            })
        );
        let set: SaveSecuritySettingsRequest = serde_json::from_value(serde_json::json!({
            "sandbox_enabled": false, "allow_degraded_execution": true,
            "require_approval": [], "access_gate": "warn"
        }))
        .unwrap();
        assert_eq!(set.sandbox_enabled, Some(Some(false)));
        assert_eq!(set.allow_degraded_execution, Some(Some(true)));
        assert!(set.require_approval.unwrap().unwrap().is_empty());
        assert_eq!(set.access_gate, Some(Some(AccessGateMode::Warn)));
    }
}
