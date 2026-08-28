// Pages store — the space's agent-run tabs and the one currently open.
//
// Live refresh rides the server's page_updated event: the event is only a
// signal (page id + module id); the store refetches the page, so content
// always comes from the authoritative read path, never from event payloads.

import { fromStore, writable } from "svelte/store";
import type { PageResponse } from "../protocol/PageResponse";
import type { PageSummary } from "../protocol/PageSummary";
import type { ServerEvent } from "../types";
import * as pagesApi from "../api/pages";

export interface PagesStore {
  readonly pages: PageSummary[];
  readonly activePage: PageResponse | null;
  loadPages(spaceId: string): Promise<void>;
  openPage(pageId: string): Promise<void>;
  closePage(): void;
  createPage(spaceId: string, title: string): Promise<void>;
  writeModule(
    pageId: string,
    moduleId: string | null,
    source: string,
  ): Promise<void>;
  handleServerEvent(event: ServerEvent): void;
}

export function createPagesStore(): PagesStore {
  const pagesStore = writable<PageSummary[]>([]);
  const activePageStore = writable<PageResponse | null>(null);
  const pagesState = fromStore(pagesStore);
  const activePageState = fromStore(activePageStore);

  let currentSpaceId: string | null = null;

  async function refreshActive(pageId: string): Promise<void> {
    const page = await pagesApi.getPage(pageId);
    activePageStore.set(page);
  }

  return {
    get pages() {
      return pagesState.current;
    },
    get activePage() {
      return activePageState.current;
    },

    async loadPages(spaceId: string): Promise<void> {
      currentSpaceId = spaceId;
      const response = await pagesApi.listPages(spaceId);
      pagesStore.set(response.pages);
    },

    async openPage(pageId: string): Promise<void> {
      await refreshActive(pageId);
    },

    closePage(): void {
      activePageStore.set(null);
    },

    async createPage(spaceId: string, title: string): Promise<void> {
      const page = await pagesApi.createPage(spaceId, title);
      const response = await pagesApi.listPages(spaceId);
      pagesStore.set(response.pages);
      activePageStore.set(page);
    },

    async writeModule(
      pageId: string,
      moduleId: string | null,
      source: string,
    ): Promise<void> {
      await pagesApi.writePageModule(pageId, {
        module_id: moduleId,
        kind: null,
        source,
      });
      await refreshActive(pageId);
    },

    handleServerEvent(event: ServerEvent): void {
      if (event.type !== "page_updated") return;
      // Refetch rather than patch: the event carries identity, not content.
      const active = activePageState.current;
      if (active && active.id === event.page_id) {
        void refreshActive(active.id).catch(() => {
          // A failed refresh leaves the last-known page; the next event or
          // manual open retries.
        });
      }
      if (currentSpaceId) {
        void pagesApi
          .listPages(currentSpaceId)
          .then((response) => pagesStore.set(response.pages))
          .catch(() => {});
      }
    },
  };
}
