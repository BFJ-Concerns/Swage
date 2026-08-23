#[cfg(test)]
use crate::model::Protocol;
use crate::model::{
    BrowserScreenshotPolicy, BrowserToolConfig, ChannelsConfig, ContextConfig,
    ExperimentLocationConfig, ExperimentationConfig, GenerationConfig, GenerationPresetConfig,
    HitlConfig, LogLevel, LoggingConfig, ModelCapabilitiesConfig, ModelCatalogConfig,
    ModelEndpointConfig, ModelPricing, ObservabilityConfig, OutputConfig, PricingConfig,
    ProviderCatalogConfig, ProviderEndpointConfig, PythonExecuteToolConfig, PythonRuntimePolicy,
    RequireApprovalEntry, ResolvedConfig, SandboxConfig, SchedulingConfig, SearchProviderConfig,
    ServerConfig, TelegramChannelConfig, ToolConfig, ToolPermissionConfig, WorkspaceConfig,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use swage_core::ApprovalTimeoutPolicy;

// ---------------------------------------------------------------------------
// Partial section types — mirror of model types, all fields Option<T>
// ---------------------------------------------------------------------------

/// Partial mirror of `ServerConfig`. Each field is independently overridable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialServerConfig {
    pub host: Option<String>,
    pub port: Option<u16>,
}

/// Partial mirror of `ProviderCatalogConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialProviderCatalogConfig {
    pub default: Option<String>,
    pub entries: Option<HashMap<String, ProviderEndpointConfig>>,
}

/// Partial mirror of `ModelCatalogConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialModelCatalogConfig {
    pub default: Option<String>,
    pub default_model_id: Option<String>,
    pub entries: Option<HashMap<String, ModelEndpointConfig>>,
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
    pub allow_degraded_execution: Option<bool>,
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

/// Partial mirror of `GenerationConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialGenerationConfig {
    pub presets: Option<HashMap<String, GenerationPresetConfig>>,
}

/// Partial mirror of `ChannelsConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialChannelsConfig {
    pub telegram: Option<PartialTelegramChannelConfig>,
}

/// Partial mirror of `TelegramChannelConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialTelegramChannelConfig {
    pub bot_token_secret_ref: Option<Option<String>>,
    pub bot_token_env: Option<Option<String>>,
    pub api_base_url: Option<String>,
    pub poll_timeout_secs: Option<u64>,
}

/// Partial mirror of `ObservabilityConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialObservabilityConfig {
    pub force_on: Option<bool>,
}

/// Partial mirror of `ExperimentationConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialExperimentationConfig {
    pub enabled: Option<bool>,
    pub locations: Option<HashMap<String, PartialExperimentLocationConfig>>,
}

/// Partial mirror of `ExperimentLocationConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialExperimentLocationConfig {
    pub enabled: Option<bool>,
    pub forced_variant: Option<Option<String>>,
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
    pub python_execute: Option<PartialPythonExecuteToolConfig>,
    pub max_output_bytes: Option<usize>,
    pub plugin_directory: Option<Option<String>>,
    pub search: Option<Option<SearchProviderConfig>>,
    pub browser: Option<PartialBrowserToolConfig>,
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
    pub bundles: Option<HashMap<String, Vec<String>>>,
    pub require_approval: Option<Vec<RequireApprovalEntry>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialBrowserToolConfig {
    pub binary_path: Option<Option<String>>,
    pub page_idle_secs: Option<u64>,
    pub process_idle_secs: Option<u64>,
    pub max_snapshot_bytes: Option<usize>,
    pub screenshots: Option<BrowserScreenshotPolicy>,
    pub allow_hosts: Option<Vec<String>>,
}

/// Partial mirror of `PythonExecuteToolConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialPythonExecuteToolConfig {
    pub enabled: Option<bool>,
    pub timeout_secs: Option<u64>,
    pub startup_timeout_secs: Option<u64>,
    pub idle_ttl_secs: Option<u64>,
    pub warm_target: Option<usize>,
    pub max_workers: Option<usize>,
    pub runtime_policy: Option<PythonRuntimePolicy>,
    pub runtime_path: Option<Option<String>>,
    pub require_sandbox: Option<bool>,
}

/// Partial mirror of `OutputConfig`. Each field is independently overridable.
///
/// `schema` uses a double-option to distinguish "field not present" (outer
/// `None`) from "field explicitly cleared" (`Some(None)`) from "field set to a
/// schema" (`Some(Some(value))`). The outer option controls inheritance; the
/// inner mirrors the `Option<Value>` in the resolved type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialOutputConfig {
    /// JSON Schema to validate agent output against. Outer `None` means "not
    /// configured at this level"; `Some(None)` actively clears an inherited
    /// schema.
    pub schema: Option<Option<serde_json::Value>>,
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
    pub allowed_fields: Option<Vec<crate::model::ContextTransferField>>,
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
    pub allowed_fields: Option<Vec<crate::model::ContextTransferField>>,
    pub summary_budget: Option<u32>,
    pub targets: Option<HashMap<String, PartialContextTransferTargetConfig>>,
}

/// Partial mirror of `CommunicationConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialCommunicationConfig {
    pub context_transfer: Option<PartialContextTransferConfig>,
    pub message_ttl_secs: Option<u64>,
    pub expiry_interval_secs: Option<u64>,
}

/// Partial mirror of `SchedulingConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialSchedulingConfig {
    pub run_history_max_age_days: Option<u64>,
    pub run_history_max_per_schedule: Option<u32>,
}

/// A single hierarchy level's config contribution — every section and field
/// is optional so a level need only specify what it wants to override.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialConfig {
    pub server: Option<PartialServerConfig>,
    pub providers: Option<PartialProviderCatalogConfig>,
    pub models: Option<PartialModelCatalogConfig>,
    pub workspace: Option<PartialWorkspaceConfig>,
    pub sandbox: Option<PartialSandboxConfig>,
    pub logging: Option<PartialLoggingConfig>,
    pub context: Option<PartialContextConfig>,
    pub pricing: Option<PartialPricingConfig>,
    pub observability: Option<PartialObservabilityConfig>,
    pub experimentation: Option<PartialExperimentationConfig>,
    pub tools: Option<PartialToolConfig>,
    pub output: Option<PartialOutputConfig>,
    pub hitl: Option<PartialHitlConfig>,
    pub communication: Option<PartialCommunicationConfig>,
    pub scheduling: Option<PartialSchedulingConfig>,
    pub generation: Option<PartialGenerationConfig>,
    pub channels: Option<PartialChannelsConfig>,
    /// Top-level bypass for the permission and approval system.
    /// Mirrors `ResolvedConfig::dangerously_skip_permissions`.
    #[serde(default)]
    pub dangerously_skip_permissions: Option<bool>,
}

