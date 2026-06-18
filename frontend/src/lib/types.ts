// Shared TypeScript types for the Swage web interface.
// These mirror the gateway's API response shapes.

// TODO Replace hand-written API types with generated swage-protocol output
// Every interface in this file mirrors a gateway serde type by hand, with no
// enforcement — drift surfaces only at runtime. Once wire types live in the
// swage-protocol crate, this file should be generated (ts-rs), with the
// committed JSON Schema as the canonical contract. See the wire-contract
// section in CLAUDE.md and the swage-protocol mote in .motes/TODO.md.
// ---

// -------------------------------------------------------------------------
// Bench types
// -------------------------------------------------------------------------

/** A bench is the durable container users create and return to. */
export interface Bench {
  id: string;
  name: string;
  description: string;
  is_default: boolean;
  slug: string;
  created_at: string;
  /** Client-side recency marker until the gateway persists last_opened_at. */
  last_opened_at?: string | null;
}

/** Request body for creating a new bench. */
export interface CreateBenchRequest {
  name: string;
  description?: string;
}

/** Request body for updating an existing bench. All fields are optional. */
export interface UpdateBenchRequest {
  name?: string;
  description?: string;
}

export interface WorkspaceAttachment {
  id: string;
  bench_id: string;
  label: string;
  path: string;
  canonical_path: string;
  is_missing: boolean;
  is_default: boolean;
  permissions: string[];
  is_trusted: boolean;
  created_at: string;
}

export interface CreateWorkspaceAttachmentRequest {
  path: string;
  label?: string;
  set_as_default?: boolean;
  active_session_id?: string;
  permissions?: string[];
  is_trusted?: boolean;
}

export interface UpdateWorkspaceTrustRequest {
  is_trusted: boolean;
}

export type ProviderProtocol = "openai" | "anthropic" | "google" | "openrouter";

export interface ProviderSettings {
  protocol: ProviderProtocol;
  base_url?: string | null;
  api_key_secret_ref?: string | null;
  api_key_env?: string | null;
  has_credential: boolean;
  api_key?: string;
}

export interface ModelSettings {
  provider_id: string;
  model_id: string;
  aliases: string[];
  context_window?: number | null;
  generation_preset?: string | null;
}

export interface GenerationPresetConfig {
  max_tokens?: number | null;
  temperature?: number | null;
  stop_sequences: string[];
  metadata?: unknown | null;
}

export interface ToolRuntimeSettings {
  shell_timeout_secs: number;
  python_execute: PythonExecuteToolConfig;
  max_output_bytes: number;
  plugin_directory?: string | null;
  search?: SearchProviderConfig | null;
}

export interface PythonExecuteToolConfig {
  enabled: boolean;
  timeout_secs: number;
  startup_timeout_secs: number;
  idle_ttl_secs: number;
  warm_target: number;
  max_workers: number;
  runtime_policy: "bundled_then_system";
  runtime_path?: string | null;
  require_sandbox: boolean;
}

export interface SearchProviderConfig {
  provider_type: string;
  base_url: string;
  api_key?: string | null;
}

export interface PricingConfig {
  models: Record<string, ModelPricing>;
}

export interface ModelPricing {
  input_price_per_mtok: number;
  output_price_per_mtok: number;
}

export interface ObservabilityConfig {
  force_on: boolean;
}

export interface CommunicationConfig {
  context_transfer: ContextTransferConfig;
  message_ttl_secs: number;
  expiry_interval_secs: number;
}

export interface ContextTransferConfig {
  mode: ContextTransferMode;
  allowed_fields: ContextTransferField[];
  summary_budget?: number | null;
  targets: Record<string, ContextTransferTargetConfig>;
}

export interface ContextTransferTargetConfig {
  mode: ContextTransferMode;
  allowed_fields: ContextTransferField[];
  summary_budget?: number | null;
}

export type ContextTransferMode = "none" | "explicit" | "summary";

export type ContextTransferField = "system" | "user" | "assistant";

export interface SettingsResponse {
  providers: Record<string, ProviderSettings>;
  default_provider: string;
  models: Record<string, ModelSettings>;
  default_model: string;
  generation_presets: Record<string, GenerationPresetConfig>;
  tool_runtime: ToolRuntimeSettings;
  tool_permissions: ToolPermissionConfig;
  hitl: HitlConfig;
  sandbox: SandboxConfig;
  context: ContextConfig;
  pricing: PricingConfig;
  observability: ObservabilityConfig;
  communication: CommunicationConfig;
  restart_required: boolean;
  can_restart: boolean;
}

