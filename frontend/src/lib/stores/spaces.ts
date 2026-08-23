// Space store — owns the space list, current selection, and CRUD state.
//
// Components read from this store and emit events that trigger updates.
// The store calls the HTTP API client to persist changes and fetch data.

import { fromStore, get, writable } from "svelte/store";

import type { Space } from "../types";
import * as api from "../api/spaces";

export interface SpaceStore {
  readonly spaces: Space[];
  readonly currentSpace: Space | null;
  readonly recentSpaces: Space[];
  readonly isLoading: boolean;
  loadSpaces(): Promise<void>;
  selectSpace(id: string): Promise<void>;
  /** Drop the current space selection and return to the picker. */
  closeSpace(): void;
  createSpace(name: string): Promise<void>;
  deleteSpace(id: string): Promise<void>;
}

export function createSpaceStore(): SpaceStore {
  const RECENT_SPACES_KEY = "swage:recent-spaces";
  const ACTIVE_SPACE_KEY = "swage:active-space";

  // Back the custom API with real Svelte stores so space changes propagate
  // into mounted components after async actions complete.
  const spacesStore = writable<Space[]>([]);
  const currentSpaceStore = writable<Space | null>(null);
  const recentSpaceIdsStore = writable<string[]>(readRecentSpaceIds());
  const isLoadingStore = writable(false);

  const spacesState = fromStore(spacesStore);
  const currentSpaceState = fromStore(currentSpaceStore);
  const recentSpaceIdsState = fromStore(recentSpaceIdsStore);
  const isLoadingState = fromStore(isLoadingStore);

  function readRecentSpaceIds(): string[] {
    if (typeof localStorage === "undefined") return [];
    try {
      const parsed = JSON.parse(localStorage.getItem(RECENT_SPACES_KEY) ?? "[]") as unknown;
      return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === "string") : [];
    } catch {
      return [];
    }
  }

  function writeRecentSpaceIds(ids: string[]): void {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(RECENT_SPACES_KEY, JSON.stringify(ids));
  }

  function readActiveSpaceId(): string | null {
    if (typeof localStorage === "undefined") return null;
    const stored = localStorage.getItem(ACTIVE_SPACE_KEY);
    if (stored === null) return null;
    const id = stored.trim();
    if (id) return id;
    localStorage.removeItem(ACTIVE_SPACE_KEY);
    return null;
  }

  function writeActiveSpaceId(id: string | null): void {
    if (typeof localStorage === "undefined") return;
    if (id) {
      localStorage.setItem(ACTIVE_SPACE_KEY, id);
    } else {
      localStorage.removeItem(ACTIVE_SPACE_KEY);
    }
  }

  function rememberSpaceOpen(id: string): void {
    recentSpaceIdsStore.update((ids) => {
      const next = [id, ...ids.filter((existing) => existing !== id)].slice(0, 8);
      writeRecentSpaceIds(next);
      return next;
    });
  }

  return {
    // ---------------------------------------------------------------------------
    // Reactive state (read-only to callers via readonly interface)
    // ---------------------------------------------------------------------------

    get spaces() {
      return spacesState.current;
    },

    get currentSpace() {
      return currentSpaceState.current;
    },

    get recentSpaces() {
      const spacesById = new Map(spacesState.current.map((space) => [space.id, space]));
      return recentSpaceIdsState.current
        .map((id) => spacesById.get(id))
        .filter((space): space is Space => Boolean(space));
    },

    get isLoading() {
      return isLoadingState.current;
    },

    // ---------------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------------

    async loadSpaces() {
      isLoadingStore.set(true);
      try {
        const spaces = await api.listSpaces();
        spacesStore.set(spaces);
        const activeSpaceId = readActiveSpaceId();
        const activeSpace = spaces.find((space) => space.id === activeSpaceId) ?? null;
        currentSpaceStore.set(activeSpace);
        if (activeSpaceId && !activeSpace) writeActiveSpaceId(null);
      } finally {
        // Always clear loading — even on error, so the UI isn't stuck.
        isLoadingStore.set(false);
      }
    },

    async selectSpace(id) {
      const spaces = get(spacesStore);
      const found = spaces.find((p) => p.id === id);
      if (!found) {
        throw new Error(`Space not found: ${id}`);
      }
      // Selection is purely local — no extra round-trip needed since we
      // already have the full space data from loadSpaces.
      const opened: Space = { ...found, last_opened_at: new Date().toISOString() };
      spacesStore.update((items) =>
        items.map((space) => (space.id === id ? opened : space)),
      );
      currentSpaceStore.set(opened);
      writeActiveSpaceId(id);
      rememberSpaceOpen(id);
    },

    closeSpace() {
      // Return to the space picker. The space list is preserved so reopening
      // is instant; only the active selection is dropped. The caller is
      // responsible for navigating the URL to the picker route.
      currentSpaceStore.set(null);
      writeActiveSpaceId(null);
    },

    async createSpace(name) {
      const created = await api.createSpace({
        name,
      });

      spacesStore.update((spaces) => [...spaces, created]);
      currentSpaceStore.set(created);
      writeActiveSpaceId(created.id);
      rememberSpaceOpen(created.id);
    },

    async deleteSpace(id) {
      const spaces = get(spacesStore);
      const target = spaces.find((p) => p.id === id);

      // Guard: the default space is the fallback for all conversations
      // and cannot be removed — doing so would leave the system in an
      // inconsistent state.
      if (target?.is_default) {
        throw new Error("Cannot delete the default space");
      }

      await api.deleteSpace(id);

      const remaining = spaces.filter((p) => p.id !== id);
      spacesStore.set(remaining);

      // If the deleted space was current, move focus to the next one.
      if (get(currentSpaceStore)?.id === id) {
        const next = remaining.length > 0 ? remaining[0] : null;
        currentSpaceStore.set(next);
        writeActiveSpaceId(next?.id ?? null);
      }
    },
  };
}
