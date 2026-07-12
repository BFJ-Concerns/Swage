// @vitest-environment jsdom
/**
 * Tests for the interactive SpaceSelector component.
 *
 * The component shows the current space name and opens a dropdown to
 * switch spaces or create new ones. All store calls are mocked.
 *
 * flushSync() is used whenever a click triggers a Svelte $state change and
 * the test needs to query the updated DOM immediately after.
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import SpaceSelector from "../components/SpaceSelector.svelte";
import type { SpaceStore } from "../stores/spaces";

type MountedComponent = ReturnType<typeof mount>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeSpace(id: string, name: string) {
  return {
    id,
    name,
    description: "",
    is_default: name === "default",
    slug: name.toLowerCase().replace(/\s+/g, "-"),
    created_at: "2026-01-01T00:00:00Z",
  };
}

const defaultSpace = makeSpace("proj-default", "default");
const spaceA = makeSpace("proj-a", "Space A");
const spaceB = makeSpace("proj-b", "Space B");

function mockSpaceStore(overrides: Partial<SpaceStore> = {}): SpaceStore {
  return {
    get spaces() { return [defaultSpace, spaceA]; },
    get currentSpace() { return defaultSpace; },
    get recentSpaces() { return []; },
    get isLoading() { return false; },
    loadSpaces: vi.fn(),
    selectSpace: vi.fn().mockResolvedValue(undefined),
    closeSpace: vi.fn(),
    createSpace: vi.fn().mockResolvedValue(undefined),
    deleteSpace: vi.fn().mockResolvedValue(undefined),
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
  it("renders the .space-selector container", () => {
    component = mount(SpaceSelector, { target: document.body });
    expect(document.querySelector(".space-selector")).not.toBeNull();
  });

  it("shows a fallback when no store is provided (no crash)", () => {
    component = mount(SpaceSelector, { target: document.body });
    expect(document.querySelector(".space-selector")).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Current space display
// ---------------------------------------------------------------------------

describe("current space display", () => {
  it("shows the current space name", () => {
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: mockSpaceStore() },
    });

    expect(document.querySelector(".space-selector")!.textContent).toContain(
      "default",
    );
  });

  it("shows a different space when currentSpace changes", () => {
    const store = mockSpaceStore({
      get currentSpace() { return spaceA; },
    });
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: store },
    });

    expect(document.querySelector(".space-selector")!.textContent).toContain(
      "Space A",
    );
  });
});

// ---------------------------------------------------------------------------
// Dropdown: open and close
// ---------------------------------------------------------------------------

describe("dropdown toggle", () => {
  it("dropdown is not visible initially", () => {
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: mockSpaceStore() },
    });

    expect(document.querySelector(".space-dropdown")).toBeNull();
  });

  it("opens the dropdown when the toggle button is clicked", () => {
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: mockSpaceStore() },
    });

    click(".space-selector .toggle-btn");

    expect(document.querySelector(".space-dropdown")).not.toBeNull();
  });

  it("closes the dropdown when toggled a second time", () => {
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: mockSpaceStore() },
    });

    click(".space-selector .toggle-btn");
    expect(document.querySelector(".space-dropdown")).not.toBeNull();

    click(".space-selector .toggle-btn");
    expect(document.querySelector(".space-dropdown")).toBeNull();
  });

  it("closes the dropdown on Escape key", () => {
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: mockSpaceStore() },
    });

    click(".space-selector .toggle-btn");
    expect(document.querySelector(".space-dropdown")).not.toBeNull();

    flushSync(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    expect(document.querySelector(".space-dropdown")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Space list
// ---------------------------------------------------------------------------

describe("space list in dropdown", () => {
  it("lists all spaces from the store", () => {
    const store = mockSpaceStore({
      get spaces() { return [defaultSpace, spaceA, spaceB]; },
    });
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: store },
    });

    click(".space-selector .toggle-btn");

    const dropdown = document.querySelector(".space-dropdown")!;
    expect(dropdown.textContent).toContain("default");
    expect(dropdown.textContent).toContain("Space A");
    expect(dropdown.textContent).toContain("Space B");
  });

  it("clicking a space calls selectSpace with its id", async () => {
    const store = mockSpaceStore({
      get spaces() { return [defaultSpace, spaceA]; },
    });
    const onSpaceChange = vi.fn();
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: store, onSpaceChange },
    });

    click(".space-selector .toggle-btn");

    // Click the Space A item in the dropdown.
    const items = document.querySelectorAll(".space-dropdown .space-item");
    const spaceAItem = Array.from(items).find((el) =>
      el.textContent?.includes("Space A"),
    )?.querySelector(".space-select-btn") as HTMLButtonElement;

    expect(spaceAItem).not.toBeUndefined();
    flushSync(() => spaceAItem.click());

    expect(store.selectSpace).toHaveBeenCalledWith("proj-a");
  });

  it("triggers onSpaceChange after selecting a space", async () => {
    const store = mockSpaceStore({
      get spaces() { return [defaultSpace, spaceA]; },
    });
    const onSpaceChange = vi.fn();
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: store, onSpaceChange },
    });

    click(".space-selector .toggle-btn");

    const items = document.querySelectorAll(".space-dropdown .space-item");
    const spaceAItem = Array.from(items).find((el) =>
      el.textContent?.includes("Space A"),
    )?.querySelector(".space-select-btn") as HTMLButtonElement;

    flushSync(() => spaceAItem.click());
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(onSpaceChange).toHaveBeenCalled();
  });

  it("closes the dropdown after selecting a space", async () => {
    const store = mockSpaceStore({
      get spaces() { return [defaultSpace, spaceA]; },
    });
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: store },
    });

    click(".space-selector .toggle-btn");

    const items = document.querySelectorAll(".space-dropdown .space-item");
    const firstSpaceButton = items[0]?.querySelector(
      ".space-select-btn",
    ) as HTMLButtonElement;
    flushSync(() => firstSpaceButton.click());

    expect(document.querySelector(".space-dropdown")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Create space form
// ---------------------------------------------------------------------------

describe("create space form", () => {
  it("shows a create space button in the dropdown", () => {
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: mockSpaceStore() },
    });

    click(".space-selector .toggle-btn");

    expect(
      document.querySelector(".space-dropdown .create-space-btn"),
    ).not.toBeNull();
  });

  it("clicking create reveals the inline form", () => {
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: mockSpaceStore() },
    });

    click(".space-selector .toggle-btn");
    click(".space-dropdown .create-space-btn");

    expect(document.querySelector(".create-space-form")).not.toBeNull();
  });

  it("submitting the form calls createSpace with only a name", async () => {
    const store = mockSpaceStore();
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: store },
    });

    // Open dropdown → open form.
    click(".space-selector .toggle-btn");
    click(".space-dropdown .create-space-btn");

    // Fill in the form.
    const nameInput = document.querySelector(
      ".create-space-form input[name='name']",
    ) as HTMLInputElement;

    expect(nameInput).not.toBeNull();

    flushSync(() => {
      nameInput.value = "My Space";
      nameInput.dispatchEvent(new Event("input"));
    });

    const form = document.querySelector(".create-space-form") as HTMLFormElement;
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await Promise.resolve();

    expect(store.createSpace).toHaveBeenCalledWith("My Space");
  });
});

// ---------------------------------------------------------------------------
// Delete space
// ---------------------------------------------------------------------------

describe("delete space", () => {
  it("shows a delete action for non-default spaces", () => {
    const store = mockSpaceStore({
      get spaces() { return [defaultSpace, spaceA]; },
    });
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: store },
    });

    click(".space-selector .toggle-btn");

    const dropdown = document.querySelector(".space-dropdown")!;
    // Space A's item should have a delete affordance.
    const spaceAItem = Array.from(
      dropdown.querySelectorAll(".space-item"),
    ).find((el) => el.textContent?.includes("Space A"));
    expect(spaceAItem?.querySelector(".delete-space-btn")).not.toBeNull();
  });

  it("does not show a delete action for the default space", () => {
    const store = mockSpaceStore({
      get spaces() { return [defaultSpace, spaceA]; },
    });
    component = mount(SpaceSelector, {
      target: document.body,
      props: { spaceStore: store },
    });

    click(".space-selector .toggle-btn");

    const dropdown = document.querySelector(".space-dropdown")!;
    const defaultItem = Array.from(
      dropdown.querySelectorAll(".space-item"),
    ).find((el) => el.textContent?.includes("default") && !el.textContent?.includes("Space"));
    // Default space must not have a delete button.
    expect(defaultItem?.querySelector(".delete-space-btn")).toBeNull();
  });
});
