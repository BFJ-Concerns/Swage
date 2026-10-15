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
  extra: Partial<{ slug: string }> = {},
) {
  return {
    id,
    name,
    description: "",
    slug: extra.slug ?? name.toLowerCase().replace(/\s+/g, "-"),
    created_at: "2026-01-01T00:00:00Z",
  };
}

const defaultTopic = makeTopic("proj-default", "default");

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
  it("clears the live selection", async () => {
    const store = createTopicStore();
    vi.mocked(api.listTopics).mockResolvedValueOnce([topicA]);
    await store.loadTopics();
    await store.selectTopic("proj-a");

    store.closeTopic();

    expect(store.currentTopic).toBeNull();
  });
});


describe("selection is local to this visit", () => {
  it("never reads or writes retired persistence through load, select, create, delete or close", async () => {
    localStorage.setItem("swage:active-topic", topicB.id);
    localStorage.setItem("swage:recent-topics", JSON.stringify([topicB.id]));
    const read = vi.spyOn(localStorage, "getItem");
    const write = vi.spyOn(localStorage, "setItem");
    const remove = vi.spyOn(localStorage, "removeItem");
    try {
      const store = createTopicStore();
      vi.mocked(api.listTopics).mockResolvedValue([topicA, topicB]);
      await store.loadTopics();
      expect(store.currentTopic).toBeNull();
      await store.selectTopic(topicB.id);
      expect(store.currentTopic?.id).toBe(topicB.id);
      vi.mocked(api.createTopic).mockResolvedValue(makeTopic("created", "Created"));
      await store.createTopic("Created");
      await store.deleteTopic("created");
      store.closeTopic();
      expect(store.currentTopic).toBeNull();
      expect(read).not.toHaveBeenCalled();
      expect(write).not.toHaveBeenCalled();
      expect(remove).not.toHaveBeenCalled();
    } finally {
      read.mockRestore(); write.mockRestore(); remove.mockRestore();
    }
  });
});
