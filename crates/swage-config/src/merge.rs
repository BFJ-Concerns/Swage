use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use swage_core::ApprovalTimeoutPolicy;
use crate::model::{
    ContextConfig, HitlConfig, LogLevel, ModelPricing, ObservabilityConfig, OutputConfig,
    PricingConfig, Protocol, RequireApprovalEntry, ResolvedConfig, SearchProviderConfig,
    ServerConfig, ProviderConfig, ToolConfig, ToolPermissionConfig, WorkspaceConfig, SandboxConfig,
    LifecycleConfig, LoggingConfig,
};

// ---------------------------------------------------------------------------
// Partial section types — mirror of model types, all fields Option<T>
// ---------------------------------------------------------------------------

/// Partial mirror of `ServerConfig`. Each field is independently overridable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialServerConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
}

/// Partial mirror of `ProviderConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialProviderConfig {
    pub protocol: Option<Protocol>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
}

/// Partial mirror of `WorkspaceConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialWorkspaceConfig {
    pub path: Option<String>,
}

/// Partial mirror of `SandboxConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialSandboxConfig {
    pub enabled: Option<bool>,
}

/// Partial mirror of `LifecycleConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialLifecycleConfig {
    pub idle_timeout: Option<u64>,
}

/// Partial mirror of `LoggingConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialLoggingConfig {
    pub level: Option<LogLevel>,
}

/// Partial mirror of `ContextConfig`.
///
/// `budget` uses a double-option to distinguish "field not present" (outer `None`)
/// from "field explicitly set to None" (`Some(None)`) from "field set to N"
/// (`Some(Some(n))`). The outer option controls inheritance; the inner mirrors
/// the `Option<usize>` in the resolved type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialContextConfig {
    pub assembly: Option<String>,
    pub timeout: Option<u64>,
    pub budget: Option<Option<usize>>,
}

/// Partial mirror of `PricingConfig`.
///
/// When present, model entries from the overlay are merged into the base map
/// (overlay entry wins per model name). Absent base entries survive.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialPricingConfig {
    pub models: Option<HashMap<String, ModelPricing>>,
}

/// Partial mirror of `ObservabilityConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialObservabilityConfig {
    pub force_on: Option<bool>,
}

/// Partial mirror of `ToolConfig` + `ToolPermissionConfig`.
///
/// Scalar fields use overlay-or-base semantics. Permission fields (allow, deny)
/// use replacement semantics: if an overlay specifies `allow`, it replaces the
/// base `allow` entirely rather than appending. Bundles merge map-style (overlay
/// entry wins per key, base entries absent from overlay survive).
///
/// `search` uses a double-option to distinguish "not set" (outer `None`) from
/// "explicitly disabled" (`Some(None)`).
///
/// `require_approval` uses replacement semantics: an overlay that specifies the
/// field replaces the base list entirely (same as allow/deny). This prevents
/// partial overrides that would silently inherit unexpected approval gates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialToolConfig {
    pub shell_timeout_secs: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub search: Option<Option<SearchProviderConfig>>,
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
    pub bundles: Option<HashMap<String, Vec<String>>>,
    pub require_approval: Option<Vec<RequireApprovalEntry>>,
}

/// Partial mirror of `OutputConfig`. Each field is independently overridable.
///
/// `schema` uses `Option<Option<serde_json::Value>>` so that a layer can
/// explicitly set schema to `None` (disabling it) without being treated the
/// same as "not configured". In practice most callers set it to `Some(value)`
/// or leave it as `None` (absent), so the outer option behaves like all other
/// fields: absent means "don't override the base".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialOutputConfig {
    /// JSON Schema to validate agent output against. `None` means "not
    /// configured at this level" — base value wins.
    pub schema: Option<serde_json::Value>,
    /// Override for the maximum number of validation-retry attempts.
    pub max_retries: Option<u32>,
}

/// Partial mirror of `HitlConfig`. Each field is independently overridable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialHitlConfig {
    /// Override for the approval timeout in seconds.
    pub timeout: Option<u64>,
    /// Override for the action taken when an approval request times out.
    pub timeout_policy: Option<ApprovalTimeoutPolicy>,
}

/// Partial mirror of `ContextTransferTargetConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialContextTransferTargetConfig {
    pub mode: Option<crate::model::ContextTransferMode>,
    pub allowed_fields: Option<Vec<String>>,
    pub summary_budget: Option<u32>,
}