export interface SettingsPayload {
  providers: Record<string, ProviderSettings>;
  default_provider: string;
  models: Record<string, ModelSettings>;
  default_model: string;
  generation_presets: Record<string, GenerationPresetConfig>;
  tool_runtime?: ToolRuntimeSettings;
  tool_permissions: ToolPermissionConfig;
  hitl: HitlConfig;
  sandbox: SandboxConfig;
  context: ContextConfig;
  pricing?: PricingConfig;
  observability?: ObservabilityConfig;
  communication?: CommunicationConfig;
}

export interface ToolPermissionConfig {
  allow: string[];
  deny: string[];
  bundles: Record<string, string[]>;
  require_approval: RequireApprovalEntry[];
}

export type ApprovalTimeoutPolicy = "reject" | "interrupt";

export type RequireApprovalEntry =
  | string
  | {
      tool: string;
      timeout?: number | null;
      timeout_policy?: ApprovalTimeoutPolicy | null;
    };

export interface HitlConfig {
  timeout: number;
  timeout_policy: ApprovalTimeoutPolicy;
}

export interface SandboxConfig {
  enabled: boolean;
}

export interface ContextConfig {
  assembly: string;
  timeout: number;
  budget?: number | null;
}

export interface DiscoveredModel {
  id: string;
  name: string;
  context_window?: number | null;
}

export interface ModelDiscoveryResponse {
  provider_id: string;
  models: DiscoveredModel[];
}

export type ProviderDiscoveryDraft = Omit<ProviderSettings, "has_credential" | "api_key">;

export interface RestartStatusResponse {
  restart_required: boolean;
  can_restart: boolean;
  guidance: string;
}

/** Compact conversation entry for the list view. */
export interface ConversationSummary {
  id: string;
  selected_model_id?: string | null;
  created_at: string;
  last_activity: string;
  message_preview: string | null;
  has_interrupted_turn: boolean;
}

/** Full conversation with message history. */
export interface ConversationDetail {
  id: string;
  bench_id: string;
  selected_model_id?: string | null;
  created_at: string;
  last_activity: string;
  messages: MessageView[];
  input_tokens?: number | null;
  output_tokens?: number | null;
}

/** A single tool call recorded against a message. */
export interface ToolCallView {
  call_id: string;
  tool: string;
  // The arguments passed to the tool, as a free-form key-value map.
  arguments: Record<string, unknown>;
  // Null while the tool is still running; populated once it completes.
  result: string | Record<string, unknown> | null;
  // Null while in progress; "success" or "error" once resolved.
  status: "success" | "error" | null;
  // Wall time for the call in milliseconds; null while in progress.
  duration_ms: number | null;
  // True when the result was cut off to fit the wire size limit.
  truncated: boolean;
  // Untruncated size of the result in bytes; null when not available.
  total_bytes: number | null;
  // Set on sub-tool calls (tools invoked within python_execute); absent/null on top-level calls.
  parent_call_id?: string | null;
  // Sub-tool calls nested under this invocation (populated client-side by the streaming store
  // and the historical tree builder; not present in the raw API response).
  sub_calls?: ToolCallView[];
}

/** A single message in a conversation. */
export interface MessageView {
  thread_id?: string;
  turn_id?: string | null;
  role: "user" | "assistant";
  content: string;
  timestamp: string;
  turn_state:
    | "pending"
    | "processing"
    | "completed"
    | "failed"
    | "interrupted"
    | "awaiting_approval"
    | "delegating"
    | null;
  // Model id that produced an assistant message, when the API carries it.
  // Absent on user messages and on assistant messages whose model wasn't
  // reported — the message surface renders no model chip in that case rather
  // than a permanent "Model not reported" label (UI audit f-9).
  model?: string | null;
  // Present on every message in the conversation API; empty when no tool calls
  // were recorded for that message. Kept optional here so local client-created
  // message objects do not need to synthesise it eagerly.
  tool_calls?: ToolCallView[];
}

/** Response from creating a conversation. */
export interface ConversationResponse {
  id: string;
  bench_id: string;
  selected_model_id?: string | null;
  created_at: string;
  last_activity: string;
}

// -------------------------------------------------------------------------
// Lens runtime types
// -------------------------------------------------------------------------

