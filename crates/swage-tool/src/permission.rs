//! Permission resolver — per-agent allow/deny enforcement for tool access.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use swage_core::{PermissionDeniedReason, ToolError};

// ---------------------------------------------------------------------------
// PermissionConfig
// ---------------------------------------------------------------------------

/// Per-agent tool permission configuration.
///
/// Controls which tools an agent may invoke. The resolution order is strict —
/// see [`check_permission`] for full semantics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionConfig {
    /// Tool names or `"*"` (wildcard) that this agent is permitted to invoke.
    /// A bundle name here expands to all tools in the bundle.
    pub allow: Vec<String>,

    /// Tool names that are always denied, overriding any `allow` entry.
    pub deny: Vec<String>,

    /// Named bundles mapping a group label to a list of tool names.
    ///
    /// e.g. `"filesystem" → ["file_read", "file_write", "file_list", "file_search"]`
    ///
    /// A bundle name in `allow` expands to its constituent tools at resolution time.
    pub bundles: HashMap<String, Vec<String>>,
}

// ---------------------------------------------------------------------------
// check_permission
// ---------------------------------------------------------------------------

/// Check whether `tool_name` is permitted by `config`.
///
/// Returns `Ok(())` if the tool is allowed, or `Err(ToolError::PermissionDenied)`
/// with the appropriate [`PermissionDeniedReason`] variant if denied.
///
/// Resolution rules (applied in strict order):
///
/// 1. **Deny wins**: if `tool_name` appears in `config.deny`, return `ExplicitlyDenied`.
/// 2. **Wildcard**: if `config.allow` contains `"*"`, return `Ok(())`.
/// 3. **Direct match**: if `config.allow` contains `tool_name` directly, return `Ok(())`.
/// 4. **Bundle expansion**: for each entry in `config.allow` that names a bundle,
///    expand it; if `tool_name` is in the expansion, return `Ok(())`.
/// 5. **Default-deny**: return `NotAllowed`.
pub fn check_permission(config: &PermissionConfig, tool_name: &str) -> Result<(), ToolError> {
    // Rule 1: deny list is absolute — no allow entry can override it.
    if config.deny.iter().any(|d| d == tool_name) {
        return Err(ToolError::PermissionDenied {
            name: tool_name.to_string(),
            reason: PermissionDeniedReason::ExplicitlyDenied,
        });
    }

    // Rules 2–4: scan allow entries for a match.
    for entry in &config.allow {
        // Wildcard: allows everything not explicitly denied (checked above).
        if entry == "*" {
            return Ok(());
        }
        // Direct name match.
        if entry == tool_name {
            return Ok(());
        }
        // Bundle expansion: treat `entry` as a bundle label and search its tools.
        if let Some(bundle_tools) = config.bundles.get(entry) {
            if bundle_tools.iter().any(|t| t == tool_name) {
                return Ok(());
            }
        }
    }

    // Rule 5: default-deny — not found in any allow entry.
    Err(ToolError::PermissionDenied {
        name: tool_name.to_string(),
        reason: PermissionDeniedReason::NotAllowed,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use swage_core::{PermissionDeniedReason, ToolError};

    use super::{PermissionConfig, check_permission};

    // -- Default-deny ---------------------------------------------------------

    #[test]
    fn empty_config_denies_all() {
        let cfg = PermissionConfig::default();
        let err = check_permission(&cfg, "shell").unwrap_err();
        match err {
            ToolError::PermissionDenied { name, reason } => {
                assert_eq!(name, "shell");
                assert_eq!(reason, PermissionDeniedReason::NotAllowed);
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
    }

    #[test]
    fn unlisted_tool_not_allowed_reason() {
        let cfg = PermissionConfig {
            allow: vec!["file_read".into()],
            deny: vec![],
            bundles: HashMap::new(),
        };
        let err = check_permission(&cfg, "shell").unwrap_err();
        match err {
            ToolError::PermissionDenied { name, reason } => {
                assert_eq!(name, "shell");
                assert_eq!(reason, PermissionDeniedReason::NotAllowed);
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
    }

    // -- Direct allow ---------------------------------------------------------

    #[test]
    fn direct_allow_permits_tool() {
        let cfg = PermissionConfig {
            allow: vec!["shell".into()],
            deny: vec![],
            bundles: HashMap::new(),
        };
        assert!(check_permission(&cfg, "shell").is_ok());
    }

    // -- Wildcard -------------------------------------------------------------

    #[test]
    fn wildcard_allows_any_tool() {
        let cfg = PermissionConfig {
            allow: vec!["*".into()],
            deny: vec![],
            bundles: HashMap::new(),
        };
        assert!(check_permission(&cfg, "shell").is_ok());
        assert!(check_permission(&cfg, "web_search").is_ok());
        assert!(check_permission(&cfg, "anything_at_all").is_ok());
    }

    #[test]
    fn wildcard_with_deny_blocks_specific_tool() {
        let cfg = PermissionConfig {
            allow: vec!["*".into()],
            deny: vec!["shell".into()],
            bundles: HashMap::new(),
        };
        let err = check_permission(&cfg, "shell").unwrap_err();
        match err {
            ToolError::PermissionDenied { reason, .. } => {
                assert_eq!(reason, PermissionDeniedReason::ExplicitlyDenied);
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
        // Other tools still allowed.
        assert!(check_permission(&cfg, "file_read").is_ok());
    }

    // -- Explicit deny --------------------------------------------------------

    #[test]
    fn deny_overrides_direct_allow() {
        let cfg = PermissionConfig {
            allow: vec!["shell".into()],
            deny: vec!["shell".into()],
            bundles: HashMap::new(),
        };
        let err = check_permission(&cfg, "shell").unwrap_err();
        match err {
            ToolError::PermissionDenied { reason, .. } => {
                assert_eq!(reason, PermissionDeniedReason::ExplicitlyDenied);
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
    }

    // -- Bundle expansion -----------------------------------------------------

    #[test]
    fn bundle_allow_expands_to_member_tools() {
        let mut bundles = HashMap::new();
        bundles.insert(
            "filesystem".into(),
            vec!["file_read".into(), "file_write".into(), "file_list".into()],
        );
        let cfg = PermissionConfig {
            allow: vec!["filesystem".into()],
            deny: vec![],
            bundles,
        };
        assert!(check_permission(&cfg, "file_read").is_ok());
        assert!(check_permission(&cfg, "file_write").is_ok());
        assert!(check_permission(&cfg, "file_list").is_ok());
        // Tool outside the bundle must still be denied.
        assert!(check_permission(&cfg, "shell").is_err());
    }

    #[test]
    fn deny_overrides_bundle_allow() {
        let mut bundles = HashMap::new();
        bundles.insert(
            "filesystem".into(),
            vec!["file_read".into(), "file_write".into()],
        );
        let cfg = PermissionConfig {
            allow: vec!["filesystem".into()],
            deny: vec!["file_write".into()],
            bundles,
        };
        // file_read is in bundle and not denied.
        assert!(check_permission(&cfg, "file_read").is_ok());
        // file_write is in bundle but explicitly denied.
        let err = check_permission(&cfg, "file_write").unwrap_err();
        match err {
            ToolError::PermissionDenied { name, reason } => {
                assert_eq!(name, "file_write");
                assert_eq!(reason, PermissionDeniedReason::ExplicitlyDenied);
            }
            other => panic!("expected PermissionDenied, got: {other:?}"),
        }
    }
}
