// Surfaces store — the topic's agent-run tabs and the one currently open.
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
import type { SurfaceTemplateSummary } from "../protocol/SurfaceTemplateSummary";
import type { ServerEvent } from "../types";
import * as surfacesApi from "../api/surfaces";
import { createWorkSurfaceChannel } from "../api/work-surface";
import type { WorkSurfaceChannel } from "../api/work-surface";

export interface SurfacesStore {
  readonly surfaces: SurfaceSummary[];
  readonly activeSurface: SurfaceResponse | null;
  readonly templates: SurfaceTemplateSummary[];
  loadSurfaces(topicId: string): Promise<void>;
  openSurface(surfaceId: string): Promise<void>;
  closeSurface(): void;
  createSurface(topicId: string, title: string, templateId?: string): Promise<void>;
  templateName(templateId: string): string | null;
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
  const templatesStore = writable<SurfaceTemplateSummary[]>([]);
  const surfacesState = fromStore(surfacesStore);
  const activeSurfaceState = fromStore(activeSurfaceStore);
  const templatesState = fromStore(templatesStore);

  let currentTopicId: string | null = null;

  async function refreshActive(surfaceId: string): Promise<void> {
    const surface = await surfacesApi.getSurface(surfaceId);
    activeSurfaceStore.set(surface);
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
    if (currentTopicId) {
      void surfacesApi
        .listSurfaces(currentTopicId)
        .then((response) => surfacesStore.set(response.surfaces))
        .catch(() => {});
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
    get templates() {
      return templatesState.current;
    },

    async loadSurfaces(topicId: string): Promise<void> {
      if (currentTopicId !== null && currentTopicId !== topicId) {
        // A topic switch closes the previous topic's surface — the surface
        // must never leave another topic's surface visible and writable.
        activeSurfaceStore.set(null);
        // The picker must not offer a previous topic's templates while this
        // topic's request is still in flight.
        templatesStore.set([]);
      }
      currentTopicId = topicId;
      const response = await surfacesApi.listSurfaces(topicId);
      if (topicId !== currentTopicId) return;
      surfacesStore.set(response.surfaces);
      // Templates are topic-local: the picker only ever offers this topic's.
      const templates = await surfacesApi.listSurfaceTemplates(topicId);
      if (topicId !== currentTopicId) return;
      templatesStore.set(templates.templates);
    },

    async openSurface(surfaceId: string): Promise<void> {
      await refreshActive(surfaceId);
    },

    closeSurface(): void {
      activeSurfaceStore.set(null);
    },

    async createSurface(
      topicId: string,
      title: string,
      templateId?: string,
    ): Promise<void> {
      const surface = await surfacesApi.createSurface(topicId, title, templateId);
      const response = await surfacesApi.listSurfaces(topicId);
      surfacesStore.set(response.surfaces);
      activeSurfaceStore.set(surface);
    },

    templateName(templateId: string): string | null {
      // The recorded origin is an id; the topic's loaded templates give it a
      // name. A template that no longer exists resolves to nothing, and the
      // caller shows the bare id rather than losing the lineage.
      const template = templatesState.current.find(
        (candidate) => candidate.id === templateId,
      );
      return template ? template.name : null;
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
