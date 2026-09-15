// @vitest-environment jsdom
/**
 * Tests for the interactive TopicSelector component.
 *
 * The component shows the current topic name and opens a dropdown to
 * switch topics or create new ones. All store calls are mocked.
 *
 * flushSync() is used whenever a click triggers a Svelte $state change and
 * the test needs to query the updated DOM immediately after.
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import TopicSelector from "../components/TopicSelector.svelte";
import type { TopicStore } from "../stores/topics";

type MountedComponent = ReturnType<typeof mount>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeTopic(id: string, name: string) {
  return {
    id,
    name,
    description: "",
    is_default: name === "default",
    slug: name.toLowerCase().replace(/\s+/g, "-"),
    created_at: "2026-01-01T00:00:00Z",
  };
}

const defaultTopic = makeTopic("proj-default", "default");
const topicA = makeTopic("proj-a", "Topic A");
const topicB = makeTopic("proj-b", "Topic B");

function mockTopicStore(overrides: Partial<TopicStore> = {}): TopicStore {
  return {
    get topics() { return [defaultTopic, topicA]; },
    get currentTopic() { return defaultTopic; },
    get recentTopics() { return []; },
    get isLoading() { return false; },
    loadTopics: vi.fn(),
    selectTopic: vi.fn().mockResolvedValue(undefined),
    closeTopic: vi.fn(),
    createTopic: vi.fn().mockResolvedValue(undefined),
    deleteTopic: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

/** Click an element and flush Svelte's pending DOM updates synchronously. */
function click(selector: string): HTMLElement {
  const el = document.querySelector(selector) as HTMLElement;
  expect(el).not.toBeNull();
  flushSync(() => el.click());
  return el;
}

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

let component: MountedComponent | undefined;

afterEach(() => {
  if (component) {
    unmount(component);
    component = undefined;
  }
  while (document.body.firstChild) {
    document.body.removeChild(document.body.firstChild);
  }
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// Static render (no store provided)
// ---------------------------------------------------------------------------

describe("without a store", () => {
  it("renders the .topic-selector container", () => {
    component = mount(TopicSelector, { target: document.body });
    expect(document.querySelector(".topic-selector")).not.toBeNull();
  });

  it("shows a fallback when no store is provided (no crash)", () => {
    component = mount(TopicSelector, { target: document.body });
    expect(document.querySelector(".topic-selector")).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Current topic display
// ---------------------------------------------------------------------------

describe("current topic display", () => {
  it("shows the current topic name", () => {
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: mockTopicStore() },
    });

    expect(document.querySelector(".topic-selector")!.textContent).toContain(
      "default",
    );
  });

  it("shows a different topic when currentTopic changes", () => {
    const store = mockTopicStore({
      get currentTopic() { return topicA; },
    });
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: store },
    });

    expect(document.querySelector(".topic-selector")!.textContent).toContain(
      "Topic A",
    );
  });
});

// ---------------------------------------------------------------------------
// Dropdown: open and close
// ---------------------------------------------------------------------------

