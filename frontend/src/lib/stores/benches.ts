// Bench store — owns the bench list, current selection, and CRUD state.
//
// Components read from this store and emit events that trigger updates.
// The store calls the HTTP API client to persist changes and fetch data.

import { fromStore, get, writable } from "svelte/store";

import type { Bench } from "../types";
import * as api from "../api/benches";

export interface BenchStore {
  readonly benches: Bench[];
  readonly currentBench: Bench | null;
  readonly recentBenches: Bench[];
  readonly isLoading: boolean;
  loadBenches(): Promise<void>;
  selectBench(id: string): Promise<void>;
  createBench(name: string): Promise<void>;
  deleteBench(id: string): Promise<void>;
}

export function createBenchStore(): BenchStore {
  const RECENT_BENCHES_KEY = "swage:recent-benches";

  // Back the custom API with real Svelte stores so bench changes propagate
  // into mounted components after async actions complete.
  const benchesStore = writable<Bench[]>([]);
  const currentBenchStore = writable<Bench | null>(null);
  const recentBenchIdsStore = writable<string[]>(readRecentBenchIds());
  const isLoadingStore = writable(false);

  const benchesState = fromStore(benchesStore);
  const currentBenchState = fromStore(currentBenchStore);
  const recentBenchIdsState = fromStore(recentBenchIdsStore);
  const isLoadingState = fromStore(isLoadingStore);

  function readRecentBenchIds(): string[] {
    if (typeof localStorage === "undefined") return [];
    try {
      const parsed = JSON.parse(localStorage.getItem(RECENT_BENCHES_KEY) ?? "[]") as unknown;
      return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === "string") : [];
    } catch {
      return [];
    }
  }

  function writeRecentBenchIds(ids: string[]): void {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(RECENT_BENCHES_KEY, JSON.stringify(ids));
  }

  function rememberBenchOpen(id: string): void {
    recentBenchIdsStore.update((ids) => {
      const next = [id, ...ids.filter((existing) => existing !== id)].slice(0, 8);
      writeRecentBenchIds(next);
      return next;
    });
  }

  return {
    // ---------------------------------------------------------------------------
    // Reactive state (read-only to callers via readonly interface)
    // ---------------------------------------------------------------------------

    get benches() {
      return benchesState.current;
    },

    get currentBench() {
      return currentBenchState.current;
    },

    get recentBenches() {
      const benchesById = new Map(benchesState.current.map((bench) => [bench.id, bench]));
      return recentBenchIdsState.current
        .map((id) => benchesById.get(id))
        .filter((bench): bench is Bench => Boolean(bench));
    },

    get isLoading() {
      return isLoadingState.current;
    },

    // ---------------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------------

    async loadBenches() {
      isLoadingStore.set(true);
      try {
        const benches = await api.listBenches();
        benchesStore.set(benches);

        // Auto-select the default bench on load. The default bench is
        // identified by name so it works regardless of its generated ID.
        const defaultBench = benches.find((p) => p.is_default);
        if (defaultBench) {
          currentBenchStore.set(defaultBench);
        }
      } finally {
        // Always clear loading — even on error, so the UI isn't stuck.
        isLoadingStore.set(false);
      }
    },

    async selectBench(id) {
      const benches = get(benchesStore);
      const found = benches.find((p) => p.id === id);
      if (!found) {
        throw new Error(`Bench not found: ${id}`);
      }
      // Selection is purely local — no extra round-trip needed since we
      // already have the full bench data from loadBenches.
      const opened: Bench = { ...found, last_opened_at: new Date().toISOString() };
      benchesStore.update((items) =>
        items.map((bench) => (bench.id === id ? opened : bench)),
      );
      currentBenchStore.set(opened);
      rememberBenchOpen(id);
    },

    async createBench(name) {
      const created = await api.createBench({
        name,
      });

      benchesStore.update((benches) => [...benches, created]);
      currentBenchStore.set(created);
    },

    async deleteBench(id) {
      const benches = get(benchesStore);
      const target = benches.find((p) => p.id === id);

      // Guard: the default bench is the fallback for all conversations
      // and cannot be removed — doing so would leave the system in an
      // inconsistent state.
      if (target?.is_default) {
        throw new Error("Cannot delete the default bench");
      }

      await api.deleteBench(id);

      const remaining = benches.filter((p) => p.id !== id);
      benchesStore.set(remaining);

      // If the deleted bench was current, move focus to the next one.
      if (get(currentBenchStore)?.id === id) {
        currentBenchStore.set(remaining.length > 0 ? remaining[0] : null);
      }
    },
  };
}
