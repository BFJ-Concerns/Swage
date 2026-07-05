/**
 * Unit tests for the bench store.
 *
 * All API calls are mocked so tests run in isolation without a gateway.
 * Each test creates a fresh store instance via createBenchStore().
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

import { createBenchStore } from "../stores/benches";
import * as api from "../api/benches";

// ---------------------------------------------------------------------------
// Module mock
// ---------------------------------------------------------------------------

vi.mock("../api/benches", () => ({
  listBenches: vi.fn(),
  createBench: vi.fn(),
  getBench: vi.fn(),
  updateBench: vi.fn(),
  deleteBench: vi.fn(),
  listBenchSessions: vi.fn(),
  createBenchSession: vi.fn(),
  setBenchesBaseUrl: vi.fn(),
}));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeBench(
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

// The default bench — special-cased: cannot be deleted.
const defaultBench = makeBench("proj-default", "default", {
  is_default: true,
});

const benchA = makeBench("proj-a", "Bench A");
const benchB = makeBench("proj-b", "Bench B");

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

beforeEach(() => {
  vi.resetAllMocks();
});

// ---------------------------------------------------------------------------
// Initial state
// ---------------------------------------------------------------------------

describe("initial state", () => {
  it("starts with an empty bench list", () => {
    const store = createBenchStore();
    expect(store.benches).toEqual([]);
  });

  it("starts with no current bench", () => {
    const store = createBenchStore();
    expect(store.currentBench).toBeNull();
  });

  it("starts with isLoading false", () => {
    const store = createBenchStore();
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// loadBenches
// ---------------------------------------------------------------------------

describe("loadBenches", () => {
  it("fetches benches and stores them", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA, benchB]);

    await store.loadBenches();

    expect(store.benches).toHaveLength(2);
    expect(store.benches[0].id).toBe("proj-a");
  });

  it("does not auto-select any bench — the user lands on the picker", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([
      benchA,
      defaultBench,
      benchB,
    ]);

    await store.loadBenches();

    // Even when a default bench exists, leave currentBench null so the
    // bench picker is shown on entry.
    expect(store.currentBench).toBeNull();
  });

  it("leaves currentBench null when no bench is flagged as default", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA, benchB]);

    await store.loadBenches();

    expect(store.currentBench).toBeNull();
  });

  it("sets isLoading true during fetch and false after", async () => {
    const store = createBenchStore();
    let loadingDuringFetch = false;

    vi.mocked(api.listBenches).mockImplementationOnce(async () => {
      loadingDuringFetch = store.isLoading;
      return [];
    });

    await store.loadBenches();

    expect(loadingDuringFetch).toBe(true);
    expect(store.isLoading).toBe(false);
  });

  it("clears isLoading even if the API call throws", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockRejectedValueOnce(
      new Error("network error"),
    );

    await expect(store.loadBenches()).rejects.toThrow("network error");
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// selectBench
// ---------------------------------------------------------------------------

describe("selectBench", () => {
  it("sets currentBench to the matching bench from the loaded list", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA, benchB]);
    await store.loadBenches();

    await store.selectBench("proj-b");

    expect(store.currentBench?.id).toBe("proj-b");
    expect(store.currentBench?.name).toBe("Bench B");
  });

  it("does not call the API — selection is from the local list", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA]);
    await store.loadBenches();

    await store.selectBench("proj-a");

    // getBench should never be called — we already have the data.
    expect(api.getBench).not.toHaveBeenCalled();
  });

  it("throws if the bench is not found in the list", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA]);
    await store.loadBenches();

    await expect(store.selectBench("proj-missing")).rejects.toThrow();
  });
});

// ---------------------------------------------------------------------------
// createBench
// ---------------------------------------------------------------------------

describe("createBench", () => {
  it("calls the API, adds the bench to the list, and selects it", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([defaultBench]);
    await store.loadBenches();

    const newBench = makeBench("proj-new", "New Bench");
    vi.mocked(api.createBench).mockResolvedValueOnce(newBench);

    await store.createBench("New Bench");

    expect(api.createBench).toHaveBeenCalledWith({
      name: "New Bench",
    });
    expect(store.benches).toHaveLength(2);
    expect(store.currentBench?.id).toBe("proj-new");
  });

  it("adds the new bench to an existing list", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA]);
    await store.loadBenches();

    const newBench = makeBench("proj-b", "Bench B");
    vi.mocked(api.createBench).mockResolvedValueOnce(newBench);

    await store.createBench("Bench B");

    expect(store.benches).toHaveLength(2);
  });
});

// ---------------------------------------------------------------------------
// deleteBench
// ---------------------------------------------------------------------------

describe("deleteBench", () => {
  it("removes the bench from the list", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA, benchB]);
    await store.loadBenches();
    vi.mocked(api.deleteBench).mockResolvedValueOnce(undefined);

    await store.deleteBench("proj-b");

    expect(store.benches.map((p) => p.id)).toEqual(["proj-a"]);
  });

  it("throws and does not call the API when deleting the default bench", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([
      defaultBench,
      benchA,
    ]);
    await store.loadBenches();

    await expect(store.deleteBench("proj-default")).rejects.toThrow();
    expect(api.deleteBench).not.toHaveBeenCalled();
  });

  it("selects the next bench when the current bench is deleted", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA, benchB]);
    await store.loadBenches();

    await store.selectBench("proj-a");
    vi.mocked(api.deleteBench).mockResolvedValueOnce(undefined);

    await store.deleteBench("proj-a");

    // Should automatically move to proj-b (the remaining bench).
    expect(store.currentBench?.id).toBe("proj-b");
    expect(store.benches.map((p) => p.id)).toEqual(["proj-b"]);
  });

  it("sets currentBench to null when the last bench is deleted", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA]);
    await store.loadBenches();

    await store.selectBench("proj-a");
    vi.mocked(api.deleteBench).mockResolvedValueOnce(undefined);

    await store.deleteBench("proj-a");

    expect(store.currentBench).toBeNull();
    expect(store.benches).toHaveLength(0);
  });

  it("leaves currentBench unchanged when a non-current bench is deleted", async () => {
    const store = createBenchStore();
    vi.mocked(api.listBenches).mockResolvedValueOnce([benchA, benchB]);
    await store.loadBenches();

    await store.selectBench("proj-a");
    vi.mocked(api.deleteBench).mockResolvedValueOnce(undefined);

    await store.deleteBench("proj-b");

    expect(store.currentBench?.id).toBe("proj-a");
    expect(store.benches).toHaveLength(1);
  });
});
