/**
 * Unit tests for the project store.
 *
 * All API calls are mocked so tests run in isolation without a gateway.
 * Each test creates a fresh store instance via createProjectStore().
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

import { createProjectStore } from "../stores/projects";
import * as api from "../api/projects";

// ---------------------------------------------------------------------------
// Module mock
// ---------------------------------------------------------------------------

vi.mock("../api/projects", () => ({
  listProjects: vi.fn(),
  createProject: vi.fn(),
  getProject: vi.fn(),
  updateProject: vi.fn(),
  deleteProject: vi.fn(),
  listProjectSessions: vi.fn(),
  createProjectSession: vi.fn(),
  setProjectsBaseUrl: vi.fn(),
}));

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeProject(
  id: string,
  name: string,
  extra: Partial<{ slug: string; workspace_path: string }> = {},
) {
  return {
    id,
    name,
    slug: extra.slug ?? name.toLowerCase().replace(/\s+/g, "-"),
    workspace_path: extra.workspace_path ?? `/home/user/${id}`,
    created_at: "2026-01-01T00:00:00Z",
  };
}

// The default project — special-cased: cannot be deleted.
const defaultProject = makeProject("proj-default", "default", {
  workspace_path: "/home/user/default",
});

const projectA = makeProject("proj-a", "Project A");
const projectB = makeProject("proj-b", "Project B");

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
  it("starts with an empty project list", () => {
    const store = createProjectStore();
    expect(store.projects).toEqual([]);
  });

  it("starts with no current project", () => {
    const store = createProjectStore();
    expect(store.currentProject).toBeNull();
  });

  it("starts with isLoading false", () => {
    const store = createProjectStore();
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// loadProjects
// ---------------------------------------------------------------------------

describe("loadProjects", () => {
  it("fetches projects and stores them", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA, projectB]);

    await store.loadProjects();

    expect(store.projects).toHaveLength(2);
    expect(store.projects[0].id).toBe("proj-a");
  });

  it("auto-selects the project named 'default' on load", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([
      projectA,
      defaultProject,
      projectB,
    ]);

    await store.loadProjects();

    expect(store.currentProject?.name).toBe("default");
    expect(store.currentProject?.id).toBe("proj-default");
  });

  it("leaves currentProject null when there is no default project", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA, projectB]);

    await store.loadProjects();

    expect(store.currentProject).toBeNull();
  });

  it("sets isLoading true during fetch and false after", async () => {
    const store = createProjectStore();
    let loadingDuringFetch = false;

    vi.mocked(api.listProjects).mockImplementationOnce(async () => {
      loadingDuringFetch = store.isLoading;
      return [];
    });

    await store.loadProjects();

    expect(loadingDuringFetch).toBe(true);
    expect(store.isLoading).toBe(false);
  });

  it("clears isLoading even if the API call throws", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockRejectedValueOnce(
      new Error("network error"),
    );

    await expect(store.loadProjects()).rejects.toThrow("network error");
    expect(store.isLoading).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// selectProject
// ---------------------------------------------------------------------------

describe("selectProject", () => {
  it("sets currentProject to the matching project from the loaded list", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA, projectB]);
    await store.loadProjects();

    await store.selectProject("proj-b");

    expect(store.currentProject?.id).toBe("proj-b");
    expect(store.currentProject?.name).toBe("Project B");
  });

  it("does not call the API — selection is from the local list", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA]);
    await store.loadProjects();

    await store.selectProject("proj-a");

    // getProject should never be called — we already have the data.
    expect(api.getProject).not.toHaveBeenCalled();
  });

  it("throws if the project is not found in the list", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA]);
    await store.loadProjects();

    await expect(store.selectProject("proj-missing")).rejects.toThrow();
  });
});

// ---------------------------------------------------------------------------
// createProject
// ---------------------------------------------------------------------------

describe("createProject", () => {
  it("calls the API, adds the project to the list, and selects it", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([defaultProject]);
    await store.loadProjects();

    const newProject = makeProject("proj-new", "New Project");
    vi.mocked(api.createProject).mockResolvedValueOnce(newProject);

    await store.createProject("New Project", "/home/user/new-project");

    expect(api.createProject).toHaveBeenCalledWith({
      name: "New Project",
      workspace_path: "/home/user/new-project",
    });
    expect(store.projects).toHaveLength(2);
    expect(store.currentProject?.id).toBe("proj-new");
  });

  it("adds the new project to an existing list", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA]);
    await store.loadProjects();

    const newProject = makeProject("proj-b", "Project B");
    vi.mocked(api.createProject).mockResolvedValueOnce(newProject);

    await store.createProject("Project B", "/home/user/proj-b");

    expect(store.projects).toHaveLength(2);
  });
});

// ---------------------------------------------------------------------------
// deleteProject
// ---------------------------------------------------------------------------

describe("deleteProject", () => {
  it("removes the project from the list", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA, projectB]);
    await store.loadProjects();
    vi.mocked(api.deleteProject).mockResolvedValueOnce(undefined);

    await store.deleteProject("proj-b");

    expect(store.projects.map((p) => p.id)).toEqual(["proj-a"]);
  });

  it("throws and does not call the API when deleting the default project", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([
      defaultProject,
      projectA,
    ]);
    await store.loadProjects();

    await expect(store.deleteProject("proj-default")).rejects.toThrow();
    expect(api.deleteProject).not.toHaveBeenCalled();
  });

  it("selects the next project when the current project is deleted", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA, projectB]);
    await store.loadProjects();

    await store.selectProject("proj-a");
    vi.mocked(api.deleteProject).mockResolvedValueOnce(undefined);

    await store.deleteProject("proj-a");

    // Should automatically move to proj-b (the remaining project).
    expect(store.currentProject?.id).toBe("proj-b");
    expect(store.projects.map((p) => p.id)).toEqual(["proj-b"]);
  });

  it("sets currentProject to null when the last project is deleted", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA]);
    await store.loadProjects();

    await store.selectProject("proj-a");
    vi.mocked(api.deleteProject).mockResolvedValueOnce(undefined);

    await store.deleteProject("proj-a");

    expect(store.currentProject).toBeNull();
    expect(store.projects).toHaveLength(0);
  });

  it("leaves currentProject unchanged when a non-current project is deleted", async () => {
    const store = createProjectStore();
    vi.mocked(api.listProjects).mockResolvedValueOnce([projectA, projectB]);
    await store.loadProjects();

    await store.selectProject("proj-a");
    vi.mocked(api.deleteProject).mockResolvedValueOnce(undefined);

    await store.deleteProject("proj-b");

    expect(store.currentProject?.id).toBe("proj-a");
    expect(store.projects).toHaveLength(1);
  });
});
