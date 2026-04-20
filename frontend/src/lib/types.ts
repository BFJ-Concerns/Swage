// Shared TypeScript types for the Swage web interface.
// These mirror the gateway's API response shapes.

// -------------------------------------------------------------------------
// Project types
// -------------------------------------------------------------------------

/** A project that groups related conversations and has a workspace directory. */
export interface Project {
  id: string;
  name: string;
  is_default: boolean;
  slug: string;
  workspace_path: string;
  created_at: string;
}

/** Request body for creating a new project. */
export interface CreateProjectRequest {
  name: string;
  workspace_path: string;
}

/** Request body for updating an existing project. All fields are optional. */
export interface UpdateProjectRequest {
  name?: string;
  workspace_path?: string;
}

/** Compact conversation entry for the list view. */
export interface ConversationSummary {
  id: string;
  created_at: string;
  last_activity: string;
  message_preview: string | null;
  has_interrupted_turn: boolean;
}

/** Full conversation with message history. */
export interface ConversationDetail {
  id: string;
  project_id: string;
  created_at: string;
  last_activity: string;
  messages: MessageView[];
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
  // Present on every message in the conversation API; empty when no tool calls
  // were recorded for that message. Kept optional here so local client-created
  // message objects do not need to synthesise it eagerly.
  tool_calls?: ToolCallView[];
}

/** Response from creating a conversation. */
export interface ConversationResponse {
  id: string;
  project_id: string;
  created_at: string;
  last_activity: string;
}

// -------------------------------------------------------------------------
// Lens runtime types
// -------------------------------------------------------------------------

export type AppLens = string;
export type LensSurface = "conversation" | "operator" | "builder";
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
  active_lens: AppLens;
  updated_at: string;
  available_lenses: LensDescriptor[];
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
  project_id: string;
  name: string;
  description: string;
  entry_agent_id: string;
  members: string[];
  created_at: string;
  updated_at: string;
}

export interface CreateTeamRequest {
  name: string;
  description: string;
  entry_agent_id: string;
  members: string[];
}

export interface UpdateTeamRequest {
  name?: string;
  description?: string;
  entry_agent_id?: string;
  members?: string[];
}

export type ProjectExtensionStatus = "active" | "approval_required";
export type ProjectExtensionSourceKind = "managed" | "workspace_candidate";

export interface ProjectExtensionSummary {
  name: string;
  display_name: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
  status: ProjectExtensionStatus;
  source_kind: ProjectExtensionSourceKind;
}

export interface CreateProjectExtensionRequest {
  name: string;
  display_name?: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
}

export interface UpdateProjectExtensionRequest {
  display_name?: string | null;
  description: string;
  requested_tools: string[];
  instructions: string;
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
      kind: "create_project_extension";
      name: string;
      display_name?: string | null;
      description: string;
      requested_tools: string[];
      instructions: string;
    }
  | {
      kind: "update_project_extension";
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
      instructions?: string | null;
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

export type SharedReferenceKind = "team_topology" | "context_snapshot";

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
  project_id: string;
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
  project_id: string;
  project_name: string;
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

export interface HeartbeatSummary {
  id: string;
  session_id: string;
  message: string;
  status: string;
  interval_seconds: number;
  next_beat_at: string | null;
  last_beat_at: string | null;
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
  | { type: "send_message"; content: string; thread_id?: string }
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
