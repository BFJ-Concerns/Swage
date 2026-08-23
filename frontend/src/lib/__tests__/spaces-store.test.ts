/**
 * Unit tests for the space store.
 *
 * All API calls are mocked so tests run in isolation without a gateway.
 * Each test creates a fresh store instance via createSpaceStore().
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

import { createSpaceStore } from "../stores/spaces";
import * as api from "../api/spaces";

// ---------------------------------------------------------------------------
// Module mock
// ---------------------------------------------------------------------------

vi.mock("../api/spaces", () => ({
  listSpaces: vi.fn(),
  createSpace: vi.fn(),
  getSpace: vi.fn(),
  updateSpace: vi.fn(),
  deleteSpace: vi.fn(),
  listSpaceSessions: vi.fn(),
  createSpaceSession: vi.fn(),
  setSpacesBaseUrl: vi.fn(),
}));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeSpace(
  id: string,
  name: string,
  extra: Partial<{ slug: string; is_default: boolean }> = {},
) {
  return {
    id,
    name,
    description: "",
    is_default: extra.is_default ?? false,
    slug: extra.slug ?? name.toLowerCase().replace(/\s+/g, "-"),
    created_at: "2026-01-01T00:00:00Z",
  };
}

// The default space — special-cased: cannot be deleted.
const defaultSpace = makeSpace("proj-default", "default", {
  is_default: true,
});

const spaceA = makeSpace("proj-a", "Space A");
const spaceB = makeSpace("proj-b", "Space B");

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

beforeEach(() => {
  vi.resetAllMocks();
  localStorage.clear();
});

// ---------------------------------------------------------------------------
// Initial state
// ---------------------------------------------------------------------------

describe("initial state", () => {
  it("starts with an empty space list", () => {
    const store = createSpaceStore();
    expect(store.spaces).toEqual([]);
  });

  it("starts with no current space", () => {
    const store = createSpaceStore();
    expect(store.currentSpace).toBeNull();
  });

  it("starts with isLoading false", () => {
    const store = createSpaceStore();
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// loadSpaces
// ---------------------------------------------------------------------------

describe("loadSpaces", () => {
  it("fetches spaces and stores them", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA, spaceB]);

    await store.loadSpaces();

    expect(store.spaces).toHaveLength(2);
    expect(store.spaces[0].id).toBe("proj-a");
  });

  it("restores the active space after the available spaces load", async () => {
    const store = createSpaceStore();
    localStorage.setItem("swage:active-space", "proj-b");
    vi.mocked(api.listSpaces).mockResolvedValueOnce([
      spaceA,
      defaultSpace,
      spaceB,
    ]);

    await store.loadSpaces();

    expect(store.currentSpace?.id).toBe("proj-b");
  });

  it("clears a stale stored space and leaves the picker open", async () => {
    const store = createSpaceStore();
    localStorage.setItem("swage:active-space", "proj-missing");
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA, spaceB]);

    await store.loadSpaces();

    expect(store.currentSpace).toBeNull();
    expect(localStorage.getItem("swage:active-space")).toBeNull();
  });

  it("ignores and clears an empty stored space id", async () => {
    const store = createSpaceStore();
    localStorage.setItem("swage:active-space", "   ");
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA]);

    await store.loadSpaces();

    expect(store.currentSpace).toBeNull();
    expect(localStorage.getItem("swage:active-space")).toBeNull();
  });

  it("sets isLoading true during fetch and false after", async () => {
    const store = createSpaceStore();
    let loadingDuringFetch = false;

    vi.mocked(api.listSpaces).mockImplementationOnce(async () => {
      loadingDuringFetch = store.isLoading;
      return [];
    });

    await store.loadSpaces();

    expect(loadingDuringFetch).toBe(true);
    expect(store.isLoading).toBe(false);
  });

  it("clears isLoading even if the API call throws", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockRejectedValueOnce(
      new Error("network error"),
    );

    await expect(store.loadSpaces()).rejects.toThrow("network error");
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// selectSpace
// ---------------------------------------------------------------------------

describe("selectSpace", () => {
  it("sets currentSpace to the matching space from the loaded list", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA, spaceB]);
    await store.loadSpaces();

    await store.selectSpace("proj-b");

    expect(store.currentSpace?.id).toBe("proj-b");
    expect(store.currentSpace?.name).toBe("Space B");
    expect(localStorage.getItem("swage:active-space")).toBe("proj-b");
  });

  it("does not call the API — selection is from the local list", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA]);
    await store.loadSpaces();

    await store.selectSpace("proj-a");

    // getSpace should never be called — we already have the data.
    expect(api.getSpace).not.toHaveBeenCalled();
  });

  it("throws if the space is not found in the list", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA]);
    await store.loadSpaces();

    await expect(store.selectSpace("proj-missing")).rejects.toThrow();
  });
});

// ---------------------------------------------------------------------------
// createSpace
// ---------------------------------------------------------------------------

describe("createSpace", () => {
  it("calls the API, adds the space to the list, and selects it", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([defaultSpace]);
    await store.loadSpaces();

    const newSpace = makeSpace("proj-new", "New Space");
    vi.mocked(api.createSpace).mockResolvedValueOnce(newSpace);

    await store.createSpace("New Space");

    expect(api.createSpace).toHaveBeenCalledWith({
      name: "New Space",
    });
    expect(store.spaces).toHaveLength(2);
    expect(store.currentSpace?.id).toBe("proj-new");
    expect(localStorage.getItem("swage:active-space")).toBe("proj-new");
  });

  it("adds the new space to an existing list", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA]);
    await store.loadSpaces();

    const newSpace = makeSpace("proj-b", "Space B");
    vi.mocked(api.createSpace).mockResolvedValueOnce(newSpace);

    await store.createSpace("Space B");

    expect(store.spaces).toHaveLength(2);
  });
});

// ---------------------------------------------------------------------------
// deleteSpace
// ---------------------------------------------------------------------------

describe("deleteSpace", () => {
  it("removes the space from the list", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA, spaceB]);
    await store.loadSpaces();
    vi.mocked(api.deleteSpace).mockResolvedValueOnce(undefined);

    await store.deleteSpace("proj-b");

    expect(store.spaces.map((p) => p.id)).toEqual(["proj-a"]);
  });

  it("throws and does not call the API when deleting the default space", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([
      defaultSpace,
      spaceA,
    ]);
    await store.loadSpaces();

    await expect(store.deleteSpace("proj-default")).rejects.toThrow();
    expect(api.deleteSpace).not.toHaveBeenCalled();
  });

  it("selects the next space when the current space is deleted", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA, spaceB]);
    await store.loadSpaces();

    await store.selectSpace("proj-a");
    vi.mocked(api.deleteSpace).mockResolvedValueOnce(undefined);

    await store.deleteSpace("proj-a");

    // Should automatically move to proj-b (the remaining space).
    expect(store.currentSpace?.id).toBe("proj-b");
    expect(store.spaces.map((p) => p.id)).toEqual(["proj-b"]);
    expect(localStorage.getItem("swage:active-space")).toBe("proj-b");
  });

  it("sets currentSpace to null when the last space is deleted", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA]);
    await store.loadSpaces();

    await store.selectSpace("proj-a");
    vi.mocked(api.deleteSpace).mockResolvedValueOnce(undefined);

    await store.deleteSpace("proj-a");

    expect(store.currentSpace).toBeNull();
    expect(store.spaces).toHaveLength(0);
    expect(localStorage.getItem("swage:active-space")).toBeNull();
  });

  it("leaves currentSpace unchanged when a non-current space is deleted", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA, spaceB]);
    await store.loadSpaces();

    await store.selectSpace("proj-a");
    vi.mocked(api.deleteSpace).mockResolvedValueOnce(undefined);

    await store.deleteSpace("proj-b");

    expect(store.currentSpace?.id).toBe("proj-a");
    expect(store.spaces).toHaveLength(1);
  });
});

describe("closeSpace", () => {
  it("clears the persisted selection so reload returns to the picker", async () => {
    const store = createSpaceStore();
    vi.mocked(api.listSpaces).mockResolvedValueOnce([spaceA]);
    await store.loadSpaces();
    await store.selectSpace("proj-a");

    store.closeSpace();

    expect(store.currentSpace).toBeNull();
    expect(localStorage.getItem("swage:active-space")).toBeNull();
  });
});
