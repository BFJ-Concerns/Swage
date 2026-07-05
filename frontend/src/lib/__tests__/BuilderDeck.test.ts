// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import BuilderDeck from "../components/BuilderDeck.svelte";
import type { BenchStore } from "../stores/benches";

import * as builderApi from "../api/builder";
import * as operatorApi from "../api/operator";

vi.mock("../api/builder", () => ({
  createBuilderProposal: vi.fn(),
  applyBuilderProposal: vi.fn(),
  listTeams: vi.fn(),
  createTeam: vi.fn(),
  updateTeam: vi.fn(),
  listBenchExtensions: vi.fn(),
  createBenchExtension: vi.fn(),
  updateBenchExtension: vi.fn(),
  approveWorkspaceExtension: vi.fn(),
  listSharedReferences: vi.fn(),
  getSharedReference: vi.fn(),
  createSharedReferenceEvent: vi.fn(),
  updateTeamTopologyReference: vi.fn(),
  isTeamTopologyPayload: (payload: Record<string, unknown>) => "nodes" in payload,
  isContextSnapshotPayload: (payload: Record<string, unknown>) => "elements" in payload,
}));

vi.mock("../api/operator", () => ({
  fetchOperatorOverview: vi.fn(),
  approvePendingAction: vi.fn(),
  rejectPendingAction: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
  fetchScheduleRuns: vi.fn(),
}));

