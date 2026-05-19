// @vitest-environment jsdom
/**
 * Tests for the interactive BenchSelector component.
 *
 * The component shows the current bench name and opens a dropdown to
 * switch benches or create new ones. All store calls are mocked.
 *
 * flushSync() is used whenever a click triggers a Svelte $state change and
 * the test needs to query the updated DOM immediately after.
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import BenchSelector from "../components/BenchSelector.svelte";
import type { BenchStore } from "../stores/benches";

type MountedComponent = ReturnType<typeof mount>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeBench(id: string, name: string) {
  return {
    id,
    name,
    description: "",
    is_default: name === "default",
    slug: name.toLowerCase().replace(/\s+/g, "-"),
    created_at: "2026-01-01T00:00:00Z",
  };
}

const defaultBench = makeBench("proj-default", "default");
const benchA = makeBench("proj-a", "Bench A");
const benchB = makeBench("proj-b", "Bench B");

function mockBenchStore(overrides: Partial<BenchStore> = {}): BenchStore {
  return {
    get benches() { return [defaultBench, benchA]; },
    get currentBench() { return defaultBench; },
    get recentBenches() { return []; },
    get isLoading() { return false; },
    loadBenches: vi.fn(),
    selectBench: vi.fn().mockResolvedValue(undefined),
    createBench: vi.fn().mockResolvedValue(undefined),
    deleteBench: vi.fn().mockResolvedValue(undefined),
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
  it("renders the .bench-selector container", () => {
    component = mount(BenchSelector, { target: document.body });
    expect(document.querySelector(".bench-selector")).not.toBeNull();
  });

  it("shows a fallback when no store is provided (no crash)", () => {
    component = mount(BenchSelector, { target: document.body });
    expect(document.querySelector(".bench-selector")).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Current bench display
// ---------------------------------------------------------------------------

describe("current bench display", () => {
  it("shows the current bench name", () => {
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: mockBenchStore() },
    });

    expect(document.querySelector(".bench-selector")!.textContent).toContain(
      "default",
    );
  });

  it("shows a different bench when currentBench changes", () => {
    const store = mockBenchStore({
      get currentBench() { return benchA; },
    });
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: store },
    });

    expect(document.querySelector(".bench-selector")!.textContent).toContain(
      "Bench A",
    );
  });
});

// ---------------------------------------------------------------------------
// Dropdown: open and close
// ---------------------------------------------------------------------------

describe("dropdown toggle", () => {
  it("dropdown is not visible initially", () => {
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: mockBenchStore() },
    });

    expect(document.querySelector(".bench-dropdown")).toBeNull();
  });

  it("opens the dropdown when the toggle button is clicked", () => {
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: mockBenchStore() },
    });

    click(".bench-selector .toggle-btn");

    expect(document.querySelector(".bench-dropdown")).not.toBeNull();
  });

  it("closes the dropdown when toggled a second time", () => {
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: mockBenchStore() },
    });

    click(".bench-selector .toggle-btn");
    expect(document.querySelector(".bench-dropdown")).not.toBeNull();

    click(".bench-selector .toggle-btn");
    expect(document.querySelector(".bench-dropdown")).toBeNull();
  });

  it("closes the dropdown on Escape key", () => {
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: mockBenchStore() },
    });

    click(".bench-selector .toggle-btn");
    expect(document.querySelector(".bench-dropdown")).not.toBeNull();

    flushSync(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    expect(document.querySelector(".bench-dropdown")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Bench list
// ---------------------------------------------------------------------------

describe("bench list in dropdown", () => {
  it("lists all benches from the store", () => {
    const store = mockBenchStore({
      get benches() { return [defaultBench, benchA, benchB]; },
    });
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: store },
    });

    click(".bench-selector .toggle-btn");

    const dropdown = document.querySelector(".bench-dropdown")!;
    expect(dropdown.textContent).toContain("default");
    expect(dropdown.textContent).toContain("Bench A");
    expect(dropdown.textContent).toContain("Bench B");
  });

  it("clicking a bench calls selectBench with its id", async () => {
    const store = mockBenchStore({
      get benches() { return [defaultBench, benchA]; },
    });
    const onBenchChange = vi.fn();
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: store, onBenchChange },
    });

    click(".bench-selector .toggle-btn");

    // Click the Bench A item in the dropdown.
    const items = document.querySelectorAll(".bench-dropdown .bench-item");
    const benchAItem = Array.from(items).find((el) =>
      el.textContent?.includes("Bench A"),
    )?.querySelector(".bench-select-btn") as HTMLButtonElement;

    expect(benchAItem).not.toBeUndefined();
    flushSync(() => benchAItem.click());

    expect(store.selectBench).toHaveBeenCalledWith("proj-a");
  });

  it("triggers onBenchChange after selecting a bench", async () => {
    const store = mockBenchStore({
      get benches() { return [defaultBench, benchA]; },
    });
    const onBenchChange = vi.fn();
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: store, onBenchChange },
    });

    click(".bench-selector .toggle-btn");

    const items = document.querySelectorAll(".bench-dropdown .bench-item");
    const benchAItem = Array.from(items).find((el) =>
      el.textContent?.includes("Bench A"),
    )?.querySelector(".bench-select-btn") as HTMLButtonElement;

    flushSync(() => benchAItem.click());
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(onBenchChange).toHaveBeenCalled();
  });

  it("closes the dropdown after selecting a bench", async () => {
    const store = mockBenchStore({
      get benches() { return [defaultBench, benchA]; },
    });
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: store },
    });

    click(".bench-selector .toggle-btn");

    const items = document.querySelectorAll(".bench-dropdown .bench-item");
    const firstBenchButton = items[0]?.querySelector(
      ".bench-select-btn",
    ) as HTMLButtonElement;
    flushSync(() => firstBenchButton.click());

    expect(document.querySelector(".bench-dropdown")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Create bench form
// ---------------------------------------------------------------------------

describe("create bench form", () => {
  it("shows a create bench button in the dropdown", () => {
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: mockBenchStore() },
    });

    click(".bench-selector .toggle-btn");

    expect(
      document.querySelector(".bench-dropdown .create-bench-btn"),
    ).not.toBeNull();
  });

  it("clicking create reveals the inline form", () => {
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: mockBenchStore() },
    });

    click(".bench-selector .toggle-btn");
    click(".bench-dropdown .create-bench-btn");

    expect(document.querySelector(".create-bench-form")).not.toBeNull();
  });

  it("submitting the form calls createBench with only a name", async () => {
    const store = mockBenchStore();
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: store },
    });

    // Open dropdown → open form.
    click(".bench-selector .toggle-btn");
    click(".bench-dropdown .create-bench-btn");

    // Fill in the form.
    const nameInput = document.querySelector(
      ".create-bench-form input[name='name']",
    ) as HTMLInputElement;

    expect(nameInput).not.toBeNull();

    flushSync(() => {
      nameInput.value = "My Bench";
      nameInput.dispatchEvent(new Event("input"));
    });

    const form = document.querySelector(".create-bench-form") as HTMLFormElement;
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await Promise.resolve();

    expect(store.createBench).toHaveBeenCalledWith("My Bench");
  });
});

// ---------------------------------------------------------------------------
// Delete bench
// ---------------------------------------------------------------------------

describe("delete bench", () => {
  it("shows a delete action for non-default benches", () => {
    const store = mockBenchStore({
      get benches() { return [defaultBench, benchA]; },
    });
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: store },
    });

    click(".bench-selector .toggle-btn");

    const dropdown = document.querySelector(".bench-dropdown")!;
    // Bench A's item should have a delete affordance.
    const benchAItem = Array.from(
      dropdown.querySelectorAll(".bench-item"),
    ).find((el) => el.textContent?.includes("Bench A"));
    expect(benchAItem?.querySelector(".delete-bench-btn")).not.toBeNull();
  });

  it("does not show a delete action for the default bench", () => {
    const store = mockBenchStore({
      get benches() { return [defaultBench, benchA]; },
    });
    component = mount(BenchSelector, {
      target: document.body,
      props: { benchStore: store },
    });

    click(".bench-selector .toggle-btn");

    const dropdown = document.querySelector(".bench-dropdown")!;
    const defaultItem = Array.from(
      dropdown.querySelectorAll(".bench-item"),
    ).find((el) => el.textContent?.includes("default") && !el.textContent?.includes("Bench"));
    // Default bench must not have a delete button.
    expect(defaultItem?.querySelector(".delete-bench-btn")).toBeNull();
  });
});
