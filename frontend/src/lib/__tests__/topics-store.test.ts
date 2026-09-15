/**
 * Unit tests for the topic store.
 *
 * All API calls are mocked so tests run in isolation without a gateway.
 * Each test creates a fresh store instance via createTopicStore().
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

import { createTopicStore } from "../stores/topics";
import * as api from "../api/topics";

// ---------------------------------------------------------------------------
// Module mock
// ---------------------------------------------------------------------------

vi.mock("../api/topics", () => ({
  listTopics: vi.fn(),
  createTopic: vi.fn(),
  getTopic: vi.fn(),
  updateTopic: vi.fn(),
  deleteTopic: vi.fn(),
  listTopicSessions: vi.fn(),
  createTopicSession: vi.fn(),
  setTopicsBaseUrl: vi.fn(),
}));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeTopic(
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

// The default topic — special-cased: cannot be deleted.
const defaultTopic = makeTopic("proj-default", "default", {
  is_default: true,
});

const topicA = makeTopic("proj-a", "Topic A");
const topicB = makeTopic("proj-b", "Topic B");

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
  it("starts with an empty topic list", () => {
    const store = createTopicStore();
    expect(store.topics).toEqual([]);
  });

  it("starts with no current topic", () => {
    const store = createTopicStore();
    expect(store.currentTopic).toBeNull();
  });

  it("starts with isLoading false", () => {
    const store = createTopicStore();
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// loadTopics
// ---------------------------------------------------------------------------

describe("loadTopics", () => {
  it("fetches topics and stores them", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA, topicB]);

    await store.loadTopics();

    expect(store.topics).toHaveLength(2);
    expect(store.topics[0].id).toBe("proj-a");
  });

  it("restores the active topic after the available topics load", async () => {
    const store = createTopicStore();
    localStorage.setItem("swage:active-topic", "proj-b");
    vi.mocked(api.listTopics).mockResolvedValueOnce([
      topicA,
      defaultTopic,
      topicB,
    ]);

    await store.loadTopics();

    expect(store.currentTopic?.id).toBe("proj-b");
  });

  it("clears a stale stored topic and leaves the picker open", async () => {
    const store = createTopicStore();
    localStorage.setItem("swage:active-topic", "proj-missing");
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA, topicB]);

    await store.loadTopics();

    expect(store.currentTopic).toBeNull();
    expect(localStorage.getItem("swage:active-topic")).toBeNull();
  });

  it("ignores and clears an empty stored topic id", async () => {
    const store = createTopicStore();
    localStorage.setItem("swage:active-topic", "   ");
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA]);

    await store.loadTopics();

    expect(store.currentTopic).toBeNull();
    expect(localStorage.getItem("swage:active-topic")).toBeNull();
  });

  it("sets isLoading true during fetch and false after", async () => {
    const store = createTopicStore();
    let loadingDuringFetch = false;

    vi.mocked(api.listTopics).mockImplementationOnce(async () => {
      loadingDuringFetch = store.isLoading;
      return [];
    });

    await store.loadTopics();

    expect(loadingDuringFetch).toBe(true);
    expect(store.isLoading).toBe(false);
  });

  it("clears isLoading even if the API call throws", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockRejectedValueOnce(
      new Error("network error"),
    );

    await expect(store.loadTopics()).rejects.toThrow("network error");
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// selectTopic
// ---------------------------------------------------------------------------

describe("selectTopic", () => {
  it("sets currentTopic to the matching topic from the loaded list", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA, topicB]);
    await store.loadTopics();

    await store.selectTopic("proj-b");

    expect(store.currentTopic?.id).toBe("proj-b");
    expect(store.currentTopic?.name).toBe("Topic B");
    expect(localStorage.getItem("swage:active-topic")).toBe("proj-b");
  });

  it("does not call the API — selection is from the local list", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA]);
    await store.loadTopics();

    await store.selectTopic("proj-a");

    // getTopic should never be called — we already have the data.
    expect(api.getTopic).not.toHaveBeenCalled();
  });

  it("throws if the topic is not found in the list", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA]);
    await store.loadTopics();

    await expect(store.selectTopic("proj-missing")).rejects.toThrow();
  });
});

// ---------------------------------------------------------------------------
// createTopic
// ---------------------------------------------------------------------------

describe("createTopic", () => {
  it("calls the API, adds the topic to the list, and selects it", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([defaultTopic]);
    await store.loadTopics();

    const newTopic = makeTopic("proj-new", "New Topic");
    vi.mocked(api.createTopic).mockResolvedValueOnce(newTopic);

    await store.createTopic("New Topic");

    expect(api.createTopic).toHaveBeenCalledWith({
      name: "New Topic",
    });
    expect(store.topics).toHaveLength(2);
    expect(store.currentTopic?.id).toBe("proj-new");
    expect(localStorage.getItem("swage:active-topic")).toBe("proj-new");
  });

  it("adds the new topic to an existing list", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA]);
    await store.loadTopics();

    const newTopic = makeTopic("proj-b", "Topic B");
    vi.mocked(api.createTopic).mockResolvedValueOnce(newTopic);

    await store.createTopic("Topic B");

    expect(store.topics).toHaveLength(2);
  });
});

// ---------------------------------------------------------------------------
// deleteTopic
// ---------------------------------------------------------------------------

describe("deleteTopic", () => {
  it("removes the topic from the list", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA, topicB]);
    await store.loadTopics();
    vi.mocked(api.deleteTopic).mockResolvedValueOnce(undefined);

    await store.deleteTopic("proj-b");

    expect(store.topics.map((p) => p.id)).toEqual(["proj-a"]);
  });

  it("throws and does not call the API when deleting the default topic", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([
      defaultTopic,
      topicA,
    ]);
    await store.loadTopics();

    await expect(store.deleteTopic("proj-default")).rejects.toThrow();
    expect(api.deleteTopic).not.toHaveBeenCalled();
  });

  it("selects the next topic when the current topic is deleted", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA, topicB]);
    await store.loadTopics();

    await store.selectTopic("proj-a");
    vi.mocked(api.deleteTopic).mockResolvedValueOnce(undefined);

    await store.deleteTopic("proj-a");

    // Should automatically move to proj-b (the remaining topic).
    expect(store.currentTopic?.id).toBe("proj-b");
    expect(store.topics.map((p) => p.id)).toEqual(["proj-b"]);
    expect(localStorage.getItem("swage:active-topic")).toBe("proj-b");
  });

  it("sets currentTopic to null when the last topic is deleted", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA]);
    await store.loadTopics();

    await store.selectTopic("proj-a");
    vi.mocked(api.deleteTopic).mockResolvedValueOnce(undefined);

    await store.deleteTopic("proj-a");

    expect(store.currentTopic).toBeNull();
    expect(store.topics).toHaveLength(0);
    expect(localStorage.getItem("swage:active-topic")).toBeNull();
  });

  it("leaves currentTopic unchanged when a non-current topic is deleted", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA, topicB]);
    await store.loadTopics();

    await store.selectTopic("proj-a");
    vi.mocked(api.deleteTopic).mockResolvedValueOnce(undefined);

    await store.deleteTopic("proj-b");

    expect(store.currentTopic?.id).toBe("proj-a");
    expect(store.topics).toHaveLength(1);
  });
});

describe("closeTopic", () => {
  it("clears the persisted selection so reload returns to the picker", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA]);
    await store.loadTopics();
    await store.selectTopic("proj-a");

    store.closeTopic();

    expect(store.currentTopic).toBeNull();
    expect(localStorage.getItem("swage:active-topic")).toBeNull();
  });
});
