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
  listBranches: vi.fn(),
  createBranch: vi.fn(),
  getThread: vi.fn(),
  abandonThread: vi.fn(),
}));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeSummary(id: string, last_activity = "2026-01-01T00:00:00Z") {
  return {
    id,
    agent_id: "gateway",
    created_at: "2026-01-01T00:00:00Z",
    last_activity,
    message_preview: null,
    has_interrupted_turn: false,
  };
}

function makeDetail(id: string) {
  return {
    id,
    topic_id: "default",
    agent_id: "gateway",
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
  // A conversation with no branches is the ordinary case; a test about
  // branches sets its own listing.
  vi.mocked(api.listBranches).mockResolvedValue([]);
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
      agent_id: "gateway",
      topic_id: "default",
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
      agent_id: "gateway",
      topic_id: "default",
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
      agent_id: "gateway",
      topic_id: "default",
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

  it("installs a message the gateway recorded on the open conversation, as recorded", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    store.appendRecordedMessage(
      {
        id: "msg-brief",
        thread_id: "thread-1",
        turn_id: "turn-7",
        role: "user",
        origin: "schedule",
        content: "Summarise the inbox.",
        timestamp: "2026-01-01T00:05:00Z",
        turn_state: "pending",
        tool_calls: [],
      },
      "conv-1",
    );

    const [installed] = store.activeConversation!.messages;
    expect(installed.id).toBe("msg-brief");
    expect(installed.origin).toBe("schedule");
    expect(installed.turn_id).toBe("turn-7");
    expect(installed.thread_id).toBe("thread-1");
    expect(store.conversations[0].message_preview).toBe("Summarise the inbox.");
  });

  it("keeps the first optimistic ping when an identical second send queues", async () => {
    const store = createConversationStore();
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");
    store.appendOptimisticUserMessage({ client_id: "first-send", content: "ping", timestamp: "2026-01-01T00:00:01Z" });
    store.appendOptimisticUserMessage({ client_id: "second-send", content: "ping", timestamp: "2026-01-01T00:00:02Z" });
    const queued = { id: "q-second", client_id: "second-send", origin: "user" as const, content: "ping", queued_at: "2026-01-01T00:00:03Z" };
    store.appendQueuedMessage(queued, "conv-1");
    store.appendQueuedMessage(queued, "conv-1");
    expect({
      messages: store.activeConversation!.messages.map(message => ({ client_id: message.client_id, timestamp: message.timestamp })),
      queued: store.activeConversation!.queued,
    }).toEqual({ messages: [{ client_id: "first-send", timestamp: "2026-01-01T00:00:01Z" }], queued: [queued] });
  });

  it("shows a message the gateway holds for the next turn, once, until the queue drains", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    const queued = {
      id: "q-1",
      origin: "user" as const,
      content: "while you were answering…",
      queued_at: "2026-01-01T00:02:00Z",
    };
    store.appendQueuedMessage(queued, "conv-1");
    store.appendQueuedMessage(queued, "conv-1");
    expect(store.activeConversation!.queued).toEqual([queued]);
    expect(store.activeConversation!.messages).toHaveLength(0);

    store.appendQueuedMessage({ ...queued, id: "q-other" }, "conv-2");
    expect(store.activeConversation!.queued).toHaveLength(1);

    // A message queued after the drain took its batch stays held.
    store.appendQueuedMessage({ ...queued, id: "q-2", content: "and this" }, "conv-1");
    store.clearQueuedMessages("conv-1", ["q-1"]);
    expect(store.activeConversation!.queued!.map((item) => item.id)).toEqual(["q-2"]);
  });

  it("a recorded message for another conversation updates only that conversation's summary", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-1", "2026-01-01T00:00:00Z"),
      makeSummary("conv-2", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");

    store.appendRecordedMessage(
      {
        id: "msg-notice",
        thread_id: "thread-2",
        role: "user",
        origin: "delegation",
        content: "Delegated agent worker completed: done.",
        timestamp: "2026-01-01T00:09:00Z",
        turn_state: null,
        tool_calls: [],
      },
      "conv-2",
    );

    expect(store.activeConversation!.messages).toHaveLength(0);
    const other = store.conversations.find((c) => c.id === "conv-2")!;
    expect(other.message_preview).toBe("Delegated agent worker completed: done.");
    expect(store.conversations[0].id).toBe("conv-2");
  });

  it("installs a platform refusal account without replacing the session preview", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      { ...makeSummary("conv-1"), message_preview: "Read README.md" },
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");
    store.appendRecordedMessage({
      id: "refusal-notice", thread_id: "thread-1", turn_id: "refused-turn",
      role: "user", origin: "platform", content: "The model refused this request.",
      timestamp: "2026-01-01T00:01:00Z", turn_state: "refused", tool_calls: [],
    }, "conv-1");
    expect(store.activeConversation!.messages.at(-1)?.content).toBe("The model refused this request.");
    expect(store.conversations[0].message_preview, "platform accounts must preserve the person's preview").toBe("Read README.md");
  });

  it("installs a platform refusal account without changing activity or sidebar order", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-2", "2026-01-01T00:00:30Z"),
      { ...makeSummary("conv-1"), message_preview: "Read README.md" },
    ]);
    await store.loadConversations("proj-1");
    vi.mocked(api.getConversation).mockResolvedValueOnce(makeDetail("conv-1"));
    await store.selectConversation("conv-1");
    const summaries = [...store.conversations];
    const lastActivity = store.activeConversation!.last_activity;

    store.appendRecordedMessage({
      id: "refusal-notice", thread_id: "thread-1", turn_id: "refused-turn",
      role: "user", origin: "platform", content: "The model refused this request.",
      timestamp: "2026-01-01T00:01:00Z", turn_state: "refused", tool_calls: [],
    }, "conv-1");

    expect(store.activeConversation!.messages.at(-1)?.id).toBe("refusal-notice");
    expect.soft(store.conversations.find(conversation => conversation.id === "conv-1")!.last_activity).toBe(lastActivity);
    expect.soft(store.conversations.map(conversation => conversation.id)).toEqual(["conv-2", "conv-1"]);
    expect.soft(store.activeConversation!.last_activity).toBe(lastActivity);
    expect.soft(store.conversations).toEqual(summaries);
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
// loadConversations — topic-scoped
// ---------------------------------------------------------------------------