/// Partial mirror of `ContextTransferConfig`.
///
/// `mode` uses overlay-or-base semantics. `allowed_fields` uses replacement
/// semantics (overlay list replaces base entirely). `targets` merge map-style
/// (per-target entry from overlay replaces base entry for the same key).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialContextTransferConfig {
    pub mode: Option<crate::model::ContextTransferMode>,
    pub allowed_fields: Option<Vec<String>>,
    pub summary_budget: Option<u32>,
    pub targets: Option<HashMap<String, PartialContextTransferTargetConfig>>,
}

/// Partial mirror of `CommunicationConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialCommunicationConfig {
    pub context_transfer: Option<PartialContextTransferConfig>,
}

/// A single hierarchy level's config contribution — every section and field
/// is optional so a level need only specify what it wants to override.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialConfig {
    pub server: Option<PartialServerConfig>,
    pub provider: Option<PartialProviderConfig>,
    pub workspace: Option<PartialWorkspaceConfig>,
    pub sandbox: Option<PartialSandboxConfig>,
    pub lifecycle: Option<PartialLifecycleConfig>,
    pub logging: Option<PartialLoggingConfig>,
    pub context: Option<PartialContextConfig>,
    pub pricing: Option<PartialPricingConfig>,
    pub observability: Option<PartialObservabilityConfig>,
    pub tools: Option<PartialToolConfig>,
    pub output: Option<PartialOutputConfig>,
    pub hitl: Option<PartialHitlConfig>,
    pub communication: Option<PartialCommunicationConfig>,
}

// ---------------------------------------------------------------------------
// Merge helpers
// ---------------------------------------------------------------------------

