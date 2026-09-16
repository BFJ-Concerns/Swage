// Surfaces store — the session's work surfaces, the one currently open, and
// the shared library's catalogue.
//
// The store owns its own live route: it opens the work-surface signal channel
// on creation, so an open surface hears about changes whether or not the person
// reading it has a conversation selected. The surface_updated signal is only a
// signal (surface id + module id); the store refetches the surface, so content
// always comes from the authoritative read path, never from event payloads.
// Annotations — the notes the user and the agent hang on the same module —
// travel with the surface on that refetch, so an agent's note appears without a
// reload by the same route a module edit does.

import { fromStore, writable } from "svelte/store";
import type { SurfaceResponse } from "../protocol/SurfaceResponse";
import type { SurfaceSummary } from "../protocol/SurfaceSummary";
import type { LibraryEntrySummary } from "../protocol/LibraryEntrySummary";
import type { ServerEvent } from "../types";
import * as surfacesApi from "../api/surfaces";
import { createWorkSurfaceChannel } from "../api/work-surface";
import type { WorkSurfaceChannel } from "../api/work-surface";

export interface SurfacesStore {
  readonly surfaces: SurfaceSummary[];
  readonly activeSurface: SurfaceResponse | null;
  /** The library's catalogue: names, descriptions and tags, never content. */
  readonly library: LibraryEntrySummary[];
  loadSurfaces(sessionId: string): Promise<void>;
  openSurface(surfaceId: string): Promise<void>;
  closeSurface(): void;
  // Create a surface for the session: blank, or seeded from a library entry.
  createSurface(sessionId: string, title: string, libraryEntryId?: string): Promise<void>;
  // Preserve a surface in the library. Opens nothing; the catalogue refreshes.
  saveToLibrary(
    surfaceId: string,
    name: string,
    description: string,
    tags?: string[],
  ): Promise<void>;
  entryName(entryId: string): string | null;
  writeModule(
    surfaceId: string,
    moduleId: string | null,
    source: string,
  ): Promise<void>;
  // Leave a note on one of the surface's modules — the only anchor there is.
  annotate(
    surfaceId: string,
    moduleId: string,
    body: string,
    tags?: string[],
  ): Promise<void>;
  removeAnnotation(surfaceId: string, annotationId: string): Promise<void>;
  // Release the live channel. The app-lifetime store never needs this;
  // tests and short-lived surfaces do.
  stopListening(): void;
}

export interface SurfacesStoreOptions {
  // Injectable factory for the live signal channel — overridden in tests.
  channelFactory?: () => WorkSurfaceChannel;
}

export function createSurfacesStore(options?: SurfacesStoreOptions): SurfacesStore {
  const surfacesStore = writable<SurfaceSummary[]>([]);
  const activeSurfaceStore = writable<SurfaceResponse | null>(null);
  const libraryStore = writable<LibraryEntrySummary[]>([]);
  const surfacesState = fromStore(surfacesStore);
  const activeSurfaceState = fromStore(activeSurfaceStore);
  const libraryState = fromStore(libraryStore);

  let currentSessionId: string | null = null;

  async function refreshActive(surfaceId: string): Promise<void> {
    const surface = await surfacesApi.getSurface(surfaceId);
    activeSurfaceStore.set(surface);
  }

  async function refreshList(sessionId: string): Promise<void> {
    const response = await surfacesApi.listSurfaces(sessionId);
    if (sessionId !== currentSessionId) return;
    surfacesStore.set(response.surfaces);
  }

  async function refreshLibrary(): Promise<void> {
    const response = await surfacesApi.listLibrary();
    libraryStore.set(response.entries);
  }

  // The live route, opened here rather than wired in by the shell: an open
  // surface's liveness is the store's own contract, not the caller's to remember.
  function handleSignal(event: ServerEvent): void {
    if (event.type !== "surface_updated") return;
    // Refetch rather than patch: the event carries identity, not content.
    const active = activeSurfaceState.current;
    if (active && active.id === event.surface_id) {
      void refreshActive(active.id).catch(() => {
        // A failed refresh leaves the last-known surface; the next signal or
        // manual open retries.
      });
    }
    if (currentSessionId) {
      void refreshList(currentSessionId).catch(() => {});
    }
  }

  const channel = (options?.channelFactory ?? createWorkSurfaceChannel)();
  channel.onSignal(handleSignal);
  channel.onReconnect(() => {
    const active = activeSurfaceState.current;
    if (active) {
      void refreshActive(active.id).catch(() => {});
    }
  });

  return {
    get surfaces() {
      return surfacesState.current;
    },
    get activeSurface() {
      return activeSurfaceState.current;
    },
    get library() {
      return libraryState.current;
    },

    async loadSurfaces(sessionId: string): Promise<void> {
      if (currentSessionId !== null && currentSessionId !== sessionId) {
        // A session switch closes the previous session's surface — the pane
        // must never leave another session's surface visible and writable.
        activeSurfaceStore.set(null);
      }
      currentSessionId = sessionId;
      const response = await surfacesApi.listSurfaces(sessionId);
      if (sessionId !== currentSessionId) return;
      surfacesStore.set(response.surfaces);
      // The library is shared: one catalogue whichever session is open.
      const library = await surfacesApi.listLibrary();
      if (sessionId !== currentSessionId) return;
      libraryStore.set(library.entries);
    },

    async openSurface(surfaceId: string): Promise<void> {
      await refreshActive(surfaceId);
    },

    closeSurface(): void {
      activeSurfaceStore.set(null);
    },

    async createSurface(
      sessionId: string,
      title: string,
      libraryEntryId?: string,
    ): Promise<void> {
      const surface = await surfacesApi.createSurface(sessionId, title, libraryEntryId);
      const response = await surfacesApi.listSurfaces(sessionId);
      surfacesStore.set(response.surfaces);
      activeSurfaceStore.set(surface);
    },

    async saveToLibrary(surfaceId, name, description, tags = []): Promise<void> {
      await surfacesApi.saveSurfaceToLibrary(surfaceId, { name, description, tags });
      await refreshLibrary();
    },

    entryName(entryId: string): string | null {
      // The recorded origin is an id; the loaded catalogue gives it a name.
      // An entry deleted since the seeding resolves to nothing, and the
      // caller shows the bare id rather than losing the lineage.
      const entry = libraryState.current.find((candidate) => candidate.id === entryId);
      return entry ? entry.name : null;
    },

    async writeModule(
      surfaceId: string,
      moduleId: string | null,
      source: string,
    ): Promise<void> {
      await surfacesApi.writeSurfaceModule(surfaceId, {
        module_id: moduleId ?? undefined,
        source,
      });
      await refreshActive(surfaceId);
    },

    async annotate(
      surfaceId: string,
      moduleId: string,
      body: string,
      tags: string[] = [],
    ): Promise<void> {
      await surfacesApi.createSurfaceAnnotation(surfaceId, {
        module_id: moduleId,
        body,
        tags,
      });
      // The note comes back through the same read path as everything else
      // on the surface — the response body is a receipt, not the source.
      await refreshActive(surfaceId);
    },

    async removeAnnotation(
      surfaceId: string,
      annotationId: string,
    ): Promise<void> {
      await surfacesApi.deleteSurfaceAnnotation(annotationId);
      await refreshActive(surfaceId);
    },

    stopListening(): void {
      channel.close();
    },
  };
}