export type AppLens = string;
export type LensSurface = "conversation" | "operator" | "builder";
export type BuilderMode = "team" | "agent" | "context" | "lens" | "skill" | "tool";
export type LensSourceKind = "built_in" | "authored" | "installed";

export interface LensDescriptor {
  id: AppLens;
  title: string;
  summary: string;
  default_surface: LensSurface;
  ui_modules: LensSurface[];
  prominent_team_ids: string[];
  source_kind: LensSourceKind;
}

export interface LensRuntimeResponse {
  bench_id: string;
  active_lens: AppLens;
  updated_at: string;
  available_lenses: LensDescriptor[];
  stage_modules: LensStageModulePlacement[];
}

export type LensStageModuleKind = "shared_reference" | "active_team_topology";

export interface LensStageModulePlacement {
  id: string;
  kind: LensStageModuleKind;
  title: string;
  team_ids: string[];
}

export interface CreateLensRequest {
  id: string;
  title: string;
  summary: string;
  default_surface: LensSurface;
  ui_modules: LensSurface[];
  prominent_team_ids: string[];
}

export interface UpdateLensRequest {
  title: string;
  summary: string;
  default_surface: LensSurface;
  ui_modules: LensSurface[];
  prominent_team_ids: string[];
}

// -------------------------------------------------------------------------
// Builder authoring types
// -------------------------------------------------------------------------

export interface TeamSummary {
  id: string;
  bench_id: string;
  name: string;
  description: string;
  entry_agent_id: string;
  agents: TeamAgentDefinition[];
  tasks: TeamTaskDefinition[];
  members: string[];
  created_at: string;
  updated_at: string;
}

export interface TeamAgentDefinition {
  id: string;
  display_name: string;
  description: string;
  instructions: string;
  model?: string | null;
}

export interface TeamTaskDefinition {
  name: string;
  description: string;
  instructions: string;
  model?: string | null;
  output_schema?: Record<string, unknown> | null;
  max_output_retries?: number | null;
  tool_allow?: string[] | null;
  tool_deny?: string[] | null;
}

export interface CreateTeamRequest {
  name: string;
  description: string;
  entry_agent_id: string;
  agents: TeamAgentDefinition[];
  tasks: TeamTaskDefinition[];
}

export interface UpdateTeamRequest {
  name?: string;
  description?: string;
  entry_agent_id?: string;
  agents?: TeamAgentDefinition[];
  tasks?: TeamTaskDefinition[];
}

export type BenchExtensionStatus = "active" | "approval_required";
export type BenchExtensionSourceKind = "managed" | "workspace_candidate";

export interface BenchExtensionSummary {
  name: string;
  display_name: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
  status: BenchExtensionStatus;
  source_kind: BenchExtensionSourceKind;
}

export interface CreateBenchExtensionRequest {
  name: string;
  display_name?: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
}

export interface UpdateBenchExtensionRequest {
  display_name?: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
}

// -------------------------------------------------------------------------
// Materials and attachments
// -------------------------------------------------------------------------

export type MaterialScopeKind = "session" | "bench";

export interface MaterialRecord {
  id: string;
  scope_kind: MaterialScopeKind;
  session_id: string | null;
  bench_id: string | null;
  source_kind: string;
  source_attachment_id: string | null;
  status: string;
  title: string;
  relative_path: string;
  absolute_path: string;
  mime_type: string;
  content_text: string;
  created_at: string;
  updated_at: string;
  archived_at: string | null;
}

export interface AttachmentRecord {
  id: string;
  scope_kind: MaterialScopeKind;
  session_id: string | null;
  bench_id: string | null;
  kind: string;
  status: string;
  filename: string;
  mime_type: string;
  byte_count: number;
  ingested_markdown: string | null;
  created_at: string;
  updated_at: string;
}

export interface BackgroundJobRecord {
  id: string;
  kind: string;
  state: string;
  attachment_id: string | null;
  result: Record<string, unknown> | null;
  error_message: string | null;
  attempt_count: number;
  created_at: string;
  updated_at: string;
  started_at: string | null;
  completed_at: string | null;
}

export interface CreateAttachmentRequest {
  scope_kind: MaterialScopeKind;
  session_id?: string;
  bench_id?: string;
  filename: string;
  mime_type: string;
  content_base64: string;
}

export interface AttachmentQueuedResponse {
  attachment: AttachmentRecord;
  job: BackgroundJobRecord;
}

