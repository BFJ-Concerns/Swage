// Streaming store — owns the current turn's streaming state.
//
// Accumulates tokens during an active turn, tracks whether the agent is
// generating, and controls the send/stop toggle state.

import { fromStore, get, writable } from "svelte/store";

import type { MessageView, ServerEvent, ToolCallView } from "../types";

// Approval state surfaced for the ApprovalCard / MessageInput swap. Mirrors
// the workup's "approval replaces input" view (riso-cyanotype.html, view 03).
// Fields match the `approval_required` server event so the UI can render
// without an extra fetch.
export interface PendingApproval {
  approval_id: string;
  turn_id: string;
  agent_id: string;
  kind: string;
  tool_name: string | null;
  prompt: string;
  timeout_at: string;
  timeout_policy: string;
}

export interface StreamingStore {
  readonly isStreaming: boolean;
  readonly streamedContent: string;
  readonly turnError: { code: string; message: string } | null;
  readonly turnStartedAt: string | null;
  readonly lastInputTokens: number | null;
  readonly lastOutputTokens: number | null;
  // Live tool calls for the current turn. Populated as tool_start/tool_end
  // events arrive. Cleared when the turn completes normally. Preserved (for
  // display) when a turn ends in error or is cancelled.
  readonly activeToolCalls: ToolCallView[];
  // The most recently raised approval still awaiting a decision. Cleared
  // when an approval_confirmed/expired/decision_error event arrives.
  // The ApprovalCard reads this and replaces the input box while it is set,
  // matching the Riso workup view 03.
  readonly pendingApproval: PendingApproval | null;
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
    appendOptimisticUserMessage(message: {
      content: string;
      timestamp: string;
    }): void;
    appendAssistantMessage(message: {
      content: string;
      timestamp: string;
      turn_state: Extract<MessageView["turn_state"], "completed" | "interrupted">;
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
  const isStreamingStore = writable(false);
  const streamedContentStore = writable("");
  const turnErrorStore = writable<{ code: string; message: string } | null>(null);
  const turnStartedAtStore = writable<string | null>(null);
  const lastInputTokensStore = writable<number | null>(null);
  const lastOutputTokensStore = writable<number | null>(null);
  // Tracks in-flight and completed tool calls for the current turn.
  const activeToolCallsStore = writable<ToolCallView[]>([]);
  // Currently active approval awaiting user decision; null when none.
  const pendingApprovalStore = writable<PendingApproval | null>(null);

  const isStreamingState = fromStore(isStreamingStore);
  const streamedContentState = fromStore(streamedContentStore);
  const turnErrorState = fromStore(turnErrorStore);
  const turnStartedAtState = fromStore(turnStartedAtStore);
  const lastInputTokensState = fromStore(lastInputTokensStore);
  const lastOutputTokensState = fromStore(lastOutputTokensStore);
  const activeToolCallsState = fromStore(activeToolCallsStore);
  const pendingApprovalState = fromStore(pendingApprovalStore);

  // ---- Helpers ----------------------------------------------------------------

  function pushAssistantMessage(
    content: string,
    turn_state: Extract<MessageView["turn_state"], "completed" | "interrupted">,
  ): void {
    options?.conversationStore?.appendAssistantMessage({
      content,
      timestamp: new Date().toISOString(),
      turn_state,
    });
  }

  // Commits any accumulated content to the conversation store. Called when a
  // turn ends before the server explicitly completes it (error or cancellation).
  function preservePartialResponse(): void {
    const streamedContent = get(streamedContentStore);
    if (streamedContent) {
      pushAssistantMessage(streamedContent, "interrupted");
    }
  }

  // ---- Public interface -------------------------------------------------------

  return {
    get isStreaming(): boolean {
      return isStreamingState.current;
    },

    get streamedContent(): string {
      return streamedContentState.current;
    },

    get turnError(): { code: string; message: string } | null {
      return turnErrorState.current;
    },

    get turnStartedAt(): string | null {
      return turnStartedAtState.current;
    },

    get lastInputTokens(): number | null {
      return lastInputTokensState.current;
    },

    get lastOutputTokens(): number | null {
      return lastOutputTokensState.current;
    },

    get activeToolCalls(): ToolCallView[] {
      return activeToolCallsState.current;
    },

    get pendingApproval(): PendingApproval | null {
      return pendingApprovalState.current;
    },

    handleServerEvent(event: ServerEvent): void {
      switch (event.type) {
        case "token":
          // Append streaming token — the UI renders this live.
          streamedContentStore.update((streamedContent) => streamedContent + event.content);
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
            activeToolCallsStore.update((activeToolCalls) =>
              activeToolCalls.map((call) =>
                call.call_id === event.parent_call_id
                  ? { ...call, sub_calls: [...(call.sub_calls ?? []), startEntry] }
                  : call,
              ),
            );
          } else {
            // Top-level call — append to the flat list as before.
            activeToolCallsStore.update((activeToolCalls) => [
              ...activeToolCalls,
              startEntry,
            ]);
          }
          break;
        }

        case "tool_end":
          if (event.parent_call_id) {
            // Sub-tool completion — update within the parent's sub_calls.
            activeToolCallsStore.update((activeToolCalls) =>
              activeToolCalls.map((call) =>
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
              ),
            );
          } else {
            // Top-level tool call finished — update the matching entry.
            activeToolCallsStore.update((activeToolCalls) =>
              activeToolCalls.map((call) =>
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
              ),
            );
          }
          break;

        case "turn_complete":
          // The gateway has committed the full turn. Push the completed
          // assistant message and return to idle state.
          {
            const streamedContent = get(streamedContentStore);
          if (streamedContent) {
            pushAssistantMessage(streamedContent, "completed");
          }
          }
          streamedContentStore.set("");
          turnErrorStore.set(null);
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
          lastInputTokensStore.set(event.input_tokens ?? null);
          lastOutputTokensStore.set(event.output_tokens ?? null);
          // Tool calls are now captured in the message history — clear live state.
          activeToolCallsStore.set([]);
          break;

        case "error":
          // Turn failed mid-stream. Preserve whatever the user saw so the
          // conversation history reflects the partial response.
          preservePartialResponse();
          turnErrorStore.set({ code: event.code, message: event.message });
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
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
          turnErrorStore.set({
            code: "output_schema_failed",
            message: details
              ? `Structured output validation failed after ${event.attempts} attempts: ${details}`
              : `Structured output validation failed after ${event.attempts} attempts.`,
          });
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
          break;
        }

        case "cancelled":
          // User-initiated cancel. Preserve partial content (they may want
          // to read what was generated before they stopped it).
          preservePartialResponse();
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
          // activeToolCalls preserved — same rationale as the error case.
          break;

        case "shutdown":
          // The gateway is shutting down. Preserve any partial content and
          // surface the shutdown as a turn error so the UI can notify the user.
          // The WebSocket will close shortly after this event arrives.
          preservePartialResponse();
          turnErrorStore.set({ code: "gateway_shutdown", message: event.reason });
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
          break;

        case "approval_required":
          // Surface the request so MessageInput can swap itself out for the
          // ApprovalCard (workup view 03). We retain only the latest active
          // approval; cross-conversation approval queues are out of scope per
          // Progress.md.
          pendingApprovalStore.set({
            approval_id: event.approval_id,
            turn_id: event.turn_id,
            agent_id: event.agent_id,
            kind: event.kind,
            tool_name: event.tool_name ?? null,
            prompt: event.prompt,
            timeout_at: event.timeout_at,
            timeout_policy: event.timeout_policy,
          });
          break;

        case "approval_confirmed":
        case "approval_expired":
        case "approval_decision_error":
          // Approval resolved or aborted — clear the pending state so the
          // input returns. The approval API responds via fetch; the WS event
          // is an authoritative signal that the gate has closed.
          pendingApprovalStore.set(null);
          break;

        case "context_assembled":
        case "output_schema_retry":
        case "delegation_started":
        case "delegation_completed":
        case "topology_run_started":
        case "topology_node_started":
        case "topology_node_completed":
        case "topology_run_completed":
          // Diagnostic event carrying the context assembly report for this turn.
          // No action required in the streaming store — display is handled
          // separately. The case is listed explicitly to avoid a fall-through
          // and to make the handler exhaustive for future extensions.
          break;

        default:
          // Forward-compatible: ignore gateway events this store does not
          // understand yet rather than pretending they were impossible.
          break;
      }
    },

    startTurn(content: string): void {
      // Clear state from any previous turn.
      streamedContentStore.set("");
      turnErrorStore.set(null);
      activeToolCallsStore.set([]);
      lastInputTokensStore.set(null);
      lastOutputTokensStore.set(null);
      turnStartedAtStore.set(new Date().toISOString());
      isStreamingStore.set(true);

      // Immediately record the user's message so the conversation history
      // updates before the first token arrives.
      options?.conversationStore?.appendOptimisticUserMessage({
        content,
        timestamp: new Date().toISOString(),
      });

      // Dispatch the outgoing message over the WebSocket.
      options?.connectionStore?.sendMessage(content);
    },

    cancelTurn(): void {
      options?.connectionStore?.cancelTurn();
    },
  };
}
