/**
 * Unit tests for the conversation store.
 *
 * All API calls are mocked so tests run in isolation without a gateway.
 * Each test creates a fresh store instance via createConversationStore().
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

import { createConversationStore } from "../stores/conversations";
import * as api from "../api/conversations";

// ---------------------------------------------------------------------------
// Module mock
// ---------------------------------------------------------------------------

vi.mock("../api/conversations", () => ({
  listConversations: vi.fn(),
  createConversation: vi.fn(),
  getConversation: vi.fn(),
  deleteConversation: vi.fn(),
}));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeSummary(id: string, last_activity = "2026-01-01T00:00:00Z") {
  return {
    id,
    created_at: "2026-01-01T00:00:00Z",
    last_activity,
    message_preview: null,
    has_interrupted_turn: false,
  };
}

function makeDetail(id: string) {
  return {
    id,
    space_id: "default",
    created_at: "2026-01-01T00:00:00Z",
    last_activity: "2026-01-01T00:00:00Z",
    messages: [] as import("../types").MessageView[],
  };
}

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
  it("starts with an empty conversation list", () => {
    const store = createConversationStore();
    expect(store.conversations).toEqual([]);
  });

  it("starts with no active conversation", () => {
    const store = createConversationStore();
    expect(store.activeConversation).toBeNull();
  });

  it("starts with isLoading false", () => {
    const store = createConversationStore();
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// loadConversations
// ---------------------------------------------------------------------------

describe("loadConversations", () => {
  it("fetches conversations and sorts them by last_activity descending", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-old", "2026-01-01T00:00:00Z"),
      makeSummary("conv-new", "2026-01-03T00:00:00Z"),
      makeSummary("conv-mid", "2026-01-02T00:00:00Z"),
    ]);

    await store.loadConversations("proj-1");

    expect(store.conversations.map((c) => c.id)).toEqual([
      "conv-new",
      "conv-mid",
      "conv-old",
    ]);
  });

  it("sets isLoading true during fetch and false after", async () => {
    const store = createConversationStore();
    let loadingDuringFetch = false;

    vi.mocked(api.listConversations).mockImplementationOnce(async () => {
      loadingDuringFetch = store.isLoading;
      return [];
    });

    await store.loadConversations("proj-1");

    expect(loadingDuringFetch).toBe(true);
    expect(store.isLoading).toBe(false);
  });

  it("clears isLoading even if the API call throws", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockRejectedValueOnce(new Error("network error"));

    await expect(store.loadConversations("proj-1")).rejects.toThrow(
      "network error",
    );
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// selectConversation
// ---------------------------------------------------------------------------

describe("selectConversation", () => {
  it("fetches the conversation detail and sets it as active", async () => {
    const store = createConversationStore();
    const detail = makeDetail("conv-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(detail);

    await store.selectConversation("conv-1");

    expect(api.getConversation).toHaveBeenCalledWith("conv-1");
    expect(store.activeConversation).toEqual(detail);
  });

  it("sets isLoading true during fetch and false after", async () => {
    const store = createConversationStore();
    let loadingDuringFetch = false;

    vi.mocked(api.getConversation).mockImplementationOnce(async () => {
      loadingDuringFetch = store.isLoading;
      return makeDetail("conv-1");
    });

    await store.selectConversation("conv-1");

    expect(loadingDuringFetch).toBe(true);
    expect(store.isLoading).toBe(false);
  });

  it("uses the fetched detail as authoritative after an off-screen partial response", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-a"),
      makeSummary("conv-b"),
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-b"));
    await store.selectConversation("conv-b");

    store.appendAssistantMessage({
      content: "Partial response",
      timestamp: "2026-01-01T00:00:01Z",
      turn_state: "failed",
    }, "conv-a");

    vi.mocked(api.getConversation).mockResolvedValueOnce({
      ...makeDetail("conv-a"),
      messages: [{
        role: "assistant",
        content: "Partial response",
        timestamp: "2026-01-01T00:00:02Z",
        turn_state: "failed",
      }],
    });
    await store.selectConversation("conv-a");

    expect(store.activeConversation?.messages).toHaveLength(1);
    expect(store.activeConversation?.messages[0]?.timestamp).toBe("2026-01-01T00:00:02Z");
  });
});

// ---------------------------------------------------------------------------
// createConversation
// ---------------------------------------------------------------------------

describe("createConversation", () => {
  it("creates a conversation, adds it to the list, and selects it", async () => {
    const store = createConversationStore();
    const resp = {
      id: "new-conv",
      space_id: "default",
      created_at: "2026-01-05T00:00:00Z",
      last_activity: "2026-01-05T00:00:00Z",
    };
    vi.mocked(api.createConversation).mockResolvedValueOnce(resp);

    await store.createConversation("proj-1");

    expect(api.createConversation).toHaveBeenCalledWith(
      "proj-1",
      undefined,
      undefined,
      undefined,
    );
    expect(store.conversations).toHaveLength(1);
    expect(store.conversations[0].id).toBe("new-conv");
    expect(store.activeConversation?.id).toBe("new-conv");
    expect(store.activeConversation?.messages).toEqual([]);
  });

  it("prepends the new conversation to an existing list", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("existing-conv", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");

    vi.mocked(api.createConversation).mockResolvedValueOnce({
      id: "new-conv",
      space_id: "default",
      created_at: "2026-01-05T00:00:00Z",
      last_activity: "2026-01-05T00:00:00Z",
    });
    await store.createConversation("proj-1");

    expect(store.conversations[0].id).toBe("new-conv");
    expect(store.conversations[1].id).toBe("existing-conv");
  });

  it("threads the selected agent and effort to the creation API", async () => {
    const store = createConversationStore();
    vi.mocked(api.createConversation).mockResolvedValueOnce({
      id: "new-conv",
      space_id: "default",
      created_at: "2026-01-05T00:00:00Z",
      last_activity: "2026-01-05T00:00:00Z",
    });

    await store.createConversation("proj-1", "small-model", "researcher", "high");

    expect(api.createConversation).toHaveBeenCalledWith(
      "proj-1",
      "small-model",
      "researcher",
      "high",
    );
  });
});

// ---------------------------------------------------------------------------
// deleteConversation
// ---------------------------------------------------------------------------

describe("deleteConversation", () => {
  it("removes the conversation from the list", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-02T00:00:00Z"),
      makeSummary("conv-2", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.deleteConversation).mockResolvedValueOnce(undefined);

    await store.deleteConversation("conv-2");

    expect(store.conversations.map((c) => c.id)).toEqual(["conv-1"]);
  });

  it("selects the next most recent conversation when active is deleted", async () => {
    const store = createConversationStore();
    // List sorted newest-first after loadConversations.
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-02T00:00:00Z"),
      makeSummary("conv-2", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");

    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    vi.mocked(api.deleteConversation).mockResolvedValueOnce(undefined);
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-2"));
    await store.deleteConversation("conv-1");

    expect(store.activeConversation?.id).toBe("conv-2");
  });

  it("sets active to null when the only conversation is deleted", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([makeSummary("conv-1")]);
    await store.loadConversations("proj-1");

    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    vi.mocked(api.deleteConversation).mockResolvedValueOnce(undefined);
    await store.deleteConversation("conv-1");

    expect(store.activeConversation).toBeNull();
    expect(store.conversations).toHaveLength(0);
  });

  it("leaves active conversation unchanged when a non-active is deleted", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-02T00:00:00Z"),
      makeSummary("conv-2", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");

    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    vi.mocked(api.deleteConversation).mockResolvedValueOnce(undefined);
    await store.deleteConversation("conv-2");

    expect(store.activeConversation?.id).toBe("conv-1");
    expect(store.conversations).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// durable local updates
// ---------------------------------------------------------------------------

describe("durable local updates", () => {
  it("appends an optimistic user message to the active conversation", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    store.appendOptimisticUserMessage({
      content: "Hello, Swage!",
      timestamp: "2026-01-01T12:00:00Z",
    });

    expect(store.activeConversation!.messages).toHaveLength(1);
    expect(store.activeConversation!.messages[0]).toMatchObject({
      role: "user",
      content: "Hello, Swage!",
      turn_state: "pending",
    });
    expect(store.conversations[0].message_preview).toBe("Hello, Swage!");
    expect(store.conversations[0].last_activity).toBe("2026-01-01T12:00:00Z");
  });

  it("does not throw and does not modify state when there is no active conversation", () => {
    const store = createConversationStore();

    expect(() =>
      store.appendOptimisticUserMessage({
        content: "Ignored",
        timestamp: "2026-01-01T00:00:00Z",
      }),
    ).not.toThrow();

    expect(store.activeConversation).toBeNull();
  });

  it("preserves existing messages when appending an assistant message", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");
    const detailWithMessage = {
      ...makeDetail("conv-1"),
      messages: [
        {
          role: "user" as const,
          content: "First message",
          timestamp: "2026-01-01T00:00:00Z",
          turn_state: "completed" as const,
        },
      ],
    };
    vi.mocked(api.getConversation).mockResolvedValueOnce(detailWithMessage);
    await store.selectConversation("conv-1");

    store.appendAssistantMessage({
      content: "Reply",
      timestamp: "2026-01-01T00:01:00Z",
      turn_state: "completed",
      turn_id: "turn-1",
    });

    expect(store.activeConversation!.messages).toHaveLength(2);
    expect(store.activeConversation!.messages[1].role).toBe("assistant");
    expect(store.activeConversation!.messages[1].turn_id).toBe("turn-1");
    expect(store.conversations[0].message_preview).toBe("Reply");
  });

  it("preserves the existing preview for a tool-only assistant message", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-2", "2026-01-01T00:00:30Z"),
      { ...makeSummary("conv-1"), message_preview: "Read README.md" },
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    store.appendAssistantMessage({
      content: "",
      timestamp: "2026-01-01T00:01:00Z",
      turn_state: "completed",
      turn_id: "turn-1",
      tool_calls: [
        {
          call_id: "call-1",
          tool: "file_read",
          arguments: {},
          result: "contents",
          status: "success",
          duration_ms: 8,
          truncated: false,
          total_bytes: 8,
        },
      ],
    });

    expect(store.activeConversation!.messages.at(-1)).toMatchObject({
      content: "",
      tool_calls: [expect.objectContaining({ call_id: "call-1" })],
    });
    expect(store.conversations[0]).toMatchObject({
      id: "conv-1",
      last_activity: "2026-01-01T00:01:00Z",
      message_preview: "Read README.md",
    });
    expect(store.conversations.map((conversation) => conversation.id)).toEqual([
      "conv-1",
      "conv-2",
    ]);
  });

  it("marks the summary as interrupted when an interrupted assistant message is appended", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    store.appendAssistantMessage({
      content: "Partial reply",
      timestamp: "2026-01-01T00:02:00Z",
      turn_state: "interrupted",
    });

    expect(store.conversations[0].has_interrupted_turn).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// refreshActiveConversation
// ---------------------------------------------------------------------------

describe("refreshActiveConversation", () => {
  it("re-fetches the active conversation from HTTP", async () => {
    const store = createConversationStore();
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    const updated = {
      ...makeDetail("conv-1"),
      last_activity: "2026-01-02T00:00:00Z",
    };
    vi.mocked(api.getConversation).mockResolvedValueOnce(updated);
    await store.refreshActiveConversation();

    expect(store.activeConversation!.last_activity).toBe("2026-01-02T00:00:00Z");
    expect(api.getConversation).toHaveBeenCalledTimes(2);
  });

  it("is a no-op when there is no active conversation", async () => {
    const store = createConversationStore();
    await store.refreshActiveConversation();
    expect(api.getConversation).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// loadConversations — space-scoped
// ---------------------------------------------------------------------------

describe("loadConversations (space-scoped)", () => {
  it("passes the spaceId through to the API", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([]);

    await store.loadConversations("proj-1");

    expect(api.listConversations).toHaveBeenCalledWith("proj-1");
  });

  it("replaces the conversation list when switching spaces", async () => {
    const store = createConversationStore();

    // Load space A — one conversation.
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-a1", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-a");
    expect(store.conversations.map((c) => c.id)).toEqual(["conv-a1"]);

    // Switch to space B — result should replace space A's list entirely.
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-b1", "2026-01-02T00:00:00Z"),
    ]);
    await store.loadConversations("proj-b");

    expect(store.conversations.map((c) => c.id)).toEqual(["conv-b1"]);
  });
});

// ---------------------------------------------------------------------------
// createConversation — space-scoped
// ---------------------------------------------------------------------------

describe("createConversation (space-scoped)", () => {
  it("passes the spaceId through to the API", async () => {
    const store = createConversationStore();
    vi.mocked(api.createConversation).mockResolvedValueOnce({
      id: "new-conv",
      space_id: "proj-1",
      created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z",
    });

    await store.createConversation("proj-1");

    expect(api.createConversation).toHaveBeenCalledWith(
      "proj-1",
      undefined,
      undefined,
      undefined,
    );
  });
});
