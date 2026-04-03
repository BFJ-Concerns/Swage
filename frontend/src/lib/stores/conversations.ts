// Conversation store — owns the conversation list, active conversation, and CRUD state.
//
// Components read from this store and emit events that trigger updates.
// The store calls the HTTP API client to persist changes and fetch data.

import type { ConversationSummary, ConversationDetail, MessageView } from "../types";
import * as api from "../api/conversations";

export interface ConversationStore {
  readonly conversations: ConversationSummary[];
  readonly activeConversation: ConversationDetail | null;
  readonly isLoading: boolean;
  loadConversations(projectId: string): Promise<void>;
  selectConversation(id: string): Promise<void>;
  // projectId is required by the underlying store but optional here so that
  // the router-aware proxy in App.svelte can satisfy this interface without
  // threading projectId through every intermediate component.
  createConversation(projectId?: string): Promise<void>;
  deleteConversation(id: string): Promise<void>;
  addMessage(message: { role: string; content: string; timestamp: string; turn_state: string }): void;
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
  // Per-instance state — each call creates a fresh, isolated store.
  // Components read these through the getter properties below;
  // Svelte's fine-grained reactivity tracks property reads automatically.
  let conversations: ConversationSummary[] = [];
  let activeConversation: ConversationDetail | null = null;
  let isLoading = false;

  return {
    // ---------------------------------------------------------------------------
    // Reactive state (read-only to callers via readonly interface)
    // ---------------------------------------------------------------------------

    get conversations() {
      return conversations;
    },

    get activeConversation() {
      return activeConversation;
    },

    get isLoading() {
      return isLoading;
    },

    // ---------------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------------

    async loadConversations(projectId) {
      isLoading = true;
      try {
        const list = await api.listConversations(projectId);
        conversations = sortByRecency(list);
      } finally {
        // Always clear loading — even on error, so the UI isn't stuck.
        isLoading = false;
      }
    },

    async selectConversation(id) {
      isLoading = true;
      try {
        activeConversation = await api.getConversation(id);
      } finally {
        isLoading = false;
      }
    },

    async createConversation(projectId) {
      if (!projectId) {
        throw new Error("projectId is required to create a conversation");
      }

      const resp = await api.createConversation(projectId);

      // Build the detail object locally — avoids an extra round-trip.
      const detail: ConversationDetail = {
        id: resp.id,
        project_id: resp.project_id,
        created_at: resp.created_at,
        last_activity: resp.last_activity,
        messages: [],
      };

      // Build the summary for the list. The new conversation has no messages yet.
      const summary: ConversationSummary = {
        id: resp.id,
        created_at: resp.created_at,
        last_activity: resp.last_activity,
        message_preview: null,
        has_interrupted_turn: false,
      };

      // Prepend: the freshly created conversation is the most recent.
      conversations = [summary, ...conversations];
      activeConversation = detail;
    },

    async deleteConversation(id) {
      await api.deleteConversation(id);

      conversations = conversations.filter((c) => c.id !== id);

      // If the deleted conversation was active, select the next most recent.
      // The list is already sorted by last_activity desc, so index 0 is best.
      if (activeConversation?.id === id) {
        if (conversations.length > 0) {
          activeConversation = await api.getConversation(conversations[0].id);
        } else {
          activeConversation = null;
        }
      }
    },

    addMessage(message) {
      // No-op if there's nothing to append to. This happens if the streaming
      // store fires before a conversation is selected (shouldn't occur in
      // normal use, but guards against race conditions).
      if (!activeConversation) return;

      activeConversation = {
        ...activeConversation,
        messages: [
          ...activeConversation.messages,
          {
            role: message.role as MessageView["role"],
            content: message.content,
            timestamp: message.timestamp,
            turn_state: message.turn_state as MessageView["turn_state"],
          },
        ],
      };
    },

    async refreshActiveConversation() {
      // Called after WebSocket reconnect to reconcile any state changes
      // that happened while the connection was down.
      if (!activeConversation) return;
      activeConversation = await api.getConversation(activeConversation.id);
    },
  };
}
