// Streaming store — owns the current turn's streaming state.
//
// Accumulates tokens during an active turn, tracks whether the agent is
// generating, and controls the send/stop toggle state.

import { fromStore, get, writable } from "svelte/store";

import type { MessageView, ServerEvent, ToolCallView } from "../types";

// Approval state surfaced for the ApprovalCard / MessageInput swap.
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

export interface ProviderRetryState {
  turn_id: string;
  attempt: number;
  max_retries: number;
  provider: string;
  wait_ms: number;
  status: "waiting" | "resuming" | "failed";
}

export interface StreamingStore {
  readonly isStreaming: boolean;
  readonly streamedContent: string;
  readonly turnError: { code: string; message: string } | null;
  readonly turnStartedAt: string | null;
  // Live tool calls for the current turn. Populated as tool_start/tool_end
  // events arrive. Cleared when the turn completes normally. Preserved (for
  // display) when a turn ends in error or is cancelled.
  readonly activeToolCalls: ToolCallView[];
  readonly providerRetry: ProviderRetryState | null;
  // The most recently raised approval still awaiting a decision. Cleared
  // when an approval_confirmed/expired/decision_error event arrives.
  // The ApprovalCard reads this and replaces the input box while it is set,
  // matching the approval UI.
  readonly pendingApproval: PendingApproval | null;
  handleServerEvent(event: ServerEvent): void;
  reconcileAfterReconnect(): void;
  startTurn(content: string, selectedModelId?: string | null, agentId?: string | null): void;
  cancelTurn(): void;
}

// ---------------------------------------------------------------------------
// Injected dependencies
// ---------------------------------------------------------------------------

// Both dependencies are optional so the store can be constructed and tested
// in isolation. In the running app, these are bound at wiring time (App.svelte)
// to the singleton conversation and connection stores.

