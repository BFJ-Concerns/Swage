use serde::{Deserialize, Serialize};
use crate::model::{
    ContextConfig, LogLevel, Protocol, ResolvedConfig, ServerConfig, ProviderConfig, ToolConfig,
    WorkspaceConfig, SandboxConfig, LifecycleConfig, LoggingConfig,
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
        // ToolConfig is not yet merged from partial layers — defaults only for now.
        tools: ToolConfig::default(),
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
}
