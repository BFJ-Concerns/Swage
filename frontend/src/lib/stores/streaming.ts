// Streaming store — owns the current turn's streaming state.
//
// Accumulates tokens during an active turn, tracks whether the agent is
// generating, and controls the send/stop toggle state.

import { fromStore, get, writable, type Writable } from "svelte/store";

import type { MessageAttachment } from "../protocol/MessageAttachment";
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

/** Live turn state for one conversation. The conversation id is deliberately
 * outside this projection so callers can select the state for the surface
 * they are rendering. */
export interface ConversationStreamState {
  readonly isStreaming: boolean;
  readonly activeTurnGeneration: number;
  readonly streamedContent: string;
  readonly turnError: { code: string; message: string } | null;
  readonly turnStartedAt: string | null;
  readonly activeToolCalls: ToolCallView[];
  readonly providerRetry: ProviderRetryState | null;
  readonly pendingApproval: PendingApproval | null;
}

export interface StreamingStore {
  readonly isStreaming: boolean;
  /** Select the live projection owned by a conversation. */
  streamFor?(conversationId: string | null): ConversationStreamState;
  /** Monotonic local identity for the current streamed projection. */
  readonly activeTurnGeneration: number;
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
  handleServerEvent(event: ServerEvent, conversationId?: string): void;
  reconcileAfterReconnect(
    recoveredTurnGeneration: number,
    conversationId?: string | null,
    recoveredMessages?: MessageView[],
  ): void;
  startTurn(content: string, selectedModelId?: string | null, agentId?: string | null, effort?: import("../protocol/Effort").Effort | null): void;
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
    readonly activeConversation?: { id?: string; messages: MessageView[] } | null;
    /** The thread the composer sends on; null or absent is the root thread. */
    readonly activeThreadId?: string | null;
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
      thread_id?: string;
      turn_id?: string;
      tool_calls?: ToolCallView[];
    }, conversationId?: string | null): void;
    /** Installs a message the gateway recorded and announced on its conversation. */
    appendRecordedMessage?(
      message: import("../protocol/MessageView").MessageView,
      conversationId: string,
    ): void;
  };
  /** Sends WebSocket events for outgoing user messages and cancellation. */
  connectionStore?: {
    sendMessage(
      conversationId: string,
      content: string,
      selectedModelId?: string | null,
      attachments?: MessageAttachment[],
      threadId?: string | null,
    ): void;
    cancelTurn(conversationId: string): void;
    setStreamActive?(conversationId: string, active: boolean): void;
  };
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

