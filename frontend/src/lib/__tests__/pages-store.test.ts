// Pages store — list/open behaviour and the live-refresh contract: a
// page_updated event is a signal only, answered by refetching the page
// through the authoritative read path.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { createPagesStore } from "../stores/pages";
import type { ServerEvent } from "../types";

const PAGE_ID = "11111111-1111-4111-8111-111111111111";
const MODULE_ID = "22222222-2222-4222-8222-222222222222";
const SPACE_ID = "33333333-3333-4333-8333-333333333333";

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

function pageBody(source: string) {
  return {
    id: PAGE_ID,
    space_id: SPACE_ID,
    title: "Planning",
    created_at: "2026-08-28T00:00:00Z",
    updated_at: "2026-08-28T00:00:00Z",
    modules: [
      {
        id: MODULE_ID,
        kind: "document",
        position: 0,
        source,
        updated_at: "2026-08-28T00:00:00Z",
      },
    ],
  };
}

const listBody = {
  pages: [{ id: PAGE_ID, title: "Planning", updated_at: "2026-08-28T00:00:00Z" }],
};

const TEMPLATE_ID = "44444444-4444-4444-8444-444444444444";

const templateListBody = {
  templates: [
    { id: TEMPLATE_ID, name: "Standup", updated_at: "2026-08-28T00:00:00Z" },
  ],
};

function requestBody(call: unknown[]): Record<string, unknown> {
  const init = call[1] as RequestInit;
  return JSON.parse(init.body as string) as Record<string, unknown>;
}

describe("pages store", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("loads the space's pages and opens one", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(pageBody("# Hello")));
    vi.stubGlobal("fetch", fetchMock);

    const store = createPagesStore();
    await store.loadPages(SPACE_ID);
    expect(store.pages).toHaveLength(1);
    expect(store.pages[0].title).toBe("Planning");

    await store.openPage(PAGE_ID);
    expect(store.activePage?.modules[0].source).toBe("# Hello");
    expect(fetchMock).toHaveBeenCalledWith(
      `/api/spaces/${SPACE_ID}/pages`,
      expect.anything(),
    );
  });

  it("refetches the open page when its page_updated signal arrives", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(pageBody("before")))
      // The event answers with a refetch of the page and the list.
      .mockResolvedValueOnce(jsonResponse(pageBody("after")))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createPagesStore();
    await store.loadPages(SPACE_ID);
    await store.openPage(PAGE_ID);
    expect(store.activePage?.modules[0].source).toBe("before");

    store.handleServerEvent({
      type: "page_updated",
      page_id: PAGE_ID,
      module_id: MODULE_ID,
    } as ServerEvent);
    await vi.waitFor(() => {
      expect(store.activePage?.modules[0].source).toBe("after");
    });
  });

  it("ignores page_updated for a page that is not open", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createPagesStore();
    store.handleServerEvent({
      type: "page_updated",
      page_id: PAGE_ID,
      module_id: MODULE_ID,
    } as ServerEvent);
    // No space loaded, nothing open: the signal is inert.
    expect(fetchMock).not.toHaveBeenCalled();
    expect(store.activePage).toBeNull();
  });
  it("offers the space's templates and forks a page from the chosen one", async () => {
    const forked = {
      ...pageBody("## Yesterday"),
      forked_from_template_id: TEMPLATE_ID,
      forked_at: "2026-08-28T00:00:00Z",
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      // The create, then the list refresh that follows it.
      .mockResolvedValueOnce(jsonResponse(forked))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createPagesStore();
    await store.loadPages(SPACE_ID);
    expect(store.templates.map((template) => template.name)).toEqual([
      "Standup",
    ]);

    await store.createPage(SPACE_ID, "Monday standup", TEMPLATE_ID);
    const createCall = fetchMock.mock.calls[2];
    expect(createCall[0]).toBe(`/api/spaces/${SPACE_ID}/pages`);
    expect(requestBody(createCall)).toEqual({
      title: "Monday standup",
      template_id: TEMPLATE_ID,
    });
    // The seeded modules and the recorded origin come back on the page the
    // create returned — the surface reads lineage from there.
    expect(store.activePage?.modules[0].source).toBe("## Yesterday");
    expect(store.activePage?.forked_from_template_id).toBe(TEMPLATE_ID);
  });

  it("names the template a forked page came from, and yields nothing for one that is gone", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createPagesStore();
    await store.loadPages(SPACE_ID);

    expect(store.templateName(TEMPLATE_ID)).toBe("Standup");
    // A template deleted since the fork resolves to nothing, so the surface
    // falls back to the recorded id rather than dropping the lineage.
    expect(store.templateName("55555555-5555-4555-8555-555555555555")).toBeNull();
  });

  it("creates a blank page when no template is chosen", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(pageBody("")))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createPagesStore();
    await store.loadPages(SPACE_ID);
    await store.createPage(SPACE_ID, "Scratch");
    expect(requestBody(fetchMock.mock.calls[2]).template_id).toBeUndefined();
  });
});