export interface CreateMaterialRequest {
  scope_kind: MaterialScopeKind;
  session_id?: string;
  bench_id?: string;
  title: string;
  relative_path: string;
  mime_type?: string;
  content_text: string;
}

export type BuilderConfigOperation =
  | {
      kind: "create_lens";
      id: string;
      title: string;
      summary: string;
      default_surface: LensSurface;
      ui_modules: string[];
      prominent_team_ids: string[];
    }
  | {
      kind: "update_lens";
      lens_id: string;
      title: string;
      summary: string;
      default_surface: LensSurface;
      ui_modules: string[];
      prominent_team_ids: string[];
    }
  | {
      kind: "install_lens_preset";
      preset_id: string;
    }
  | {
      kind: "create_team";
      name: string;
      description: string;
      entry_agent_id: string;
      members: string[];
    }
  | {
      kind: "update_team";
      team_id?: string;
      team_name?: string;
      name?: string;
      description?: string;
      entry_agent_id?: string;
      members?: string[];
    }
  | {
      kind: "create_bench_extension";
      name: string;
      display_name?: string | null;
      description: string;
      requested_tools: string[];
      instructions: string;
    }
  | {
      kind: "update_bench_extension";
      extension_name: string;
      display_name?: string | null;
      description: string;
      requested_tools: string[];
      instructions: string;
    }
  | {
      kind: "approve_workspace_extension";
      extension_name: string;
    }
  | {
      kind: "create_agent" | "update_agent";
      agent_id: string;
      team_name?: string | null;
      display_name?: string | null;
      description?: string | null;
      instructions?: string | null;
      model?: string | null;
    }
  | {
      kind: "create_task" | "update_task";
      task_name: string;
      team_name?: string | null;
      description?: string | null;
      instructions?: string | null;
      model?: string | null;
      output_schema?: Record<string, unknown> | null;
    };

export interface BuilderReviewedOperation {
  kind: string;
  summary: string;
  ready: boolean;
  diagnostics: string[];
  operation: BuilderConfigOperation;
}

export interface BuilderProposalResponse {
  summary: string;
  operations: BuilderReviewedOperation[];
  diagnostics: string[];
  can_apply: boolean;
}

export interface BuilderApplyResponse {
  success: boolean;
  summary: string;
  diagnostics: string[];
  applied: Array<{
    kind: string;
    summary: string;
    entity_kind: string;
    entity_id: string;
  }>;
}

// -------------------------------------------------------------------------
// Shared reference object types
// -------------------------------------------------------------------------

export type SharedReferenceKind =
  | "team_topology"
  | "context_snapshot"
  | "orchestration_flow"
  | "context_definition";

export interface SharedReferenceSummary {
  kind: SharedReferenceKind;
  id: string;
  title: string;
  subtitle: string;
  updated_at: string;
}

export interface SharedReferenceInteraction {
  id: string;
  event_kind: string;
  payload: Record<string, unknown>;
  created_at: string;
}

export interface SharedReferenceNode {
  agent_id: string;
  label: string;
  column: number;
  is_entry: boolean;
}

export interface SharedReferenceEdge {
  from: string;
  to: string;
  kind: string;
}

export interface TeamTopologySharedReferencePayload {
  team_id: string;
  bench_id: string;
  entry_agent_id: string;
  members: string[];
  nodes: SharedReferenceNode[];
  edges: SharedReferenceEdge[];
}

export interface ContextSnapshotElementReference {
  element_id: string;
  name: string;
  source: string;
  token_count: number;
  priority: number;
  preview: string | null;
  child_count: number;
}

export interface ContextSnapshotSharedReferencePayload {
  turn_id: string;
  assembly_name: string;
  total_tokens: number;
  budget: number | null;
  warnings: string[];
  elements: ContextSnapshotElementReference[];
  provider_visible?: {
    requests?: unknown[];
    reasoning?: unknown[];
  };
}

export interface SharedReferenceObject {
  kind: SharedReferenceKind;
  id: string;
  title: string;
  subtitle: string;
  payload:
    | TeamTopologySharedReferencePayload
    | ContextSnapshotSharedReferencePayload;
}

export interface SharedReferenceDetailResponse {
  object: SharedReferenceObject;
  interactions: SharedReferenceInteraction[];
}

// -------------------------------------------------------------------------
// Operator surface types
// -------------------------------------------------------------------------

