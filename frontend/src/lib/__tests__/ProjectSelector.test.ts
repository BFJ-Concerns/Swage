// @vitest-environment jsdom
/**
 * Tests for the interactive ProjectSelector component.
 *
 * The component shows the current project name and opens a dropdown to
 * switch projects or create new ones. All store calls are mocked.
 *
 * flushSync() is used whenever a click triggers a Svelte $state change and
 * the test needs to query the updated DOM immediately after.
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import ProjectSelector from "../components/ProjectSelector.svelte";
import type { ProjectStore } from "../stores/projects";

type MountedComponent = ReturnType<typeof mount>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeProject(id: string, name: string) {
  return {
    id,
    name,
    slug: name.toLowerCase().replace(/\s+/g, "-"),
    workspace_path: `/home/user/${id}`,
    created_at: "2026-01-01T00:00:00Z",
  };
}

const defaultProject = makeProject("proj-default", "default");
const projectA = makeProject("proj-a", "Project A");
const projectB = makeProject("proj-b", "Project B");

function mockProjectStore(overrides: Partial<ProjectStore> = {}): ProjectStore {
  return {
    get projects() { return [defaultProject, projectA]; },
    get currentProject() { return defaultProject; },
    get isLoading() { return false; },
    loadProjects: vi.fn(),
    selectProject: vi.fn().mockResolvedValue(undefined),
    createProject: vi.fn().mockResolvedValue(undefined),
    deleteProject: vi.fn().mockResolvedValue(undefined),
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
  it("renders the .project-selector container", () => {
    component = mount(ProjectSelector, { target: document.body });
    expect(document.querySelector(".project-selector")).not.toBeNull();
  });

  it("shows a fallback when no store is provided (no crash)", () => {
    component = mount(ProjectSelector, { target: document.body });
    expect(document.querySelector(".project-selector")).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Current project display
// ---------------------------------------------------------------------------

describe("current project display", () => {
  it("shows the current project name", () => {
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: mockProjectStore() },
    });

    expect(document.querySelector(".project-selector")!.textContent).toContain(
      "default",
    );
  });

  it("shows a different project when currentProject changes", () => {
    const store = mockProjectStore({
      get currentProject() { return projectA; },
    });
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: store },
    });

    expect(document.querySelector(".project-selector")!.textContent).toContain(
      "Project A",
    );
  });
});

// ---------------------------------------------------------------------------
// Dropdown: open and close
// ---------------------------------------------------------------------------

describe("dropdown toggle", () => {
  it("dropdown is not visible initially", () => {
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: mockProjectStore() },
    });

    expect(document.querySelector(".project-dropdown")).toBeNull();
  });

  it("opens the dropdown when the toggle button is clicked", () => {
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: mockProjectStore() },
    });

    click(".project-selector .toggle-btn");

    expect(document.querySelector(".project-dropdown")).not.toBeNull();
  });

  it("closes the dropdown when toggled a second time", () => {
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: mockProjectStore() },
    });

    click(".project-selector .toggle-btn");
    expect(document.querySelector(".project-dropdown")).not.toBeNull();

    click(".project-selector .toggle-btn");
    expect(document.querySelector(".project-dropdown")).toBeNull();
  });

  it("closes the dropdown on Escape key", () => {
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: mockProjectStore() },
    });

    click(".project-selector .toggle-btn");
    expect(document.querySelector(".project-dropdown")).not.toBeNull();

    flushSync(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });

    expect(document.querySelector(".project-dropdown")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Project list
// ---------------------------------------------------------------------------

describe("project list in dropdown", () => {
  it("lists all projects from the store", () => {
    const store = mockProjectStore({
      get projects() { return [defaultProject, projectA, projectB]; },
    });
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: store },
    });

    click(".project-selector .toggle-btn");

    const dropdown = document.querySelector(".project-dropdown")!;
    expect(dropdown.textContent).toContain("default");
    expect(dropdown.textContent).toContain("Project A");
    expect(dropdown.textContent).toContain("Project B");
  });

  it("clicking a project calls selectProject with its id", async () => {
    const store = mockProjectStore({
      get projects() { return [defaultProject, projectA]; },
    });
    const onProjectChange = vi.fn();
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: store, onProjectChange },
    });

    click(".project-selector .toggle-btn");

    // Click the Project A item in the dropdown.
    const items = document.querySelectorAll(".project-dropdown .project-item");
    const projectAItem = Array.from(items).find((el) =>
      el.textContent?.includes("Project A"),
    )?.querySelector(".project-select-btn") as HTMLButtonElement;

    expect(projectAItem).not.toBeUndefined();
    flushSync(() => projectAItem.click());

    expect(store.selectProject).toHaveBeenCalledWith("proj-a");
  });

  it("triggers onProjectChange after selecting a project", async () => {
    const store = mockProjectStore({
      get projects() { return [defaultProject, projectA]; },
    });
    const onProjectChange = vi.fn();
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: store, onProjectChange },
    });

    click(".project-selector .toggle-btn");

    const items = document.querySelectorAll(".project-dropdown .project-item");
    const projectAItem = Array.from(items).find((el) =>
      el.textContent?.includes("Project A"),
    )?.querySelector(".project-select-btn") as HTMLButtonElement;

    flushSync(() => projectAItem.click());

    expect(onProjectChange).toHaveBeenCalled();
  });

  it("closes the dropdown after selecting a project", async () => {
    const store = mockProjectStore({
      get projects() { return [defaultProject, projectA]; },
    });
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: store },
    });

    click(".project-selector .toggle-btn");

    const items = document.querySelectorAll(".project-dropdown .project-item");
    const firstProjectButton = items[0]?.querySelector(
      ".project-select-btn",
    ) as HTMLButtonElement;
    flushSync(() => firstProjectButton.click());

    expect(document.querySelector(".project-dropdown")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Create project form
// ---------------------------------------------------------------------------

describe("create project form", () => {
  it("shows a create project button in the dropdown", () => {
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: mockProjectStore() },
    });

    click(".project-selector .toggle-btn");

    expect(
      document.querySelector(".project-dropdown .create-project-btn"),
    ).not.toBeNull();
  });

  it("clicking create reveals the inline form", () => {
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: mockProjectStore() },
    });

    click(".project-selector .toggle-btn");
    click(".project-dropdown .create-project-btn");

    expect(document.querySelector(".create-project-form")).not.toBeNull();
  });

  it("submitting the form calls createProject with name and path", async () => {
    const store = mockProjectStore();
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: store },
    });

    // Open dropdown → open form.
    click(".project-selector .toggle-btn");
    click(".project-dropdown .create-project-btn");

    // Fill in the form.
    const nameInput = document.querySelector(
      ".create-project-form input[name='name']",
    ) as HTMLInputElement;
    const pathInput = document.querySelector(
      ".create-project-form input[name='workspace_path']",
    ) as HTMLInputElement;

    expect(nameInput).not.toBeNull();
    expect(pathInput).not.toBeNull();

    flushSync(() => {
      nameInput.value = "My Project";
      nameInput.dispatchEvent(new Event("input"));
      pathInput.value = "/home/user/my-project";
      pathInput.dispatchEvent(new Event("input"));
    });

    const form = document.querySelector(".create-project-form") as HTMLFormElement;
    form.dispatchEvent(new Event("submit", { cancelable: true }));
    await Promise.resolve();

    expect(store.createProject).toHaveBeenCalledWith(
      "My Project",
      "/home/user/my-project",
    );
  });
});

// ---------------------------------------------------------------------------
// Delete project
// ---------------------------------------------------------------------------

describe("delete project", () => {
  it("shows a delete action for non-default projects", () => {
    const store = mockProjectStore({
      get projects() { return [defaultProject, projectA]; },
    });
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: store },
    });

    click(".project-selector .toggle-btn");

    const dropdown = document.querySelector(".project-dropdown")!;
    // Project A's item should have a delete affordance.
    const projectAItem = Array.from(
      dropdown.querySelectorAll(".project-item"),
    ).find((el) => el.textContent?.includes("Project A"));
    expect(projectAItem?.querySelector(".delete-project-btn")).not.toBeNull();
  });

  it("does not show a delete action for the default project", () => {
    const store = mockProjectStore({
      get projects() { return [defaultProject, projectA]; },
    });
    component = mount(ProjectSelector, {
      target: document.body,
      props: { projectStore: store },
    });

    click(".project-selector .toggle-btn");

    const dropdown = document.querySelector(".project-dropdown")!;
    const defaultItem = Array.from(
      dropdown.querySelectorAll(".project-item"),
    ).find((el) => el.textContent?.includes("default") && !el.textContent?.includes("Project"));
    // Default project must not have a delete button.
    expect(defaultItem?.querySelector(".delete-project-btn")).toBeNull();
  });
});
