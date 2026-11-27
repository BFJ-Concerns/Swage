// Conversation store — owns the conversation list, active conversation, and CRUD state.
//
// Components read from this store and emit events that trigger updates.
// The store calls the HTTP API client to persist changes and fetch data.

import { fromStore, get, writable } from "svelte/store";

import type {
  ConversationBranch,
  ConversationSummary,
  ConversationDetail,
  MessageView,
} from "../types";
import type { MessageView as WireMessageView } from "../protocol/MessageView";
import type { QueuedMessageView as WireQueuedMessageView } from "../protocol/QueuedMessageView";
import * as api from "../api/conversations";

type QueuedSource = { id: string; clientId?: string | null };

export interface ConversationStore {
  readonly conversations: ConversationSummary[];
  readonly activeConversation: ConversationDetail | null;
  readonly isLoading: boolean;
  loadConversations(topicId: string | null): Promise<void>;
  refreshConversations?(): Promise<void>;
  selectConversation(id: string, shouldInstall?: () => boolean): Promise<void>;
  // topicId is required by the underlying store but optional here so that
  // the router-aware proxy in App.svelte can satisfy this interface without
  // threading topicId through every intermediate component. `shouldInstall`
  // is asked once the gateway has answered: when it says no, the created
  // session is neither listed nor activated here — the caller has moved on,
  // and the session waits in its own scope's records.
  createConversation(
    topicId?: string | null,
    selectedModelId?: string | null,
    agentId?: string | null,
    effort?: import("../protocol/Effort").Effort | null,
    shouldInstall?: () => boolean,
  ): Promise<void>;
  deleteConversation(id: string): Promise<void>;
  // Drops the active conversation without touching the server — used when the
  // selected topic changes so a conversation from the old topic can't linger
  // as the target of the composer.
  clearActiveConversation(): void;
  appendOptimisticUserMessage(message: {
    attachments?: MessageView["attachments"];
    client_id?: string;
    content: string;
    timestamp: string;
    thread_id?: string;
    failure_message?: string;
  }, conversationId?: string | null): void;
  appendAssistantMessage(message: {
    content: string;
    timestamp: string;
    turn_state: Extract<
      MessageView["turn_state"],
      "completed" | "refused" | "incomplete" | "failed" | "interrupted" | "cancelled"
    >;
    /** The thread the reply belongs to; absent means the thread in view. */
    thread_id?: string;
    turn_id?: string;
    tool_calls?: MessageView["tool_calls"];
    failure_message?: string;
  }, conversationId?: string | null): void;
  /**
   * Install a message the gateway recorded and announced — a row nobody at
   * this client typed, such as a schedule's brief or a delegation's notice —
   * on the conversation it names, exactly as the gateway holds it.
   */
  appendRecordedMessage(message: WireMessageView, conversationId: string, queuedSource?: QueuedSource): void;
  /** Show a message the gateway is holding for the conversation's next turn. */
  appendQueuedMessage(queued: WireQueuedMessageView, conversationId: string): void;
  /** Drop the held messages a drain took, by id; later arrivals stay held. */
  clearQueuedMessages(conversationId: string, queuedIds: string[]): void;
  /**
   * Settle the person's pending request as a turn that ended without a
   * reply, with its terminal state and the tool calls it issued.
   */
  settleUnansweredRequest?(
    settlement: {
      turn_state: Extract<MessageView["turn_state"], "completed" | "refused" | "incomplete" | "failed" | "interrupted" | "cancelled">;
      tool_calls: MessageView["tool_calls"];
      turn_id?: string;
      failure_message?: string;
    },
    conversationId?: string | null,
  ): void;
  /** Install a fetched snapshot only when its owner is still selected. */
  replaceActiveConversationIfCurrent?(conversation: ConversationDetail): void;
  refreshActiveConversation(): Promise<void>;

  // ---- Threads and branches ------------------------------------------------
  // A conversation starts on its root thread. Branching at an earlier message
  // opens a sibling thread that carries the history up to that message and
  // diverges after it; the history and the composer follow whichever thread
  // is active, and the root is never overwritten by exploring a branch.

