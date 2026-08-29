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
function fakeChannel(): WorkSurfaceChannel & { emit(event: unknown): void } {
  const handlers: ((event: ServerEvent) => void)[] = [];
  return {
    onSignal(handler) {
      handlers.push(handler);
    },
    close() {},
    emit(event: unknown) {
      for (const handler of handlers) handler(event as ServerEvent);
    },
  };
}

const PAGE_ID = "11111111-1111-4111-8111-111111111111";
const MODULE_ID = "22222222-2222-4222-8222-222222222222";
const SPACE_ID = "33333333-3333-4333-8333-333333333333";
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

describe("pages store", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("loads the space's pages and opens one", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
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

  it("refetches the open page when its page_updated signal arrives", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
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

  it("shows an agent's note on its module after the signal, without a reload", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
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

    const [url, init] = fetchMock.mock.calls[2];
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
});