export interface OperatorGatewaySummary {
  status: string;
  health: string;
  shutdown_requested: boolean;
  seconds_since_activity: number;
  active_turn_count: number;
  active_connection_count: number;
}

export interface OperatorSessionSummary {
  id: string;
  bench_id: string;
  bench_name: string;
  created_at: string;
  last_activity: string;
  message_preview: string | null;
  has_interrupted_turn: boolean;
  has_active_connection: boolean;
  has_running_turn: boolean;
}

export interface OperatorApprovalSummary {
  id: string;
  turn_id: string;
  agent_id: string;
  kind: string;
  tool_name: string | null;
  prompt: string;
  timeout_at: string | null;
}

export interface ScheduledJobSummary {
  id: string;
  session_id: string;
  message: string;
  status: string;
  schedule: Record<string, unknown>;
  routing: Record<string, unknown>;
  next_run_at: string | null;
  last_run_at: string | null;
}

export type ScheduleSpecRequest =
  | { kind: "one_shot"; at: string }
  | { kind: "interval"; every: string }
  | { kind: "cron"; cron: string };

export interface CreateScheduledJobRequest {
  session_id: string;
  message: string;
  schedule: ScheduleSpecRequest;
  routing?: Record<string, unknown>;
}

export interface HeartbeatSummary {
  id: string;
  session_id: string;
  message: string;
  status: string;
  interval_seconds: number;
  next_beat_at: string | null;
  last_beat_at: string | null;
}

export interface CreateHeartbeatRequest {
  session_id: string;
  message: string;
  interval_seconds: number;
}

export interface VoiceCapabilitiesResponse {
  transcription: boolean;
  synthesis: boolean;
  default_voice: string | null;
  supported_voices: string[];
}

export interface RecoveryExportSummary {
  id: string;
  created_at: string;
  size_bytes: number;
  benches: number;
  workspaces: number;
  sessions: number;
  secrets: number;
  channel_links: number;
  execution_hosts: number;
  scheduled_jobs: number;
  heartbeats: number;
}

export interface RecoveryExportListResponse {
  exports: RecoveryExportSummary[];
}

export interface ChannelStatusSummary {
  integration: string;
  status: string;
  route_count: number;
  delivery_count: number;
  last_delivery_at: string | null;
}

export interface OperatorNodeSummary {
  host_id: string;
  label: string | null;
  kind: string;
  base_url: string | null;
  workspace_root: string;
  capabilities: string[];
  registered_at: string;
  state: string;
  last_seen_at: string | null;
  disconnected_at: string | null;
}

export interface OperatorOverview {
  gateway: OperatorGatewaySummary;
  sessions: OperatorSessionSummary[];
  approvals: OperatorApprovalSummary[];
  scheduled_jobs: {
    jobs: ScheduledJobSummary[];
  };
  heartbeats: {
    heartbeats: HeartbeatSummary[];
  };
  channels: {
    channels: ChannelStatusSummary[];
  };
  nodes: OperatorNodeSummary[];
}

// -------------------------------------------------------------------------
// WebSocket event types
// -------------------------------------------------------------------------

/** Client → Server events. */
export type ClientEvent =
  | { type: "send_message"; content: string; thread_id?: string; selected_model_id?: string }
  | { type: "cancel" }
  // Subscribe to one or more observability channels (e.g. "tools", "context").
  | { type: "subscribe"; channels: string[] }
  // Unsubscribe from previously subscribed channels.
  | { type: "unsubscribe"; channels: string[] };

/** Server → Client events. */
export interface SchemaValidationError {
  path: string;
  message: string;
}

export const KNOWN_SERVER_EVENT_TYPES = [
  "token",
  "turn_complete",
  "error",
  "cancelled",
  "tool_start",
  "tool_end",
  "context_assembled",
  "output_schema_retry",
  "provider_retry_started",
  "provider_retry_settled",
  "output_schema_failed",
  "delegation_started",
  "delegation_completed",
  "topology_run_started",
  "topology_node_started",
  "topology_node_completed",
  "topology_run_completed",
  "shutdown",
  "skill_activated",
  "skill_already_active",
  "skill_deactivated",
  "approval_required",
  "approval_confirmed",
  "approval_expired",
  "approval_decision_error",
] as const;

export type KnownServerEventType = (typeof KNOWN_SERVER_EVENT_TYPES)[number];

