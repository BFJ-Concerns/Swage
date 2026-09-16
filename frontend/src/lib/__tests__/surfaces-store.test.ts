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
const SESSION_ID = "33333333-3333-4333-8333-333333333333";
const OTHER_SESSION_ID = "66666666-6666-4666-8666-666666666666";
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
    session_id: SESSION_ID,
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

const ENTRY_ID = "44444444-4444-4444-8444-444444444444";

const libraryBody = {
  entries: [
    {
      id: ENTRY_ID,
      name: "Standup",
      description: "Daily standup notes — use at the start of a working day.",
      tags: ["standup"],
      updated_at: "2026-08-28T00:00:00Z",
    },
  ],
};

const otherListBody = {
  surfaces: [
    { id: "77777777-7777-4777-8777-777777777777", title: "Other planning", updated_at: "2026-08-28T00:00:00Z" },
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

  it("loads the session's surfaces and the library, and opens one", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(libraryBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("# Hello")));
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(SESSION_ID);
    expect(store.surfaces).toHaveLength(1);
    expect(store.surfaces[0].title).toBe("Planning");
    expect(store.library.map((entry) => entry.name)).toEqual(["Standup"]);

    await store.openSurface(PAGE_ID);
    expect(store.activeSurface?.modules[0].source).toBe("# Hello");
    expect(fetchMock).toHaveBeenCalledWith(
      `/api/sessions/${SESSION_ID}/surfaces`,
      expect.anything(),
    );
    expect(fetchMock).toHaveBeenCalledWith("/api/surface-library", expect.anything());
  });

  it("closes the previous session's surface on a session switch and ignores its late responses", async () => {
    let resolvePreviousLibrary: (response: Response) => void;
    let libraryRequests = 0;
    const previousLibrary = new Promise<Response>((resolve) => {
      resolvePreviousLibrary = resolve;
    });
    const fetchMock = vi.fn((url: string) => {
      if (url === `/api/sessions/${SESSION_ID}/surfaces`) {
        return Promise.resolve(jsonResponse(listBody));
      }
      if (url === `/api/surfaces/${PAGE_ID}`) {
        return Promise.resolve(jsonResponse(surfaceBody("# Mine")));
      }
      if (url === `/api/sessions/${OTHER_SESSION_ID}/surfaces`) {
        return Promise.resolve(jsonResponse(otherListBody));
      }
      if (url === "/api/surface-library") {
        libraryRequests += 1;
        return libraryRequests === 2
          ? previousLibrary
          : Promise.resolve(jsonResponse(libraryBody));
      }
      throw new Error(`unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(SESSION_ID);
    await store.openSurface(PAGE_ID);
    expect(store.activeSurface?.id).toBe(PAGE_ID);
    const previousLoad = store.loadSurfaces(SESSION_ID);
    await vi.waitFor(() => expect(libraryRequests).toBe(2));

    const currentLoad = store.loadSurfaces(OTHER_SESSION_ID);
    // The other session's surface is never left open and writable.
    expect(store.activeSurface).toBeNull();
    await currentLoad;
    expect(store.surfaces.map((surface) => surface.title)).toEqual(["Other planning"]);

    resolvePreviousLibrary!(jsonResponse(libraryBody));
    await previousLoad;
    expect(store.surfaces.map((surface) => surface.title)).toEqual(["Other planning"]);
  });

  it("refetches the open surface when its surface_updated signal arrives", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(libraryBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("before")))
      // The event answers with a refetch of the surface and the list.
      .mockResolvedValueOnce(jsonResponse(surfaceBody("after")))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createSurfacesStore({ channelFactory: () => channel });
    await store.loadSurfaces(SESSION_ID);
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
      .mockResolvedValueOnce(jsonResponse(libraryBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("before disconnect")))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("written while disconnected")));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createSurfacesStore({ channelFactory: () => channel });
    await store.loadSurfaces(SESSION_ID);
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
      .mockResolvedValueOnce(jsonResponse(libraryBody))
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
    await store.loadSurfaces(SESSION_ID);
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
      .mockResolvedValueOnce(jsonResponse(libraryBody))
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
    await store.loadSurfaces(SESSION_ID);
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
  it("refetches what is open when the server reports dropped signals", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(libraryBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("before the gap")))
      // The lag signal answers with a refetch of the surface and the list.
      .mockResolvedValueOnce(jsonResponse(surfaceBody("written during the gap")))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const channel = fakeChannel();
    const store = createSurfacesStore({ channelFactory: () => channel });
    await store.loadSurfaces(SESSION_ID);
    await store.openSurface(PAGE_ID);

    channel.emit({ type: "lagged", dropped: 3 });
    await vi.waitFor(() => {
      expect(store.activeSurface?.modules[0].source).toBe("written during the gap");
    });
  });

  it("drops a surface read that settles after the session it belonged to was left", async () => {
    let resolveOldRead: (response: Response) => void;
    const oldRead = new Promise<Response>((resolve) => {
      resolveOldRead = resolve;
    });
    const fetchMock = vi.fn((url: string) => {
      if (url === `/api/sessions/${SESSION_ID}/surfaces`) return Promise.resolve(jsonResponse(listBody));
      if (url === `/api/sessions/${OTHER_SESSION_ID}/surfaces`) return Promise.resolve(jsonResponse(otherListBody));
      if (url === "/api/surface-library") return Promise.resolve(jsonResponse(libraryBody));
      if (url === `/api/surfaces/${PAGE_ID}`) return oldRead;
      throw new Error(`unexpected request: ${url}`);
    });
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(SESSION_ID);
    const opening = store.openSurface(PAGE_ID);
    await store.loadSurfaces(OTHER_SESSION_ID);
    expect(store.activeSurface).toBeNull();

    // The old session's surface arrives late: it must not become visible.
    resolveOldRead!(jsonResponse(surfaceBody("# Mine")));
    await opening;
    expect(store.activeSurface).toBeNull();
  });

  it("offers the library and adds a surface seeded from the chosen entry", async () => {
    const seeded = {
      ...surfaceBody("## Yesterday"),
      library_entry_id: ENTRY_ID,
      seeded_at: "2026-08-28T00:00:00Z",
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(libraryBody))
      // The create, then the list refresh that follows it.
      .mockResolvedValueOnce(jsonResponse(seeded))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(SESSION_ID);
    expect(store.library.map((entry) => entry.name)).toEqual(["Standup"]);

    await store.createSurface(SESSION_ID, "Monday standup", ENTRY_ID);
    const createCall = fetchMock.mock.calls[2];
    expect(createCall[0]).toBe(`/api/sessions/${SESSION_ID}/surfaces`);
    expect(requestBody(createCall)).toEqual({
      title: "Monday standup",
      library_entry_id: ENTRY_ID,
    });
    // The seeded modules and the recorded origin come back on the surface the
    // create returned — the pane reads lineage from there.
    expect(store.activeSurface?.modules[0].source).toBe("## Yesterday");
    expect(store.activeSurface?.library_entry_id).toBe(ENTRY_ID);
  });

  it("names the entry a seeded surface came from, and yields nothing for one that is gone", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(libraryBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(SESSION_ID);

    expect(store.entryName(ENTRY_ID)).toBe("Standup");
    // An entry deleted since the seeding resolves to nothing, so the pane
    // falls back to the recorded id rather than dropping the lineage.
    expect(store.entryName("55555555-5555-4555-8555-555555555555")).toBeNull();
  });

  it("creates a blank surface when no entry is chosen", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(libraryBody))
      .mockResolvedValueOnce(jsonResponse(surfaceBody("")))
      .mockResolvedValueOnce(jsonResponse(listBody));
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(SESSION_ID);
    await store.createSurface(SESSION_ID, "Scratch");
    expect(requestBody(fetchMock.mock.calls[2]).library_entry_id).toBeUndefined();
  });

  it("saves the open surface to the library with its description and tags, then refreshes the catalogue", async () => {
    const saved = {
      id: "99999999-9999-4999-8999-999999999999",
      name: "Planning",
      description: "A planning board — use when a piece of work needs sequencing.",
      tags: ["planning"],
      updated_at: "2026-08-28T00:00:00Z",
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse(libraryBody))
      .mockResolvedValueOnce(jsonResponse({ ...saved, created_at: saved.updated_at, modules: [] }))
      .mockResolvedValueOnce(jsonResponse({ entries: [saved, ...libraryBody.entries] }));
    vi.stubGlobal("fetch", fetchMock);

    const store = createSurfacesStore({ channelFactory: fakeChannel });
    await store.loadSurfaces(SESSION_ID);
    await store.saveToLibrary(PAGE_ID, "Planning", saved.description, ["planning"]);

    const [url, init] = fetchMock.mock.calls[2];
    expect(url).toBe(`/api/surfaces/${PAGE_ID}/save`);
    expect(JSON.parse(init.body as string)).toEqual({
      name: "Planning",
      description: saved.description,
      tags: ["planning"],
    });
    // Saving opened nothing: the session's own list is untouched.
    expect(store.surfaces).toHaveLength(1);
    expect(store.library.map((entry) => entry.name)).toEqual(["Planning", "Standup"]);
  });
});