export interface StreamingStoreOptions {
  /** Receives messages pushed by the streaming store (user turns and settled
   *  assistant turns). */
  conversationStore?: {
    readonly activeConversation?: { messages: MessageView[] } | null;
    appendOptimisticUserMessage(message: {
      content: string;
      timestamp: string;
    }): void;
    appendAssistantMessage(message: {
      content: string;
      timestamp: string;
      turn_state: Extract<
        MessageView["turn_state"],
        "completed" | "failed" | "interrupted"
      >;
      turn_id?: string;
      tool_calls?: ToolCallView[];
    }): void;
  };
  /** Sends WebSocket events for outgoing user messages and cancellation. */
  connectionStore?: {
    sendMessage(content: string, selectedModelId?: string | null): void;
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
  // Tracks in-flight and completed tool calls for the current turn.
  const activeToolCallsStore = writable<ToolCallView[]>([]);
  const providerRetryStore = writable<ProviderRetryState | null>(null);
  // Currently active approval awaiting user decision; null when none.
  const pendingApprovalStore = writable<PendingApproval | null>(null);

  const isStreamingState = fromStore(isStreamingStore);
  const streamedContentState = fromStore(streamedContentStore);
  const turnErrorState = fromStore(turnErrorStore);
  const turnStartedAtState = fromStore(turnStartedAtStore);
  const activeToolCallsState = fromStore(activeToolCallsStore);
  const providerRetryState = fromStore(providerRetryStore);
  const pendingApprovalState = fromStore(pendingApprovalStore);

  // ---- Helpers ----------------------------------------------------------------

  function pushAssistantMessage(
    content: string,
    turn_state: Extract<
      MessageView["turn_state"],
      "completed" | "failed" | "interrupted"
    >,
    turn_id?: string,
    tool_calls?: ToolCallView[],
  ): void {
    options?.conversationStore?.appendAssistantMessage({
      content,
      timestamp: new Date().toISOString(),
      turn_state,
      ...(turn_id === undefined ? {} : { turn_id }),
      ...(tool_calls === undefined ? {} : { tool_calls }),
    });
  }

  // Commits any accumulated content to the conversation store. Called when a
  // turn ends before the server explicitly completes it (error or cancellation).
  function settlePartialResponse(
    turnState: Extract<MessageView["turn_state"], "failed" | "interrupted">,
  ): void {
    const streamedContent = get(streamedContentStore);
    if (streamedContent) {
      pushAssistantMessage(streamedContent, turnState);
    }
    streamedContentStore.set("");
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

    get activeToolCalls(): ToolCallView[] {
      return activeToolCallsState.current;
    },

    get providerRetry(): ProviderRetryState | null {
      return providerRetryState.current;
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
            const activeToolCalls = get(activeToolCallsStore);
            if (streamedContent || activeToolCalls.length > 0) {
              pushAssistantMessage(
                streamedContent,
                "completed",
                event.turn_id,
                activeToolCalls,
              );
            }
          }
          streamedContentStore.set("");
          turnErrorStore.set(null);
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
          // Tool calls are now captured in the message history — clear live state.
          activeToolCallsStore.set([]);
          providerRetryStore.set(null);
          break;

        case "error":
          // Turn failed mid-stream. Preserve whatever the user saw so the
          // conversation history reflects the partial response.
          settlePartialResponse("failed");
          turnErrorStore.set({ code: event.code, message: event.message });
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
          // activeToolCalls are preserved so the UI can show what was
          // in-flight when the error occurred.
          providerRetryStore.set(null);
          break;

        case "output_schema_failed": {
          settlePartialResponse("failed");
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
          providerRetryStore.set(null);
          break;
        }

        case "cancelled":
          // User-initiated cancel. Preserve partial content (they may want
          // to read what was generated before they stopped it).
          settlePartialResponse("interrupted");
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
          // activeToolCalls preserved — same rationale as the error case.
          providerRetryStore.set(null);
          break;

        case "shutdown":
          // The gateway is shutting down. Preserve any partial content and
          // surface the shutdown as a turn error so the UI can notify the user.
          // The WebSocket will close shortly after this event arrives.
          settlePartialResponse("failed");
          turnErrorStore.set({ code: "gateway_shutdown", message: event.reason });
          isStreamingStore.set(false);
          turnStartedAtStore.set(null);
          providerRetryStore.set(null);
          break;

        case "provider_retry_started":
          providerRetryStore.set({
            turn_id: event.turn_id,
            attempt: event.attempt,
            max_retries: event.max_retries,
            provider: event.provider,
            wait_ms: event.wait_ms,
            status: "waiting",
          });
          break;

        case "provider_retry_settled":
          providerRetryStore.set({
            turn_id: event.turn_id,
            attempt: event.attempt,
            max_retries: event.max_retries,
            provider: event.provider,
            wait_ms: 0,
            status: event.success ? "resuming" : "failed",
          });
          if (event.success) {
            globalThis.setTimeout(() => {
              const current = get(providerRetryStore);
              if (current?.turn_id === event.turn_id && current.attempt === event.attempt) {
                providerRetryStore.set(null);
              }
            }, 1_200);
          }
          break;

        case "approval_required":
          // Surface the request so MessageInput can swap itself out for the
          // ApprovalCard. We retain only the latest active approval;
          // cross-conversation approval queues are not modelled here.
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

    reconcileAfterReconnect(): void {
      const lastMessage = options?.conversationStore?.activeConversation?.messages.at(-1);
      if (
        !get(isStreamingStore) ||
        lastMessage?.role !== "assistant" ||
        !["completed", "failed", "interrupted"].includes(lastMessage.turn_state ?? "")
      ) return;

      // The HTTP refresh recovered the terminal assistant message that the
      // dropped socket could not deliver. Discard the stale live projection;
      // the durable conversation is now authoritative.
      streamedContentStore.set("");
      isStreamingStore.set(false);
      turnStartedAtStore.set(null);
      activeToolCallsStore.set([]);
      providerRetryStore.set(null);
      pendingApprovalStore.set(null);
    },

    startTurn(content: string, selectedModelId?: string | null): void {
      // Clear state from any previous turn.
      streamedContentStore.set("");
      turnErrorStore.set(null);
      activeToolCallsStore.set([]);
      providerRetryStore.set(null);
      turnStartedAtStore.set(new Date().toISOString());
      isStreamingStore.set(true);

      // Immediately record the user's message so the conversation history
      // updates before the first token arrives.
      options?.conversationStore?.appendOptimisticUserMessage({
        content,
        timestamp: new Date().toISOString(),
      });

      // Dispatch the outgoing message over the WebSocket.
      options?.connectionStore?.sendMessage(content, selectedModelId);
    },

    cancelTurn(): void {
      options?.connectionStore?.cancelTurn();
    },
  };
}

// ---------------------------------------------------------------------------
// Conversation auto-create wrapper
// ---------------------------------------------------------------------------

export interface ConversationAutoCreateOptions {
  /** Whether a conversation is currently in scope to receive the turn. */
  hasActiveConversation(): boolean;
  /**
   * Create (and connect to) a conversation for the turn. Resolves to the new
   * conversation's id, or null when creation failed. The WebSocket client
   * queues frames during the handshake, so the turn can be dispatched as soon
   * as this resolves without racing the connection.
   */
  createConversation(selectedModelId?: string | null, agentId?: string | null): Promise<string | null>;
}

// Wraps a streaming store so the composer works in a space with no
// conversation yet: startTurn transparently creates one, connects, then
// dispatches. Every other member delegates to the wrapped store.
export function withConversationAutoCreate(
  base: StreamingStore,
  options: ConversationAutoCreateOptions,
): StreamingStore {
  // True while a conversation is being created for a pending turn. Folded
  // into isStreaming so the composer's in-flight guard also covers the
  // creation window (no second send, draft preserved).
  const isCreatingStore = writable(false);
  const isCreatingState = fromStore(isCreatingStore);

  return {
    get isStreaming(): boolean {
      return isCreatingState.current || base.isStreaming;
    },
    get streamedContent(): string {
      return base.streamedContent;
    },
    get turnError(): { code: string; message: string } | null {
      return base.turnError;
    },
    get turnStartedAt(): string | null {
      return base.turnStartedAt;
    },
    get activeToolCalls(): ToolCallView[] {
      return base.activeToolCalls;
    },
    get providerRetry(): ProviderRetryState | null {
      return base.providerRetry;
    },
    get pendingApproval(): PendingApproval | null {
      return base.pendingApproval;
    },
    handleServerEvent: (event) => base.handleServerEvent(event),
    reconcileAfterReconnect: () => base.reconcileAfterReconnect(),
    cancelTurn: () => base.cancelTurn(),

    startTurn(content: string, selectedModelId?: string | null, agentId?: string | null): void {
      if (options.hasActiveConversation()) {
        base.startTurn(content, selectedModelId);
        return;
      }
      if (get(isCreatingStore)) return;
      isCreatingStore.set(true);

      void (async () => {
        try {
          // A rejection counts as a failed creation, same as a null id.
          const id = await options
            .createConversation(selectedModelId, agentId)
            .catch(() => null);
          if (id) {
            base.startTurn(content, selectedModelId);
            return;
          }
          // Creation failed and the composer has already cleared the draft —
          // echo the content back so it isn't lost.
          base.handleServerEvent({
            type: "error",
            code: "conversation_create_failed",
            message: `Could not create a conversation, so this was not sent: ${content}`,
          });
        } finally {
          isCreatingStore.set(false);
        }
      })();
    },
  };
}