export type KnownServerEvent =
  | { type: "token"; content: string }
  | {
      type: "turn_complete";
      turn_id: string;
      input_tokens?: number;
      output_tokens?: number;
      cost_microcents?: number;
      structured_output?: Record<string, unknown>;
    }
  | { type: "error"; code: string; message: string }
  | { type: "cancelled" }
  // Fires when the gateway begins executing a tool call.
  | {
      type: "tool_start";
      call_id: string;
      tool: string;
      arguments: Record<string, unknown>;
      // Present when this call was invoked within a parent call (e.g. python_execute).
      parent_call_id?: string;
    }
  // Fires when a tool call finishes (success or error).
  | {
      type: "tool_end";
      call_id: string;
      tool: string;
      result: string | Record<string, unknown>;
      status: "success" | "error";
      duration_ms: number;
      truncated: boolean;
      total_bytes: number | null;
      // Mirrors parent_call_id from the corresponding tool_start.
      parent_call_id?: string;
    }
  // Fires once per turn after context assembly completes, before streaming begins.
  // Contains a diagnostic report that includes token budgets and any assembly errors.
  // Always delivered when the report has errors; otherwise requires a "context" subscription.
  | {
      type: "context_assembled";
      turn_id: string;
      report: Record<string, unknown>;
    }
  | {
      type: "output_schema_retry";
      turn_id: string;
      attempt: number;
      max_retries: number;
      errors: SchemaValidationError[];
    }
  | {
      type: "provider_retry_started";
      turn_id: string;
      attempt: number;
      max_retries: number;
      provider: string;
      wait_ms: number;
    }
  | {
      type: "provider_retry_settled";
      turn_id: string;
      attempt: number;
      max_retries: number;
      provider: string;
      success: boolean;
    }
  | {
      type: "output_schema_failed";
      turn_id: string;
      attempts: number;
      errors: SchemaValidationError[];
    }
  | {
    type: "delegation_started";
    parent_turn_id: string;
    child_turn_id: string | null;
    target_agent_id: string;
  }
  | {
    type: "delegation_completed";
    parent_turn_id: string;
    child_turn_id: string | null;
    target_agent_id: string;
    success: boolean;
  }
  | {
      type: "topology_run_started";
      session_id: string;
      topology_run_id: string;
      team_id: string;
    }
  | {
      type: "topology_node_started";
      session_id: string;
      topology_run_id: string;
      node_execution_id: string;
      node_id: string;
    }
  | {
      type: "topology_node_completed";
      session_id: string;
      topology_run_id: string;
      node_execution_id: string;
      node_id: string;
      success: boolean;
    }
  | {
      type: "topology_run_completed";
      session_id: string;
      topology_run_id: string;
      success: boolean;
      final_output?: Record<string, unknown>;
    }
  // Fired by the gateway before it shuts down. Clients should surface this as
  // a disconnection notice; the WebSocket will close shortly after.
  | {
      type: "shutdown";
      reason: string;
    }
  // A skill was successfully activated for the current session. Includes metadata
  // so the client can display the skill without a separate lookup.
  | {
      type: "skill_activated";
      name: string;
      display_name: string;
      description: string;
      token_count: number;
    }
  // A skill was already active — no state change occurred.
  | {
      type: "skill_already_active";
      name: string;
    }
  // A skill was successfully deactivated for the current session.
  | {
      type: "skill_deactivated";
      name: string;
    }
  | {
      type: "approval_required";
      approval_id: string;
      turn_id: string;
      agent_id: string;
      kind: string;
      tool_name?: string;
      tool_arguments?: Record<string, unknown>;
      prompt: string;
      context?: Record<string, unknown>;
      timeout_at: string;
      timeout_policy: string;
    }
  | {
      type: "approval_confirmed";
      approval_id: string;
      decision: string;
      reason?: string;
    }
  | {
      type: "approval_expired";
      approval_id: string;
      timeout_policy: string;
    }
  | {
      type: "approval_decision_error";
      approval_id: string;
      error: string;
      message: string;
      current_state?: string;
    };

export interface UnknownServerEvent {
  type: "unknown_event";
  original_type: string;
  raw: Record<string, unknown>;
}

export type ServerEvent = KnownServerEvent | UnknownServerEvent;

// -------------------------------------------------------------------------
// Connection state
// -------------------------------------------------------------------------

export type ConnectionStatus = "connected" | "disconnected" | "reconnecting" | "error";