// ---------------------------------------------------------------------------
// ResolvedConfig → PartialConfig conversion (for overlay merging)
// ---------------------------------------------------------------------------

impl From<ResolvedConfig> for PartialConfig {
    fn from(r: ResolvedConfig) -> Self {
        PartialConfig {
            server: Some(PartialServerConfig {
                host: Some(r.server.host),
                port: Some(r.server.port),
            }),
            providers: Some(PartialProviderCatalogConfig {
                default: Some(r.providers.default),
                entries: if r.providers.entries.is_empty() {
                    None
                } else {
                    Some(r.providers.entries)
                },
            }),
            models: Some(PartialModelCatalogConfig {
                default: Some(r.models.default),
                default_model_id: None,
                entries: if r.models.entries.is_empty() {
                    None
                } else {
                    Some(r.models.entries)
                },
            }),
            workspace: Some(PartialWorkspaceConfig {
                path: Some(r.workspace.path),
            }),
            sandbox: Some(PartialSandboxConfig {
                enabled: Some(r.sandbox.enabled),
                allow_degraded_execution: Some(r.sandbox.allow_degraded_execution),
            }),
            logging: Some(PartialLoggingConfig {
                level: Some(r.logging.level),
            }),
            context: Some(PartialContextConfig {
                assembly: Some(r.context.assembly),
                timeout: Some(r.context.timeout),
                budget: Some(r.context.budget),
            }),
            pricing: Some(PartialPricingConfig {
                models: if r.pricing.models.is_empty() {
                    None
                } else {
                    Some(r.pricing.models)
                },
            }),
            observability: Some(PartialObservabilityConfig {
                force_on: Some(r.observability.force_on),
            }),
            experimentation: Some(PartialExperimentationConfig {
                enabled: Some(r.experimentation.enabled),
                locations: if r.experimentation.locations.is_empty() {
                    None
                } else {
                    Some(
                        r.experimentation
                            .locations
                            .into_iter()
                            .map(|(key, location)| {
                                (
                                    key,
                                    PartialExperimentLocationConfig {
                                        enabled: Some(location.enabled),
                                        forced_variant: Some(location.forced_variant),
                                    },
                                )
                            })
                            .collect(),
                    )
                },
            }),
            tools: Some(PartialToolConfig {
                shell_timeout_secs: Some(r.tools.shell_timeout_secs),
                python_execute: Some(PartialPythonExecuteToolConfig {
                    enabled: Some(r.tools.python_execute.enabled),
                    timeout_secs: Some(r.tools.python_execute.timeout_secs),
                    startup_timeout_secs: Some(r.tools.python_execute.startup_timeout_secs),
                    idle_ttl_secs: Some(r.tools.python_execute.idle_ttl_secs),
                    warm_target: Some(r.tools.python_execute.warm_target),
                    max_workers: Some(r.tools.python_execute.max_workers),
                    runtime_policy: Some(r.tools.python_execute.runtime_policy),
                    runtime_path: Some(r.tools.python_execute.runtime_path),
                    require_sandbox: Some(r.tools.python_execute.require_sandbox),
                }),
                max_output_bytes: Some(r.tools.max_output_bytes),
                plugin_directory: Some(r.tools.plugin_directory),
                search: Some(r.tools.search),
                browser: Some(PartialBrowserToolConfig {
                    binary_path: Some(r.tools.browser.binary_path),
                    page_idle_secs: Some(r.tools.browser.page_idle_secs),
                    process_idle_secs: Some(r.tools.browser.process_idle_secs),
                    max_snapshot_bytes: Some(r.tools.browser.max_snapshot_bytes),
                    screenshots: Some(r.tools.browser.screenshots),
                    allow_hosts: Some(r.tools.browser.allow_hosts),
                }),
                allow: Some(r.tools.permissions.allow),
                deny: Some(r.tools.permissions.deny),
                bundles: Some(r.tools.permissions.bundles),
                require_approval: Some(r.tools.permissions.require_approval),
            }),
            output: Some(PartialOutputConfig {
                schema: Some(r.output.schema),
                max_retries: Some(r.output.max_retries),
            }),
            hitl: Some(PartialHitlConfig {
                timeout: Some(r.hitl.timeout),
                timeout_policy: Some(r.hitl.timeout_policy),
            }),
            communication: Some(PartialCommunicationConfig {
                context_transfer: Some(PartialContextTransferConfig {
                    mode: Some(r.communication.context_transfer.mode),
                    allowed_fields: Some(r.communication.context_transfer.allowed_fields),
                    summary_budget: r.communication.context_transfer.summary_budget,
                    targets: if r.communication.context_transfer.targets.is_empty() {
                        None
                    } else {
                        Some(
                            r.communication
                                .context_transfer
                                .targets
                                .into_iter()
                                .map(|(k, v)| {
                                    (
                                        k,
                                        PartialContextTransferTargetConfig {
                                            mode: Some(v.mode),
                                            allowed_fields: Some(v.allowed_fields),
                                            summary_budget: v.summary_budget,
                                        },
                                    )
                                })
                                .collect(),
                        )
                    },
                }),
                message_ttl_secs: Some(r.communication.message_ttl_secs),
                expiry_interval_secs: Some(r.communication.expiry_interval_secs),
            }),
            scheduling: Some(PartialSchedulingConfig {
                run_history_max_age_days: Some(r.scheduling.run_history_max_age_days),
                run_history_max_per_schedule: Some(r.scheduling.run_history_max_per_schedule),
            }),
            generation: Some(PartialGenerationConfig {
                presets: if r.generation.presets.is_empty() {
                    None
                } else {
                    Some(r.generation.presets)
                },
            }),
            channels: Some(PartialChannelsConfig {
                telegram: r
                    .channels
                    .telegram
                    .map(|telegram| PartialTelegramChannelConfig {
                        bot_token_secret_ref: Some(telegram.bot_token_secret_ref),
                        bot_token_env: Some(telegram.bot_token_env),
                        api_base_url: Some(telegram.api_base_url),
                        poll_timeout_secs: Some(telegram.poll_timeout_secs),
                    }),
            }),
            dangerously_skip_permissions: Some(r.dangerously_skip_permissions),
        }
    }
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

trait MergeOverlay {
    fn merge_overlay(self, overlay: Self) -> Self;
}

macro_rules! impl_merge_overlay {
    ($type:ty { $($field:ident),+ $(,)? }) => {
        impl MergeOverlay for $type {
            fn merge_overlay(self, overlay: Self) -> Self {
                Self {
                    $($field: overlay.$field.or(self.$field)),+
                }
            }
        }
    };
}

impl_merge_overlay!(PartialServerConfig { host, port });
impl_merge_overlay!(PartialWorkspaceConfig { path });
impl_merge_overlay!(PartialSandboxConfig {
    enabled,
    allow_degraded_execution,
});
impl_merge_overlay!(PartialLoggingConfig { level });
impl_merge_overlay!(PartialContextConfig {
    assembly,
    timeout,
    budget,
});
impl_merge_overlay!(PartialObservabilityConfig { force_on });
impl_merge_overlay!(PartialPythonExecuteToolConfig {
    enabled,
    timeout_secs,
    startup_timeout_secs,
    idle_ttl_secs,
    warm_target,
    max_workers,
    runtime_policy,
    runtime_path,
    require_sandbox,
});
impl_merge_overlay!(PartialBrowserToolConfig {
    binary_path,
    page_idle_secs,
    process_idle_secs,
    max_snapshot_bytes,
    screenshots,
    allow_hosts,
});
impl_merge_overlay!(PartialOutputConfig {
    schema,
    max_retries,
});
impl_merge_overlay!(PartialHitlConfig {
    timeout,
    timeout_policy,
});
impl_merge_overlay!(PartialSchedulingConfig {
    run_history_max_age_days,
    run_history_max_per_schedule,
});
impl_merge_overlay!(PartialTelegramChannelConfig {
    bot_token_secret_ref,
    bot_token_env,
    api_base_url,
    poll_timeout_secs,
});

fn merge_overlay_section<S: MergeOverlay>(base: Option<S>, overlay: Option<S>) -> Option<S> {
    merge_section(base, overlay, MergeOverlay::merge_overlay)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Merge two `PartialConfig` values with child-wins semantics.
///
/// Sections are merged independently — an overlay that sets `[providers]`
/// does not erase a base `[server]`. Within a section, each field is merged
/// independently: the overlay value wins when present, otherwise the base
/// value is kept.
pub fn merge_partial(base: PartialConfig, overlay: PartialConfig) -> PartialConfig {
    PartialConfig {
        server: merge_overlay_section(base.server, overlay.server),
        providers: merge_section(base.providers, overlay.providers, |b, o| {
            let entries = match (b.entries, o.entries) {
                (Some(mut base_entries), Some(overlay_entries)) => {
                    base_entries.extend(overlay_entries);
                    Some(base_entries)
                }
                (base_entries, overlay_entries) => overlay_entries.or(base_entries),
            };
            PartialProviderCatalogConfig {
                default: o.default.or(b.default),
                entries,
            }
        }),
        models: merge_section(base.models, overlay.models, |b, o| {
            let overlay_changes_default = o.default.is_some();
            let effective_default = o.default.as_ref().or(b.default.as_ref());
            let overlay_replaces_default_entry = o
                .entries
                .as_ref()
                .zip(effective_default)
                .is_some_and(|(entries, default)| entries.contains_key(default));
            let entries = match (b.entries, o.entries) {
                (Some(mut base_entries), Some(overlay_entries)) => {
                    base_entries.extend(overlay_entries);
                    Some(base_entries)
                }
                (base_entries, overlay_entries) => overlay_entries.or(base_entries),
            };
            PartialModelCatalogConfig {
                default: o.default.or(b.default),
                default_model_id: if o.default_model_id.is_some() {
                    o.default_model_id
                } else if overlay_changes_default || overlay_replaces_default_entry {
                    None
                } else {
                    b.default_model_id
                },
                entries,
            }
        }),
        workspace: merge_overlay_section(base.workspace, overlay.workspace),
        sandbox: merge_overlay_section(base.sandbox, overlay.sandbox),
        logging: merge_overlay_section(base.logging, overlay.logging),
        context: merge_overlay_section(base.context, overlay.context),
        pricing: merge_section(base.pricing, overlay.pricing, |b, o| {
            // Overlay model entries win; base entries absent from the overlay survive.
            let mut merged = b.models.unwrap_or_default();
            if let Some(overlay_models) = o.models {
                merged.extend(overlay_models);
            }
            PartialPricingConfig {
                models: Some(merged),
            }
        }),
        observability: merge_overlay_section(base.observability, overlay.observability),
        experimentation: merge_section(base.experimentation, overlay.experimentation, |b, o| {
            let locations = match (b.locations, o.locations) {
                (Some(mut base_locations), Some(overlay_locations)) => {
                    for (key, overlay_location) in overlay_locations {
                        match base_locations.entry(key) {
                            std::collections::hash_map::Entry::Occupied(mut entry) => {
                                let base_location = entry.get_mut();
                                base_location.enabled =
                                    overlay_location.enabled.or(base_location.enabled);
                                base_location.forced_variant = overlay_location
                                    .forced_variant
                                    .clone()
                                    .or_else(|| base_location.forced_variant.clone());
                            }
                            std::collections::hash_map::Entry::Vacant(entry) => {
                                entry.insert(overlay_location);
                            }
                        }
                    }
                    Some(base_locations)
                }
                (base_locations, overlay_locations) => overlay_locations.or(base_locations),
            };
            PartialExperimentationConfig {
                enabled: o.enabled.or(b.enabled),
                locations,
            }
        }),
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
                python_execute: merge_overlay_section(b.python_execute, o.python_execute),
                max_output_bytes: o.max_output_bytes.or(b.max_output_bytes),
                plugin_directory: o.plugin_directory.or(b.plugin_directory),
                search: o.search.or(b.search),
                browser: merge_overlay_section(b.browser, o.browser),
                allow: o.allow.or(b.allow),
                deny: o.deny.or(b.deny),
                bundles,
                // Replacement semantics: overlay replaces base list entirely.
                require_approval: o.require_approval.or(b.require_approval),
            }
        }),
        output: merge_overlay_section(base.output, overlay.output),
        hitl: merge_overlay_section(base.hitl, overlay.hitl),
        communication: merge_section(base.communication, overlay.communication, |b, o| {
            let ct = merge_section(b.context_transfer, o.context_transfer, |bt, ot| {
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
            });
            PartialCommunicationConfig {
                context_transfer: ct,
                message_ttl_secs: o.message_ttl_secs.or(b.message_ttl_secs),
                expiry_interval_secs: o.expiry_interval_secs.or(b.expiry_interval_secs),
            }
        }),
        scheduling: merge_overlay_section(base.scheduling, overlay.scheduling),
        generation: merge_section(base.generation, overlay.generation, |b, o| {
            let mut presets = b.presets.unwrap_or_default();
            if let Some(overlay_presets) = o.presets {
                presets.extend(overlay_presets);
            }
            PartialGenerationConfig {
                presets: Some(presets),
            }
        }),
        channels: merge_section(base.channels, overlay.channels, |b, o| {
            PartialChannelsConfig {
                telegram: merge_overlay_section(b.telegram, o.telegram),
            }
        }),
        dangerously_skip_permissions: overlay
            .dangerously_skip_permissions
            .or(base.dangerously_skip_permissions),
    }
}

