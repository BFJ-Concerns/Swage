/**
 * Tests for the HTTP API client for conversation CRUD.
 *
 * All network calls are intercepted via a global fetch mock so these tests
 * run in isolation without a live gateway. Each test verifies the correct
 * URL, method, and return type for one API function, plus a shared
 * error-throwing contract for non-2xx responses.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import {
  listConversations,
  createConversation,
  getConversation,
  deleteConversation,
  listBranches,
  createBranch,
  getThread,
  abandonThread,
  setBaseUrl,
} from "../api/conversations";

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
  setBaseUrl("");
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// listConversations
// ---------------------------------------------------------------------------

describe("listConversations", () => {
  it("sends GET /api/topics/:topicId/sessions and unwraps the conversations array", async () => {
    const fetchMock = vi.mocked(fetch);
    // Gateway returns { conversations: [...] } — the client must unwrap.
    fetchMock.mockResolvedValueOnce(
      mockResponse({
        conversations: [
          {
            id: "conv-1",
            agent_id: "gateway",
            created_at: "2026-01-01T00:00:00Z",
            last_activity: "2026-01-01T00:00:00Z",
            message_preview: "Hello",
            has_interrupted_turn: false,
          },
        ],
      }),
    );

    const result = await listConversations("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/topics/proj-1/sessions", {
      method: "GET",
    });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("conv-1");
    expect(result[0].has_interrupted_turn).toBe(false);
  });

  it("returns an empty array when the topic has no conversations", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(
      mockResponse({ conversations: [] }),
    );

    const result = await listConversations("proj-empty");
    expect(result).toEqual([]);
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));

    await expect(listConversations("proj-1")).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// createConversation
// ---------------------------------------------------------------------------

describe("createConversation", () => {
  it("sends POST /api/topics/:topicId/sessions and returns ConversationResponse", async () => {
    const fetchMock = vi.mocked(fetch);
    const created = {
      id: "new-conv",
      agent_id: "gateway",
      topic_id: "proj-1",
      created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z",
    };
    fetchMock.mockResolvedValueOnce(mockResponse(created, 201));

    const result = await createConversation("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/topics/proj-1/sessions", {
      method: "POST",
    });
    expect(result.id).toBe("new-conv");
    expect(result.topic_id).toBe("proj-1");
  });

  it("creates a general-scope conversation when no topic is given", async () => {
    const fetchMock = vi.mocked(fetch);
    const created = {
      id: "general-conv",
      agent_id: "gateway",
      created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z",
    };
    fetchMock.mockResolvedValueOnce(mockResponse(created, 201));

    const result = await createConversation(null);

    expect(fetchMock).toHaveBeenCalledWith("/api/conversations", { method: "POST" });
    expect(result.id).toBe("general-conv");
    expect(result.topic_id).toBeUndefined();
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 400));

    await expect(createConversation("proj-1")).rejects.toThrow("400");
  });

  it("sends the selected model, agent, and effort together", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse({ id: "new-conv" }, 201));

    await createConversation("proj-1", "small-model", "researcher", "high");

    expect(fetchMock).toHaveBeenCalledWith("/api/topics/proj-1/sessions", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        selected_model_id: "small-model",
        agent_id: "researcher",
        effort: "high",
      }),
    });
  });
});

// ---------------------------------------------------------------------------
// getConversation
// ---------------------------------------------------------------------------

describe("getConversation", () => {
  it("sends GET /api/conversations/:id and returns ConversationDetail", async () => {
    const fetchMock = vi.mocked(fetch);
    const detail = {
      id: "conv-42",
      agent_id: "gateway",
      topic_id: "default",
      created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T01:00:00Z",
      messages: [
        {
          role: "user",
          content: "Hi",
          timestamp: "2026-01-01T00:00:00Z",
          turn_state: "completed",
        },
      ],
    };
    fetchMock.mockResolvedValueOnce(mockResponse(detail));

    const result = await getConversation("conv-42");

    expect(fetchMock).toHaveBeenCalledWith("/api/conversations/conv-42", {
      method: "GET",
    });
    expect(result.id).toBe("conv-42");
    expect(result.messages).toHaveLength(1);
    expect(result.messages[0].role).toBe("user");
  });

  it("throws on 404", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));

    await expect(getConversation("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// deleteConversation
// ---------------------------------------------------------------------------

describe("deleteConversation", () => {
  it("sends DELETE /api/conversations/:id and returns undefined", async () => {
    const fetchMock = vi.mocked(fetch);
    // Gateway returns 204 No Content on successful delete.
    fetchMock.mockResolvedValueOnce(mockResponse(null, 204));

    const result = await deleteConversation("conv-99");

    expect(fetchMock).toHaveBeenCalledWith("/api/conversations/conv-99", {
      method: "DELETE",
    });
    expect(result).toBeUndefined();
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 403));

    await expect(deleteConversation("conv-99")).rejects.toThrow("403");
  });
});

// ---------------------------------------------------------------------------
// Configurable base URL
// ---------------------------------------------------------------------------

describe("setBaseUrl", () => {
  it("prepends the base URL to all requests", async () => {
    setBaseUrl("http://localhost:3000");
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ conversations: [] }));

    await listConversations("proj-1");

    expect(vi.mocked(fetch)).toHaveBeenCalledWith(
      "http://localhost:3000/api/topics/proj-1/sessions",
      expect.objectContaining({ method: "GET" }),
    );
  });
});

// ---------------------------------------------------------------------------
// Branches
// ---------------------------------------------------------------------------

describe("branches", () => {
  const branch = {
    child_thread_id: "branch-1",
    parent_thread_id: "root-1",
    branched_from_message_id: "msg-1",
    branched_from_preview: "first",
    state: "idle",
    created_at: "2026-01-01T00:00:00Z",
  };

  it("listBranches reads the conversation's branches", async () => {
    vi.mocked(fetch).mockResolvedValue(mockResponse({ branches: [branch] }));

    const result = await listBranches("conv-1");

    expect(fetch).toHaveBeenCalledWith(
      "/api/conversations/conv-1/branches",
      expect.objectContaining({ method: "GET" }),
    );
    expect(result).toEqual([branch]);
  });

  it("createBranch posts the thread and message to branch from", async () => {
    vi.mocked(fetch).mockResolvedValue(mockResponse(branch, 201));

    const result = await createBranch("conv-1", "root-1", "msg-1");

    expect(fetch).toHaveBeenCalledWith(
      "/api/conversations/conv-1/branches",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ thread_id: "root-1", from_message_id: "msg-1" }),
      }),
    );
    expect(result).toEqual(branch);
  });

  it("getThread reads one thread", async () => {
    const thread = { id: "branch-1", session_id: "conv-1", agent_id: "gateway", state: "idle", messages: [], branch };
    vi.mocked(fetch).mockResolvedValue(mockResponse(thread));

    const result = await getThread("branch-1");

    expect(fetch).toHaveBeenCalledWith("/api/threads/branch-1", expect.objectContaining({ method: "GET" }));
    expect(result).toEqual(thread);
  });

  it("abandonThread posts to the thread's abandon route", async () => {
    const thread = { id: "branch-1", session_id: "conv-1", agent_id: "gateway", state: "abandoned", messages: [], branch };
    vi.mocked(fetch).mockResolvedValue(mockResponse(thread));

    const result = await abandonThread("branch-1");

    expect(fetch).toHaveBeenCalledWith(
      "/api/threads/branch-1/abandon",
      expect.objectContaining({ method: "POST" }),
    );
    expect(result.state).toBe("abandoned");
  });
});