  /** The thread the history and composer show; null is the root thread. */
  readonly activeThreadId?: string | null;
  /** The active conversation's branches, oldest first. */
  readonly branches?: ConversationBranch[];
  /** Show `threadId`'s messages, or the root thread's for null. */
  selectThread?(threadId: string | null): Promise<void>;
  /** Branch the active thread at `messageId` and move onto the branch. */
  branchFrom?(messageId: string): Promise<void>;
  /** Set the active branch aside and return to the root thread. */
  abandonActiveBranch?(): Promise<void>;
}

// Sort conversations by last_activity, newest first.
function sortByRecency(list: ConversationSummary[]): ConversationSummary[] {
  return [...list].sort(
    (a, b) =>
      new Date(b.last_activity).getTime() - new Date(a.last_activity).getTime(),
  );
}

export function createConversationStore(): ConversationStore {
  const drainedQueueIds = new Map<string, Set<string>>();

  // Back the custom API with real Svelte stores so async updates re-render
  // mounted components instead of leaving them stuck on initial state.
  const conversationsStore = writable<ConversationSummary[]>([]);
  const activeConversationStore = writable<ConversationDetail | null>(null);
  const isLoadingStore = writable(false);
  const isSelectingStore = writable(false);
  let selectionRequest = 0;
  const activeThreadIdStore = writable<string | null>(null);
  const branchesStore = writable<ConversationBranch[]>([]);

  const conversationsState = fromStore(conversationsStore);
  const activeConversationState = fromStore(activeConversationStore);
  const isLoadingState = fromStore(isLoadingStore);
  const isSelectingState = fromStore(isSelectingStore);
  const activeThreadIdState = fromStore(activeThreadIdStore);
  const branchesState = fromStore(branchesStore);

  // Load a conversation's root view and its branches together, so the
  // thread switcher never shows a stale set beside a fresh history.
  async function loadConversationDetail(id: string, shouldInstall?: () => boolean): Promise<void> {
    const [detail, branches] = await Promise.all([
      api.getConversation(id),
      api.listBranches(id),
    ]);
    if (shouldInstall && !shouldInstall()) return;
    activeConversationStore.set(detail);
    activeThreadIdStore.set(null);
    branchesStore.set(branches);
  }

  // Show one thread's messages inside the active conversation. The root is
  // re-read whole (its view is the conversation's); a branch replaces only
  // the message list, so the conversation's identity and scope stay put.
  async function showThread(threadId: string | null): Promise<void> {
    const activeConversation = get(activeConversationStore);
    if (!activeConversation) return;
    if (threadId === null) {
      const root = await api.getConversation(activeConversation.id);
      // The person may have moved to another conversation while the read
      // was in flight; a snapshot of the one they left is not installed.
      if (get(activeConversationStore)?.id !== activeConversation.id) return;
      activeConversationStore.set(root);
      activeThreadIdStore.set(null);
      return;
    }
    const thread = await api.getThread(threadId);
    const current = get(activeConversationStore);
    if (current?.id !== activeConversation.id) return;
    activeConversationStore.set({ ...current, messages: thread.messages });
    activeThreadIdStore.set(threadId);
  }

  function replaceActiveConversationIfCurrent(conversation: ConversationDetail): void {
    if (get(activeConversationStore)?.id === conversation.id) {
      activeConversationStore.set(conversation);
    }
  }

  function rememberDrained(conversationId: string, ids: string[]): void {
    const seen = drainedQueueIds.get(conversationId) ?? new Set<string>();
    for (const id of ids) seen.add(id);
    drainedQueueIds.set(conversationId, seen);
  }

  function appendMessage(
    message: {
      attachments?: MessageView["attachments"];
      id?: string;
      client_id?: string;
      role: MessageView["role"];
      origin?: MessageView["origin"];
      content: string;
      timestamp: string;
      turn_state: MessageView["turn_state"];
      thread_id?: string;
      turn_id?: string;
      tool_calls?: MessageView["tool_calls"];
      failure_message?: string;
    },
    conversationId?: string | null,
    queuedSource?: QueuedSource,
  ): void {
    if (queuedSource && conversationId) rememberDrained(conversationId, [queuedSource.id]);
    const activeConversation = get(activeConversationStore);
    if (!activeConversation || (conversationId != null && activeConversation.id !== conversationId)) return;

    // A message belongs to the thread it names, else to the thread in view.
    // It joins the visible list only when that is the thread in view: a
    // reply to a turn sent on a branch the person has since left waits
    // on its own thread.
    const threadInView = get(activeThreadIdStore) ?? activeConversation.root_thread_id;
    const threadId = message.thread_id ?? threadInView;
    const belongsToView =
      message.thread_id === undefined || threadInView === undefined || message.thread_id === threadInView;
    const pending = queuedSource
      ? (activeConversation.queued ?? []).filter((item) => item.id !== queuedSource.id)
      : activeConversation.queued;
    if (!belongsToView) {
      if (queuedSource) activeConversationStore.set({ ...activeConversation, queued: pending });
      return;
    }
    const appendedMessage: MessageView = {
      ...(message.attachments?.length ? { attachments: message.attachments } : {}),
      role: message.role,
      content: message.content,
      timestamp: message.timestamp,
      turn_state: message.turn_state,
      ...(message.id === undefined ? {} : { id: message.id }),
      ...(message.client_id === undefined ? {} : { client_id: message.client_id }),
      ...(message.origin === undefined ? {} : { origin: message.origin }),
      ...(threadId === undefined ? {} : { thread_id: threadId }),
      ...(message.turn_id === undefined ? {} : { turn_id: message.turn_id }),
      ...(message.tool_calls === undefined ? {} : { tool_calls: message.tool_calls }),
      ...(message.failure_message === undefined ? {} : { failure_message: message.failure_message }),
    };

    const messages = activeConversation.messages.filter((existing) =>
      !queuedSource?.clientId || existing.id !== undefined || existing.client_id !== queuedSource.clientId,
    );
    const recordedIndex = message.id === undefined
      ? -1
      : messages.findIndex((existing) => existing.id === message.id);
    if (recordedIndex < 0) messages.push(appendedMessage);
    else messages[recordedIndex] = appendedMessage;

    activeConversationStore.set({
      ...activeConversation,
      last_activity: message.origin === "platform" ? activeConversation.last_activity : message.timestamp,
      queued: pending,
      messages,
    });
  }

  let listGeneration = 0;
  let currentScope: { topicId: string | null; initial: Promise<void> } | undefined;

  async function readConversationList(topicId: string | null): Promise<void> {
    const generation = ++listGeneration;
    const list = await api.listConversations(topicId);
    if (generation === listGeneration) conversationsStore.set(sortByRecency(list));
  }

  return {
    // ---------------------------------------------------------------------------
    // Reactive state (read-only to callers via readonly interface)
    // ---------------------------------------------------------------------------

    get conversations() {
      return conversationsState.current;
    },

    get activeConversation() {
      return activeConversationState.current;
    },

    get isLoading() {
      return isLoadingState.current || isSelectingState.current;
    },

    get activeThreadId() {
      return activeThreadIdState.current;
    },

    get branches() {
      return branchesState.current;
    },

    // ---------------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------------

    async loadConversations(topicId) {
      conversationsStore.set([]);
      isLoadingStore.set(true);
      const scope = { topicId, initial: readConversationList(topicId) };
      currentScope = scope;
      try {
        await scope.initial;
      } finally {
        if (currentScope === scope) isLoadingStore.set(false);
      }
    },

    async refreshConversations() {
      const scope = currentScope;
      if (!scope) return;
      await scope.initial.catch(() => {});
      if (currentScope !== scope) return;
      try {
        await readConversationList(scope.topicId);
      } catch {
        // A transient refresh failure leaves the current list visible.
      }
    },

    async selectConversation(id, shouldInstall) {
      const request = ++selectionRequest;
      isSelectingStore.set(true);
      try {
        await loadConversationDetail(id, () => request === selectionRequest && (shouldInstall?.() ?? true));
      } finally {
        if (request === selectionRequest) isSelectingStore.set(false);
      }
    },

    async createConversation(topicId, selectedModelId, agentId, effort, shouldInstall) {
      // No topic means the general scope: the gateway creates the session
      // there and reports no topic on it.
      const resp = await api.createConversation(topicId ?? null, selectedModelId, agentId, effort);
      if (shouldInstall && !shouldInstall()) return;

      // Build the detail object locally — avoids an extra round-trip.
      const detail: ConversationDetail = {
        id: resp.id,
        topic_id: resp.topic_id ?? undefined,
        agent_id: resp.agent_id,
        agent_display_name: resp.agent_display_name,
        selected_model_id: resp.selected_model_id ?? null,
        created_at: resp.created_at,
        last_activity: resp.last_activity,
        messages: [],
      };

      // Build the summary for the list. The new conversation has no messages yet.
      const summary: ConversationSummary = {
        id: resp.id,
        agent_id: resp.agent_id,
        selected_model_id: resp.selected_model_id ?? null,
        created_at: resp.created_at,
        last_activity: resp.last_activity,
        message_preview: null,
        has_interrupted_turn: false,
      };

      // A live refresh can already have installed the gateway's summary.
      conversationsStore.update((conversations) =>
        conversations.some((conversation) => conversation.id === summary.id)
          ? conversations
          : [summary, ...conversations],
      );
      activeConversationStore.set(detail);
      activeThreadIdStore.set(null);
      branchesStore.set([]);
    },

    async deleteConversation(id) {
      await api.deleteConversation(id);

      const remaining = get(conversationsStore).filter((c) => c.id !== id);
      conversationsStore.set(remaining);

      // If the deleted conversation was active, select the next most recent.
      // The list is already sorted by last_activity desc, so index 0 is best.
      const activeConversation = get(activeConversationStore);
      if (activeConversation?.id === id) {
        if (remaining.length > 0) {
          await loadConversationDetail(remaining[0].id);
        } else {
          activeConversationStore.set(null);
          activeThreadIdStore.set(null);
          branchesStore.set([]);
        }
      }
    },

    clearActiveConversation() {
      activeConversationStore.set(null);
      activeThreadIdStore.set(null);
      branchesStore.set([]);
    },

    appendOptimisticUserMessage(message, conversationId) {
      appendMessage({
        ...(message.attachments?.length ? { attachments: message.attachments } : {}),
        client_id: message.client_id,
        role: "user",
        content: message.content,
        timestamp: message.timestamp,
        turn_state: message.failure_message ? "failed" : "pending",
        ...(message.thread_id === undefined ? {} : { thread_id: message.thread_id }),
        ...(message.failure_message === undefined ? {} : { failure_message: message.failure_message }),
      }, conversationId);
    },

    appendAssistantMessage(message, conversationId) {
      appendMessage({
        role: "assistant",
        content: message.content,
        timestamp: message.timestamp,
        turn_state: message.turn_state,
        ...(message.thread_id === undefined ? {} : { thread_id: message.thread_id }),
        ...(message.turn_id === undefined ? {} : { turn_id: message.turn_id }),
        ...(message.tool_calls === undefined ? {} : { tool_calls: message.tool_calls }),
        ...(message.failure_message === undefined ? {} : { failure_message: message.failure_message }),
      }, conversationId);
    },

    settleUnansweredRequest(settlement, conversationId) {
      const activeConversation = get(activeConversationStore);
      if (!activeConversation || (conversationId && activeConversation.id !== conversationId)) return;
      const index = activeConversation.messages.findLastIndex(
        (message) => message.role === "user" &&
          (settlement.turn_id ? message.turn_id === settlement.turn_id :
            ["pending", "processing", "delegating", "awaiting_approval"].includes(message.turn_state ?? "")),
      );
      if (index < 0) return;
      const messages = [...activeConversation.messages];
      const settledMessage = {
        ...messages[index],
        turn_state: settlement.turn_state,
        ...(settlement.failure_message ? { failure_message: settlement.failure_message } : {}),
        ...(settlement.tool_calls && settlement.tool_calls.length > 0
          ? { tool_calls: settlement.tool_calls }
          : {}),
      };
      messages[index] = settledMessage;
      activeConversationStore.set({ ...activeConversation, messages });
    },

    appendQueuedMessage(queued, conversationId) {
      if (drainedQueueIds.get(conversationId)?.has(queued.id)) return;
      const activeConversation = get(activeConversationStore);
      if (activeConversation?.id !== conversationId) return;
      const held = activeConversation.queued ?? [];
      if (held.some((item) => item.id === queued.id)) return;
      // The acknowledgement identifies this send even when another pending
      // message has identical text. Other clients have no matching local row.
      const optimisticIndex = queued.client_id
        ? activeConversation.messages.findIndex((message) =>
          message.id === undefined && message.client_id === queued.client_id)
        : -1;
      activeConversationStore.set({
        ...activeConversation,
        messages: activeConversation.messages.filter((_, index) => index !== optimisticIndex),
        queued: [...held, queued],
      });
    },

    clearQueuedMessages(conversationId, queuedIds) {
      rememberDrained(conversationId, queuedIds);
      const activeConversation = get(activeConversationStore);
      if (activeConversation?.id !== conversationId) return;
      const drained = new Set(queuedIds);
      activeConversationStore.set({
        ...activeConversation,
        queued: (activeConversation.queued ?? []).filter((item) => !drained.has(item.id)),
      });
    },

    appendRecordedMessage(message, conversationId, queuedSource) {
      // The gateway's own view of the row, installed as recorded: its
      // identity (so it can be branched from), its origin (so it is labelled
      // by its author), and the thread it belongs to.
      appendMessage({
        ...(message.attachments?.length ? { attachments: message.attachments } : {}),
        id: message.id,
        role: message.role === "assistant" ? "assistant" : "user",
        origin: message.origin,
        content: message.content,
        timestamp: message.timestamp,
        turn_state: message.turn_state,
        thread_id: message.thread_id,
        ...(message.turn_id === undefined || message.turn_id === null
          ? {}
          : { turn_id: message.turn_id }),
        ...(message.failure_message === undefined ? {} : { failure_message: message.failure_message }),
        // The wire tool-call view narrows to the display view the same way
        // the conversation API's fetch does.
        ...(message.tool_calls.length === 0
          ? {}
          : { tool_calls: message.tool_calls as unknown as MessageView["tool_calls"] }),
      }, conversationId, queuedSource);
    },

    replaceActiveConversationIfCurrent(conversation) {
      replaceActiveConversationIfCurrent(conversation);
    },

    async refreshActiveConversation() {
      // Called after WebSocket reconnect to reconcile any state changes
      // that happened while the connection was down. A branch in view is
      // re-read as that thread, so the reconciliation lands on what the
      // person is looking at.
      const activeConversation = get(activeConversationStore);
      if (!activeConversation) return;
      const activeThreadId = get(activeThreadIdStore);
      if (activeThreadId !== null) {
        const thread = await api.getThread(activeThreadId);
        if (get(activeConversationStore)?.id === activeConversation.id) {
          activeConversationStore.set({ ...activeConversation, messages: thread.messages });
        }
        return;
      }
      replaceActiveConversationIfCurrent(await api.getConversation(activeConversation.id));
    },

    async selectThread(threadId) {
      await showThread(threadId);
    },

    async branchFrom(messageId) {
      const activeConversation = get(activeConversationStore);
      if (!activeConversation) return;
      const parentThreadId = get(activeThreadIdStore) ?? activeConversation.root_thread_id;
      if (parentThreadId === undefined) {
        // A locally built detail that has not been fetched yet has no root
        // thread to branch; reading the conversation settles it.
        await loadConversationDetail(activeConversation.id);
        return this.branchFrom!(messageId);
      }
      const branch = await api.createBranch(activeConversation.id, parentThreadId, messageId);
      branchesStore.update((branches) => [...branches, branch]);
      await showThread(branch.child_thread_id);
    },

    async abandonActiveBranch() {
      const activeThreadId = get(activeThreadIdStore);
      if (activeThreadId === null) return;
      const thread = await api.abandonThread(activeThreadId);
      branchesStore.update((branches) =>
        branches.map((branch) =>
          branch.child_thread_id === activeThreadId ? { ...branch, state: thread.state } : branch,
        ),
      );
      await showThread(null);
    },
  };
}
