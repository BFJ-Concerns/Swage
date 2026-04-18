// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import BuilderDeck from "../components/BuilderDeck.svelte";
import type { ProjectStore } from "../stores/projects";

import * as builderApi from "../api/builder";

vi.mock("../api/builder", () => ({
  listTeams: vi.fn(),
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  listProjectExtensions: vi.fn(),
  createProjectExtension: vi.fn(),
  updateProjectExtension: vi.fn(),
  approveWorkspaceExtension: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function mockProjectStore(): ProjectStore {
  return {
    get projects() {
      return [
        {
          id: "project-1",
          name: "Builder Project",
          is_default: false,
          slug: "builder-project",
          workspace_path: "/tmp/workspace",
          created_at: "2026-01-01T00:00:00Z",
        },
      ];
    },
    get currentProject() {
      return this.projects[0];
    },
    get isLoading() {
      return false;
    },
    loadProjects: async () => {},
    selectProject: async () => {},
    createProject: async () => {},
    deleteProject: async () => {},
  };
}

describe("BuilderDeck", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(builderApi.listTeams).mockResolvedValue([
      {
        id: "team-1",
        project_id: "project-1",
        name: "core",
        description: "Core team",
        entry_agent_id: "default",
        members: ["default"],
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      },
    ]);
    vi.mocked(builderApi.listProjectExtensions).mockResolvedValue([
      {
        name: "builder-guardrails",
        display_name: "Builder Guardrails",
        description: "Managed extension",
        requested_tools: ["shell"],
        instructions: "Do the careful thing.",
        status: "active",
        source_kind: "managed",
      },
      {
        name: "workspace-skill",
        display_name: null,
        description: "Pending workspace skill",
        requested_tools: ["file_read"],
        instructions: "Wait for approval.",
        status: "approval_required",
        source_kind: "workspace_candidate",
      },
    ]);
    vi.mocked(builderApi.createTeam).mockResolvedValue({
      id: "team-2",
      project_id: "project-1",
      name: "delivery",
      description: "Delivery team",
      entry_agent_id: "default",
      members: ["default", "reviewer"],
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T00:00:00Z",
    });
    vi.mocked(builderApi.createProjectExtension).mockResolvedValue({
      name: "delivery-brief",
      display_name: "Delivery Brief",
      description: "Managed extension",
      requested_tools: ["shell"],
      instructions: "Keep the brief explicit.",
      status: "active",
      source_kind: "managed",
    });
    vi.mocked(builderApi.approveWorkspaceExtension).mockResolvedValue({
      name: "workspace-skill",
      display_name: null,
      description: "Pending workspace skill",
      requested_tools: ["file_read"],
      instructions: "Wait for approval.",
      status: "active",
      source_kind: "managed",
    });
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    document.body.innerHTML = "";
  });

  it("loads existing teams and extensions for the current project", async () => {
    component = mount(BuilderDeck, {
      target: document.body,
      props: {
        projectStore: mockProjectStore(),
        activeLens: "builder",
        onSelectLens: vi.fn(),
      },
    });

    await flushUi();

    expect(document.body.textContent).toContain("Builder Guardrails");
    expect(document.body.textContent).toContain("workspace-skill");
  });

  it("creates a team and a managed extension through the builder forms", async () => {
    component = mount(BuilderDeck, {
      target: document.body,
      props: {
        projectStore: mockProjectStore(),
        activeLens: "builder",
        onSelectLens: vi.fn(),
      },
    });

    const inputs = Array.from(document.querySelectorAll("input"));
    const textareas = Array.from(document.querySelectorAll("textarea"));
    (inputs[0] as HTMLInputElement).value = "delivery";
    inputs[0].dispatchEvent(new Event("input", { bubbles: true }));
    (textareas[0] as HTMLTextAreaElement).value = "Delivery team";
    textareas[0].dispatchEvent(new Event("input", { bubbles: true }));
    (
      Array.from(document.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Create team",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.createTeam).toHaveBeenCalledWith("project-1", {
      name: "delivery",
      description: "Delivery team",
      entry_agent_id: "default",
      members: [],
    });

    (inputs[3] as HTMLInputElement).value = "delivery-brief";
    inputs[3].dispatchEvent(new Event("input", { bubbles: true }));
    (textareas[1] as HTMLTextAreaElement).value = "Managed extension";
    textareas[1].dispatchEvent(new Event("input", { bubbles: true }));
    (textareas[2] as HTMLTextAreaElement).value = "Keep the brief explicit.";
    textareas[2].dispatchEvent(new Event("input", { bubbles: true }));
    (
      Array.from(document.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Create extension",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.createProjectExtension).toHaveBeenCalledWith("project-1", {
      name: "delivery-brief",
      display_name: null,
      description: "Managed extension",
      requested_tools: [],
      instructions: "Keep the brief explicit.",
    });
  });

  it("approves workspace extension candidates", async () => {
    component = mount(BuilderDeck, {
      target: document.body,
      props: {
        projectStore: mockProjectStore(),
        activeLens: "builder",
        onSelectLens: vi.fn(),
      },
    });

    await flushUi();
    (
      Array.from(document.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Approve import",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.approveWorkspaceExtension).toHaveBeenCalledWith(
      "project-1",
      "workspace-skill",
    );
  });
});
