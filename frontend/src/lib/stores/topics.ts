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
  readonly recentTopics: Topic[];
  readonly isLoading: boolean;
  loadTopics(): Promise<void>;
  selectTopic(id: string): Promise<void>;
  /** Drop the current topic selection and return to the picker. */
  closeTopic(): void;
  createTopic(name: string): Promise<void>;
  deleteTopic(id: string): Promise<void>;
}

export function createTopicStore(): TopicStore {
  const RECENT_TOPICS_KEY = "swage:recent-topics";
  const ACTIVE_TOPIC_KEY = "swage:active-topic";

  // Back the custom API with real Svelte stores so topic changes propagate
  // into mounted components after async actions complete.
  const topicsStore = writable<Topic[]>([]);
  const currentTopicStore = writable<Topic | null>(null);
  const recentTopicIdsStore = writable<string[]>(readRecentTopicIds());
  const isLoadingStore = writable(false);

  const topicsState = fromStore(topicsStore);
  const currentTopicState = fromStore(currentTopicStore);
  const recentTopicIdsState = fromStore(recentTopicIdsStore);
  const isLoadingState = fromStore(isLoadingStore);

  function readRecentTopicIds(): string[] {
    if (typeof localStorage === "undefined") return [];
    try {
      const parsed = JSON.parse(localStorage.getItem(RECENT_TOPICS_KEY) ?? "[]") as unknown;
      return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === "string") : [];
    } catch {
      return [];
    }
  }

  function writeRecentTopicIds(ids: string[]): void {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(RECENT_TOPICS_KEY, JSON.stringify(ids));
  }

  function readActiveTopicId(): string | null {
    if (typeof localStorage === "undefined") return null;
    const stored = localStorage.getItem(ACTIVE_TOPIC_KEY);
    if (stored === null) return null;
    const id = stored.trim();
    if (id) return id;
    localStorage.removeItem(ACTIVE_TOPIC_KEY);
    return null;
  }

  function writeActiveTopicId(id: string | null): void {
    if (typeof localStorage === "undefined") return;
    if (id) {
      localStorage.setItem(ACTIVE_TOPIC_KEY, id);
    } else {
      localStorage.removeItem(ACTIVE_TOPIC_KEY);
    }
  }

  function rememberTopicOpen(id: string): void {
    recentTopicIdsStore.update((ids) => {
      const next = [id, ...ids.filter((existing) => existing !== id)].slice(0, 8);
      writeRecentTopicIds(next);
      return next;
    });
  }

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

    get recentTopics() {
      const topicsById = new Map(topicsState.current.map((topic) => [topic.id, topic]));
      return recentTopicIdsState.current
        .map((id) => topicsById.get(id))
        .filter((topic): topic is Topic => Boolean(topic));
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
        const activeTopicId = readActiveTopicId();
        const activeTopic = topics.find((topic) => topic.id === activeTopicId) ?? null;
        currentTopicStore.set(activeTopic);
        if (activeTopicId && !activeTopic) writeActiveTopicId(null);
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
      writeActiveTopicId(id);
      rememberTopicOpen(id);
    },

    closeTopic() {
      // Return to the topic picker. The topic list is preserved so reopening
      // is instant; only the active selection is dropped. The caller is
      // responsible for navigating the URL to the picker route.
      currentTopicStore.set(null);
      writeActiveTopicId(null);
    },

    async createTopic(name) {
      const created = await api.createTopic({
        name,
      });

      topicsStore.update((topics) => [...topics, created]);
      currentTopicStore.set(created);
      writeActiveTopicId(created.id);
    },

    async deleteTopic(id) {
      const topics = get(topicsStore);
      const target = topics.find((p) => p.id === id);

      // Guard: the default topic is the fallback for all conversations
      // and cannot be removed — doing so would leave the system in an
      // inconsistent state.
      if (target?.is_default) {
        throw new Error("Cannot delete the default topic");
      }

      await api.deleteTopic(id);

      const remaining = topics.filter((p) => p.id !== id);
      topicsStore.set(remaining);

      // If the deleted topic was current, move focus to the next one.
      if (get(currentTopicStore)?.id === id) {
        const next = remaining.length > 0 ? remaining[0] : null;
        currentTopicStore.set(next);
        writeActiveTopicId(next?.id ?? null);
      }
    },
  };
}
