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
// reload by the same route a module edit does. The library_updated signal
// refetches the shared catalogue the same way, so an entry saved by an agent
// or from another session is offered here without a reload.

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
  // Resolves to the surface, or null when the session was left meanwhile.
  createSurface(
    sessionId: string,
    title: string,
    libraryEntryId?: string,
  ): Promise<SurfaceResponse | null>;
  // Reread the shared catalogue — the library page's own load, and the
  // answer to a library_updated signal.
  refreshLibrary(): Promise<void>;
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
  // Reword a note in place; the annotation keeps its module and author.
  updateAnnotation(
    surfaceId: string,
    annotationId: string,
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

  // The surface read is checked against the session current when it
  // settles, not when it started: a read that outlives a session switch
  // must never reinstate the previous session's surface.
  async function refreshActive(surfaceId: string): Promise<void> {
    const surface = await surfacesApi.getSurface(surfaceId);
    if (currentSessionId !== null && surface.session_id !== currentSessionId) return;
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
    if (event.type === "lagged") {
      // The server dropped signals; one may have been the only signal for
      // what is open. Refetch what is open, as after a reconnect.
      const active = activeSurfaceState.current;
      if (active) void refreshActive(active.id).catch(() => {});
      if (currentSessionId) void refreshList(currentSessionId).catch(() => {});
      return;
    }
    if (event.type === "library_updated") {
      // The catalogue is shared: whoever saved or removed an entry, the
      // picker offering it must show the library as it now is.
      void refreshLibrary().catch(() => {});
      return;
    }
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
    ): Promise<SurfaceResponse | null> {
      // A session that has not listed its surfaces yet (the library page
      // reached straight from a conversation) becomes the current one: the
      // surface it just gained is the one to show.
      if (currentSessionId === null) currentSessionId = sessionId;
      const surface = await surfacesApi.createSurface(sessionId, title, libraryEntryId);
      const response = await surfacesApi.listSurfaces(sessionId);
      if (sessionId !== currentSessionId) return null;
      surfacesStore.set(response.surfaces);
      activeSurfaceStore.set(surface);
      return surface;
    },

    refreshLibrary,

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

    async updateAnnotation(
      surfaceId: string,
      annotationId: string,
      body: string,
      tags: string[] = [],
    ): Promise<void> {
      await surfacesApi.updateSurfaceAnnotation(annotationId, { body, tags });
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
