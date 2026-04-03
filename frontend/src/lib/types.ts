// Shared TypeScript types for the Swage web interface.
// These mirror the gateway's API response shapes.

// -------------------------------------------------------------------------
// Project types
// -------------------------------------------------------------------------

/** A project that groups related conversations and has a workspace directory. */
export interface Project {
  id: string;
  name: string;
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
  role: "user" | "assistant";
  content: string;
  timestamp: string;
  turn_state: "completed" | "interrupted" | "failed" | "in_progress";
  // Present only on assistant messages that invoked tools.
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
// WebSocket event types (the subset currently modelled by the web client)
// -------------------------------------------------------------------------

/** Client → Server events. */
export type ClientEvent =
  | { type: "send_message"; content: string }
  | { type: "cancel" }
  // Subscribe to one or more observability channels (e.g. "tools", "context").
  | { type: "subscribe"; channels: string[] }
  // Unsubscribe from previously subscribed channels.
  | { type: "unsubscribe"; channels: string[] };

/** Server → Client events. */
export type ServerEvent =
  | { type: "token"; content: string }
  | { type: "turn_complete"; turn_id: string }
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
    };

// -------------------------------------------------------------------------
// Connection state
// -------------------------------------------------------------------------

export type ConnectionStatus = "connected" | "disconnected" | "reconnecting" | "error";
