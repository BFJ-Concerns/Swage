//! Permission resolver — per-agent allow/deny enforcement for tool access.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use swage_core::{ApprovalTimeoutPolicy, PermissionDeniedReason, ToolError};

// ---------------------------------------------------------------------------
// RequireApprovalEntry
// ---------------------------------------------------------------------------

/// An entry in the `require_approval` list, specifying a tool (or bundle)
/// that requires human approval before execution.
///
/// A `tool_name` of a bundle label expands to all constituent tools, the same
/// way bundle names work in the `allow` list. The per-entry `timeout_secs` and
/// `timeout_policy` override the agent-level HITL configuration for that tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequireApprovalEntry {
    /// Tool name or bundle label that requires approval.
    pub tool_name: String,

    /// Timeout override in seconds. `None` means use the agent-level config.
    pub timeout_secs: Option<u64>,

    /// What to do when the approval request times out. `None` falls back to
    /// the agent-level policy (default: Reject).
    pub timeout_policy: Option<ApprovalTimeoutPolicy>,
}

// ---------------------------------------------------------------------------
// PermissionCheckResult
// ---------------------------------------------------------------------------

/// Outcome of a successful permission check (tool not denied).
///
/// Callers that receive `RequiresApproval` must gate execution on human
/// approval before proceeding. The dispatcher signals this to the agent via
/// `ToolError::RequiresApproval` (wired in the dispatch pipeline), which
/// suspends the turn.
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionCheckResult {
    /// Tool is permitted unconditionally — proceed with execution.
    Allowed,
    /// Tool is permitted but requires a human approval gate before execution.
    RequiresApproval {
        /// Per-entry timeout override; `None` defers to the agent-level config.
        timeout_secs: Option<u64>,
        /// Per-entry timeout policy override; `None` defers to agent-level config.
        timeout_policy: Option<ApprovalTimeoutPolicy>,
    },
}

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

    /// Tool names that are always denied, overriding any `allow` or
    /// `require_approval` entry.
    pub deny: Vec<String>,

    /// Named bundles mapping a group label to a list of tool names.
    ///
    /// e.g. `"filesystem" → ["file_read", "file_write", "file_list", "file_search"]`
    ///
    /// A bundle name in `allow` or `require_approval` expands to its
    /// constituent tools at resolution time.
    pub bundles: HashMap<String, Vec<String>>,

    /// Tools that require human approval before execution. Resolution order
    /// places this between deny (higher priority) and allow (lower priority):
    /// a denied tool is never approved; a required-approval tool is implicitly
    /// permitted but gated. Bundle names here expand just like in `allow`.
    #[serde(default)]
    pub require_approval: Vec<RequireApprovalEntry>,
}

// ---------------------------------------------------------------------------
// check_permission
// ---------------------------------------------------------------------------

