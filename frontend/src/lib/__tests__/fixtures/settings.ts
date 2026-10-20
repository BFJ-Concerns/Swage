import type { SettingsResponse } from "../../types";

export function emptySettings(): SettingsResponse {
  return {
    providers: {},
    default_provider: "",
    models: {},
    default_model: "",
    generation_presets: {},
    tool_runtime: {
      shell_timeout_secs: 30,
      python_execute: {
        enabled: true,
        timeout_secs: 30,
        startup_timeout_secs: 10,
        idle_ttl_secs: 60,
        warm_target: 1,
        max_workers: 1,
        runtime_policy: "bundled_then_system",
        runtime_path: null,
        require_sandbox: true,
      },
      max_output_bytes: 100_000,
      plugin_directory: null,
      search: null,
    },
    tool_permissions: {
      allow: ["*"],
      deny: [],
      bundles: {},
      require_approval: ["shell", "file_edit", "file_write", "python_execute"],
    },
    hitl: {
      timeout: 300,
      timeout_policy: "reject",
    },
    sandbox: {
      enabled: true,
      allow_degraded_execution: false,
      egress_hosts: ["github.com"],
    },
    context: {
      assembly: "default",
      timeout: 5,
      budget: null,
    },
    pricing: {
      models: {},
    },
    observability: {
      force_on: false,
    },
    communication: {
      context_transfer: {
        mode: "none",
        allowed_fields: [],
        summary_budget: null,
        targets: {},
      },
      message_ttl_secs: 86_400,
      expiry_interval_secs: 60,
    },
    sessions: { archive_after_days: 7, max_delegation_depth: 4 },
    memory: { curator_idle_minutes: 10 },
    attachments: { accepted_schemes: ["https", "data"] },
    judge: { model: "" },
    voice: {},
    restart_required: false,
    can_restart: false,
  };
}