/// Fold a chain of `PartialConfig` values left-to-right, with each later
/// entry winning over earlier ones.
///
/// An empty chain yields an empty `PartialConfig` (all defaults will be
/// applied by `resolve`).
pub fn merge_chain(configs: Vec<PartialConfig>) -> PartialConfig {
    configs
        .into_iter()
        .fold(PartialConfig::default(), merge_partial)
}

/// Resolve a `PartialConfig` into a `ResolvedConfig` by filling any absent
/// fields with their default values.
pub fn resolve(partial: PartialConfig) -> ResolvedConfig {
    let ds = ServerConfig::default();
    let dw = WorkspaceConfig::default();
    let dsb = SandboxConfig::default();
    let dlg = LoggingConfig::default();

    let s = partial.server.unwrap_or_default();
    let providers = partial.providers.unwrap_or_default();
    let models = partial.models.unwrap_or_default();
    let default_model_id_override = models.default_model_id.clone();
    let w = partial.workspace.unwrap_or_default();
    let sb = partial.sandbox.unwrap_or_default();
    let lg = partial.logging.unwrap_or_default();

    let mut provider_catalog = ProviderCatalogConfig::default();
    provider_catalog.default = providers.default.unwrap_or(provider_catalog.default);
    if let Some(entries) = providers.entries {
        provider_catalog.entries.extend(entries);
    }

    let mut model_catalog = ModelCatalogConfig::default();
    model_catalog.default = models.default.unwrap_or(model_catalog.default);
    if let Some(entries) = models.entries {
        model_catalog.entries.extend(entries);
    }
    if let Some(model_id) = default_model_id_override {
        if model_catalog.default.is_empty() {
            model_catalog.default = "default".to_string();
        }
        let model = model_catalog
            .entries
            .entry(model_catalog.default.clone())
            .or_insert_with(|| ModelEndpointConfig {
                provider_id: String::new(),
                model_id: String::new(),
                aliases: Vec::new(),
                context_window: None,
                generation_preset: None,
                default_effort: None,
                capabilities: ModelCapabilitiesConfig::default(),
            });
        model.model_id = model_id;
    }

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
        providers: provider_catalog,
        models: model_catalog,
        workspace: WorkspaceConfig {
            path: w.path.unwrap_or(dw.path),
        },
        sandbox: SandboxConfig {
            enabled: sb.enabled.unwrap_or(dsb.enabled),
            allow_degraded_execution: sb
                .allow_degraded_execution
                .unwrap_or(dsb.allow_degraded_execution),
        },
        logging: LoggingConfig {
            level: lg.level.unwrap_or(dlg.level),
        },
        tools: {
            let dt = ToolConfig::default();
            let t = partial.tools.unwrap_or_default();
            let dp = ToolPermissionConfig::default();
            let dpy = PythonExecuteToolConfig::default();
            let db = BrowserToolConfig::default();
            let py = t.python_execute.unwrap_or_default();
            let browser = t.browser.unwrap_or_default();
            ToolConfig {
                shell_timeout_secs: t.shell_timeout_secs.unwrap_or(dt.shell_timeout_secs),
                python_execute: PythonExecuteToolConfig {
                    enabled: py.enabled.unwrap_or(dpy.enabled),
                    timeout_secs: py.timeout_secs.unwrap_or(dpy.timeout_secs),
                    startup_timeout_secs: py
                        .startup_timeout_secs
                        .unwrap_or(dpy.startup_timeout_secs),
                    idle_ttl_secs: py.idle_ttl_secs.unwrap_or(dpy.idle_ttl_secs),
                    warm_target: py.warm_target.unwrap_or(dpy.warm_target),
                    max_workers: py.max_workers.unwrap_or(dpy.max_workers),
                    runtime_policy: py.runtime_policy.unwrap_or(dpy.runtime_policy),
                    runtime_path: py.runtime_path.unwrap_or(dpy.runtime_path),
                    require_sandbox: py.require_sandbox.unwrap_or(dpy.require_sandbox),
                },
                max_output_bytes: t.max_output_bytes.unwrap_or(dt.max_output_bytes),
                plugin_directory: t.plugin_directory.unwrap_or(dt.plugin_directory),
                // Double-option: outer None → default (None), Some(x) → use x.
                search: t.search.unwrap_or(dt.search),
                browser: BrowserToolConfig {
                    binary_path: browser.binary_path.unwrap_or(db.binary_path),
                    page_idle_secs: browser.page_idle_secs.unwrap_or(db.page_idle_secs),
                    process_idle_secs: browser.process_idle_secs.unwrap_or(db.process_idle_secs),
                    max_snapshot_bytes: browser.max_snapshot_bytes.unwrap_or(db.max_snapshot_bytes),
                    screenshots: browser.screenshots.unwrap_or(db.screenshots),
                    allow_hosts: browser.allow_hosts.unwrap_or(db.allow_hosts),
                },
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
            let models = partial.pricing.and_then(|p| p.models).unwrap_or_default();
            PricingConfig { models }
        },
        generation: {
            let presets = partial
                .generation
                .and_then(|generation| generation.presets)
                .unwrap_or_default();
            GenerationConfig { presets }
        },
        channels: {
            let default = TelegramChannelConfig::default();
            let channels = partial.channels.unwrap_or_default();
            ChannelsConfig {
                telegram: channels.telegram.map(|telegram| TelegramChannelConfig {
                    bot_token_secret_ref: telegram
                        .bot_token_secret_ref
                        .unwrap_or(default.bot_token_secret_ref),
                    bot_token_env: telegram.bot_token_env.unwrap_or(default.bot_token_env),
                    api_base_url: telegram.api_base_url.unwrap_or(default.api_base_url),
                    poll_timeout_secs: telegram
                        .poll_timeout_secs
                        .unwrap_or(default.poll_timeout_secs),
                }),
            }
        },
        observability: {
            let do_ = ObservabilityConfig::default();
            let o = partial.observability.unwrap_or_default();
            ObservabilityConfig {
                force_on: o.force_on.unwrap_or(do_.force_on),
            }
        },
        experimentation: {
            let de = ExperimentationConfig::default();
            let e = partial.experimentation.unwrap_or_default();
            ExperimentationConfig {
                enabled: e.enabled.unwrap_or(de.enabled),
                locations: e
                    .locations
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(key, location)| {
                        let dl = ExperimentLocationConfig::default();
                        (
                            key,
                            ExperimentLocationConfig {
                                enabled: location.enabled.unwrap_or(dl.enabled),
                                forced_variant: location.forced_variant.unwrap_or(None),
                            },
                        )
                    })
                    .collect(),
            }
        },
        output: {
            let do_ = OutputConfig::default();
            let o = partial.output.unwrap_or_default();
            OutputConfig {
                schema: o.schema.unwrap_or(do_.schema),
                max_retries: o.max_retries.unwrap_or(do_.max_retries),
            }
        },
        declared_skills: Vec::new(),
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

            let targets = ct
                .targets
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        ContextTransferTargetConfig {
                            mode: v.mode.unwrap_or_else(|| resolved_mode.clone()),
                            allowed_fields: v
                                .allowed_fields
                                .unwrap_or_else(|| resolved_fields.clone()),
                            summary_budget: v.summary_budget.or(resolved_budget),
                        },
                    )
                })
                .collect();

            CommunicationConfig {
                message_ttl_secs: c.message_ttl_secs.unwrap_or(86400),
                expiry_interval_secs: c.expiry_interval_secs.unwrap_or(60),
                context_transfer: ContextTransferConfig {
                    mode: resolved_mode,
                    allowed_fields: resolved_fields,
                    summary_budget: resolved_budget,
                    targets,
                },
            }
        },
        scheduling: {
            let ds = SchedulingConfig::default();
            let s = partial.scheduling.unwrap_or_default();
            SchedulingConfig {
                run_history_max_age_days: s
                    .run_history_max_age_days
                    .unwrap_or(ds.run_history_max_age_days),
                run_history_max_per_schedule: s
                    .run_history_max_per_schedule
                    .unwrap_or(ds.run_history_max_per_schedule),
            }
        },
        // Instructions are assembled separately by the instructions module
        instructions: String::new(),
        dangerously_skip_permissions: partial.dangerously_skip_permissions.unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ContextTransferField;

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

    fn partial_with_default_model_id(model: &str) -> PartialConfig {
        PartialConfig {
            models: Some(PartialModelCatalogConfig {
                default_model_id: Some(model.to_string()),
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
        assert_eq!(
            server.host.as_deref(),
            Some("127.0.0.1"),
            "host from base should survive"
        );
        assert_eq!(
            server.port,
            Some(4000),
            "port from overlay should be present"
        );
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
        let overlay = partial_with_default_model_id("gpt-4o");
        let merged = merge_partial(base, overlay);

        // Server from base must survive even though overlay touched only provider
        let server = merged.server.expect("server from base should survive");
        assert_eq!(server.host.as_deref(), Some("10.0.0.1"));

        let models = merged
            .models
            .expect("models from overlay should be present");
        assert_eq!(models.default_model_id.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn three_level_chain_respects_priority() {
        // global: host=10.0.0.1, port=3000
        // space: model=claude-opus
        // agent: port=4000
        let global = PartialConfig {
            server: Some(PartialServerConfig {
                host: Some("10.0.0.1".to_string()),
                port: Some(3000),
            }),
            ..Default::default()
        };
        let space = partial_with_default_model_id("claude-opus");
        let agent = partial_with_server_port(4000);

        let merged = merge_chain(vec![global, space, agent]);

        // Agent overrides global's port
        let server = merged.server.expect("server should exist");
        assert_eq!(
            server.host.as_deref(),
            Some("10.0.0.1"),
            "host from global survives"
        );
        assert_eq!(server.port, Some(4000), "agent overrides global port");

        // Space's model survives
        let models = merged.models.expect("models should exist");
        assert_eq!(
            models.default_model_id.as_deref(),
            Some("claude-opus"),
            "space model survives"
        );
    }

    #[test]
    fn absent_intermediate_level_passes_through() {
        // global: port=5000, empty middle, agent: model=foo
        let global = partial_with_server_port(5000);
        let middle = PartialConfig::default(); // nothing
        let agent = partial_with_default_model_id("foo");

        let merged = merge_chain(vec![global, middle, agent]);

        let server = merged.server.expect("server from global should survive");
        assert_eq!(server.port, Some(5000));
        let models = merged.models.expect("models from agent should be present");
        assert_eq!(models.default_model_id.as_deref(), Some("foo"));
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
        assert_eq!(
            resolved.server.host, "192.168.1.1",
            "should use partial host"
        );
        assert_eq!(resolved.server.port, 8080, "should use partial port");
    }

    #[test]
    fn resolve_fills_absent_fields_with_defaults() {
        let partial = PartialConfig::default(); // all None
        let resolved = resolve(partial);
        let default = ResolvedConfig::default();
        assert_eq!(resolved.server.host, default.server.host);
        assert_eq!(resolved.server.port, default.server.port);
        assert_eq!(resolved.default_model_id(), default.default_model_id());
        assert_eq!(resolved.sandbox.enabled, default.sandbox.enabled);
    }

    #[test]
    fn resolve_uses_explicit_provider_model_catalogues() {
        let resolved = resolve(PartialConfig {
            providers: Some(PartialProviderCatalogConfig {
                default: Some("openai".to_string()),
                entries: Some(HashMap::from([(
                    "openai".to_string(),
                    ProviderEndpointConfig {
                        protocol: Protocol::OpenAi,
                        api_key_secret_ref: None,
                        api_key_env: Some("OPENAI_API_KEY".to_string()),
                        base_url: None,
                        command: None,
                        state_dir: None,
                    },
                )])),
            }),
            models: Some(PartialModelCatalogConfig {
                default: Some("fast".to_string()),
                entries: Some(HashMap::from([(
                    "fast".to_string(),
                    ModelEndpointConfig {
                        provider_id: "openai".to_string(),
                        model_id: "gpt-4o-mini".to_string(),
                        aliases: Vec::new(),
                        context_window: None,
                        generation_preset: None,
                        default_effort: None,
                        capabilities: ModelCapabilitiesConfig::default(),
                    },
                )])),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(resolved.providers.default, "openai");
        assert_eq!(resolved.models.default, "fast");
        assert_eq!(resolved.default_model_id(), "gpt-4o-mini");

        let provider = resolved
            .default_provider_endpoint()
            .expect("default provider should resolve");
        assert_eq!(provider.protocol, Protocol::OpenAi);
        assert_eq!(provider.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
    }

    #[test]
    fn model_override_updates_existing_default_model_entry() {
        let mut base = ResolvedConfig::default();
        base.models.default = "workhorse".to_string();
        base.models.entries = HashMap::from([(
            "workhorse".to_string(),
            ModelEndpointConfig {
                provider_id: "anthropic".to_string(),
                model_id: "base-model".to_string(),
                aliases: Vec::new(),
                context_window: None,
                generation_preset: None,
                default_effort: None,
                capabilities: ModelCapabilitiesConfig::default(),
            },
        )]);

        let resolved = resolve(merge_partial(
            PartialConfig::from(base),
            PartialConfig {
                models: Some(PartialModelCatalogConfig {
                    default_model_id: Some("overlay-model".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ));

        assert_eq!(resolved.default_model_id(), "overlay-model");
        assert_eq!(
            resolved.models.entries["workhorse"].provider_id, "anthropic",
            "model overrides must not erase the provider reference"
        );
    }

    #[test]
    fn changing_default_model_clears_inherited_default_model_id_override() {
        let base = PartialConfig {
            models: Some(PartialModelCatalogConfig {
                default: Some("smart".to_string()),
                default_model_id: Some("parent-model-id".to_string()),
                entries: Some(HashMap::from([(
                    "smart".to_string(),
                    ModelEndpointConfig {
                        provider_id: "anthropic".to_string(),
                        model_id: "claude-3-7-sonnet".to_string(),
                        aliases: Vec::new(),
                        context_window: None,
                        generation_preset: None,
                        default_effort: None,
                        capabilities: ModelCapabilitiesConfig::default(),
                    },
                )])),
            }),
            ..Default::default()
        };
        let overlay = PartialConfig {
            models: Some(PartialModelCatalogConfig {
                default: Some("fast".to_string()),
                entries: Some(HashMap::from([(
                    "fast".to_string(),
                    ModelEndpointConfig {
                        provider_id: "openai".to_string(),
                        model_id: "gpt-4o-mini".to_string(),
                        aliases: Vec::new(),
                        context_window: None,
                        generation_preset: None,
                        default_effort: None,
                        capabilities: ModelCapabilitiesConfig::default(),
                    },
                )])),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resolved = resolve(merge_partial(base, overlay));
        assert_eq!(resolved.models.default, "fast");
        assert_eq!(resolved.default_model_id(), "gpt-4o-mini");
        assert_eq!(resolved.models.entries["fast"].provider_id, "openai");
    }

    #[test]
    fn replacing_default_entry_clears_inherited_default_model_id_override() {
        let base = PartialConfig {
            models: Some(PartialModelCatalogConfig {
                default: Some("smart".to_string()),
                default_model_id: Some("parent-model-id".to_string()),
                entries: Some(HashMap::from([(
                    "smart".to_string(),
                    ModelEndpointConfig {
                        provider_id: "anthropic".to_string(),
                        model_id: "claude-3-7-sonnet".to_string(),
                        aliases: Vec::new(),
                        context_window: None,
                        generation_preset: None,
                        default_effort: None,
                        capabilities: ModelCapabilitiesConfig::default(),
                    },
                )])),
            }),
            ..Default::default()
        };
        let overlay = PartialConfig {
            models: Some(PartialModelCatalogConfig {
                entries: Some(HashMap::from([(
                    "smart".to_string(),
                    ModelEndpointConfig {
                        provider_id: "openai".to_string(),
                        model_id: "gpt-4.1-mini".to_string(),
                        aliases: Vec::new(),
                        context_window: None,
                        generation_preset: None,
                        default_effort: None,
                        capabilities: ModelCapabilitiesConfig::default(),
                    },
                )])),
                ..Default::default()
            }),
            ..Default::default()
        };

        let resolved = resolve(merge_partial(base, overlay));
        assert_eq!(resolved.models.default, "smart");
        assert_eq!(resolved.default_model_id(), "gpt-4.1-mini");
        assert_eq!(resolved.models.entries["smart"].provider_id, "openai");
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
        assert_eq!(
            ctx.assembly.as_deref(),
            Some("base-assembly"),
            "assembly from base survives"
        );
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
        assert!(resolved.tools.permissions.bundles.contains_key("browsing"));
        assert!(
            resolved
                .tools
                .permissions
                .bundles
                .contains_key("browser_actions")
        );
    }

    #[test]
    fn tool_permission_agent_allow_overrides_global_deny() {
        // Global denies "shell", agent allows "read_file" — agent's allow replaces.
        let global = partial_with_tool_deny(vec!["shell"]);
        let agent = partial_with_tool_allow(vec!["read_file"]);

        let merged = merge_chain(vec![global, agent]);
        let resolved = resolve(merged);

        assert_eq!(
            resolved.tools.permissions.allow,
            vec!["read_file".to_string()]
        );
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
        assert_eq!(
            resolved.tools.permissions.deny,
            vec!["web_search".to_string()]
        );
    }

    #[test]
    fn tool_permission_bundles_merge_map_style() {
        let mut global_bundles = HashMap::new();
        global_bundles.insert("read_only".to_string(), vec!["file_read".into()]);
        global_bundles.insert("write".to_string(), vec!["file_write".into()]);

        let mut agent_bundles = HashMap::new();
        // Override the "read_only" bundle, leave "write" from global.
        agent_bundles.insert(
            "read_only".to_string(),
            vec!["file_read".into(), "web_search".into()],
        );

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

        assert_eq!(
            resolved.tools.shell_timeout_secs, 120,
            "agent overrides global"
        );
        assert_eq!(
            resolved.tools.max_output_bytes, 200_000,
            "global survives when agent absent"
        );
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
    fn output_config_explicit_none_clears_inherited_schema() {
        let global = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: Some(Some(serde_json::json!({"type": "object"}))),
                max_retries: Some(5),
            }),
            ..Default::default()
        };
        let agent = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: Some(None),
                max_retries: None,
            }),
            ..Default::default()
        };

        let resolved = resolve(merge_chain(vec![global, agent]));
        assert!(
            resolved.output.schema.is_none(),
            "explicit None should clear an inherited output schema"
        );
        assert_eq!(
            resolved.output.max_retries, 5,
            "clearing schema should not clear unrelated output fields"
        );
    }

    #[test]
    fn output_config_three_level_chain_agent_schema_wins() {
        let global = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: Some(Some(serde_json::json!({"type": "string"}))),
                max_retries: Some(5),
            }),
            ..Default::default()
        };
        let space = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: None,
                max_retries: Some(2),
            }),
            ..Default::default()
        };
        let agent = PartialConfig {
            output: Some(PartialOutputConfig {
                schema: Some(Some(serde_json::json!({"type": "object"}))),
                max_retries: Some(1),
            }),
            ..Default::default()
        };

        let resolved = resolve(merge_chain(vec![global, space, agent]));
        // Agent's values win at every level.
        assert_eq!(
            resolved.output.schema,
            Some(serde_json::json!({"type": "object"}))
        );
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
        assert_eq!(
            hitl.timeout_policy,
            Some(ApprovalTimeoutPolicy::Reject),
            "base policy should survive"
        );
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
        let space = PartialConfig {
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
        let resolved = resolve(merge_chain(vec![global, space, agent]));
        assert_eq!(resolved.hitl.timeout, 600, "space timeout wins over global");
        assert_eq!(
            resolved.hitl.timeout_policy,
            ApprovalTimeoutPolicy::Interrupt,
            "agent policy wins"
        );
    }

    #[test]
    fn hitl_resolve_defaults_when_absent() {
        use swage_core::ApprovalTimeoutPolicy;
        let resolved = resolve(PartialConfig::default());
        assert_eq!(resolved.hitl.timeout, 300, "default HITL timeout is 300");
        assert_eq!(
            resolved.hitl.timeout_policy,
            ApprovalTimeoutPolicy::Reject,
            "default policy is Reject"
        );
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
        assert_eq!(
            ra[0].tool_name(),
            "write_file",
            "base require_approval survives"
        );
    }

    #[test]
    fn context_transfer_per_target_inherits_resolved_defaults() {
        // Top-level sets mode=Summary with a budget; per-target entry only
        // overrides allowed_fields. The target should inherit Summary mode
        // and the budget — not fall back to None/empty.
        let partial = PartialConfig {
            communication: Some(PartialCommunicationConfig {
                message_ttl_secs: None,
                expiry_interval_secs: None,
                context_transfer: Some(PartialContextTransferConfig {
                    mode: Some(crate::model::ContextTransferMode::Summary),
                    allowed_fields: Some(vec![
                        ContextTransferField::System,
                        ContextTransferField::Assistant,
                    ]),
                    summary_budget: Some(1024),
                    targets: Some({
                        let mut m = std::collections::HashMap::new();
                        m.insert(
                            "agent-b".into(),
                            PartialContextTransferTargetConfig {
                                mode: None,                                               // should inherit Summary
                                allowed_fields: Some(vec![ContextTransferField::System]), // overrides
                                summary_budget: None, // should inherit 1024
                            },
                        );
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
            vec![ContextTransferField::System],
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
        assert_eq!(
            resolved.tools.permissions.require_approval[0].tool_name(),
            "shell_exec"
        );
    }

    // --- ResolvedConfig → PartialConfig round-trip ---

    #[test]
    fn resolved_to_partial_round_trip_preserves_fields() {
        let mut original = ResolvedConfig::default();
        original.server.port = 8080;
        original.set_default_model_id("test-model".to_string());
        original.tools.shell_timeout_secs = 120;

        let partial = PartialConfig::from(original.clone());
        let restored = resolve(partial);

        assert_eq!(restored.server.port, 8080);
        assert_eq!(restored.default_model_id(), "test-model");
        assert_eq!(restored.tools.shell_timeout_secs, 120);
    }

    #[test]
    fn resolved_to_partial_then_overlay_merge() {
        let mut base = ResolvedConfig::default();
        base.server.port = 9000;
        base.set_default_model_id("base-model".to_string());

        let base_partial = PartialConfig::from(base);
        let overlay = PartialConfig {
            models: Some(PartialModelCatalogConfig {
                default_model_id: Some("overlay-model".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let merged = merge_partial(base_partial, overlay);
        let resolved = resolve(merged);

        assert_eq!(resolved.server.port, 9000, "base port should survive");
        assert_eq!(
            resolved.default_model_id(),
            "overlay-model",
            "overlay should win"
        );
    }

    #[test]
    fn scheduling_retention_fields_merge_overlay_wins() {
        let global = PartialConfig {
            scheduling: Some(PartialSchedulingConfig {
                run_history_max_age_days: Some(30),
                run_history_max_per_schedule: Some(250),
            }),
            ..Default::default()
        };
        let agent = PartialConfig {
            scheduling: Some(PartialSchedulingConfig {
                run_history_max_age_days: None,
                run_history_max_per_schedule: Some(50),
            }),
            ..Default::default()
        };

        let resolved = resolve(merge_chain(vec![global, agent]));

        assert_eq!(resolved.scheduling.run_history_max_age_days, 30);
        assert_eq!(resolved.scheduling.run_history_max_per_schedule, 50);
    }

    #[test]
    fn special_map_merges_preserve_base_keys_and_replace_collisions() {
        let mut base_providers = HashMap::new();
        base_providers.insert("base".to_string(), ProviderEndpointConfig::default());
        base_providers.insert(
            "shared".to_string(),
            ProviderEndpointConfig {
                base_url: Some("https://base.invalid".to_string()),
                ..Default::default()
            },
        );
        let mut overlay_providers = HashMap::new();
        overlay_providers.insert("overlay".to_string(), ProviderEndpointConfig::default());
        overlay_providers.insert(
            "shared".to_string(),
            ProviderEndpointConfig {
                base_url: Some("https://overlay.invalid".to_string()),
                ..Default::default()
            },
        );
        let mut base_pricing = HashMap::new();
        base_pricing.insert(
            "base".to_string(),
            ModelPricing {
                input_price_per_mtok: 1,
                output_price_per_mtok: 2,
            },
        );
        base_pricing.insert(
            "shared".to_string(),
            ModelPricing {
                input_price_per_mtok: 5,
                output_price_per_mtok: 6,
            },
        );
        let mut overlay_pricing = HashMap::new();
        overlay_pricing.insert(
            "overlay".to_string(),
            ModelPricing {
                input_price_per_mtok: 3,
                output_price_per_mtok: 4,
            },
        );
        overlay_pricing.insert(
            "shared".to_string(),
            ModelPricing {
                input_price_per_mtok: 7,
                output_price_per_mtok: 8,
            },
        );
        let mut base_presets = HashMap::new();
        base_presets.insert(
            "base".to_string(),
            GenerationPresetConfig {
                max_tokens: Some(100),
                temperature: None,
                stop_sequences: vec![],
                metadata: None,
            },
        );
        base_presets.insert(
            "shared".to_string(),
            GenerationPresetConfig {
                max_tokens: Some(300),
                temperature: None,
                stop_sequences: vec![],
                metadata: None,
            },
        );
        let mut overlay_presets = HashMap::new();
        overlay_presets.insert(
            "overlay".to_string(),
            GenerationPresetConfig {
                max_tokens: Some(200),
                temperature: None,
                stop_sequences: vec![],
                metadata: None,
            },
        );
        overlay_presets.insert(
            "shared".to_string(),
            GenerationPresetConfig {
                max_tokens: Some(400),
                temperature: None,
                stop_sequences: vec![],
                metadata: None,
            },
        );

        let merged = merge_partial(
            PartialConfig {
                providers: Some(PartialProviderCatalogConfig {
                    entries: Some(base_providers),
                    ..Default::default()
                }),
                pricing: Some(PartialPricingConfig {
                    models: Some(base_pricing),
                }),
                generation: Some(PartialGenerationConfig {
                    presets: Some(base_presets),
                }),
                ..Default::default()
            },
            PartialConfig {
                providers: Some(PartialProviderCatalogConfig {
                    entries: Some(overlay_providers),
                    ..Default::default()
                }),
                pricing: Some(PartialPricingConfig {
                    models: Some(overlay_pricing),
                }),
                generation: Some(PartialGenerationConfig {
                    presets: Some(overlay_presets),
                }),
                ..Default::default()
            },
        );

        let providers = merged.providers.unwrap().entries.unwrap();
        assert_eq!(providers.len(), 3);
        assert_eq!(
            providers["shared"].base_url.as_deref(),
            Some("https://overlay.invalid")
        );
        let pricing = merged.pricing.unwrap().models.unwrap();
        assert_eq!(pricing.len(), 3);
        assert_eq!(pricing["shared"].input_price_per_mtok, 7);
        let presets = merged.generation.unwrap().presets.unwrap();
        assert_eq!(presets.len(), 3);
        assert_eq!(presets["shared"].max_tokens, Some(400));
    }

    #[test]
    fn experimentation_location_overlay_merges_fields_without_erasing_base() {
        let merged = merge_partial(
            PartialConfig {
                experimentation: Some(PartialExperimentationConfig {
                    enabled: Some(true),
                    locations: Some(HashMap::from([(
                        "compaction".to_string(),
                        PartialExperimentLocationConfig {
                            enabled: Some(false),
                            forced_variant: Some(Some("base".to_string())),
                        },
                    )])),
                }),
                ..Default::default()
            },
            PartialConfig {
                experimentation: Some(PartialExperimentationConfig {
                    enabled: None,
                    locations: Some(HashMap::from([(
                        "compaction".to_string(),
                        PartialExperimentLocationConfig {
                            enabled: Some(true),
                            forced_variant: None,
                        },
                    )])),
                }),
                ..Default::default()
            },
        );

        let experimentation = merged.experimentation.unwrap();
        let location = &experimentation.locations.unwrap()["compaction"];
        assert_eq!(experimentation.enabled, Some(true));
        assert_eq!(location.enabled, Some(true));
        assert_eq!(location.forced_variant, Some(Some("base".to_string())));
    }

    #[test]
    fn double_option_fields_can_explicitly_clear_inherited_values() {
        let merged = merge_partial(
            PartialConfig {
                tools: Some(PartialToolConfig {
                    plugin_directory: Some(Some("/plugins".to_string())),
                    search: Some(Some(SearchProviderConfig {
                        provider_type: "searxng".to_string(),
                        base_url: "http://search.invalid".to_string(),
                        api_key: None,
                    })),
                    ..Default::default()
                }),
                channels: Some(PartialChannelsConfig {
                    telegram: Some(PartialTelegramChannelConfig {
                        bot_token_secret_ref: Some(Some("secret-ref".to_string())),
                        bot_token_env: Some(Some("TOKEN".to_string())),
                        ..Default::default()
                    }),
                }),
                ..Default::default()
            },
            PartialConfig {
                tools: Some(PartialToolConfig {
                    plugin_directory: Some(None),
                    search: Some(None),
                    ..Default::default()
                }),
                channels: Some(PartialChannelsConfig {
                    telegram: Some(PartialTelegramChannelConfig {
                        bot_token_secret_ref: Some(None),
                        bot_token_env: Some(None),
                        ..Default::default()
                    }),
                }),
                ..Default::default()
            },
        );

        let tools = merged.tools.unwrap();
        assert_eq!(tools.plugin_directory, Some(None));
        assert_eq!(tools.search, Some(None));
        let telegram = merged.channels.unwrap().telegram.unwrap();
        assert_eq!(telegram.bot_token_secret_ref, Some(None));
        assert_eq!(telegram.bot_token_env, Some(None));
    }
}
