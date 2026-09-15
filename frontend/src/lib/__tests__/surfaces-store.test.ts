// Surfaces store — list/open behaviour and the live-refresh contract: a
// surface_updated signal is a signal only, answered by refetching the surface
// through the authoritative read path.
//
// Signals are delivered the way the product delivers them — through the
// work-surface channel the store opens for itself. The store exposes no
// direct event entry point, so a test cannot reach past the wiring: if the
// store stopped subscribing, every live test here would fail.

import { describe, it, expect, vi, beforeEach } from "vitest";
import { createSurfacesStore } from "../stores/surfaces";
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
const TOPIC_ID = "33333333-3333-4333-8333-333333333333";
const OTHER_TOPIC_ID = "66666666-6666-4666-8666-666666666666";
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

function surfaceBody(source: string, annotations: unknown[] = []) {
  return {
    id: PAGE_ID,
    topic_id: TOPIC_ID,
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
  surfaces: [
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
  surfaces: [
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

describe("surfaces store", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("loads the topic's surfaces and opens one", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("# Hello")));
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(TOPIC_ID);
    expect(store.surfaces).toHaveLength(1);
    expect(store.surfaces[0].title).toBe("Planning");

    await store.openSurface(PAGE_ID);
    expect(store.activeSurface?.modules[0].source).toBe("# Hello");
    expect(fetchMock).toHaveBeenCalledWith(
      `/api/topics/${TOPIC_ID}/surfaces`,
      expect.anything(),
    );
  });

  it("keeps the current topic's surfaces and templates when a previous template load finishes late", async () => {
    let resolvePreviousTemplates: (response: Response) => void;
    let previousTemplateRequests = 0;
    const previousTemplates = new Promise<Response>((resolve) => {
      resolvePreviousTemplates = resolve;
    });
    const fetchMock = vi.fn((url: string) => {
      if (url === `/api/topics/${TOPIC_ID}/surfaces`) {
        return Promise.resolve(jsonResponse(listBody));
      }
      if (url === `/api/topics/${TOPIC_ID}/surface-templates`) {
        previousTemplateRequests += 1;
        return previousTemplateRequests === 1
          ? Promise.resolve(jsonResponse(templateListBody))
          : previousTemplates;
      }
      if (url === `/api/topics/${OTHER_TOPIC_ID}/surfaces`) {
        return Promise.resolve(jsonResponse(otherListBody));
      }
      if (url === `/api/topics/${OTHER_TOPIC_ID}/surface-templates`) {
        return Promise.resolve(jsonResponse(otherTemplateListBody));
      }
      throw new Error(`unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(TOPIC_ID);
    const previousLoad = store.loadSurfaces(TOPIC_ID);
    await vi.waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        `/api/topics/${TOPIC_ID}/surface-templates`,
        expect.anything(),
      );
    });

    const currentLoad = store.loadSurfaces(OTHER_TOPIC_ID);
    // A visible previous-topic picker is cleared before B's response settles.
    expect(store.templates).toEqual([]);
    await currentLoad;
    expect(store.surfaces.map((surface) => surface.title)).toEqual(["Other planning"]);
    expect(store.templates.map((template) => template.name)).toEqual([
      "Retrospective",
    ]);

    resolvePreviousTemplates!(jsonResponse(templateListBody));
    await previousLoad;
    expect(store.surfaces.map((surface) => surface.title)).toEqual(["Other planning"]);
    expect(store.templates.map((template) => template.name)).toEqual([
      "Retrospective",
    ]);
  });

  it("refetches the open surface when its surface_updated signal arrives", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("before")))
      // The event answers with a refetch of the surface and the list.
      .mockResolvedValueOnce(jsonResponse(surfaceBody("after")))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createSurfacesStore({ channelFactory: () => channel });
    await store.loadSurfaces(TOPIC_ID);
    await store.openSurface(PAGE_ID);
    expect(store.activeSurface?.modules[0].source).toBe("before");

    channel.emit({
      type: "surface_updated",
      surface_id: PAGE_ID,
      module_id: MODULE_ID,
    });
    await vi.waitFor(() => {
      expect(store.activeSurface?.modules[0].source).toBe("after");
    });
  });

  it("refetches the open surface after reconnecting, even when no surface signal arrived", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("before disconnect")))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("written while disconnected")));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createSurfacesStore({ channelFactory: () => channel });
    await store.loadSurfaces(TOPIC_ID);
    await store.openSurface(PAGE_ID);

    channel.reconnect();
    await vi.waitFor(() => {
      expect(store.activeSurface?.modules[0].source).toBe("written while disconnected");
    });
  });

  it("shows an agent's note on its module after the signal, without a reload", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("# Draft")))
      // The signal answers with a refetch: the note the agent left arrives
      // on the module it names, through the authoritative read path.
      .mockResolvedValueOnce(
        jsonResponse(
          surfaceBody("# Draft", [
            annotationBody("this claim needs a source", "assistant"),
          ]),
        ),
      )
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createSurfacesStore({ channelFactory: () => channel });
    await store.loadSurfaces(TOPIC_ID);
    await store.openSurface(PAGE_ID);
    expect(store.activeSurface?.modules[0].annotations).toEqual([]);

    channel.emit({
      type: "surface_updated",
      surface_id: PAGE_ID,
      module_id: MODULE_ID,
      // A payload the store must never render: content comes from the
      // refetch, so nothing shaped like this may reach the surface.
      body: "words smuggled on the event",
    });

    await vi.waitFor(() => {
      expect(store.activeSurface?.modules[0].annotations).toHaveLength(1);
    });
    const note = store.activeSurface!.modules[0].annotations[0];
    expect(note.body).toBe("this claim needs a source");
    expect(note.author).toBe("assistant");
    expect(note.module_id).toBe(MODULE_ID);
    expect(JSON.stringify(store.activeSurface)).not.toContain("smuggled");
  });

  it("posts a user note against the module it hangs on, then refetches", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("# Draft")))
      .mockResolvedValueOnce(
        jsonResponse(annotationBody("mine, on this module", "user")),
      )
      .mockResolvedValueOnce(
        jsonResponse(
          surfaceBody("# Draft", [annotationBody("mine, on this module", "user")]),
        ),
      );
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(TOPIC_ID);
    await store.openSurface(PAGE_ID);

    await store.annotate(PAGE_ID, MODULE_ID, "mine, on this module");

    const [url, init] = fetchMock.mock.calls[3];
    expect(url).toBe(`/api/surfaces/${PAGE_ID}/annotations`);
    expect(JSON.parse(init.body as string)).toEqual({
      module_id: MODULE_ID,
      body: "mine, on this module",
      tags: [],
    });
    expect(store.activeSurface?.modules[0].annotations[0].body).toBe(
      "mine, on this module",
    );
  });

  it("ignores surface_updated for a surface that is not open", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createSurfacesStore({ channelFactory: () => channel });
    channel.emit({
      type: "surface_updated",
      surface_id: PAGE_ID,
      module_id: MODULE_ID,
    });
    // No topic loaded, nothing open: the signal is inert.
    expect(fetchMock).not.toHaveBeenCalled();
    expect(store.activeSurface).toBeNull();
  });
  it("offers the topic's templates and forks a surface from the chosen one", async () => {
    const forked = {
      ...surfaceBody("## Yesterday"),
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

    const store = createSurfacesStore();
    await store.loadSurfaces(TOPIC_ID);
    expect(store.templates.map((template) => template.name)).toEqual([
      "Standup",
    ]);

    await store.createSurface(TOPIC_ID, "Monday standup", TEMPLATE_ID);
    const createCall = fetchMock.mock.calls[2];
    expect(createCall[0]).toBe(`/api/topics/${TOPIC_ID}/surfaces`);
    expect(requestBody(createCall)).toEqual({
      title: "Monday standup",
      template_id: TEMPLATE_ID,
    });
    // The seeded modules and the recorded origin come back on the surface the
    // create returned — the surface reads lineage from there.
    expect(store.activeSurface?.modules[0].source).toBe("## Yesterday");
    expect(store.activeSurface?.forked_from_template_id).toBe(TEMPLATE_ID);
  });

  it("names the template a forked surface came from, and yields nothing for one that is gone", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore();
    await store.loadSurfaces(TOPIC_ID);

    expect(store.templateName(TEMPLATE_ID)).toBe("Standup");
    // A template deleted since the fork resolves to nothing, so the surface
    // falls back to the recorded id rather than dropping the lineage.
    expect(store.templateName("55555555-5555-4555-8555-555555555555")).toBeNull();
  });

  it("creates a blank surface when no template is chosen", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(templateListBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("")))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore();
    await store.loadSurfaces(TOPIC_ID);
    await store.createSurface(TOPIC_ID, "Scratch");
    expect(requestBody(fetchMock.mock.calls[2]).template_id).toBeUndefined();
  });
});