vi.mock("../api/lens", () => ({
  createLens: vi.fn(),
  updateLens: vi.fn(),
  installLensPreset: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function mockBenchStore(): BenchStore {
  return {
    get benches() {
      return [
        {
          id: "bench-1",
          name: "Builder Bench",
          description: "",
          is_default: false,
          slug: "builder-bench",
          created_at: "2026-01-01T00:00:00Z",
        },
      ];
    },
    get currentBench() {
      return this.benches[0];
    },
    get recentBenches() {
      return [];
    },
    get isLoading() {
      return false;
    },
    loadBenches: async () => {},
    selectBench: async () => {},
    closeBench: () => {},
    createBench: async () => {},
    deleteBench: async () => {},
  };
}

function buildTeam(id: string, name: string) {
  return {
    id,
    bench_id: "bench-1",
    name,
    description: `${name} team`,
    entry_agent_id: "default",
    agents: [
      {
        id: "default",
        display_name: "Default",
        description: "",
        instructions: "",
        model: null,
      },
      {
        id: "reviewer",
        display_name: "Reviewer",
        description: "Reviews changes",
        instructions: "Review the plan.",
        model: "review-model",
      },
    ],
    tasks: [
      {
        name: "review-plan",
        description: "Review the plan",
        instructions: "Check the plan.",
        model: null,
        output_schema: null,
        max_output_retries: null,
        tool_allow: ["file_read"],
        tool_deny: null,
      },
    ],
    members: ["default", "reviewer"],
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
  };
}

describe("BuilderDeck", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(builderApi.listTeams).mockResolvedValue([
      buildTeam("team-1", "core"),
    ]);
    vi.mocked(builderApi.listBenchExtensions).mockResolvedValue([
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
      ...buildTeam("team-2", "delivery"),
      description: "Delivery team",
      agents: [
        {
          id: "default",
          display_name: "Default",
          description: "",
          instructions: "",
          model: null,
        },
      ],
      tasks: [],
      members: ["default", "reviewer"],
    });
    vi.mocked(builderApi.createBenchExtension).mockResolvedValue({
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
    vi.mocked(builderApi.createBuilderProposal).mockResolvedValue({
      summary: "Create a delivery workspace.",
      operations: [
        {
          kind: "create_team",
          summary: "Create team delivery-core.",
          ready: true,
          diagnostics: [],
          operation: {
            kind: "create_team",
            name: "delivery-core",
            description: "Delivery team",
            entry_agent_id: "default",
            members: ["reviewer"],
          },
        },
      ],
      diagnostics: [],
      can_apply: true,
    });
    vi.mocked(builderApi.applyBuilderProposal).mockResolvedValue({
      status: 200,
      body: {
        success: true,
        summary: "Applied 1 builder change(s).",
        diagnostics: [],
        applied: [
          {
            kind: "create_team",
            summary: "Created team delivery-core.",
            entity_kind: "team",
            entity_id: "team-2",
          },
        ],
      },
    });
    vi.mocked(builderApi.listSharedReferences).mockResolvedValue([]);
    vi.mocked(operatorApi.fetchOperatorOverview).mockResolvedValue({
      gateway: {
        status: "running",
        health: "healthy",
        shutdown_requested: false,
        seconds_since_activity: 0,
        active_turn_count: 0,
        active_connection_count: 0,
      },
      sessions: [],
      approvals: [],
      schedules: { schedules: [] },
      channels: { channels: [] },
      nodes: [],
    });
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    document.body.innerHTML = "";
  });

  it("loads existing teams and extensions for the current bench", async () => {
    component = mount(BuilderDeck, {
      target: document.body,
      props: {
        benchStore: mockBenchStore(),
        activeLens: "builder",
        availableLenses: [
          {
            id: "assistant",
            title: "Assistant lens",
            summary: "Conversation-first view",
            default_surface: "conversation",
            ui_modules: ["conversation", "operator"],
            prominent_team_ids: ["default"],
            source_kind: "built_in",
          },
          {
            id: "builder",
            title: "Builder lens",
            summary: "Workspace-first view",
            default_surface: "builder",
            ui_modules: ["builder", "conversation"],
            prominent_team_ids: ["workspace-core"],
            source_kind: "built_in",
          },
        ],
        onSelectLens: vi.fn(),
        onRefreshLenses: vi.fn(),
      },
    });

    await flushUi();

    expect(document.body.textContent).toContain("Builder Guardrails");
    expect(document.body.textContent).toContain("workspace-skill");
  });

  it("renders interactive builder modules and attaches tools to the selected task", async () => {
    component = mount(BuilderDeck, {
      target: document.body,
      props: {
        benchStore: mockBenchStore(),
        activeLens: "builder",
        availableLenses: [
          {
            id: "builder",
            title: "Builder lens",
            summary: "Workspace-first view",
            default_surface: "builder",
            ui_modules: ["builder", "conversation"],
            prominent_team_ids: ["workspace-core"],
            source_kind: "built_in",
          },
        ],
        onSelectLens: vi.fn(),
        onRefreshLenses: vi.fn(),
      },
    });

    await flushUi();

    expect(document.body.textContent).toContain("Orchestration flow");
    expect(document.body.textContent).toContain("Agent inspector");
    expect(document.body.textContent).toContain("Tool palette");
    expect(document.body.textContent).not.toContain("SHELL");

    (
      Array.from(document.querySelectorAll(".flow-node")).find((node) =>
        node.textContent?.includes("review-plan"),
      ) as HTMLButtonElement
    ).click();
    await flushUi();
    (
      Array.from(document.querySelectorAll(".tool-chip")).find((node) =>
        node.textContent?.includes("shell"),
      ) as HTMLButtonElement
    ).click();
    await flushUi();
    (
      Array.from(document.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Update task draft",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(document.body.textContent).toContain("file_read, shell");
  });

  it("creates a team and a managed extension through the builder forms", async () => {
    const refreshLenses = vi.fn();
    component = mount(BuilderDeck, {
      target: document.body,
      props: {
        benchStore: mockBenchStore(),
        activeLens: "builder",
        availableLenses: [
          {
            id: "assistant",
            title: "Assistant lens",
            summary: "Conversation-first view",
            default_surface: "conversation",
            ui_modules: ["conversation", "operator"],
            prominent_team_ids: ["default"],
            source_kind: "built_in",
          },
          {
            id: "builder",
            title: "Builder lens",
            summary: "Workspace-first view",
            default_surface: "builder",
            ui_modules: ["builder", "conversation"],
            prominent_team_ids: ["workspace-core"],
            source_kind: "built_in",
          },
        ],
        onSelectLens: vi.fn(),
        onRefreshLenses: refreshLenses,
      },
    });

    await flushUi();

    const teamCard = Array.from(document.querySelectorAll("article.builder-card")).find((card) =>
      card.textContent?.includes("Entry team"),
    ) as HTMLElement;
    const teamNameInput = teamCard.querySelector(
      'input[placeholder="workspace-core"]',
    ) as HTMLInputElement;
    const teamDescriptionInput = teamCard.querySelector("textarea") as HTMLTextAreaElement;
    teamNameInput.value = "delivery";
    teamNameInput.dispatchEvent(new Event("input", { bubbles: true }));
    teamDescriptionInput.value = "Delivery team";
    teamDescriptionInput.dispatchEvent(new Event("input", { bubbles: true }));
    (
      Array.from(document.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Create team",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.createTeam).toHaveBeenCalledWith("bench-1", {
      name: "delivery",
      description: "Delivery team",
      entry_agent_id: "default",
      agents: [
        {
          id: "default",
          display_name: "Default",
          description: "",
          instructions: "",
          model: null,
        },
      ],
      tasks: [],
    });

    const extensionCard = document.querySelector("article.extension-card") as HTMLElement;
    const extensionNameInput = extensionCard.querySelector(
      'input[placeholder="builder-guardrails"]',
    ) as HTMLInputElement;
    const extensionTextareas = Array.from(
      extensionCard.querySelectorAll("textarea"),
    ) as HTMLTextAreaElement[];
    extensionNameInput.value = "delivery-brief";
    extensionNameInput.dispatchEvent(new Event("input", { bubbles: true }));
    extensionTextareas[0].value = "Managed extension";
    extensionTextareas[0].dispatchEvent(new Event("input", { bubbles: true }));
    extensionTextareas[1].value = "Keep the brief explicit.";
    extensionTextareas[1].dispatchEvent(new Event("input", { bubbles: true }));
    (
      Array.from(document.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Create extension",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.createBenchExtension).toHaveBeenCalledWith("bench-1", {
      name: "delivery-brief",
      display_name: null,
      description: "Managed extension",
      requested_tools: [],
      instructions: "Keep the brief explicit.",
    });
    expect(refreshLenses).not.toHaveBeenCalled();
  });

  it("approves workspace extension candidates", async () => {
    component = mount(BuilderDeck, {
      target: document.body,
      props: {
        benchStore: mockBenchStore(),
        activeLens: "builder",
        availableLenses: [
          {
            id: "assistant",
            title: "Assistant lens",
            summary: "Conversation-first view",
            default_surface: "conversation",
            ui_modules: ["conversation", "operator"],
            prominent_team_ids: ["default"],
            source_kind: "built_in",
          },
          {
            id: "builder",
            title: "Builder lens",
            summary: "Workspace-first view",
            default_surface: "builder",
            ui_modules: ["builder", "conversation"],
            prominent_team_ids: ["workspace-core"],
            source_kind: "built_in",
          },
        ],
        onSelectLens: vi.fn(),
        onRefreshLenses: vi.fn(),
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
      "bench-1",
      "workspace-skill",
    );
  });

  it("proposes and applies conversational builder changes", async () => {
    const refreshLenses = vi.fn();
    component = mount(BuilderDeck, {
      target: document.body,
      props: {
        benchStore: mockBenchStore(),
        activeLens: "builder",
        availableLenses: [
          {
            id: "assistant",
            title: "Assistant lens",
            summary: "Conversation-first view",
            default_surface: "conversation",
            ui_modules: ["conversation", "operator"],
            prominent_team_ids: ["default"],
            source_kind: "built_in",
          },
          {
            id: "builder",
            title: "Builder lens",
            summary: "Workspace-first view",
            default_surface: "builder",
            ui_modules: ["builder", "conversation"],
            prominent_team_ids: ["workspace-core"],
            source_kind: "built_in",
          },
        ],
        onSelectLens: vi.fn(),
        onRefreshLenses: refreshLenses,
      },
    });

    await flushUi();

    const conversationalCard = Array.from(
      document.querySelectorAll("article.builder-card"),
    ).find((card) => card.textContent?.includes("Conversational builder runtime")) as HTMLElement;
    const prompt = conversationalCard.querySelector("textarea") as HTMLTextAreaElement;
    prompt.value = "Create a delivery workspace.";
    prompt.dispatchEvent(new Event("input", { bubbles: true }));

    (
      Array.from(conversationalCard.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Propose changes",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.createBuilderProposal).toHaveBeenCalledWith(
      "bench-1",
      "Create a delivery workspace.",
    );
    expect(document.body.textContent).toContain("Create team delivery-core.");

    (
      Array.from(conversationalCard.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Apply reviewed changes",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.applyBuilderProposal).toHaveBeenCalled();
    expect(refreshLenses).toHaveBeenCalled();
  });
});