/// Check whether `tool_name` is permitted by `config`.
///
/// Returns `Ok(PermissionCheckResult)` if the tool is allowed (with or without
/// a required approval gate), or `Err(ToolError::PermissionDenied)` with the
/// appropriate [`PermissionDeniedReason`] if denied.
///
/// Resolution rules (applied in strict order):
///
/// 1. **Deny wins**: if `tool_name` appears in `config.deny`, return `ExplicitlyDenied`.
/// 2. **Require-approval**: if `tool_name` matches any `require_approval` entry
///    (directly or via bundle expansion), return `RequiresApproval`.
/// 3. **Wildcard**: if `config.allow` contains `"*"`, return `Allowed`.
/// 4. **Direct allow match**: if `config.allow` contains `tool_name` directly,
///    return `Allowed`.
/// 5. **Bundle allow expansion**: for each bundle name in `config.allow`, if
///    `tool_name` is in that bundle, return `Allowed`.
/// 6. **Default-deny**: return `NotAllowed`.
pub fn check_permission(
    config: &PermissionConfig,
    tool_name: &str,
) -> Result<PermissionCheckResult, ToolError> {
    // Rule 1: deny list is absolute — overrides both allow and require_approval.
    if config.deny.iter().any(|d| d == tool_name) {
        return Err(ToolError::PermissionDenied {
            name: tool_name.to_string(),
            reason: PermissionDeniedReason::ExplicitlyDenied,
        });
    }

    // Rule 2: require_approval — check direct match and bundle expansion.
    for entry in &config.require_approval {
        // Direct tool name match.
        if entry.tool_name == tool_name {
            return Ok(PermissionCheckResult::RequiresApproval {
                timeout_secs: entry.timeout_secs,
                timeout_policy: entry.timeout_policy.clone(),
            });
        }
        // Bundle expansion: the entry's tool_name may be a bundle label.
        if let Some(bundle_tools) = config.bundles.get(&entry.tool_name)
            && bundle_tools.iter().any(|t| t == tool_name)
        {
            return Ok(PermissionCheckResult::RequiresApproval {
                timeout_secs: entry.timeout_secs,
                timeout_policy: entry.timeout_policy.clone(),
            });
        }
    }

    // Rules 3–5: scan allow entries for a match.
    for entry in &config.allow {
        // Wildcard: allows everything not explicitly denied (checked above).
        if entry == "*" {
            return Ok(PermissionCheckResult::Allowed);
        }
        // Direct name match.
        if entry == tool_name {
            return Ok(PermissionCheckResult::Allowed);
        }
        // Bundle expansion: treat `entry` as a bundle label and search its tools.
        if let Some(bundle_tools) = config.bundles.get(entry)
            && bundle_tools.iter().any(|t| t == tool_name)
        {
            return Ok(PermissionCheckResult::Allowed);
        }
    }

    // Rule 6: default-deny — not found in any allow or require_approval entry.
    Err(ToolError::PermissionDenied {
        name: tool_name.to_string(),
        reason: PermissionDeniedReason::NotAllowed,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use swage_core::{PermissionDeniedReason, ToolError};

    use super::{PermissionCheckResult, PermissionConfig, RequireApprovalEntry, check_permission};

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
            require_approval: vec![],
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
            require_approval: vec![],
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
            require_approval: vec![],
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
            require_approval: vec![],
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
            require_approval: vec![],
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
            require_approval: vec![],
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
            require_approval: vec![],
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

    // -- RequireApproval entries -------------------------------------------

    #[test]
    fn require_approval_tool_returns_requires_approval() {
        let cfg = PermissionConfig {
            allow: vec![],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry {
                tool_name: "shell".into(),
                timeout_secs: None,
                timeout_policy: None,
            }],
        };
        let result = check_permission(&cfg, "shell").unwrap();
        assert!(
            matches!(result, PermissionCheckResult::RequiresApproval { .. }),
            "expected RequiresApproval, got: {result:?}"
        );
    }

    #[test]
    fn require_approval_carries_timeout_secs() {
        let cfg = PermissionConfig {
            allow: vec![],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry {
                tool_name: "shell".into(),
                timeout_secs: Some(120),
                timeout_policy: None,
            }],
        };
        let result = check_permission(&cfg, "shell").unwrap();
        match result {
            PermissionCheckResult::RequiresApproval { timeout_secs, .. } => {
                assert_eq!(timeout_secs, Some(120));
            }
            other @ PermissionCheckResult::Allowed => {
                panic!("expected RequiresApproval, got: {other:?}");
            }
        }
    }

    #[test]
    fn require_approval_carries_timeout_policy() {
        use swage_core::ApprovalTimeoutPolicy;
        let cfg = PermissionConfig {
            allow: vec![],
            deny: vec![],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry {
                tool_name: "shell".into(),
                timeout_secs: None,
                timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
            }],
        };
        let result = check_permission(&cfg, "shell").unwrap();
        match result {
            PermissionCheckResult::RequiresApproval { timeout_policy, .. } => {
                assert_eq!(timeout_policy, Some(ApprovalTimeoutPolicy::Interrupt));
            }
            other @ PermissionCheckResult::Allowed => {
                panic!("expected RequiresApproval, got: {other:?}");
            }
        }
    }

    #[test]
    fn deny_overrides_require_approval() {
        // A tool listed in both deny and require_approval is denied — deny wins.
        let cfg = PermissionConfig {
            allow: vec![],
            deny: vec!["shell".into()],
            bundles: HashMap::new(),
            require_approval: vec![RequireApprovalEntry {
                tool_name: "shell".into(),
                timeout_secs: None,
                timeout_policy: None,
            }],
        };
        let err = check_permission(&cfg, "shell").unwrap_err();
        match err {
            ToolError::PermissionDenied { reason, .. } => {
                assert_eq!(reason, PermissionDeniedReason::ExplicitlyDenied);
            }
            other => panic!("expected ExplicitlyDenied, got: {other:?}"),
        }
    }

    #[test]
    fn require_approval_bundle_expands_to_member_tools() {
        // Naming a bundle in require_approval expands to all its member tools.
        let mut bundles = HashMap::new();
        bundles.insert(
            "filesystem".into(),
            vec!["file_read".into(), "file_write".into()],
        );
        let cfg = PermissionConfig {
            allow: vec![],
            deny: vec![],
            bundles,
            require_approval: vec![RequireApprovalEntry {
                tool_name: "filesystem".into(),
                timeout_secs: Some(60),
                timeout_policy: None,
            }],
        };
        let r1 = check_permission(&cfg, "file_read").unwrap();
        let r2 = check_permission(&cfg, "file_write").unwrap();
        assert!(matches!(r1, PermissionCheckResult::RequiresApproval { .. }));
        assert!(matches!(r2, PermissionCheckResult::RequiresApproval { .. }));
        // Tools outside the bundle are still denied.
        assert!(check_permission(&cfg, "shell").is_err());
    }
}
