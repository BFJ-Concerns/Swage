// Streaming store — owns the current turn's streaming state.
//
// Accumulates tokens during an active turn, tracks whether the agent is
// generating, and controls the send/stop toggle state.

import type { ServerEvent, ToolCallView } from "../types";

export interface StreamingStore {
  readonly isStreaming: boolean;
  readonly streamedContent: string;
  readonly turnError: { code: string; message: string } | null;
  // Live tool calls for the current turn. Populated as tool_start/tool_end
  // events arrive. Cleared when the turn completes normally. Preserved (for
  // display) when a turn ends in error or is cancelled.
  readonly activeToolCalls: ToolCallView[];
  handleServerEvent(event: ServerEvent): void;
  startTurn(content: string): void;
  cancelTurn(): void;
}

// ---------------------------------------------------------------------------
// Injected dependencies
// ---------------------------------------------------------------------------

// Both dependencies are optional so the store can be constructed and tested
// in isolation. In the running app, these are bound at wiring time (App.svelte)
// to the singleton conversation and connection stores.

export interface StreamingStoreOptions {
  /** Receives messages pushed by the streaming store (user turns and completed
   *  or interrupted assistant turns). */
  conversationStore?: {
    addMessage(message: {
      role: string;
      content: string;
      timestamp: string;
      turn_state: string;
    }): void;
  };
  /** Sends WebSocket events for outgoing user messages and cancellation. */
  connectionStore?: {
    sendMessage(content: string): void;
    cancelTurn(): void;
  };
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

export function createStreamingStore(
  options?: StreamingStoreOptions,
): StreamingStore {
  let isStreaming = false;
  let streamedContent = "";
  let turnError: { code: string; message: string } | null = null;
  // Tracks in-flight and completed tool calls for the current turn.
  let activeToolCalls: ToolCallView[] = [];

  // ---- Helpers ----------------------------------------------------------------

  function pushMessage(
    role: "user" | "assistant",
    content: string,
    turn_state: string,
  ): void {
    options?.conversationStore?.addMessage({
      role,
      content,
      timestamp: new Date().toISOString(),
      turn_state,
    });
  }

  // Commits any accumulated content to the conversation store. Called when a
  // turn ends before the server explicitly completes it (error or cancellation).
  function preservePartialResponse(): void {
    if (streamedContent) {
      pushMessage("assistant", streamedContent, "interrupted");
    }
  }

  // ---- Public interface -------------------------------------------------------

  return {
    get isStreaming(): boolean {
      return isStreaming;
    },

    get streamedContent(): string {
      return streamedContent;
    },

    get turnError(): { code: string; message: string } | null {
      return turnError;
    },

    get activeToolCalls(): ToolCallView[] {
      return activeToolCalls;
    },

    handleServerEvent(event: ServerEvent): void {
      switch (event.type) {
        case "token":
          // Append streaming token — the UI renders this live.
          streamedContent += event.content;
          break;

        case "tool_start": {
          // A new tool call has begun. Build an in-progress placeholder.
          const startEntry: ToolCallView = {
            call_id: event.call_id,
            tool: event.tool,
            arguments: event.arguments,
            result: null,
            status: null,
            duration_ms: null,
            truncated: false,
            total_bytes: null,
            parent_call_id: event.parent_call_id ?? null,
          };

          if (event.parent_call_id) {
            // Sub-tool call (e.g. invoked from within python_execute) — nest it
            // under the parent rather than adding to the top-level list.
            activeToolCalls = activeToolCalls.map((call) =>
              call.call_id === event.parent_call_id
                ? { ...call, sub_calls: [...(call.sub_calls ?? []), startEntry] }
                : call,
            );
          } else {
            // Top-level call — append to the flat list as before.
            activeToolCalls = [...activeToolCalls, startEntry];
          }
          break;
        }

        case "tool_end":
          if (event.parent_call_id) {
            // Sub-tool completion — update within the parent's sub_calls.
            activeToolCalls = activeToolCalls.map((call) =>
              call.call_id === event.parent_call_id
                ? {
                    ...call,
                    sub_calls: (call.sub_calls ?? []).map((sub) =>
                      sub.call_id === event.call_id
                        ? {
                            ...sub,
                            result: event.result,
                            status: event.status,
                            duration_ms: event.duration_ms,
                            truncated: event.truncated,
                            total_bytes: event.total_bytes,
                          }
                        : sub,
                    ),
                  }
                : call,
            );
          } else {
            // Top-level tool call finished — update the matching entry.
            activeToolCalls = activeToolCalls.map((call) =>
              call.call_id === event.call_id
                ? {
                    ...call,
                    result: event.result,
                    status: event.status,
                    duration_ms: event.duration_ms,
                    truncated: event.truncated,
                    total_bytes: event.total_bytes,
                  }
                : call,
            );
          }
          break;

        case "turn_complete":
          // The gateway has committed the full turn. Push the completed
          // assistant message and return to idle state.
          if (streamedContent) {
            pushMessage("assistant", streamedContent, "completed");
          }
          streamedContent = "";
          turnError = null;
          isStreaming = false;
          // Tool calls are now captured in the message history — clear live state.
          activeToolCalls = [];
          break;

        case "error":
          // Turn failed mid-stream. Preserve whatever the user saw so the
          // conversation history reflects the partial response.
          preservePartialResponse();
          turnError = { code: event.code, message: event.message };
          isStreaming = false;
          // streamedContent is intentionally not cleared — it stays visible
          // alongside the error until the next startTurn clears it.
          // activeToolCalls are preserved so the UI can show what was
          // in-flight when the error occurred.
          break;

        case "output_schema_failed": {
          preservePartialResponse();
          const details = event.errors
            .map((error) =>
              error.path
                ? `${error.path}: ${error.message}`
                : error.message,
            )
            .join("; ");
          turnError = {
            code: "output_schema_failed",
            message: details
              ? `Structured output validation failed after ${event.attempts} attempts: ${details}`
              : `Structured output validation failed after ${event.attempts} attempts.`,
          };
          isStreaming = false;
          break;
        }

        case "cancelled":
          // User-initiated cancel. Preserve partial content (they may want
          // to read what was generated before they stopped it).
          preservePartialResponse();
          isStreaming = false;
          // activeToolCalls preserved — same rationale as the error case.
          break;

        case "shutdown":
          // The gateway is shutting down. Preserve any partial content and
          // surface the shutdown as a turn error so the UI can notify the user.
          // The WebSocket will close shortly after this event arrives.
          preservePartialResponse();
          turnError = { code: "gateway_shutdown", message: event.reason };
          isStreaming = false;
          break;

        case "context_assembled":
          // Diagnostic event carrying the context assembly report for this turn.
          // No action required in the streaming store — display is handled
          // separately. The case is listed explicitly to avoid a fall-through
          // and to make the handler exhaustive for future extensions.
          break;
      }
    },

    startTurn(content: string): void {
      // Clear state from any previous turn.
      streamedContent = "";
      turnError = null;
      activeToolCalls = [];
      isStreaming = true;

      // Immediately record the user's message so the conversation history
      // updates before the first token arrives.
      pushMessage("user", content, "completed");

      // Dispatch the outgoing message over the WebSocket.
      options?.connectionStore?.sendMessage(content);
    },

    cancelTurn(): void {
      options?.connectionStore?.cancelTurn();
    },
  };
}
