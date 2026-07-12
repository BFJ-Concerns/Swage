/**
 * Tests for the HTTP API client for space CRUD and space-scoped sessions.
 *
 * All network calls are intercepted via a global fetch mock so these tests
 * run in isolation without a live gateway. Each test verifies the correct
 * URL, method, headers, body, and return type for one API function, plus
 * a shared error-throwing contract for non-2xx responses.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import {
  listSpaces,
  createSpace,
  getSpace,
  updateSpace,
  deleteSpace,
  listSpaceWorkspaces,
  attachSpaceWorkspace,
  updateSpaceWorkspaceTrust,
  detachSpaceWorkspace,
  listSpaceSessions,
  createSpaceSession,
  setSpacesBaseUrl,
} from "../api/spaces";

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
  setSpacesBaseUrl("");
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// Sample fixtures
// ---------------------------------------------------------------------------

const sampleSpace = {
  id: "proj-1",
  name: "My Space",
  description: "",
  is_default: false,
  slug: "my-space",
  created_at: "2026-01-01T00:00:00Z",
};

const sampleWorkspace = {
  id: "workspace-1",
  space_id: "proj-1",
  label: "Codebase",
  path: "/home/user/my-space",
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
// listSpaces
// ---------------------------------------------------------------------------

describe("listSpaces", () => {
  it("sends GET /api/spaces and unwraps the spaces array", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ spaces: [sampleSpace] }),
    );

    const result = await listSpaces();

    expect(fetchMock).toHaveBeenCalledWith("/api/spaces", { method: "GET" });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("proj-1");
    expect(result[0].slug).toBe("my-space");
  });

  it("returns an empty array when there are no spaces", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ spaces: [] }));

    const result = await listSpaces();
    expect(result).toEqual([]);
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));
    await expect(listSpaces()).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// createSpace
// ---------------------------------------------------------------------------

describe("createSpace", () => {
  it("sends POST /api/spaces with JSON body and returns the new space", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleSpace, 201));

    const result = await createSpace({ name: "My Space" });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/spaces",
      expect.objectContaining({
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: "My Space" }),
      }),
    );
    expect(result.id).toBe("proj-1");
    expect(result.slug).toBe("my-space");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 422));
    await expect(createSpace({ name: "Bad" })).rejects.toThrow("422");
  });
});

// ---------------------------------------------------------------------------
// getSpace
// ---------------------------------------------------------------------------

describe("getSpace", () => {
  it("sends GET /api/spaces/:id and returns a Space", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleSpace));

    const result = await getSpace("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/spaces/proj-1", {
      method: "GET",
    });
    expect(result.id).toBe("proj-1");
    expect(result.slug).toBe("my-space");
  });

  it("throws on 404", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));
    await expect(getSpace("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// updateSpace
// ---------------------------------------------------------------------------

describe("updateSpace", () => {
  it("sends PUT /api/spaces/:id with JSON body and returns the updated space", async () => {
    const fetchMock = vi.mocked(fetch);
    const updated = { ...sampleSpace, name: "Renamed", description: "Ops space" };
    fetchMock.mockResolvedValueOnce(mockResponse(updated));

    const result = await updateSpace("proj-1", {
      name: "Renamed",
      description: "Ops space",
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/spaces/proj-1",
      expect.objectContaining({
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: "Renamed", description: "Ops space" }),
      }),
    );
    expect(result.name).toBe("Renamed");
    expect(result.description).toBe("Ops space");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 400));
    await expect(updateSpace("proj-1", {})).rejects.toThrow("400");
  });
});

describe("space workspaces", () => {
  it("lists workspace attachments for a space", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse({ workspaces: [sampleWorkspace] }));

    const result = await listSpaceWorkspaces("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/spaces/proj-1/workspaces", {
      method: "GET",
    });
    expect(result[0].path).toBe("/home/user/my-space");
  });

  it("attaches a workspace to a space", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleWorkspace, 201));

    await attachSpaceWorkspace("proj-1", {
      path: "/home/user/my-space",
      label: "Codebase",
      permissions: ["read", "write"],
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/spaces/proj-1/workspaces",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          path: "/home/user/my-space",
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

    await updateSpaceWorkspaceTrust("proj-1", "workspace-1", { is_trusted: true });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/spaces/proj-1/workspaces/workspace-1/trust",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({ is_trusted: true }),
      }),
    );
  });

  it("detaches a workspace", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(null, 204));

    await detachSpaceWorkspace("proj-1", "workspace-1");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/spaces/proj-1/workspaces/workspace-1",
      { method: "DELETE" },
    );
  });
});

// ---------------------------------------------------------------------------
// deleteSpace
// ---------------------------------------------------------------------------

describe("deleteSpace", () => {
  it("sends DELETE /api/spaces/:id and returns undefined", async () => {
    const fetchMock = vi.mocked(fetch);
    // Gateway returns 204 No Content on successful delete.
    fetchMock.mockResolvedValueOnce(mockResponse(null, 204));

    const result = await deleteSpace("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/spaces/proj-1", {
      method: "DELETE",
    });
    expect(result).toBeUndefined();
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 403));
    await expect(deleteSpace("proj-1")).rejects.toThrow("403");
  });
});

// ---------------------------------------------------------------------------
// listSpaceSessions
// ---------------------------------------------------------------------------

describe("listSpaceSessions", () => {
  it("sends GET /api/spaces/:id/sessions and unwraps the conversations array", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ conversations: [sampleSession] }),
    );

    const result = await listSpaceSessions("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/spaces/proj-1/sessions", {
      method: "GET",
    });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("sess-1");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));
    await expect(listSpaceSessions("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// createSpaceSession
// ---------------------------------------------------------------------------

describe("createSpaceSession", () => {
  it("sends POST /api/spaces/:id/sessions and returns the new session", async () => {
    const fetchMock = vi.mocked(fetch);
    const created = {
      id: "sess-new",
      space_id: "proj-1",
      created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z",
    };
    fetchMock.mockResolvedValueOnce(mockResponse(created, 201));

    const result = await createSpaceSession("proj-1");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/spaces/proj-1/sessions",
      expect.objectContaining({ method: "POST" }),
    );
    expect(result.id).toBe("sess-new");
    expect(result.space_id).toBe("proj-1");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));
    await expect(createSpaceSession("proj-1")).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// Configurable base URL
// ---------------------------------------------------------------------------

describe("setSpacesBaseUrl", () => {
  it("prepends the base URL to all requests", async () => {
    setSpacesBaseUrl("http://localhost:3000");
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ spaces: [] }));

    await listSpaces();

    expect(vi.mocked(fetch)).toHaveBeenCalledWith(
      "http://localhost:3000/api/spaces",
      expect.objectContaining({ method: "GET" }),
    );
  });
});
