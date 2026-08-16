// Conversation store — owns the conversation list, active conversation, and CRUD state.
//
// Components read from this store and emit events that trigger updates.
// The store calls the HTTP API client to persist changes and fetch data.

import { fromStore, get, writable } from "svelte/store";

import type { ConversationSummary, ConversationDetail, MessageView } from "../types";
import * as api from "../api/conversations";

export interface ConversationStore {
  readonly conversations: ConversationSummary[];
  readonly activeConversation: ConversationDetail | null;
  readonly isLoading: boolean;
  loadConversations(spaceId: string): Promise<void>;
  selectConversation(id: string): Promise<void>;
  // spaceId is required by the underlying store but optional here so that
  // the router-aware proxy in App.svelte can satisfy this interface without
  // threading spaceId through every intermediate component.
  createConversation(spaceId?: string, selectedModelId?: string | null): Promise<void>;
  deleteConversation(id: string): Promise<void>;
  // Drops the active conversation without touching the server — used when the
  // selected space changes so a conversation from the old space can't linger
  // as the target of the composer.
  clearActiveConversation(): void;
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
    tool_calls?: MessageView["tool_calls"];
  }): void;
  refreshActiveConversation(): Promise<void>;
}

// Sort conversations by last_activity, newest first.
function sortByRecency(list: ConversationSummary[]): ConversationSummary[] {
  return [...list].sort(
    (a, b) =>
      new Date(b.last_activity).getTime() - new Date(a.last_activity).getTime(),
  );
}

export function createConversationStore(): ConversationStore {
  // Back the custom API with real Svelte stores so async updates re-render
  // mounted components instead of leaving them stuck on initial state.
  const conversationsStore = writable<ConversationSummary[]>([]);
  const activeConversationStore = writable<ConversationDetail | null>(null);
  const isLoadingStore = writable(false);

  const conversationsState = fromStore(conversationsStore);
  const activeConversationState = fromStore(activeConversationStore);
  const isLoadingState = fromStore(isLoadingStore);

  function updateConversationSummary(
    conversationId: string,
    message: {
      content: string;
      timestamp: string;
      turn_state: MessageView["turn_state"];
    },
  ): void {
    const conversations = get(conversationsStore);
    const updatedSummary = conversations.find((c) => c.id === conversationId);
    if (!updatedSummary) return;

    const nextSummary: ConversationSummary = {
      ...updatedSummary,
      last_activity: message.timestamp,
      message_preview: message.content,
      has_interrupted_turn:
        updatedSummary.has_interrupted_turn || message.turn_state === "interrupted",
    };

    conversationsStore.set(
      sortByRecency(
        conversations.map((conversation) =>
          conversation.id === conversationId ? nextSummary : conversation,
        ),
      ),
    );
  }

  function appendMessage(
    message: {
      role: MessageView["role"];
      content: string;
      timestamp: string;
      turn_state: MessageView["turn_state"];
      turn_id?: string;
      tool_calls?: MessageView["tool_calls"];
    },
  ): void {
    const activeConversation = get(activeConversationStore);
    if (!activeConversation) return;

    activeConversationStore.set({
      ...activeConversation,
      last_activity: message.timestamp,
      messages: [
        ...activeConversation.messages,
        {
          role: message.role,
          content: message.content,
          timestamp: message.timestamp,
          turn_state: message.turn_state,
          ...(message.turn_id === undefined ? {} : { turn_id: message.turn_id }),
          ...(message.tool_calls === undefined ? {} : { tool_calls: message.tool_calls }),
        },
      ],
    });

    updateConversationSummary(activeConversation.id, message);
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
      return isLoadingState.current;
    },

    // ---------------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------------

    async loadConversations(spaceId) {
      isLoadingStore.set(true);
      try {
        const list = await api.listConversations(spaceId);
        conversationsStore.set(sortByRecency(list));
      } finally {
        // Always clear loading — even on error, so the UI isn't stuck.
        isLoadingStore.set(false);
      }
    },

    async selectConversation(id) {
      isLoadingStore.set(true);
      try {
        activeConversationStore.set(await api.getConversation(id));
      } finally {
        isLoadingStore.set(false);
      }
    },

    async createConversation(spaceId, selectedModelId) {
      if (!spaceId) {
        throw new Error("spaceId is required to create a conversation");
      }

      const resp = await api.createConversation(spaceId, selectedModelId);

      // Build the detail object locally — avoids an extra round-trip.
      const detail: ConversationDetail = {
        id: resp.id,
        space_id: resp.space_id,
        selected_model_id: resp.selected_model_id ?? null,
        created_at: resp.created_at,
        last_activity: resp.last_activity,
        messages: [],
      };

      // Build the summary for the list. The new conversation has no messages yet.
      const summary: ConversationSummary = {
        id: resp.id,
        selected_model_id: resp.selected_model_id ?? null,
        created_at: resp.created_at,
        last_activity: resp.last_activity,
        message_preview: null,
        has_interrupted_turn: false,
      };

      // Prepend: the freshly created conversation is the most recent.
      conversationsStore.update((conversations) => [summary, ...conversations]);
      activeConversationStore.set(detail);
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
          activeConversationStore.set(await api.getConversation(remaining[0].id));
        } else {
          activeConversationStore.set(null);
        }
      }
    },

    clearActiveConversation() {
      activeConversationStore.set(null);
    },

    appendOptimisticUserMessage(message) {
      appendMessage({
        role: "user",
        content: message.content,
        timestamp: message.timestamp,
        turn_state: "pending",
      });
    },

    appendAssistantMessage(message) {
      appendMessage({
        role: "assistant",
        content: message.content,
        timestamp: message.timestamp,
        turn_state: message.turn_state,
        ...(message.turn_id === undefined ? {} : { turn_id: message.turn_id }),
        ...(message.tool_calls === undefined ? {} : { tool_calls: message.tool_calls }),
      });
    },

    async refreshActiveConversation() {
      // Called after WebSocket reconnect to reconcile any state changes
      // that happened while the connection was down.
      const activeConversation = get(activeConversationStore);
      if (!activeConversation) return;
      activeConversationStore.set(await api.getConversation(activeConversation.id));
    },
  };
}