describe("loadConversations (topic-scoped)", () => {
  it("passes the topicId through to the API", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([]);

    await store.loadConversations("proj-1");

    expect(api.listConversations).toHaveBeenCalledWith("proj-1");
  });

  it("replaces the conversation list when switching topics", async () => {
    const store = createConversationStore();

    // Load topic A — one conversation.
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-a1", "2026-01-01T00:00:00Z"),
    ]);
    await store.loadConversations("proj-a");
    expect(store.conversations.map((c) => c.id)).toEqual(["conv-a1"]);

    // Switch to topic B — result should replace topic A's list entirely.
    vi.mocked(api.listConversations).mockResolvedValueOnce([
      makeSummary("conv-b1", "2026-01-02T00:00:00Z"),
    ]);
    await store.loadConversations("proj-b");

    expect(store.conversations.map((c) => c.id)).toEqual(["conv-b1"]);
  });
});

// ---------------------------------------------------------------------------
// createConversation — topic-scoped
// ---------------------------------------------------------------------------

describe("createConversation (topic-scoped)", () => {
  it("passes the topicId through to the API", async () => {
    const store = createConversationStore();
    vi.mocked(api.createConversation).mockResolvedValueOnce({
      id: "new-conv",
      agent_id: "gateway",
      topic_id: "proj-1",
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

// ---------------------------------------------------------------------------
// Threads and branches
// ---------------------------------------------------------------------------

describe("threads and branches", () => {
  function makeBranch(childThreadId: string, state = "idle") {
    return {
      child_thread_id: childThreadId,
      parent_thread_id: "root-1",
      branched_from_message_id: "msg-1",
      branched_from_preview: "What about the other approach?",
      state,
      created_at: "2026-01-01T00:00:00Z",
    };
  }

  function makeRootDetail(id: string) {
    return {
      ...makeDetail(id),
      root_thread_id: "root-1",
      messages: [
        {
          id: "msg-1",
          thread_id: "root-1",
          role: "user" as const,
          content: "first",
          timestamp: "2026-01-01T00:00:01Z",
          turn_state: "completed" as const,
        },
      ],
    };
  }

  it("selecting a conversation lands on the root thread with its branches listed", async () => {
    vi.mocked(api.getConversation).mockResolvedValue(makeRootDetail("conv-1"));
    vi.mocked(api.listBranches).mockResolvedValue([makeBranch("branch-1")]);
    const store = createConversationStore();

    await store.selectConversation("conv-1");

    expect(store.activeThreadId).toBeNull();
    expect(store.branches).toEqual([makeBranch("branch-1")]);
    expect(store.activeConversation?.messages.map((m) => m.content)).toEqual(["first"]);
  });

  it("selecting a branch shows that thread's messages without losing the conversation", async () => {
    vi.mocked(api.getConversation).mockResolvedValue(makeRootDetail("conv-1"));
    vi.mocked(api.listBranches).mockResolvedValue([makeBranch("branch-1")]);
    vi.mocked(api.getThread).mockResolvedValue({
      id: "branch-1",
      session_id: "conv-1",
      agent_id: "gateway",
      state: "idle",
      messages: [
        {
          id: "msg-1-copy",
          thread_id: "branch-1",
          role: "user",
          content: "first",
          timestamp: "2026-01-01T00:00:01Z",
          turn_state: "completed",
        },
        {
          id: "msg-b",
          thread_id: "branch-1",
          role: "assistant",
          content: "a different answer",
          timestamp: "2026-01-01T00:00:02Z",
          turn_state: "completed",
        },
      ],
      branch: makeBranch("branch-1"),
    });
    const store = createConversationStore();
    await store.selectConversation("conv-1");

    await store.selectThread!("branch-1");

    expect(api.getThread).toHaveBeenCalledWith("branch-1");
    expect(store.activeThreadId).toBe("branch-1");
    expect(store.activeConversation?.id).toBe("conv-1");
    expect(store.activeConversation?.messages.map((m) => m.content)).toEqual([
      "first",
      "a different answer",
    ]);

    // Back to the root: the conversation is re-read whole.
    await store.selectThread!(null);
    expect(store.activeThreadId).toBeNull();
    expect(store.activeConversation?.messages.map((m) => m.content)).toEqual(["first"]);
  });

  it("branching from a message creates the branch on the active thread and moves onto it", async () => {
    vi.mocked(api.getConversation).mockResolvedValue(makeRootDetail("conv-1"));
    vi.mocked(api.createBranch).mockResolvedValue(makeBranch("branch-2"));
    vi.mocked(api.getThread).mockResolvedValue({
      id: "branch-2",
      session_id: "conv-1",
      agent_id: "gateway",
      state: "idle",
      messages: [],
      branch: makeBranch("branch-2"),
    });
    const store = createConversationStore();
    await store.selectConversation("conv-1");

    await store.branchFrom!("msg-1");

    expect(api.createBranch).toHaveBeenCalledWith("conv-1", "root-1", "msg-1");
    expect(store.branches?.map((b) => b.child_thread_id)).toEqual(["branch-2"]);
    expect(store.activeThreadId).toBe("branch-2");
  });

  it("a message appended while a branch is active belongs to that branch", async () => {
    vi.mocked(api.getConversation).mockResolvedValue(makeRootDetail("conv-1"));
    vi.mocked(api.listBranches).mockResolvedValue([makeBranch("branch-1")]);
    vi.mocked(api.getThread).mockResolvedValue({
      id: "branch-1",
      session_id: "conv-1",
      agent_id: "gateway",
      state: "idle",
      messages: [],
      branch: makeBranch("branch-1"),
    });
    const store = createConversationStore();
    await store.selectConversation("conv-1");
    await store.selectThread!("branch-1");

    store.appendOptimisticUserMessage({ content: "on the branch", timestamp: "2026-01-01T00:00:05Z" });

    expect(store.activeConversation?.messages.at(-1)).toMatchObject({
      content: "on the branch",
      thread_id: "branch-1",
    });
  });

  it("a reply for a branch the person has left updates the summary but not the view", async () => {
    vi.mocked(api.getConversation).mockResolvedValue(makeRootDetail("conv-1"));
    vi.mocked(api.listConversations).mockResolvedValue([makeSummary("conv-1")]);
    const store = createConversationStore();
    await store.loadConversations("default");
    await store.selectConversation("conv-1");

    store.appendAssistantMessage(
      { content: "branch reply", timestamp: "2026-01-01T00:00:09Z", turn_state: "completed", thread_id: "branch-1" },
      "conv-1",
    );

    expect(store.activeConversation?.messages.map((m) => m.content)).toEqual(["first"]);
    expect(store.conversations[0].message_preview).toBe("branch reply");

    store.appendAssistantMessage(
      { content: "root reply", timestamp: "2026-01-01T00:00:10Z", turn_state: "completed", thread_id: "root-1" },
      "conv-1",
    );
    expect(store.activeConversation?.messages.at(-1)).toMatchObject({ content: "root reply", thread_id: "root-1" });
  });

  it("abandoning the active branch marks it and returns to the root thread", async () => {
    vi.mocked(api.getConversation).mockResolvedValue(makeRootDetail("conv-1"));
    vi.mocked(api.listBranches).mockResolvedValue([makeBranch("branch-1")]);
    vi.mocked(api.getThread).mockResolvedValue({
      id: "branch-1",
      session_id: "conv-1",
      agent_id: "gateway",
      state: "idle",
      messages: [],
      branch: makeBranch("branch-1"),
    });
    vi.mocked(api.abandonThread).mockResolvedValue({
      id: "branch-1",
      session_id: "conv-1",
      agent_id: "gateway",
      state: "abandoned",
      messages: [],
      branch: makeBranch("branch-1", "abandoned"),
    });
    const store = createConversationStore();
    await store.selectConversation("conv-1");
    await store.selectThread!("branch-1");

    await store.abandonActiveBranch!();

    expect(api.abandonThread).toHaveBeenCalledWith("branch-1");
    expect(store.branches?.[0].state).toBe("abandoned");
    expect(store.activeThreadId).toBeNull();
    expect(store.activeConversation?.messages.map((m) => m.content)).toEqual(["first"]);
  });

  it("abandoning with the root thread active does nothing", async () => {
    vi.mocked(api.getConversation).mockResolvedValue(makeRootDetail("conv-1"));
    const store = createConversationStore();
    await store.selectConversation("conv-1");

    await store.abandonActiveBranch!();

    expect(api.abandonThread).not.toHaveBeenCalled();
  });
});


describe("scope list isolation", () => {
  it("clears the old list immediately and ignores its late response", async () => {
    const store = createConversationStore();
    vi.mocked(api.listConversations).mockResolvedValueOnce([makeSummary("old")]);
    await store.loadConversations("old-topic");
    let releaseOld!: (rows: ReturnType<typeof makeSummary>[]) => void;
    vi.mocked(api.listConversations).mockImplementationOnce(() => new Promise(resolve => { releaseOld = resolve; }));
    const oldLoad = store.loadConversations("old-topic");
    let releaseNew!: (rows: ReturnType<typeof makeSummary>[]) => void;
    vi.mocked(api.listConversations).mockImplementationOnce(() => new Promise(resolve => { releaseNew = resolve; }));
    const newLoad = store.loadConversations("empty-topic");
    expect(store.conversations).toEqual([]);
    releaseOld([makeSummary("stale")]);
    await oldLoad;
    expect(store.conversations).toEqual([]);
    expect(store.isLoading).toBe(true);
    releaseNew([]);
    await newLoad;
    expect(store.conversations).toEqual([]);
    expect(store.isLoading).toBe(false);
  });
});
