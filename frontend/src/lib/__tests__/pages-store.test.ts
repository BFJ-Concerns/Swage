// Pages store — list/open behaviour and the live-refresh contract: a
// page_updated signal is a signal only, answered by refetching the page
// through the authoritative read path.
//
// Signals are delivered the way the product delivers them — through the
// work-surface channel the store opens for itself. The store exposes no
// direct event entry point, so a test cannot reach past the wiring: if the
// store stopped subscribing, every live test here would fail.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { createPagesStore } from "../stores/pages";
import type { WorkSurfaceChannel } from "../api/work-surface";
import type { ServerEvent } from "../types";

// A stand-in for the server end of the work-surface channel: whatever the
// store subscribed with is what `emit` pushes into.
function fakeChannel(): WorkSurfaceChannel & { emit(event: unknown): void; reconnect(): void } {
  const handlers: ((event: ServerEvent) => void)[] = [];
  const reconnectHandlers: (() => void)[] = [];
  return {
    onSignal(handler) {
      handlers.push(handler);
    },
    close() {},
    emit(event: unknown) {
      for (const handler of handlers) handler(event as ServerEvent);
    },
    onReconnect(handler) {
      reconnectHandlers.push(handler);
    },
    reconnect() {
      for (const handler of reconnectHandlers) handler();
    },
  };
}

const PAGE_ID = "11111111-1111-4111-8111-111111111111";
const MODULE_ID = "22222222-2222-4222-8222-222222222222";
const SPACE_ID = "33333333-3333-4333-8333-333333333333";
const OTHER_SPACE_ID = "66666666-6666-4666-8666-666666666666";
const ANNOTATION_ID = "44444444-4444-4444-8444-444444444444";

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

function annotationBody(body: string, author: string) {
  return {
    id: ANNOTATION_ID,
    module_id: MODULE_ID,
    body,
    tags: ["question"],
    author,
    created_at: "2026-08-28T00:00:00Z",
    updated_at: "2026-08-28T00:00:00Z",
  };
}

function pageBody(source: string, annotations: unknown[] = []) {
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
        annotations,
      },
    ],
  };
}

const listBody = {
  pages: [
    { id: PAGE_ID, title: "Planning", updated_at: "2026-08-28T00:00:00Z" },
  ],
};

const TEMPLATE_ID = "44444444-4444-4444-8444-444444444444";

const templateListBody = {
  templates: [
    { id: TEMPLATE_ID, name: "Standup", updated_at: "2026-08-28T00:00:00Z" },
  ],
};

const otherListBody = {
  pages: [
    { id: "77777777-7777-4777-8777-777777777777", title: "Other planning", updated_at: "2026-08-28T00:00:00Z" },
  ],
};

