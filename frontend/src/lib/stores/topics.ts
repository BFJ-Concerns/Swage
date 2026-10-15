// Topic store — owns the topic list, current selection, and CRUD state.
//
// Components read from this store and emit events that trigger updates.
// The store calls the HTTP API client to persist changes and fetch data.

import { fromStore, get, writable } from "svelte/store";

import type { Topic } from "../types";
import * as api from "../api/topics";

export interface TopicStore {
  readonly topics: Topic[];
  readonly currentTopic: Topic | null;
  readonly isLoading: boolean;
  loadTopics(): Promise<void>;
  selectTopic(id: string): Promise<void>;
  /** Clear the current topic selection. */
  closeTopic(): void;
  createTopic(name: string): Promise<void>;
  deleteTopic(id: string): Promise<void>;
}

export function createTopicStore(): TopicStore {
  // Back the custom API with real Svelte stores so topic changes propagate
  // into mounted components after async actions complete.
  const topicsStore = writable<Topic[]>([]);
  const currentTopicStore = writable<Topic | null>(null);
  const isLoadingStore = writable(false);

  const topicsState = fromStore(topicsStore);
  const currentTopicState = fromStore(currentTopicStore);
  const isLoadingState = fromStore(isLoadingStore);

  return {
    // ---------------------------------------------------------------------------
    // Reactive state (read-only to callers via readonly interface)
    // ---------------------------------------------------------------------------

    get topics() {
      return topicsState.current;
    },

    get currentTopic() {
      return currentTopicState.current;
    },

    get isLoading() {
      return isLoadingState.current;
    },

    // ---------------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------------

    async loadTopics() {
      isLoadingStore.set(true);
      try {
        const topics = await api.listTopics();
        topicsStore.set(topics);
      } finally {
        // Always clear loading — even on error, so the UI isn't stuck.
        isLoadingStore.set(false);
      }
    },

    async selectTopic(id) {
      const topics = get(topicsStore);
      const found = topics.find((p) => p.id === id);
      if (!found) {
        throw new Error(`Topic not found: ${id}`);
      }
      // Selection is purely local — no extra round-trip needed since we
      // already have the full topic data from loadTopics.
      const opened: Topic = { ...found, last_opened_at: new Date().toISOString() };
      topicsStore.update((items) =>
        items.map((topic) => (topic.id === id ? opened : topic)),
      );
      currentTopicStore.set(opened);
    },

    closeTopic() {
      // Keep the catalogue available while clearing the live selection.
      currentTopicStore.set(null);
    },

    async createTopic(name) {
      const created = await api.createTopic({
        name,
      });

      topicsStore.update((topics) => [...topics, created]);
      currentTopicStore.set(created);
    },

    async deleteTopic(id) {
      const topics = get(topicsStore);

      await api.deleteTopic(id);

      const remaining = topics.filter((p) => p.id !== id);
      topicsStore.set(remaining);

      // If the deleted topic was current, move focus to the next one.
      if (get(currentTopicStore)?.id === id) {
        const next = remaining.length > 0 ? remaining[0] : null;
        currentTopicStore.set(next);
      }
    },
  };
}