export function createStreamingStore(
  options?: StreamingStoreOptions,
): StreamingStore {
  interface InternalStreamState {
    isStreamingStore: Writable<boolean>;
    streamedContentStore: Writable<string>;
    turnErrorStore: Writable<{ code: string; message: string } | null>;
    turnStartedAtStore: Writable<string | null>;
    activeToolCallsStore: Writable<ToolCallView[]>;
    providerRetryStore: Writable<ProviderRetryState | null>;
    pendingApprovalStore: Writable<PendingApproval | null>;
    isStreamingState: { current: boolean };
    streamedContentState: { current: string };
    turnErrorState: { current: { code: string; message: string } | null };
    turnStartedAtState: { current: string | null };
    activeToolCallsState: { current: ToolCallView[] };
    providerRetryState: { current: ProviderRetryState | null };
    pendingApprovalState: { current: PendingApproval | null };
    activeTurnGeneration: number;
    /** The thread the running turn was sent on; null is the root thread. */
    threadId: string | null;
  }

  const streams = new Map<string | null, InternalStreamState>();
  const emptyStream: ConversationStreamState = {
    isStreaming: false,
    activeTurnGeneration: 0,
    streamedContent: "",
    turnError: null,
    turnStartedAt: null,
    activeToolCalls: [],
    providerRetry: null,
    pendingApproval: null,
  };

  function createStreamState(): InternalStreamState {
    const isStreamingStore = writable(false);
    const streamedContentStore = writable("");
    const turnErrorStore = writable<{ code: string; message: string } | null>(null);
    const turnStartedAtStore = writable<string | null>(null);
    const activeToolCallsStore = writable<ToolCallView[]>([]);
    const providerRetryStore = writable<ProviderRetryState | null>(null);
    const pendingApprovalStore = writable<PendingApproval | null>(null);
    return {
      isStreamingStore,
      streamedContentStore,
      turnErrorStore,
      turnStartedAtStore,
      activeToolCallsStore,
      providerRetryStore,
      pendingApprovalStore,
      isStreamingState: fromStore(isStreamingStore),
      streamedContentState: fromStore(streamedContentStore),
      turnErrorState: fromStore(turnErrorStore),
      turnStartedAtState: fromStore(turnStartedAtStore),
      activeToolCallsState: fromStore(activeToolCallsStore),
      providerRetryState: fromStore(providerRetryStore),
      pendingApprovalState: fromStore(pendingApprovalStore),
      activeTurnGeneration: 0,
      threadId: null,
    };
  }

  function streamStateFor(conversationId: string | null): InternalStreamState {
    const existing = streams.get(conversationId);
    if (existing) return existing;
    const created = createStreamState();
    streams.set(conversationId, created);
    return created;
  }

  function currentConversationId(): string | null {
    return options?.conversationStore?.activeConversation?.id ?? null;
  }

  function currentStreamState(): InternalStreamState | undefined {
    return streams.get(currentConversationId());
  }

  function streamView(stream: InternalStreamState | undefined): ConversationStreamState {
    if (!stream) return emptyStream;
    return {
      get isStreaming() { return stream.isStreamingState.current; },
      get activeTurnGeneration() { return stream.activeTurnGeneration; },
      get streamedContent() { return stream.streamedContentState.current; },
      get turnError() { return stream.turnErrorState.current; },
      get turnStartedAt() { return stream.turnStartedAtState.current; },
      get activeToolCalls() { return stream.activeToolCallsState.current; },
      get providerRetry() { return stream.providerRetryState.current; },
      get pendingApproval() { return stream.pendingApprovalState.current; },
    };
  }

  function setStreamActive(conversationId: string | null, stream: InternalStreamState, active: boolean): void {
    stream.isStreamingStore.set(active);
    if (conversationId) options?.connectionStore?.setStreamActive?.(conversationId, active);
  }

  // ---- Helpers ----------------------------------------------------------------

  function pushAssistantMessage(
    conversationId: string | null,
    content: string,
    turn_state: Extract<
      MessageView["turn_state"],
      "completed" | "failed" | "interrupted"
    >,
    turn_id?: string,
    tool_calls?: ToolCallView[],
  ): void {
    // The reply belongs to the thread the turn was sent on, whatever the
    // person is looking at when it completes.
    const threadId = streams.get(conversationId)?.threadId ?? null;
    options?.conversationStore?.appendAssistantMessage({
      content,
      timestamp: new Date().toISOString(),
      turn_state,
      ...(threadId === null ? {} : { thread_id: threadId }),
      ...(turn_id === undefined ? {} : { turn_id }),
      ...(tool_calls === undefined ? {} : { tool_calls }),
    }, conversationId);
  }

  // Commits any accumulated content to the conversation store. Called when a
  // turn ends before the server explicitly completes it (error or cancellation).
  function settlePartialResponse(
    conversationId: string | null,
    stream: InternalStreamState,
    turnState: Extract<MessageView["turn_state"], "failed" | "interrupted">,
  ): void {
    const streamedContent = get(stream.streamedContentStore);
    if (streamedContent) {
      pushAssistantMessage(conversationId, streamedContent, turnState);
    }
    stream.streamedContentStore.set("");
  }

  // ---- Public interface -------------------------------------------------------

  return {
    get isStreaming(): boolean {
      return Array.from(streams.values()).some((stream) => stream.isStreamingState.current);
    },
    streamFor(conversationId: string | null): ConversationStreamState {
      // Create the projection when a surface first reads it, so the Svelte
      // derivation subscribes before a later turn starts for that conversation.
      return streamView(streamStateFor(conversationId));
    },
    get activeTurnGeneration(): number {
      return currentStreamState()?.activeTurnGeneration ?? 0;
    },

    get streamedContent(): string {
      return streamView(currentStreamState()).streamedContent;
    },

    get turnError(): { code: string; message: string } | null {
      return streamView(currentStreamState()).turnError;
    },

    get turnStartedAt(): string | null {
      return streamView(currentStreamState()).turnStartedAt;
    },

    get activeToolCalls(): ToolCallView[] {
      return streamView(currentStreamState()).activeToolCalls;
    },

    get providerRetry(): ProviderRetryState | null {
      return streamView(currentStreamState()).providerRetry;
    },

    get pendingApproval(): PendingApproval | null {
      return streamView(currentStreamState()).pendingApproval;
    },

    handleServerEvent(event: ServerEvent, conversationId?: string): void {
      const ownerId = conversationId ?? currentConversationId();
      const stream = streamStateFor(ownerId);
      switch (event.type) {
        case "token":
          // Append streaming token — the UI renders this live.
          stream.streamedContentStore.update((streamedContent) => streamedContent + event.content);
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
            summary: event.summary ?? null,
          };

          if (event.parent_call_id) {
            // Sub-tool call (e.g. invoked from within python_execute) — nest it
            // under the parent rather than adding to the top-level list.
            stream.activeToolCallsStore.update((activeToolCalls) =>
              activeToolCalls.map((call) =>
                call.call_id === event.parent_call_id
                  ? { ...call, sub_calls: [...(call.sub_calls ?? []), startEntry] }
                  : call,
              ),
            );
          } else {
            // Top-level call — append to the flat list as before.
            stream.activeToolCallsStore.update((activeToolCalls) => [
              ...activeToolCalls,
              startEntry,
            ]);
          }
          break;
        }

        case "tool_end":
          if (event.parent_call_id) {
            // Sub-tool completion — update within the parent's sub_calls.
            stream.activeToolCallsStore.update((activeToolCalls) =>
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
                              total_bytes: event.total_bytes ?? null,
                            }
                          : sub,
                      ),
                    }
                  : call,
              ),
            );
          } else {
            // Top-level tool call finished — update the matching entry.
            stream.activeToolCallsStore.update((activeToolCalls) =>
              activeToolCalls.map((call) =>
                call.call_id === event.call_id
                  ? {
                      ...call,
                      result: event.result,
                      status: event.status,
                      duration_ms: event.duration_ms,
                      truncated: event.truncated,
                      total_bytes: event.total_bytes ?? null,
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
            const streamedContent = get(stream.streamedContentStore);
            const activeToolCalls = get(stream.activeToolCallsStore);
            if (streamedContent || activeToolCalls.length > 0) {
              pushAssistantMessage(
                ownerId,
                streamedContent,
                "completed",
                event.turn_id,
                activeToolCalls,
              );
            }
          }
          stream.streamedContentStore.set("");
          stream.turnErrorStore.set(null);
          setStreamActive(ownerId, stream, false);
          stream.turnStartedAtStore.set(null);
          // Tool calls are now captured in the message history — clear live state.
          stream.activeToolCallsStore.set([]);
          stream.providerRetryStore.set(null);
          break;

        case "error":
          // Turn failed mid-stream. Preserve whatever the user saw so the
          // conversation history reflects the partial response.
          settlePartialResponse(ownerId, stream, "failed");
          stream.turnErrorStore.set({ code: event.code, message: event.message });
          setStreamActive(ownerId, stream, false);
          stream.turnStartedAtStore.set(null);
          // activeToolCalls are preserved so the UI can show what was
          // in-flight when the error occurred.
          stream.providerRetryStore.set(null);
          break;

        case "output_schema_failed": {
          settlePartialResponse(ownerId, stream, "failed");
          const details = event.errors
            .map((error) =>
              error.path
                ? `${error.path}: ${error.message}`
                : error.message,
            )
            .join("; ");
          stream.turnErrorStore.set({
            code: "output_schema_failed",
            message: details
              ? `Structured output validation failed after ${event.attempts} attempts: ${details}`
              : `Structured output validation failed after ${event.attempts} attempts.`,
          });
          setStreamActive(ownerId, stream, false);
          stream.turnStartedAtStore.set(null);
          stream.providerRetryStore.set(null);
          break;
        }

        case "cancelled":
          // User-initiated cancel. Preserve partial content (they may want
          // to read what was generated before they stopped it).
          settlePartialResponse(ownerId, stream, "interrupted");
          setStreamActive(ownerId, stream, false);
          stream.turnStartedAtStore.set(null);
          // activeToolCalls preserved — same rationale as the error case.
          stream.providerRetryStore.set(null);
          break;

        case "shutdown":
          // The gateway is shutting down. Preserve any partial content and
          // surface the shutdown as a turn error so the UI can notify the user.
          // The WebSocket will close shortly after this event arrives.
          settlePartialResponse(ownerId, stream, "failed");
          stream.turnErrorStore.set({ code: "gateway_shutdown", message: event.reason });
          setStreamActive(ownerId, stream, false);
          stream.turnStartedAtStore.set(null);
          stream.providerRetryStore.set(null);
          break;

        case "provider_retry_started":
          stream.providerRetryStore.set({
            turn_id: event.turn_id,
            attempt: event.attempt,
            max_retries: event.max_retries,
            provider: event.provider,
            wait_ms: event.wait_ms,
            status: "waiting",
          });
          break;

        case "provider_retry_settled":
          stream.providerRetryStore.set({
            turn_id: event.turn_id,
            attempt: event.attempt,
            max_retries: event.max_retries,
            provider: event.provider,
            wait_ms: 0,
            status: event.success ? "resuming" : "failed",
          });
          if (event.success) {
            globalThis.setTimeout(() => {
              const current = get(stream.providerRetryStore);
              if (current?.turn_id === event.turn_id && current.attempt === event.attempt) {
                stream.providerRetryStore.set(null);
              }
            }, 1_200);
          }
          break;

        case "approval_required":
          // Surface the request so MessageInput can swap itself out for the
          // ApprovalCard. We retain only the latest active approval;
          // cross-conversation approval queues are not modelled here.
          stream.pendingApprovalStore.set({
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
          stream.pendingApprovalStore.set(null);
          break;

        case "message_appended":
          // A row the gateway recorded on this conversation that nobody here
          // typed — a schedule's brief, a delegation's notice — joins the view
          // as the gateway holds it, so an open conversation never learns of
          // it only on reload.
          options?.conversationStore?.appendRecordedMessage?.(event.message, event.session_id);
          break;

        case "context_assembled":
          break;
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

    reconcileAfterReconnect(
      recoveredTurnGeneration: number,
      conversationId = currentConversationId(),
      recoveredMessages = options?.conversationStore?.activeConversation?.messages,
    ): void {
      const ownerId = conversationId;
      const stream = streams.get(ownerId);
      const lastMessage = recoveredMessages?.at(-1);
      if (
        !stream ||
        !get(stream.isStreamingStore) ||
        stream.activeTurnGeneration !== recoveredTurnGeneration ||
        lastMessage?.role !== "assistant" ||
        !["completed", "failed", "interrupted"].includes(lastMessage.turn_state ?? "")
      ) return;

      // The HTTP refresh recovered the terminal assistant message that the
      // dropped socket could not deliver. Normal token events intentionally
      // carry no turn ID, so the monotonically increasing local generation
      // fences a later start that races the refresh. The durable conversation
      // is authoritative only for the projection captured before reconnect.
      stream.streamedContentStore.set("");
      setStreamActive(ownerId, stream, false);
      stream.turnStartedAtStore.set(null);
      stream.activeToolCallsStore.set([]);
      stream.providerRetryStore.set(null);
      stream.pendingApprovalStore.set(null);
    },

    startTurn(content: string, selectedModelId?: string | null): void {
      const conversationId = currentConversationId();
      const stream = streamStateFor(conversationId);
      // Clear state from any previous turn.
      stream.streamedContentStore.set("");
      stream.turnErrorStore.set(null);
      stream.activeToolCallsStore.set([]);
      stream.providerRetryStore.set(null);
      stream.activeTurnGeneration += 1;
      stream.turnStartedAtStore.set(new Date().toISOString());
      setStreamActive(conversationId, stream, true);

      // Immediately record the user's message so the conversation history
      // updates before the first token arrives.
      options?.conversationStore?.appendOptimisticUserMessage({
        content,
        timestamp: new Date().toISOString(),
      });

      // Dispatch the outgoing message over the WebSocket, on the thread the
      // person is standing on — a branch, or the root when none is selected.
      const threadId = options?.conversationStore?.activeThreadId ?? null;
      stream.threadId = threadId;
      if (conversationId) {
        options?.connectionStore?.sendMessage(conversationId, content, selectedModelId, undefined, threadId);
      }
    },

    cancelTurn(): void {
      const conversationId = currentConversationId();
      if (conversationId && streams.get(conversationId)?.isStreamingState.current) {
        options?.connectionStore?.cancelTurn(conversationId);
      }
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
  createConversation(selectedModelId?: string | null, agentId?: string | null, effort?: import("../protocol/Effort").Effort | null): Promise<string | null>;
}

// Wraps a streaming store so the composer works in a topic with no
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
    streamFor(conversationId): ConversationStreamState {
      const stream = base.streamFor?.(conversationId) ?? {
        isStreaming: base.isStreaming,
        activeTurnGeneration: base.activeTurnGeneration,
        streamedContent: base.streamedContent,
        turnError: base.turnError,
        turnStartedAt: base.turnStartedAt,
        activeToolCalls: base.activeToolCalls,
        providerRetry: base.providerRetry,
        pendingApproval: base.pendingApproval,
      };
      if (!isCreatingState.current || conversationId !== null) return stream;
      return { ...stream, isStreaming: true };
    },
    get activeTurnGeneration(): number {
      return base.activeTurnGeneration;
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
    handleServerEvent: (event, conversationId) => base.handleServerEvent(event, conversationId),
    reconcileAfterReconnect: (turnGeneration, conversationId, recoveredMessages) =>
      base.reconcileAfterReconnect(turnGeneration, conversationId, recoveredMessages),
    cancelTurn: () => base.cancelTurn(),

    startTurn(content: string, selectedModelId?: string | null, agentId?: string | null, effort?: import("../protocol/Effort").Effort | null): void {
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
            .createConversation(selectedModelId, agentId, effort)
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
