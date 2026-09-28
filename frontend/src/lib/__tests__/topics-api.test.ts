/**
 * Tests for the HTTP API client for topic CRUD and topic-scoped sessions.
 *
 * All network calls are intercepted via a global fetch mock so these tests
 * run in isolation without a live gateway. Each test verifies the correct
 * URL, method, headers, body, and return type for one API function, plus
 * a shared error-throwing contract for non-2xx responses.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import {
  listTopics,
  createTopic,
  getTopic,
  updateTopic,
  deleteTopic,
  listTopicWorkspaces,
  attachTopicWorkspace,
  updateTopicWorkspaceTrust,
  relinkTopicWorkspace,
  detachTopicWorkspace,
  listTopicSessions,
  createTopicSession,
  setTopicsBaseUrl,
} from "../api/topics";

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
  setTopicsBaseUrl("");
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// Sample fixtures
// ---------------------------------------------------------------------------

const sampleTopic = {
  id: "proj-1",
  name: "My Topic",
  description: "",
  is_default: false,
  slug: "my-topic",
  created_at: "2026-01-01T00:00:00Z",
};

const sampleWorkspace = {
  id: "workspace-1",
  topic_id: "proj-1",
  label: "Codebase",
  path: "/home/user/my-topic",
  permissions: ["read", "write"],
  is_trusted: false,
  created_at: "2026-01-01T00:00:00Z",
};

const sampleSession = {
  id: "sess-1",
  agent_id: "gateway",
  created_at: "2026-01-01T00:00:00Z",
  last_activity: "2026-01-01T01:00:00Z",
  message_preview: "Hello",
  has_interrupted_turn: false,
};

// ---------------------------------------------------------------------------
// listTopics
// ---------------------------------------------------------------------------

describe("listTopics", () => {
  it("sends GET /api/topics and unwraps the topics array", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ topics: [sampleTopic] }),
    );

    const result = await listTopics();

    expect(fetchMock).toHaveBeenCalledWith("/api/topics", { method: "GET" });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("proj-1");
    expect(result[0].slug).toBe("my-topic");
  });

  it("returns an empty array when there are no topics", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ topics: [] }));

    const result = await listTopics();
    expect(result).toEqual([]);
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));
    await expect(listTopics()).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// createTopic
// ---------------------------------------------------------------------------

describe("createTopic", () => {
  it("sends POST /api/topics with JSON body and returns the new topic", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleTopic, 201));

    const result = await createTopic({ name: "My Topic" });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/topics",
      expect.objectContaining({
        method: "POST",
        headers: { "Content-Type": "application/json" },
        // The generated client sends the wire contract whole: the view
        // type's optional description is the wire's required one.
        body: JSON.stringify({ description: "", name: "My Topic" }),
      }),
    );
    expect(result.id).toBe("proj-1");
    expect(result.slug).toBe("my-topic");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 422));
    await expect(createTopic({ name: "Bad" })).rejects.toThrow("422");
  });
});

// ---------------------------------------------------------------------------
// getTopic
// ---------------------------------------------------------------------------

describe("getTopic", () => {
  it("sends GET /api/topics/:id and returns a Topic", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleTopic));

    const result = await getTopic("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/topics/proj-1", {
      method: "GET",
    });
    expect(result.id).toBe("proj-1");
    expect(result.slug).toBe("my-topic");
  });

  it("throws on 404", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));
    await expect(getTopic("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// updateTopic
// ---------------------------------------------------------------------------

describe("updateTopic", () => {
  it("sends PUT /api/topics/:id with JSON body and returns the updated topic", async () => {
    const fetchMock = vi.mocked(fetch);
    const updated = { ...sampleTopic, name: "Renamed", description: "Ops topic" };
    fetchMock.mockResolvedValueOnce(mockResponse(updated));

    const result = await updateTopic("proj-1", {
      name: "Renamed",
      description: "Ops topic",
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/topics/proj-1",
      expect.objectContaining({
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: "Renamed", description: "Ops topic" }),
      }),
    );
    expect(result.name).toBe("Renamed");
    expect(result.description).toBe("Ops topic");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 400));
    await expect(updateTopic("proj-1", {})).rejects.toThrow("400");
  });
});

describe("topic workspaces", () => {
  it("lists workspace attachments for a topic", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse({ workspaces: [sampleWorkspace] }));

    const result = await listTopicWorkspaces("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/topics/proj-1/workspaces", {
      method: "GET",
    });
    expect(result[0].path).toBe("/home/user/my-topic");
  });

  it("attaches a workspace to a topic", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleWorkspace, 201));

    await attachTopicWorkspace("proj-1", {
      path: "/home/user/my-topic",
      label: "Codebase",
      permissions: ["read", "write"],
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/topics/proj-1/workspaces",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          path: "/home/user/my-topic",
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

    await updateTopicWorkspaceTrust("proj-1", "workspace-1", { is_trusted: true });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/topics/proj-1/workspaces/workspace-1/trust",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({ is_trusted: true }),
      }),
    );
  });

  it("detaches a workspace", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(null, 204));

    await detachTopicWorkspace("proj-1", "workspace-1");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/topics/proj-1/workspaces/workspace-1",
      { method: "DELETE" },
    );
  });

  it("sends a relink through the projected route with only the new path", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse({ ...sampleWorkspace, path: "/home/user/moved" }));

    await relinkTopicWorkspace("proj-1", "workspace-1", "/home/user/moved");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/topics/proj-1/workspaces/workspace-1/relink",
      expect.objectContaining({ method: "PUT", body: JSON.stringify({ path: "/home/user/moved" }) }),
    );
  });
});

// ---------------------------------------------------------------------------
// deleteTopic
// ---------------------------------------------------------------------------

describe("deleteTopic", () => {
  it("sends DELETE /api/topics/:id and returns undefined", async () => {
    const fetchMock = vi.mocked(fetch);
    // Gateway returns 204 No Content on successful delete.
    fetchMock.mockResolvedValueOnce(mockResponse(null, 204));

    const result = await deleteTopic("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/topics/proj-1", {
      method: "DELETE",
    });
    expect(result).toBeUndefined();
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 403));
    await expect(deleteTopic("proj-1")).rejects.toThrow("403");
  });
});

// ---------------------------------------------------------------------------
// listTopicSessions
// ---------------------------------------------------------------------------

describe("listTopicSessions", () => {
  it("sends GET /api/topics/:id/sessions and unwraps the conversations array", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ conversations: [sampleSession] }),
    );

    const result = await listTopicSessions("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/topics/proj-1/sessions", {
      method: "GET",
    });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("sess-1");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));
    await expect(listTopicSessions("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// createTopicSession
// ---------------------------------------------------------------------------

describe("createTopicSession", () => {
  it("sends POST /api/topics/:id/sessions and returns the new session", async () => {
    const fetchMock = vi.mocked(fetch);
    const created = {
      id: "sess-new",
      agent_id: "gateway",
      topic_id: "proj-1",
      created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z",
    };
    fetchMock.mockResolvedValueOnce(mockResponse(created, 201));

    const result = await createTopicSession("proj-1");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/topics/proj-1/sessions",
      expect.objectContaining({ method: "POST" }),
    );
    expect(result.id).toBe("sess-new");
    expect(result.topic_id).toBe("proj-1");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));
    await expect(createTopicSession("proj-1")).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// Configurable base URL
// ---------------------------------------------------------------------------

describe("setTopicsBaseUrl", () => {
  it("prepends the base URL to all requests", async () => {
    setTopicsBaseUrl("http://localhost:3000");
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ topics: [] }));

    await listTopics();

    expect(vi.mocked(fetch)).toHaveBeenCalledWith(
      "http://localhost:3000/api/topics",
      expect.objectContaining({ method: "GET" }),
    );
  });
});
