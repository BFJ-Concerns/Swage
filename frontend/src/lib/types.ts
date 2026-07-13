// Shared frontend types. Wire-contract types are generated from swage-protocol;
// this module remains a compatibility barrel while the non-wire UI state below
// stays authored locally.

import type { SpaceResponse } from "./protocol/SpaceResponse";
import type { WorkspaceAttachmentResponse } from "./protocol/WorkspaceAttachmentResponse";

import type { CreateSpaceRequest as WireCreateSpaceRequest } from "./protocol/CreateSpaceRequest";
import type { UpdateSpaceRequest as WireUpdateSpaceRequest } from "./protocol/UpdateSpaceRequest";
import type { CreateWorkspaceAttachmentRequest as WireCreateWorkspaceAttachmentRequest } from "./protocol/CreateWorkspaceAttachmentRequest";

export type CreateSpaceRequest = Omit<WireCreateSpaceRequest, "description"> & { description?: string };
export type UpdateSpaceRequest = WireUpdateSpaceRequest;
export type CreateWorkspaceAttachmentRequest = Omit<
  WireCreateWorkspaceAttachmentRequest,
  "set_as_default" | "permissions" | "is_trusted"
> & {
  set_as_default?: boolean;
  permissions?: string[];
  is_trusted?: boolean;
};
export interface UpdateWorkspaceTrustRequest { is_trusted: boolean }

export type Space = SpaceResponse & {
  /** Client-side recency marker until the gateway persists last_opened_at. */
  last_opened_at?: string | null;
};
export type WorkspaceAttachment = WorkspaceAttachmentResponse;
export type ProviderProtocol = "openai" | "anthropic" | "google" | "openrouter" | "codex";

export interface ProviderSettings {
  protocol: ProviderProtocol;
  base_url?: string | null;
  api_key_secret_ref?: string | null;
  api_key_env?: string | null;
  has_credential: boolean;
  api_key?: string;
  command?: string | null;
  state_dir?: string | null;
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

import type { ConversationSummary as WireConversationSummary } from "./protocol/ConversationSummary";
import type { ConversationDetail as WireConversationDetail } from "./protocol/ConversationDetail";
import type { ConversationResponse as WireConversationResponse } from "./protocol/ConversationResponse";
import type { MessageView as WireMessageView } from "./protocol/MessageView";
import type { ToolCallView as WireToolCallView } from "./protocol/ToolCallView";

export type ConversationSummary = Omit<WireConversationSummary, "selected_model_id"> & {
  selected_model_id?: string | null;
};
export type ConversationResponse = Omit<WireConversationResponse, "selected_model_id"> & {
  selected_model_id?: string | null;
};
export type ConversationDetail = Omit<WireConversationDetail, "messages" | "selected_model_id"> & {
  selected_model_id?: string | null;
  messages: MessageView[];
  input_tokens?: number | null;
  output_tokens?: number | null;
};

export type ToolCallView = Omit<
  WireToolCallView,
  "arguments" | "result" | "status" | "duration_ms" | "total_bytes" | "parent_call_id"
> & {
  arguments: Record<string, unknown>;
  result: string | Record<string, unknown> | null;
  status: string | null;
  duration_ms: number | null;
  total_bytes: number | null;
  parent_call_id?: string | null;
  sub_calls?: ToolCallView[];
};

export type MessageView = Omit<WireMessageView, "role" | "tool_calls" | "thread_id" | "turn_id"> & {
  thread_id?: string;
  turn_id?: string | null;
  role: "user" | "assistant";
  model?: string | null;
  tool_calls?: ToolCallView[];
};
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
  space_id: string;
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
  space_id: string;
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

export type SpaceExtensionStatus = "active" | "approval_required";
export type SpaceExtensionSourceKind = "managed" | "workspace_candidate";

export interface SpaceExtensionSummary {
  name: string;
  display_name: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
  status: SpaceExtensionStatus;
  source_kind: SpaceExtensionSourceKind;
}

export interface CreateSpaceExtensionRequest {
  name: string;
  display_name?: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
}

export interface UpdateSpaceExtensionRequest {
  display_name?: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
}

// -------------------------------------------------------------------------
// Materials and attachments
// -------------------------------------------------------------------------

export type MaterialScopeKind = "session" | "space";

export interface MaterialRecord {
  id: string;
  scope_kind: MaterialScopeKind;
  session_id: string | null;
  space_id: string | null;
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
  space_id: string | null;
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
  space_id?: string;
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
  space_id?: string;
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
      kind: "create_space_extension";
      name: string;
      display_name?: string | null;
      description: string;
      requested_tools: string[];
      instructions: string;
    }
  | {
      kind: "update_space_extension";
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
  space_id: string;
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

export type { OperatorGatewaySummary } from "./protocol/OperatorGatewaySummary";
export type { OperatorSessionSummary } from "./protocol/OperatorSessionSummary";
export type { OperatorDeviceSummary } from "./protocol/OperatorDeviceSummary";
export type { ChannelStatusResponse as ChannelStatusSummary } from "./protocol/ChannelStatusResponse";
export type { ApprovalSummary as OperatorApprovalSummary } from "./protocol/ApprovalSummary";
export type { OperatorOverviewResponse as OperatorOverview } from "./protocol/OperatorOverviewResponse";

export interface RecoveryExportSummary {
  id: string;
  created_at: string;
  size_bytes: number;
  spaces: number;
  workspaces: number;
  sessions: number;
  secrets: number;
  channel_links: number;
  devices: number;
  schedules: number;
}

export interface RecoveryExportListResponse {
  exports: RecoveryExportSummary[];
}

// -------------------------------------------------------------------------
// WebSocket event types
// -------------------------------------------------------------------------

import type { ClientEvent as WireClientEvent } from "./protocol/ClientEvent";
import type { ServerEvent as WireServerEvent } from "./protocol/ServerEvent";
export type { SchemaValidationError } from "./protocol/SchemaValidationError";

export type ClientEvent = WireClientEvent;
export type KnownServerEvent = Exclude<WireServerEvent, { type: "unknown" }>;
export type KnownServerEventType = KnownServerEvent["type"];

// Runtime validation still needs values. The type assertion makes additions to
// the generated union fail the frontend check until this parser list follows.
export const KNOWN_SERVER_EVENT_TYPES = [
  "token", "tool_start", "tool_end", "context_assembled", "turn_complete",
  "output_schema_retry", "provider_retry_started", "provider_retry_settled",
  "output_schema_failed", "delegation_started", "delegation_completed",
  "topology_run_started", "topology_node_started", "topology_node_completed",
  "topology_run_completed", "workflow_run_started", "workflow_entry_started",
  "workflow_entry_completed", "workflow_run_completed", "error", "cancelled",
  "shutdown", "skill_activated", "skill_already_active", "skill_deactivated",
  "approval_required", "approval_confirmed", "approval_expired",
  "approval_decision_error",
] as const satisfies readonly KnownServerEventType[];

export type UnknownServerEvent = Extract<WireServerEvent, { type: "unknown" }>;
export type ServerEvent = WireServerEvent;
// -------------------------------------------------------------------------
// Connection state
// -------------------------------------------------------------------------

export type ConnectionStatus = "connected" | "disconnected" | "reconnecting" | "error";