const otherTemplateListBody = {
  templates: [
    { id: "88888888-8888-4888-8888-888888888888", name: "Retrospective", updated_at: "2026-08-28T00:00:00Z" },
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

    const store = createPagesStore({ channelFactory: fakeChannel });
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

  it("keeps the current space's pages and templates when a previous template load finishes late", async () => {
    let resolvePreviousTemplates: (response: Response) => void;
    let previousTemplateRequests = 0;
    const previousTemplates = new Promise<Response>((resolve) => {
      resolvePreviousTemplates = resolve;
    });
    const fetchMock = vi.fn((url: string) => {
      if (url === `/api/spaces/${SPACE_ID}/pages`) {
        return Promise.resolve(jsonResponse(listBody));
      }
      if (url === `/api/spaces/${SPACE_ID}/page-templates`) {
        previousTemplateRequests += 1;
        return previousTemplateRequests === 1
          ? Promise.resolve(jsonResponse(templateListBody))
          : previousTemplates;
      }
      if (url === `/api/spaces/${OTHER_SPACE_ID}/pages`) {
        return Promise.resolve(jsonResponse(otherListBody));
      }
      if (url === `/api/spaces/${OTHER_SPACE_ID}/page-templates`) {
        return Promise.resolve(jsonResponse(otherTemplateListBody));
      }
      throw new Error(`unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const store = createPagesStore({ channelFactory: fakeChannel });
    await store.loadPages(SPACE_ID);
    const previousLoad = store.loadPages(SPACE_ID);
    await vi.waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        `/api/spaces/${SPACE_ID}/page-templates`,
        expect.anything(),
      );
    });

    const currentLoad = store.loadPages(OTHER_SPACE_ID);
    // A visible previous-space picker is cleared before B's response settles.
    expect(store.templates).toEqual([]);
    await currentLoad;
    expect(store.pages.map((page) => page.title)).toEqual(["Other planning"]);
    expect(store.templates.map((template) => template.name)).toEqual([
      "Retrospective",
    ]);

    resolvePreviousTemplates!(jsonResponse(templateListBody));
    await previousLoad;
    expect(store.pages.map((page) => page.title)).toEqual(["Other planning"]);
    expect(store.templates.map((template) => template.name)).toEqual([
      "Retrospective",
    ]);
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

    const channel = fakeChannel();
    const store = createPagesStore({ channelFactory: () => channel });
    await store.loadPages(SPACE_ID);
    await store.openPage(PAGE_ID);
    expect(store.activePage?.modules[0].source).toBe("before");

    channel.emit({
      type: "page_updated",
      page_id: PAGE_ID,
      module_id: MODULE_ID,
    });
    await vi.waitFor(() => {
      expect(store.activePage?.modules[0].source).toBe("after");
    });
  });

  it("refetches the open page after reconnecting, even when no page signal arrived", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(pageBody("before disconnect")))
      .mockResolvedValueOnce(jsonResponse(pageBody("written while disconnected")));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createPagesStore({ channelFactory: () => channel });
    await store.loadPages(SPACE_ID);
    await store.openPage(PAGE_ID);

    channel.reconnect();
    await vi.waitFor(() => {
      expect(store.activePage?.modules[0].source).toBe("written while disconnected");
    });
  });

  it("shows an agent's note on its module after the signal, without a reload", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(pageBody("# Draft")))
      // The signal answers with a refetch: the note the agent left arrives
      // on the module it names, through the authoritative read path.
      .mockResolvedValueOnce(
        jsonResponse(
          pageBody("# Draft", [
            annotationBody("this claim needs a source", "assistant"),
          ]),
        ),
      )
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createPagesStore({ channelFactory: () => channel });
    await store.loadPages(SPACE_ID);
    await store.openPage(PAGE_ID);
    expect(store.activePage?.modules[0].annotations).toEqual([]);

    channel.emit({
      type: "page_updated",
      page_id: PAGE_ID,
      module_id: MODULE_ID,
      // A payload the store must never render: content comes from the
      // refetch, so nothing shaped like this may reach the surface.
      body: "words smuggled on the event",
    });

    await vi.waitFor(() => {
      expect(store.activePage?.modules[0].annotations).toHaveLength(1);
    });
    const note = store.activePage!.modules[0].annotations[0];
    expect(note.body).toBe("this claim needs a source");
    expect(note.author).toBe("assistant");
    expect(note.module_id).toBe(MODULE_ID);
    expect(JSON.stringify(store.activePage)).not.toContain("smuggled");
  });

  it("posts a user note against the module it hangs on, then refetches", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(pageBody("# Draft")))
      .mockResolvedValueOnce(
        jsonResponse(annotationBody("mine, on this module", "user")),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          pageBody("# Draft", [annotationBody("mine, on this module", "user")]),
        ),
      );
    vi.stubGlobal("fetch", fetchMock);

    const store = createPagesStore({ channelFactory: fakeChannel });
    await store.loadPages(SPACE_ID);
    await store.openPage(PAGE_ID);

    await store.annotate(PAGE_ID, MODULE_ID, "mine, on this module");

    const [url, init] = fetchMock.mock.calls[3];
    expect(url).toBe(`/api/pages/${PAGE_ID}/annotations`);
    expect(JSON.parse(init.body as string)).toEqual({
      module_id: MODULE_ID,
      body: "mine, on this module",
      tags: [],
    });
    expect(store.activePage?.modules[0].annotations[0].body).toBe(
      "mine, on this module",
    );
  });

  it("ignores page_updated for a page that is not open", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createPagesStore({ channelFactory: () => channel });
    channel.emit({
      type: "page_updated",
      page_id: PAGE_ID,
      module_id: MODULE_ID,
    });
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
