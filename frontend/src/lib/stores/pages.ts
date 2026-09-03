// Pages store — the space's agent-run tabs and the one currently open.
//
// The store owns its own live route: it opens the work-surface signal channel
// on creation, so an open page hears about changes whether or not the person
// reading it has a conversation selected. The page_updated signal is only a
// signal (page id + module id); the store refetches the page, so content
// always comes from the authoritative read path, never from event payloads.
// Annotations — the notes the user and the agent hang on the same module —
// travel with the page on that refetch, so an agent's note appears without a
// reload by the same route a module edit does.

import { fromStore, writable } from "svelte/store";
import type { PageResponse } from "../protocol/PageResponse";
import type { PageSummary } from "../protocol/PageSummary";
import type { PageTemplateSummary } from "../protocol/PageTemplateSummary";
import type { ServerEvent } from "../types";
import * as pagesApi from "../api/pages";
import { createWorkSurfaceChannel } from "../api/work-surface";
import type { WorkSurfaceChannel } from "../api/work-surface";

export interface PagesStore {
  readonly pages: PageSummary[];
  readonly activePage: PageResponse | null;
  readonly templates: PageTemplateSummary[];
  loadPages(spaceId: string): Promise<void>;
  openPage(pageId: string): Promise<void>;
  closePage(): void;
  createPage(spaceId: string, title: string, templateId?: string): Promise<void>;
  templateName(templateId: string): string | null;
  writeModule(
    pageId: string,
    moduleId: string | null,
    source: string,
  ): Promise<void>;
  // Leave a note on one of the page's modules — the only anchor there is.
  annotate(
    pageId: string,
    moduleId: string,
    body: string,
    tags?: string[],
  ): Promise<void>;
  removeAnnotation(pageId: string, annotationId: string): Promise<void>;
  // Release the live channel. The app-lifetime store never needs this;
  // tests and short-lived surfaces do.
  stopListening(): void;
}

export interface PagesStoreOptions {
  // Injectable factory for the live signal channel — overridden in tests.
  channelFactory?: () => WorkSurfaceChannel;
}

export function createPagesStore(options?: PagesStoreOptions): PagesStore {
  const pagesStore = writable<PageSummary[]>([]);
  const activePageStore = writable<PageResponse | null>(null);
  const templatesStore = writable<PageTemplateSummary[]>([]);
  const pagesState = fromStore(pagesStore);
  const activePageState = fromStore(activePageStore);
  const templatesState = fromStore(templatesStore);

  let currentSpaceId: string | null = null;

  async function refreshActive(pageId: string): Promise<void> {
    const page = await pagesApi.getPage(pageId);
    activePageStore.set(page);
  }

  // The live route, opened here rather than wired in by the shell: an open
  // page's liveness is the store's own contract, not the caller's to remember.
  function handleSignal(event: ServerEvent): void {
    if (event.type !== "page_updated") return;
    // Refetch rather than patch: the event carries identity, not content.
    const active = activePageState.current;
    if (active && active.id === event.page_id) {
      void refreshActive(active.id).catch(() => {
        // A failed refresh leaves the last-known page; the next signal or
        // manual open retries.
      });
    }
    if (currentSpaceId) {
      void pagesApi
        .listPages(currentSpaceId)
        .then((response) => pagesStore.set(response.pages))
        .catch(() => {});
    }
  }

  const channel = (options?.channelFactory ?? createWorkSurfaceChannel)();
  channel.onSignal(handleSignal);

  return {
    get pages() {
      return pagesState.current;
    },
    get activePage() {
      return activePageState.current;
    },
    get templates() {
      return templatesState.current;
    },

    async loadPages(spaceId: string): Promise<void> {
      if (currentSpaceId !== null && currentSpaceId !== spaceId) {
        // A space switch closes the previous space's page — the surface
        // must never leave another space's page visible and writable.
        activePageStore.set(null);
        // The picker must not offer a previous space's templates while this
        // space's request is still in flight.
        templatesStore.set([]);
      }
      currentSpaceId = spaceId;
      const response = await pagesApi.listPages(spaceId);
      if (spaceId !== currentSpaceId) return;
      pagesStore.set(response.pages);
      // Templates are space-local: the picker only ever offers this space's.
      const templates = await pagesApi.listPageTemplates(spaceId);
      if (spaceId !== currentSpaceId) return;
      templatesStore.set(templates.templates);
    },

    async openPage(pageId: string): Promise<void> {
      await refreshActive(pageId);
    },

    closePage(): void {
      activePageStore.set(null);
    },

    async createPage(
      spaceId: string,
      title: string,
      templateId?: string,
    ): Promise<void> {
      const page = await pagesApi.createPage(spaceId, title, templateId);
      const response = await pagesApi.listPages(spaceId);
      pagesStore.set(response.pages);
      activePageStore.set(page);
    },

    templateName(templateId: string): string | null {
      // The recorded origin is an id; the space's loaded templates give it a
      // name. A template that no longer exists resolves to nothing, and the
      // caller shows the bare id rather than losing the lineage.
      const template = templatesState.current.find(
        (candidate) => candidate.id === templateId,
      );
      return template ? template.name : null;
    },

    async writeModule(
      pageId: string,
      moduleId: string | null,
      source: string,
    ): Promise<void> {
      await pagesApi.writePageModule(pageId, {
        module_id: moduleId ?? undefined,
        source,
      });
      await refreshActive(pageId);
    },

    async annotate(
      pageId: string,
      moduleId: string,
      body: string,
      tags: string[] = [],
    ): Promise<void> {
      await pagesApi.createPageAnnotation(pageId, {
        module_id: moduleId,
        body,
        tags,
      });
      // The note comes back through the same read path as everything else
      // on the page — the response body is a receipt, not the source.
      await refreshActive(pageId);
    },

    async removeAnnotation(
      pageId: string,
      annotationId: string,
    ): Promise<void> {
      await pagesApi.deletePageAnnotation(annotationId);
      await refreshActive(pageId);
    },

    stopListening(): void {
      channel.close();
    },
  };
}
