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
export type { UpdateWorkspaceTrustRequest } from "./protocol/UpdateWorkspaceTrustRequest";

export type Space = SpaceResponse & {
  /** Client-side recency marker until the gateway persists last_opened_at. */
  last_opened_at?: string | null;
};
export type WorkspaceAttachment = WorkspaceAttachmentResponse;
export type { ProviderProtocol } from "./protocol/ProviderProtocol";
export type { ToolRuntimeSettings } from "./protocol/ToolRuntimeSettings";
export type { PythonExecuteToolConfig } from "./protocol/PythonExecuteToolConfig";
export type { SearchProviderConfig } from "./protocol/SearchProviderConfig";
export type { PricingConfig } from "./protocol/PricingConfig";
export type { ModelPricing } from "./protocol/ModelPricing";
export type { ObservabilityConfig } from "./protocol/ObservabilityConfig";
export type { CommunicationConfig } from "./protocol/CommunicationConfig";
export type { ContextTransferConfig } from "./protocol/ContextTransferConfig";
export type { ContextTransferTargetConfig } from "./protocol/ContextTransferTargetConfig";
export type { ContextTransferMode } from "./protocol/ContextTransferMode";
export type { ContextTransferField } from "./protocol/ContextTransferField";
export type { ToolPermissionConfig } from "./protocol/ToolPermissionConfig";
export type { ApprovalTimeoutPolicy } from "./protocol/ApprovalTimeoutPolicy";
export type { RequireApprovalEntry } from "./protocol/RequireApprovalEntry";
export type { HitlConfig } from "./protocol/HitlConfig";
export type { ContextConfig } from "./protocol/ContextConfig";
export type { DiscoveredModel } from "./protocol/DiscoveredModel";
export type { ModelDiscoveryResponse } from "./protocol/ModelDiscoveryResponse";
export type { RestartStatusResponse } from "./protocol/RestartStatusResponse";

import type { ProviderSettings as WireProviderSettings } from "./protocol/ProviderSettings";
import type { ModelSettings as WireModelSettings } from "./protocol/ModelSettings";
import type { GenerationPresetConfig as WireGenerationPresetConfig } from "./protocol/GenerationPresetConfig";
import type { SandboxConfig as WireSandboxConfig } from "./protocol/SandboxConfig";
import type { SettingsResponse as WireSettingsResponse } from "./protocol/SettingsResponse";
import type { SaveSettingsRequest as WireSaveSettingsRequest } from "./protocol/SaveSettingsRequest";

export type ProviderSettings = Omit<
  WireProviderSettings,
  "api_key_secret_ref" | "api_key_env" | "command" | "state_dir"
> & {
  api_key_secret_ref?: string | null;
  api_key_env?: string | null;
  api_key?: string;
  command?: string | null;
  state_dir?: string | null;
};
export type ModelSettings = Omit<WireModelSettings, "context_window" | "capabilities"> & {
  context_window?: number | null;
  capabilities?: WireModelSettings["capabilities"];
};
export type GenerationPresetConfig = Omit<
  WireGenerationPresetConfig,
  "max_tokens" | "temperature" | "metadata"
> & {
  max_tokens?: number | null;
  temperature?: number | null;
  metadata?: unknown | null;
};
export type SandboxConfig = WireSandboxConfig;
export type SettingsResponse = Omit<
  WireSettingsResponse,
  "providers" | "models" | "generation_presets" | "sandbox"
> & {
  providers: Record<string, ProviderSettings>;
  models: Record<string, ModelSettings>;
  generation_presets: Record<string, GenerationPresetConfig>;
  sandbox: SandboxConfig;
};
export type SettingsPayload = Omit<
  WireSaveSettingsRequest,
  "providers" | "models" | "generation_presets" | "sandbox"
> & {
  providers: Record<string, ProviderSettings>;
  models: Record<string, ModelSettings>;
  generation_presets: Record<string, GenerationPresetConfig>;
  sandbox: SandboxConfig;
};
export type ProviderDiscoveryDraft = Omit<ProviderSettings, "has_credential" | "api_key">;

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
// Materials and attachments
// -------------------------------------------------------------------------

export type MaterialScopeKind = "session" | "space";
export type { MaterialResponse as MaterialRecord } from "./protocol/MaterialResponse";
export type { AttachmentResponse as AttachmentRecord } from "./protocol/AttachmentResponse";
export type { BackgroundJobResponse as BackgroundJobRecord } from "./protocol/BackgroundJobResponse";
export type { QueuedAttachmentResponse as AttachmentQueuedResponse } from "./protocol/QueuedAttachmentResponse";

import type { CreateAttachmentRequest as WireCreateAttachmentRequest } from "./protocol/CreateAttachmentRequest";
import type { CreateMaterialRequest as WireCreateMaterialRequest } from "./protocol/CreateMaterialRequest";
export type CreateAttachmentRequest = Omit<WireCreateAttachmentRequest, "scope_kind" | "session_id" | "space_id"> & {
  scope_kind: MaterialScopeKind;
  session_id?: string;
  space_id?: string;
};
export type CreateMaterialRequest = Omit<WireCreateMaterialRequest, "scope_kind" | "session_id" | "space_id" | "mime_type"> & {
  scope_kind: MaterialScopeKind;
  session_id?: string;
  space_id?: string;
  mime_type?: string;
};

// -------------------------------------------------------------------------
// Operator surface types
// -------------------------------------------------------------------------

export type { OperatorGatewaySummary } from "./protocol/OperatorGatewaySummary";
export type { OperatorSessionSummary } from "./protocol/OperatorSessionSummary";
export type { OperatorDeviceSummary } from "./protocol/OperatorDeviceSummary";
export type { ChannelStatusResponse as ChannelStatusSummary } from "./protocol/ChannelStatusResponse";
export type { ApprovalSummary as OperatorApprovalSummary } from "./protocol/ApprovalSummary";
export type { OperatorOverviewResponse as OperatorOverview } from "./protocol/OperatorOverviewResponse";

export type { RecoveryExportSummary } from "./protocol/RecoveryExportSummary";
export type { RecoveryExportListResponse } from "./protocol/RecoveryExportListResponse";

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
  "workflow_run_started", "workflow_entry_started",
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