describe("dropdown toggle", () => {
  it("dropdown is not visible initially", () => {
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: mockTopicStore() },
    });

    expect(document.querySelector(".topic-dropdown")).toBeNull();
  });

  it("opens the dropdown when the toggle button is clicked", () => {
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: mockTopicStore() },
    });

    click(".topic-selector .toggle-btn");

    expect(document.querySelector(".topic-dropdown")).not.toBeNull();
  });

  it("closes the dropdown when toggled a second time", () => {
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: mockTopicStore() },
    });

    click(".topic-selector .toggle-btn");
    expect(document.querySelector(".topic-dropdown")).not.toBeNull();

    click(".topic-selector .toggle-btn");
    expect(document.querySelector(".topic-dropdown")).toBeNull();
  });

  it("closes the dropdown on Escape key", () => {
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: mockTopicStore() },
    });

    click(".topic-selector .toggle-btn");
    expect(document.querySelector(".topic-dropdown")).not.toBeNull();

    flushSync(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    expect(document.querySelector(".topic-dropdown")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Topic list
// ---------------------------------------------------------------------------

describe("topic list in dropdown", () => {
  it("lists all topics from the store", () => {
    const store = mockTopicStore({
      get topics() { return [defaultTopic, topicA, topicB]; },
    });
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: store },
    });

    click(".topic-selector .toggle-btn");

    const dropdown = document.querySelector(".topic-dropdown")!;
    expect(dropdown.textContent).toContain("default");
    expect(dropdown.textContent).toContain("Topic A");
    expect(dropdown.textContent).toContain("Topic B");
  });

  it("clicking a topic calls selectTopic with its id", async () => {
    const store = mockTopicStore({
      get topics() { return [defaultTopic, topicA]; },
    });
    const onTopicChange = vi.fn();
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: store, onTopicChange },
    });

    click(".topic-selector .toggle-btn");

    // Click the Topic A item in the dropdown.
    const items = document.querySelectorAll(".topic-dropdown .topic-item");
    const topicAItem = Array.from(items).find((el) =>
      el.textContent?.includes("Topic A"),
    )?.querySelector(".topic-select-btn") as HTMLButtonElement;

    expect(topicAItem).not.toBeUndefined();
    flushSync(() => topicAItem.click());

    expect(store.selectTopic).toHaveBeenCalledWith("proj-a");
  });

  it("triggers onTopicChange after selecting a topic", async () => {
    const store = mockTopicStore({
      get topics() { return [defaultTopic, topicA]; },
    });
    const onTopicChange = vi.fn();
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: store, onTopicChange },
    });

    click(".topic-selector .toggle-btn");

    const items = document.querySelectorAll(".topic-dropdown .topic-item");
    const topicAItem = Array.from(items).find((el) =>
      el.textContent?.includes("Topic A"),
    )?.querySelector(".topic-select-btn") as HTMLButtonElement;

    flushSync(() => topicAItem.click());
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(onTopicChange).toHaveBeenCalled();
  });

  it("closes the dropdown after selecting a topic", async () => {
    const store = mockTopicStore({
      get topics() { return [defaultTopic, topicA]; },
    });
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: store },
    });

    click(".topic-selector .toggle-btn");

    const items = document.querySelectorAll(".topic-dropdown .topic-item");
    const firstTopicButton = items[0]?.querySelector(
      ".topic-select-btn",
    ) as HTMLButtonElement;
    flushSync(() => firstTopicButton.click());

    expect(document.querySelector(".topic-dropdown")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Create topic form
// ---------------------------------------------------------------------------

describe("create topic form", () => {
  it("shows a create topic button in the dropdown", () => {
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: mockTopicStore() },
    });

    click(".topic-selector .toggle-btn");

    expect(
      document.querySelector(".topic-dropdown .create-topic-btn"),
    ).not.toBeNull();
  });

  it("clicking create reveals the inline form", () => {
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: mockTopicStore() },
    });

    click(".topic-selector .toggle-btn");
    click(".topic-dropdown .create-topic-btn");

    expect(document.querySelector(".create-topic-form")).not.toBeNull();
  });

  it("submitting the form calls createTopic with only a name", async () => {
    const store = mockTopicStore();
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: store },
    });

    // Open dropdown → open form.
    click(".topic-selector .toggle-btn");
    click(".topic-dropdown .create-topic-btn");

    // Fill in the form.
    const nameInput = document.querySelector(
      ".create-topic-form input[name='name']",
    ) as HTMLInputElement;

    expect(nameInput).not.toBeNull();

    flushSync(() => {
      nameInput.value = "My Topic";
      nameInput.dispatchEvent(new Event("input"));
    });

    const form = document.querySelector(".create-topic-form") as HTMLFormElement;
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await Promise.resolve();

    expect(store.createTopic).toHaveBeenCalledWith("My Topic");
  });
});

// ---------------------------------------------------------------------------
// Delete topic
// ---------------------------------------------------------------------------

describe("delete topic", () => {
  it("shows a delete action for non-default topics", () => {
    const store = mockTopicStore({
      get topics() { return [defaultTopic, topicA]; },
    });
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: store },
    });

    click(".topic-selector .toggle-btn");

    const dropdown = document.querySelector(".topic-dropdown")!;
    // Topic A's item should have a delete affordance.
    const topicAItem = Array.from(
      dropdown.querySelectorAll(".topic-item"),
    ).find((el) => el.textContent?.includes("Topic A"));
    expect(topicAItem?.querySelector(".delete-topic-btn")).not.toBeNull();
  });

  it("does not show a delete action for the default topic", () => {
    const store = mockTopicStore({
      get topics() { return [defaultTopic, topicA]; },
    });
    component = mount(TopicSelector, {
      target: document.body,
      props: { topicStore: store },
    });

    click(".topic-selector .toggle-btn");

    const dropdown = document.querySelector(".topic-dropdown")!;
    const defaultItem = Array.from(
      dropdown.querySelectorAll(".topic-item"),
    ).find((el) => el.textContent?.includes("default") && !el.textContent?.includes("Topic"));
    // Default topic must not have a delete button.
    expect(defaultItem?.querySelector(".delete-topic-btn")).toBeNull();
  });
});