/// Merge two optional sections. If both are present, field-level merge is
/// performed with `merge_fn`. If only one is present, it passes through.
fn merge_section<S>(
    base: Option<S>,
    overlay: Option<S>,
    merge_fn: impl Fn(S, S) -> S,
) -> Option<S> {
    match (base, overlay) {
        (Some(b), Some(o)) => Some(merge_fn(b, o)),
        (Some(b), None) => Some(b),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Merge two `PartialConfig` values with child-wins semantics.
///
/// Sections are merged independently — an overlay that sets `[provider]`
/// does not erase a base `[server]`. Within a section, each field is merged
/// independently: the overlay value wins when present, otherwise the base
/// value is kept.
pub fn merge_partial(base: PartialConfig, overlay: PartialConfig) -> PartialConfig {
    PartialConfig {
        server: merge_section(base.server, overlay.server, |b, o| PartialServerConfig {
            host: o.host.or(b.host),
            port: o.port.or(b.port),
        }),
        provider: merge_section(base.provider, overlay.provider, |b, o| PartialProviderConfig {
            protocol: o.protocol.or(b.protocol),
            model: o.model.or(b.model),
            api_key_env: o.api_key_env.or(b.api_key_env),
        }),
        workspace: merge_section(base.workspace, overlay.workspace, |b, o| PartialWorkspaceConfig {
            path: o.path.or(b.path),
        }),
        sandbox: merge_section(base.sandbox, overlay.sandbox, |b, o| PartialSandboxConfig {
            enabled: o.enabled.or(b.enabled),
        }),
        lifecycle: merge_section(base.lifecycle, overlay.lifecycle, |b, o| PartialLifecycleConfig {
            idle_timeout: o.idle_timeout.or(b.idle_timeout),
        }),
        logging: merge_section(base.logging, overlay.logging, |b, o| PartialLoggingConfig {
            level: o.level.or(b.level),
        }),
        context: merge_section(base.context, overlay.context, |b, o| PartialContextConfig {
            assembly: o.assembly.or(b.assembly),
            timeout: o.timeout.or(b.timeout),
            budget: o.budget.or(b.budget),
        }),
        pricing: merge_section(base.pricing, overlay.pricing, |b, o| {
            // Overlay model entries win; base entries absent from the overlay survive.
            let mut merged = b.models.unwrap_or_default();
            if let Some(overlay_models) = o.models {
                merged.extend(overlay_models);
            }
            PartialPricingConfig { models: Some(merged) }
        }),
        observability: merge_section(
            base.observability,
            overlay.observability,
            |b, o| PartialObservabilityConfig {
                force_on: o.force_on.or(b.force_on),
            },
        ),
        tools: merge_section(base.tools, overlay.tools, |b, o| {
            // Scalar fields: overlay wins when present, otherwise base.
            // Permission fields (allow, deny): overlay REPLACES base entirely.
            // Bundles: map-style merge — overlay entry wins per key, base entries survive.
            let bundles = match (b.bundles, o.bundles) {
                (Some(mut base_b), Some(overlay_b)) => {
                    base_b.extend(overlay_b);
                    Some(base_b)
                }
                (base_b, overlay_b) => overlay_b.or(base_b),
            };
            PartialToolConfig {
                shell_timeout_secs: o.shell_timeout_secs.or(b.shell_timeout_secs),
                max_output_bytes: o.max_output_bytes.or(b.max_output_bytes),
                search: o.search.or(b.search),
                allow: o.allow.or(b.allow),
                deny: o.deny.or(b.deny),
                bundles,
                // Replacement semantics: overlay replaces base list entirely.
                require_approval: o.require_approval.or(b.require_approval),
            }
        }),
        output: merge_section(base.output, overlay.output, |b, o| PartialOutputConfig {
            // Both fields use overlay-wins semantics: if the overlay has a value,
            // it wins; otherwise the base value is kept.
            schema: o.schema.or(b.schema),
            max_retries: o.max_retries.or(b.max_retries),
        }),
        hitl: merge_section(base.hitl, overlay.hitl, |b, o| PartialHitlConfig {
            timeout: o.timeout.or(b.timeout),
            timeout_policy: o.timeout_policy.or(b.timeout_policy),
        }),
        communication: merge_section(
            base.communication,
            overlay.communication,
            |b, o| {
                let ct = merge_section(
                    b.context_transfer,
                    o.context_transfer,
                    |bt, ot| {
                        // Targets merge map-style: overlay entry replaces base entry
                        // for the same key; base entries absent from overlay survive.
                        let targets = match (bt.targets, ot.targets) {
                            (Some(mut base_t), Some(overlay_t)) => {
                                base_t.extend(overlay_t);
                                Some(base_t)
                            }
                            (base_t, overlay_t) => overlay_t.or(base_t),
                        };
                        PartialContextTransferConfig {
                            mode: ot.mode.or(bt.mode),
                            // Replacement semantics — overlay list replaces base.
                            allowed_fields: ot.allowed_fields.or(bt.allowed_fields),
                            summary_budget: ot.summary_budget.or(bt.summary_budget),
                            targets,
                        }
                    },
                );
                PartialCommunicationConfig { context_transfer: ct }
            },
        ),
    }
}

/// Fold a chain of `PartialConfig` values left-to-right, with each later
/// entry winning over earlier ones.
///
/// An empty chain yields an empty `PartialConfig` (all defaults will be
/// applied by `resolve`).
pub fn merge_chain(configs: Vec<PartialConfig>) -> PartialConfig {
    configs.into_iter().fold(PartialConfig::default(), merge_partial)
}

/// Resolve a `PartialConfig` into a `ResolvedConfig` by filling any absent
/// fields with their default values.
pub fn resolve(partial: PartialConfig) -> ResolvedConfig {
    let ds = ServerConfig::default();
    let dp = ProviderConfig::default();
    let dw = WorkspaceConfig::default();
    let dsb = SandboxConfig::default();
    let dl = LifecycleConfig::default();
    let dlg = LoggingConfig::default();

    let s = partial.server.unwrap_or_default();
    let p = partial.provider.unwrap_or_default();
    let w = partial.workspace.unwrap_or_default();
    let sb = partial.sandbox.unwrap_or_default();
    let l = partial.lifecycle.unwrap_or_default();
    let lg = partial.logging.unwrap_or_default();

    ResolvedConfig {
        server: ServerConfig {
            host: s.host.unwrap_or(ds.host),
            port: s.port.unwrap_or(ds.port),
        },
        hitl: {
            let dh = HitlConfig::default();
            let h = partial.hitl.unwrap_or_default();
            HitlConfig {
                timeout: h.timeout.unwrap_or(dh.timeout),
                timeout_policy: h.timeout_policy.unwrap_or(dh.timeout_policy),
            }
        },
        provider: ProviderConfig {
            protocol: p.protocol.unwrap_or(dp.protocol),
            model: p.model.unwrap_or(dp.model),
            api_key_env: p.api_key_env.unwrap_or(dp.api_key_env),
        },
        workspace: WorkspaceConfig {
            path: w.path.unwrap_or(dw.path),
        },
        sandbox: SandboxConfig {
            enabled: sb.enabled.unwrap_or(dsb.enabled),
        },
        lifecycle: LifecycleConfig {
            idle_timeout: l.idle_timeout.unwrap_or(dl.idle_timeout),
        },
        logging: LoggingConfig {
            level: lg.level.unwrap_or(dlg.level),
        },
        tools: {
            let dt = ToolConfig::default();
            let t = partial.tools.unwrap_or_default();
            let dp = ToolPermissionConfig::default();
            ToolConfig {
                shell_timeout_secs: t.shell_timeout_secs.unwrap_or(dt.shell_timeout_secs),
                max_output_bytes: t.max_output_bytes.unwrap_or(dt.max_output_bytes),
                // Double-option: outer None → default (None), Some(x) → use x.
                search: t.search.unwrap_or(dt.search),
                permissions: ToolPermissionConfig {
                    allow: t.allow.unwrap_or(dp.allow),
                    deny: t.deny.unwrap_or(dp.deny),
                    bundles: t.bundles.unwrap_or(dp.bundles),
                    // Replacement semantics: if the partial config specifies
                    // require_approval, it replaces the default entirely.
                    require_approval: t.require_approval.unwrap_or(dp.require_approval),
                },
            }
        },
        context: {
            let dc = ContextConfig::default();
            let c = partial.context.unwrap_or_default();
            ContextConfig {
                assembly: c.assembly.unwrap_or(dc.assembly),
                timeout: c.timeout.unwrap_or(dc.timeout),
                // Double-option: outer None → inherit default (None), Some(x) → use x.
                budget: c.budget.unwrap_or(dc.budget),
            }
        },
        pricing: {
            let models = partial.pricing
                .and_then(|p| p.models)
                .unwrap_or_default();
            PricingConfig { models }
        },
        observability: {
            let do_ = ObservabilityConfig::default();
            let o = partial.observability.unwrap_or_default();
            ObservabilityConfig {
                force_on: o.force_on.unwrap_or(do_.force_on),
            }
        },
        output: {
            let do_ = OutputConfig::default();
            let o = partial.output.unwrap_or_default();
            OutputConfig {
                schema: o.schema.or(do_.schema),
                max_retries: o.max_retries.unwrap_or(do_.max_retries),
            }
        },
        communication: {
            use crate::model::{
                CommunicationConfig, ContextTransferConfig, ContextTransferTargetConfig,
            };
            let c = partial.communication.unwrap_or_default();
            let ct = c.context_transfer.unwrap_or_default();
            let dc = ContextTransferConfig::default();

            // Resolve top-level defaults first — per-target overrides inherit
            // from the resolved policy, not from T::default().
            let resolved_mode = ct.mode.unwrap_or(dc.mode);
            let resolved_fields = ct.allowed_fields.unwrap_or(dc.allowed_fields);
            let resolved_budget = ct.summary_budget.or(dc.summary_budget);

            let targets = ct.targets.unwrap_or_default().into_iter().map(|(k, v)| {
                (k, ContextTransferTargetConfig {
                    mode: v.mode.unwrap_or(resolved_mode.clone()),
                    allowed_fields: v.allowed_fields.unwrap_or_else(|| resolved_fields.clone()),
                    summary_budget: v.summary_budget.or(resolved_budget),
                })
            }).collect();

            CommunicationConfig {
                context_transfer: ContextTransferConfig {
                    mode: resolved_mode,
                    allowed_fields: resolved_fields,
                    summary_budget: resolved_budget,
                    targets,
                },
            }
        },
        // Instructions are assembled separately by the instructions module
        instructions: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn partial_with_server_host(host: &str) -> PartialConfig {
        PartialConfig {
            server: Some(PartialServerConfig {
                host: Some(host.to_string()),
                port: None,
            }),
            ..Default::default()
        }
    }

    fn partial_with_server_port(port: u16) -> PartialConfig {
        PartialConfig {
            server: Some(PartialServerConfig {
                host: None,
                port: Some(port),
            }),
            ..Default::default()
        }
    }

    fn partial_with_provider_model(model: &str) -> PartialConfig {
        PartialConfig {
            provider: Some(PartialProviderConfig {
                model: Some(model.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn overlay_wins_on_collision() {
        let base = partial_with_server_host("base-host");
        let overlay = partial_with_server_host("overlay-host");
        let merged = merge_partial(base, overlay);
        assert_eq!(merged.server.unwrap().host.unwrap(), "overlay-host");
    }

    #[test]
    fn base_fields_survive_when_overlay_does_not_set_them() {
        // base sets server.host, overlay sets server.port — both should survive
        let base = partial_with_server_host("127.0.0.1");
        let overlay = partial_with_server_port(4000);
        let merged = merge_partial(base, overlay);
        let server = merged.server.expect("server section should exist");
        assert_eq!(server.host.as_deref(), Some("127.0.0.1"), "host from base should survive");
        assert_eq!(server.port, Some(4000), "port from overlay should be present");
    }

    #[test]
    fn sections_are_independent() {
        // base has server section, overlay only has provider — base server must survive
        let base = PartialConfig {
            server: Some(PartialServerConfig {
                host: Some("10.0.0.1".to_string()),
                port: Some(3000),
            }),
            ..Default::default()
        };
        let overlay = partial_with_provider_model("gpt-4o");
        let merged = merge_partial(base, overlay);

        // Server from base must survive even though overlay touched only provider
        let server = merged.server.expect("server from base should survive");
        assert_eq!(server.host.as_deref(), Some("10.0.0.1"));

        // Provider from overlay should be present
        let provider = merged.provider.expect("provider from overlay should be present");
        assert_eq!(provider.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn three_level_chain_respects_priority() {
        // global: host=10.0.0.1, port=3000
        // project: model=claude-opus
        // agent: port=4000
        let global = PartialConfig {
            server: Some(PartialServerConfig {
                host: Some("10.0.0.1".to_string()),
                port: Some(3000),
            }),
            ..Default::default()
        };
        let project = partial_with_provider_model("claude-opus");
        let agent = partial_with_server_port(4000);

        let merged = merge_chain(vec![global, project, agent]);

        // Agent overrides global's port
        let server = merged.server.expect("server should exist");
        assert_eq!(server.host.as_deref(), Some("10.0.0.1"), "host from global survives");
        assert_eq!(server.port, Some(4000), "agent overrides global port");

        // Project's model survives
        let provider = merged.provider.expect("provider should exist");
        assert_eq!(provider.model.as_deref(), Some("claude-opus"), "project model survives");
    }

    #[test]
    fn absent_intermediate_level_passes_through() {
        // global: port=5000, empty middle, agent: model=foo
        let global = partial_with_server_port(5000);
        let middle = PartialConfig::default(); // nothing
        let agent = partial_with_provider_model("foo");

        let merged = merge_chain(vec![global, middle, agent]);

        let server = merged.server.expect("server from global should survive");
        assert_eq!(server.port, Some(5000));
        let provider = merged.provider.expect("provider from agent should be present");
        assert_eq!(provider.model.as_deref(), Some("foo"));
    }

    #[test]
    fn resolve_uses_partial_values_over_defaults() {
        let partial = PartialConfig {
            server: Some(PartialServerConfig {
                host: Some("192.168.1.1".to_string()),
                port: Some(8080),
            }),
            ..Default::default()
        };
        let resolved = resolve(partial);
        assert_eq!(resolved.server.host, "192.168.1.1", "should use partial host");
        assert_eq!(resolved.server.port, 8080, "should use partial port");
    }

    #[test]
    fn resolve_fills_absent_fields_with_defaults() {
        let partial = PartialConfig::default(); // all None
        let resolved = resolve(partial);
        let default = ResolvedConfig::default();
        assert_eq!(resolved.server.host, default.server.host);
        assert_eq!(resolved.server.port, default.server.port);
        assert_eq!(resolved.provider.model, default.provider.model);
        assert_eq!(resolved.sandbox.enabled, default.sandbox.enabled);
    }

    #[test]
    fn resolve_partial_section_fills_gaps_from_defaults() {
        // Only port is set; host should come from defaults
        let partial = PartialConfig {
            server: Some(PartialServerConfig {
                host: None,
                port: Some(9000),
            }),
            ..Default::default()
        };
        let resolved = resolve(partial);
        // host absent in partial → default host
        assert_eq!(resolved.server.host, ServerConfig::default().host);
        // port from partial
        assert_eq!(resolved.server.port, 9000);
    }

    // --- PartialContextConfig tests ---

    fn partial_with_context_assembly(assembly: &str) -> PartialConfig {
        PartialConfig {
            context: Some(PartialContextConfig {
                assembly: Some(assembly.to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn context_overlay_wins_over_base() {
        let base = partial_with_context_assembly("base-assembly");
        let overlay = partial_with_context_assembly("overlay-assembly");
        let merged = merge_partial(base, overlay);
        assert_eq!(
            merged.context.unwrap().assembly.as_deref(),
            Some("overlay-assembly"),
        );
    }

    #[test]
    fn context_fields_are_independent() {
        // base sets assembly, overlay only sets timeout — both should survive
        let base = partial_with_context_assembly("base-assembly");
        let overlay = PartialConfig {
            context: Some(PartialContextConfig {
                timeout: Some(30),
                ..Default::default()
            }),
            ..Default::default()
        };
        let merged = merge_partial(base, overlay);
        let ctx = merged.context.unwrap();
        assert_eq!(ctx.assembly.as_deref(), Some("base-assembly"), "assembly from base survives");
        assert_eq!(ctx.timeout, Some(30), "timeout from overlay present");
    }

    #[test]
    fn context_missing_section_passes_through() {
        // base has context, overlay does not — base context should survive
        let base = partial_with_context_assembly("base-assembly");
        let overlay = PartialConfig::default();
        let merged = merge_partial(base, overlay);
        assert_eq!(
            merged.context.unwrap().assembly.as_deref(),
            Some("base-assembly"),
        );
    }

    #[test]
    fn resolve_context_uses_partial_values() {
        let partial = PartialConfig {
            context: Some(PartialContextConfig {
                assembly: Some("custom".to_string()),
                timeout: Some(15),
                budget: Some(Some(8192)),
            }),
            ..Default::default()
        };
        let resolved = resolve(partial);
        assert_eq!(resolved.context.assembly, "custom");
        assert_eq!(resolved.context.timeout, 15);
        assert_eq!(resolved.context.budget, Some(8192));
    }

    #[test]
    fn resolve_context_uses_defaults_when_absent() {
        let resolved = resolve(PartialConfig::default());
        let default = ContextConfig::default();
        assert_eq!(resolved.context.assembly, default.assembly);
        assert_eq!(resolved.context.timeout, default.timeout);
        assert_eq!(resolved.context.budget, default.budget);
    }

    #[test]
    fn resolve_context_budget_explicit_none() {
        // budget: Some(None) means "explicitly unset" — should resolve to None
        let partial = PartialConfig {
            context: Some(PartialContextConfig {
                budget: Some(None),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(partial);
        assert_eq!(resolved.context.budget, None);
    }

    // --- PartialToolConfig / permission cascade tests ---

    fn partial_with_tool_deny(deny: Vec<&str>) -> PartialConfig {
        PartialConfig {
            tools: Some(PartialToolConfig {
                deny: Some(deny.into_iter().map(String::from).collect()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn partial_with_tool_allow(allow: Vec<&str>) -> PartialConfig {
        PartialConfig {
            tools: Some(PartialToolConfig {
                allow: Some(allow.into_iter().map(String::from).collect()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn tool_permission_default_resolves_to_allow_all() {
        let resolved = resolve(PartialConfig::default());
        assert_eq!(resolved.tools.permissions.allow, vec!["*".to_string()]);
        assert!(resolved.tools.permissions.deny.is_empty());
        assert!(resolved.tools.permissions.bundles.is_empty());
    }

    #[test]
    fn tool_permission_agent_allow_overrides_global_deny() {
        // Global denies "shell", agent allows "read_file" — agent's allow replaces.
        let global = partial_with_tool_deny(vec!["shell"]);
        let agent = partial_with_tool_allow(vec!["read_file"]);

        let merged = merge_chain(vec![global, agent]);
        let resolved = resolve(merged);

        assert_eq!(resolved.tools.permissions.allow, vec!["read_file".to_string()]);
        // Deny from global survives because agent didn't specify deny.
        assert_eq!(resolved.tools.permissions.deny, vec!["shell".to_string()]);
    }

    #[test]
    fn tool_permission_overlay_deny_replaces_base_deny() {
        let global = partial_with_tool_deny(vec!["shell", "file_write"]);
        let agent = partial_with_tool_deny(vec!["web_search"]);

        let merged = merge_chain(vec![global, agent]);
        let resolved = resolve(merged);

        // Agent's deny completely replaces global's deny.
        assert_eq!(resolved.tools.permissions.deny, vec!["web_search".to_string()]);
    }

    #[test]
    fn tool_permission_bundles_merge_map_style() {
        let mut global_bundles = HashMap::new();
        global_bundles.insert("read_only".to_string(), vec!["file_read".into()]);
        global_bundles.insert("write".to_string(), vec!["file_write".into()]);

        let mut agent_bundles = HashMap::new();
        // Override the "read_only" bundle, leave "write" from global.
        agent_bundles.insert("read_only".to_string(), vec!["file_read".into(), "web_search".into()]);

        let global = PartialConfig {
            tools: Some(PartialToolConfig {
                bundles: Some(global_bundles),
                ..Default::default()
            }),
            ..Default::default()
        };
        let agent = PartialConfig {
            tools: Some(PartialToolConfig {
                bundles: Some(agent_bundles),
                ..Default::default()
            }),
            ..Default::default()
        };

        let merged = merge_chain(vec![global, agent]);
        let resolved = resolve(merged);

        // "read_only" overridden by agent.
        assert_eq!(
            resolved.tools.permissions.bundles["read_only"],
            vec!["file_read".to_string(), "web_search".to_string()],
        );
        // "write" survives from global.
        assert_eq!(
            resolved.tools.permissions.bundles["write"],
            vec!["file_write".to_string()],
        );
    }

    #[test]
    fn tool_config_scalar_fields_merge_overlay_wins() {
        let global = PartialConfig {
            tools: Some(PartialToolConfig {
                shell_timeout_secs: Some(60),
                max_output_bytes: Some(200_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        let agent = PartialConfig {
            tools: Some(PartialToolConfig {
                shell_timeout_secs: Some(120),
                ..Default::default()
            }),
            ..Default::default()
        };

        let merged = merge_chain(vec![global, agent]);
        let resolved = resolve(merged);

        assert_eq!(resolved.tools.shell_timeout_secs, 120, "agent overrides global");
        assert_eq!(resolved.tools.max_output_bytes, 200_000, "global survives when agent absent");
    }

    #[test]
    fn tool_config_absent_resolves_to_defaults() {
        let resolved = resolve(PartialConfig::default());
        assert_eq!(resolved.tools.shell_timeout_secs, 30);
        assert_eq!(resolved.tools.max_output_bytes, 100_000);
        assert_eq!(resolved.tools.search, None);
    }

    // --- PartialOutputConfig / cascade merge tests ---

    #[test]
    fn output_config_absent_resolves_to_defaults() {
        // Empty partial → defaults: schema=None, max_retries=3.
        let resolved = resolve(PartialConfig::default());
        assert!(resolved.output.schema.is_none());
        assert_eq!(resolved.output.max_retries, 3);
    }

    #[test]
    fn output_config_overlay_max_retries_overrides_base() {
        let global = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: None,
                max_retries: Some(5),
            }),
            ..Default::default()
        };
        let agent = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: None,
                max_retries: Some(1),
            }),
            ..Default::default()
        };

        let merged = merge_chain(vec![global, agent]);
        let resolved = resolve(merged);
        // The mote success criterion.
        assert_eq!(resolved.output.max_retries, 1);
    }

    #[test]
    fn output_config_absent_schema_resolves_to_none() {
        let global = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: None,
                max_retries: Some(2),
            }),
            ..Default::default()
        };
        let resolved = resolve(merge_chain(vec![global]));
        assert!(resolved.output.schema.is_none());
        assert_eq!(resolved.output.max_retries, 2);
    }

    #[test]
    fn output_config_three_level_chain_agent_schema_wins() {
        let global = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: Some(serde_json::json!({"type": "string"})),
                max_retries: Some(5),
            }),
            ..Default::default()
        };
        let project = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: None,
                max_retries: Some(2),
            }),
            ..Default::default()
        };
        let agent = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: Some(serde_json::json!({"type": "object"})),
                max_retries: Some(1),
            }),
            ..Default::default()
        };

        let resolved = resolve(merge_chain(vec![global, project, agent]));
        // Agent's values win at every level.
        assert_eq!(resolved.output.schema, Some(serde_json::json!({"type": "object"})));
        assert_eq!(resolved.output.max_retries, 1);
    }

    // -----------------------------------------------------------------------
    // HITL config merge tests [tz3w]
    // -----------------------------------------------------------------------

    #[test]
    fn hitl_overlay_timeout_wins_over_base() {
        use swage_core::ApprovalTimeoutPolicy;
        let base = PartialConfig {
            hitl: Some(PartialHitlConfig {
                timeout: Some(60),
                timeout_policy: Some(ApprovalTimeoutPolicy::Reject),
            }),
            ..Default::default()
        };
        let overlay = PartialConfig {
            hitl: Some(PartialHitlConfig {
                timeout: Some(120),
                timeout_policy: None,
            }),
            ..Default::default()
        };
        let merged = merge_partial(base, overlay);
        let hitl = merged.hitl.unwrap();
        assert_eq!(hitl.timeout, Some(120), "overlay timeout should win");
        assert_eq!(hitl.timeout_policy, Some(ApprovalTimeoutPolicy::Reject), "base policy should survive");
    }

    #[test]
    fn hitl_three_level_chain() {
        use swage_core::ApprovalTimeoutPolicy;
        let global = PartialConfig {
            hitl: Some(PartialHitlConfig {
                timeout: Some(300),
                timeout_policy: Some(ApprovalTimeoutPolicy::Reject),
            }),
            ..Default::default()
        };
        let project = PartialConfig {
            hitl: Some(PartialHitlConfig {
                timeout: Some(600),
                timeout_policy: None,
            }),
            ..Default::default()
        };
        let agent = PartialConfig {
            hitl: Some(PartialHitlConfig {
                timeout: None,
                timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
            }),
            ..Default::default()
        };
        let resolved = resolve(merge_chain(vec![global, project, agent]));
        assert_eq!(resolved.hitl.timeout, 600, "project timeout wins over global");
        assert_eq!(resolved.hitl.timeout_policy, ApprovalTimeoutPolicy::Interrupt, "agent policy wins");
    }

    #[test]
    fn hitl_resolve_defaults_when_absent() {
        use swage_core::ApprovalTimeoutPolicy;
        let resolved = resolve(PartialConfig::default());
        assert_eq!(resolved.hitl.timeout, 300, "default HITL timeout is 300");
        assert_eq!(resolved.hitl.timeout_policy, ApprovalTimeoutPolicy::Reject, "default policy is Reject");
    }

    #[test]
    fn require_approval_overlay_replaces_base() {
        use crate::model::RequireApprovalEntry;
        let base = PartialConfig {
            tools: Some(PartialToolConfig {
                require_approval: Some(vec![RequireApprovalEntry::Simple("write_file".into())]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let overlay = PartialConfig {
            tools: Some(PartialToolConfig {
                require_approval: Some(vec![
                    RequireApprovalEntry::Simple("shell_exec".into()),
                    RequireApprovalEntry::Simple("execute_command".into()),
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let merged = merge_partial(base, overlay);
        let ra = merged.tools.unwrap().require_approval.unwrap();
        // Overlay replaces base entirely — only overlay entries present.
        assert_eq!(ra.len(), 2);
        assert_eq!(ra[0].tool_name(), "shell_exec");
        assert_eq!(ra[1].tool_name(), "execute_command");
    }

    #[test]
    fn require_approval_base_survives_when_overlay_absent() {
        use crate::model::RequireApprovalEntry;
        let base = PartialConfig {
            tools: Some(PartialToolConfig {
                require_approval: Some(vec![RequireApprovalEntry::Simple("write_file".into())]),
                ..Default::default()
            }),
            ..Default::default()
        };
        // Overlay has tools section but no require_approval field.
        let overlay = PartialConfig {
            tools: Some(PartialToolConfig {
                shell_timeout_secs: Some(60),
                ..Default::default()
            }),
            ..Default::default()
        };
        let merged = merge_partial(base, overlay);
        let tools = merged.tools.unwrap();
        assert_eq!(tools.shell_timeout_secs, Some(60), "overlay scalar wins");
        let ra = tools.require_approval.unwrap();
        assert_eq!(ra[0].tool_name(), "write_file", "base require_approval survives");
    }

    #[test]
    fn context_transfer_per_target_inherits_resolved_defaults() {
        // Top-level sets mode=Summary with a budget; per-target entry only
        // overrides allowed_fields. The target should inherit Summary mode
        // and the budget — not fall back to None/empty.
        let partial = PartialConfig {
            communication: Some(PartialCommunicationConfig {
                context_transfer: Some(PartialContextTransferConfig {
                    mode: Some(crate::model::ContextTransferMode::Summary),
                    allowed_fields: Some(vec!["system".into(), "tools".into()]),
                    summary_budget: Some(1024),
                    targets: Some({
                        let mut m = std::collections::HashMap::new();
                        m.insert("agent-b".into(), PartialContextTransferTargetConfig {
                            mode: None, // should inherit Summary
                            allowed_fields: Some(vec!["system".into()]), // overrides
                            summary_budget: None, // should inherit 1024
                        });
                        m
                    }),
                }),
            }),
            ..Default::default()
        };
        let resolved = resolve(partial);
        let ct = &resolved.communication.context_transfer;

        // Top-level resolved correctly.
        assert_eq!(ct.mode, crate::model::ContextTransferMode::Summary);
        assert_eq!(ct.summary_budget, Some(1024));

        // Per-target inherits from resolved top-level, not from T::default().
        let target = ct.targets.get("agent-b").expect("target should exist");
        assert_eq!(
            target.mode,
            crate::model::ContextTransferMode::Summary,
            "per-target mode should inherit from resolved top-level"
        );
        assert_eq!(
            target.allowed_fields,
            vec!["system".to_string()],
            "per-target allowed_fields should be the override value"
        );
        assert_eq!(
            target.summary_budget,
            Some(1024),
            "per-target budget should inherit from resolved top-level"
        );
    }

    #[test]
    fn require_approval_resolved_from_partial() {
        use crate::model::RequireApprovalEntry;
        let partial = PartialConfig {
            tools: Some(PartialToolConfig {
                require_approval: Some(vec![RequireApprovalEntry::Simple("shell_exec".into())]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(partial);
        assert_eq!(resolved.tools.permissions.require_approval.len(), 1);
        assert_eq!(resolved.tools.permissions.require_approval[0].tool_name(), "shell_exec");
    }
}
