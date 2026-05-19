/**
 * Tests for the HTTP API client for bench CRUD and bench-scoped sessions.
 *
 * All network calls are intercepted via a global fetch mock so these tests
 * run in isolation without a live gateway. Each test verifies the correct
 * URL, method, headers, body, and return type for one API function, plus
 * a shared error-throwing contract for non-2xx responses.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import {
  listBenches,
  createBench,
  getBench,
  updateBench,
  deleteBench,
  listBenchWorkspaces,
  attachBenchWorkspace,
  updateBenchWorkspaceTrust,
  detachBenchWorkspace,
  listBenchSessions,
  createBenchSession,
  setBenchesBaseUrl,
} from "../api/benches";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Build a minimal fetch Response stub. */
function mockResponse<T>(body: T, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    text: async () => JSON.stringify(body),
  } as unknown as Response;
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

beforeEach(() => {
  // Replace the global fetch with a vitest spy so tests control responses.
  vi.stubGlobal("fetch", vi.fn());
  // Ensure base URL is reset to same-origin default between tests.
  setBenchesBaseUrl("");
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// Sample fixtures
// ---------------------------------------------------------------------------

const sampleBench = {
  id: "proj-1",
  name: "My Bench",
  description: "",
  is_default: false,
  slug: "my-bench",
  created_at: "2026-01-01T00:00:00Z",
};

const sampleWorkspace = {
  id: "workspace-1",
  bench_id: "proj-1",
  label: "Codebase",
  path: "/home/user/my-bench",
  permissions: ["read", "write"],
  is_trusted: false,
  created_at: "2026-01-01T00:00:00Z",
};

const sampleSession = {
  id: "sess-1",
  created_at: "2026-01-01T00:00:00Z",
  last_activity: "2026-01-01T01:00:00Z",
  message_preview: "Hello",
  has_interrupted_turn: false,
};

// ---------------------------------------------------------------------------
// listBenches
// ---------------------------------------------------------------------------

describe("listBenches", () => {
  it("sends GET /api/benches and unwraps the benches array", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ benches: [sampleBench] }),
    );

    const result = await listBenches();

    expect(fetchMock).toHaveBeenCalledWith("/api/benches", { method: "GET" });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("proj-1");
    expect(result[0].slug).toBe("my-bench");
  });

  it("returns an empty array when there are no benches", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ benches: [] }));

    const result = await listBenches();
    expect(result).toEqual([]);
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));
    await expect(listBenches()).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// createBench
// ---------------------------------------------------------------------------

describe("createBench", () => {
  it("sends POST /api/benches with JSON body and returns the new bench", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleBench, 201));

    const result = await createBench({ name: "My Bench" });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/benches",
      expect.objectContaining({
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: "My Bench" }),
      }),
    );
    expect(result.id).toBe("proj-1");
    expect(result.slug).toBe("my-bench");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 422));
    await expect(createBench({ name: "Bad" })).rejects.toThrow("422");
  });
});

// ---------------------------------------------------------------------------
// getBench
// ---------------------------------------------------------------------------

describe("getBench", () => {
  it("sends GET /api/benches/:id and returns a Bench", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleBench));

    const result = await getBench("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/benches/proj-1", {
      method: "GET",
    });
    expect(result.id).toBe("proj-1");
    expect(result.slug).toBe("my-bench");
  });

  it("throws on 404", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));
    await expect(getBench("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// updateBench
// ---------------------------------------------------------------------------

describe("updateBench", () => {
  it("sends PUT /api/benches/:id with JSON body and returns the updated bench", async () => {
    const fetchMock = vi.mocked(fetch);
    const updated = { ...sampleBench, name: "Renamed", description: "Ops bench" };
    fetchMock.mockResolvedValueOnce(mockResponse(updated));

    const result = await updateBench("proj-1", {
      name: "Renamed",
      description: "Ops bench",
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/benches/proj-1",
      expect.objectContaining({
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: "Renamed", description: "Ops bench" }),
      }),
    );
    expect(result.name).toBe("Renamed");
    expect(result.description).toBe("Ops bench");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 400));
    await expect(updateBench("proj-1", {})).rejects.toThrow("400");
  });
});

describe("bench workspaces", () => {
  it("lists workspace attachments for a bench", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse({ workspaces: [sampleWorkspace] }));

    const result = await listBenchWorkspaces("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/benches/proj-1/workspaces", {
      method: "GET",
    });
    expect(result[0].path).toBe("/home/user/my-bench");
  });

  it("attaches a workspace to a bench", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleWorkspace, 201));

    await attachBenchWorkspace("proj-1", {
      path: "/home/user/my-bench",
      label: "Codebase",
      permissions: ["read", "write"],
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/benches/proj-1/workspaces",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          path: "/home/user/my-bench",
          label: "Codebase",
          permissions: ["read", "write"],
        }),
      }),
    );
  });

  it("updates workspace trust", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ ...sampleWorkspace, is_trusted: true }),
    );

    await updateBenchWorkspaceTrust("proj-1", "workspace-1", { is_trusted: true });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/benches/proj-1/workspaces/workspace-1/trust",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({ is_trusted: true }),
      }),
    );
  });

  it("detaches a workspace", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(null, 204));

    await detachBenchWorkspace("proj-1", "workspace-1");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/benches/proj-1/workspaces/workspace-1",
      { method: "DELETE" },
    );
  });
});

// ---------------------------------------------------------------------------
// deleteBench
// ---------------------------------------------------------------------------

describe("deleteBench", () => {
  it("sends DELETE /api/benches/:id and returns undefined", async () => {
    const fetchMock = vi.mocked(fetch);
    // Gateway returns 204 No Content on successful delete.
    fetchMock.mockResolvedValueOnce(mockResponse(null, 204));

    const result = await deleteBench("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/benches/proj-1", {
      method: "DELETE",
    });
    expect(result).toBeUndefined();
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 403));
    await expect(deleteBench("proj-1")).rejects.toThrow("403");
  });
});

// ---------------------------------------------------------------------------
// listBenchSessions
// ---------------------------------------------------------------------------

describe("listBenchSessions", () => {
  it("sends GET /api/benches/:id/sessions and unwraps the conversations array", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ conversations: [sampleSession] }),
    );

    const result = await listBenchSessions("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/benches/proj-1/sessions", {
      method: "GET",
    });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("sess-1");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));
    await expect(listBenchSessions("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// createBenchSession
// ---------------------------------------------------------------------------

describe("createBenchSession", () => {
  it("sends POST /api/benches/:id/sessions and returns the new session", async () => {
    const fetchMock = vi.mocked(fetch);
    const created = {
      id: "sess-new",
      bench_id: "proj-1",
      created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z",
    };
    fetchMock.mockResolvedValueOnce(mockResponse(created, 201));

    const result = await createBenchSession("proj-1");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/benches/proj-1/sessions",
      expect.objectContaining({ method: "POST" }),
    );
    expect(result.id).toBe("sess-new");
    expect(result.bench_id).toBe("proj-1");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));
    await expect(createBenchSession("proj-1")).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// Configurable base URL
// ---------------------------------------------------------------------------

describe("setBenchesBaseUrl", () => {
  it("prepends the base URL to all requests", async () => {
    setBenchesBaseUrl("http://localhost:3000");
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ benches: [] }));

    await listBenches();

    expect(vi.mocked(fetch)).toHaveBeenCalledWith(
      "http://localhost:3000/api/benches",
      expect.objectContaining({ method: "GET" }),
    );
  });
});
