// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import SharedReferenceStudio from "../components/SharedReferenceStudio.svelte";
import * as builderApi from "../api/builder";

vi.mock("../api/builder", () => ({
  listSharedReferences: vi.fn(),
  getSharedReference: vi.fn(),
  createSharedReferenceEvent: vi.fn(),
  updateTeamTopologyReference: vi.fn(),
  isTeamTopologyPayload: (payload: Record<string, unknown>) => "nodes" in payload,
  isContextSnapshotPayload: (payload: Record<string, unknown>) => "elements" in payload,
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("SharedReferenceStudio", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(builderApi.listSharedReferences).mockResolvedValue([
      {
        kind: "team_topology",
        id: "team-1",
        title: "Builders",
        subtitle: "3 member(s) · lead builder",
        updated_at: "2026-04-17T10:00:00Z",
      },
      {
        kind: "context_snapshot",
        id: "turn-1",
        title: "Context turn-1",
        subtitle: "builder · 128 token(s)",
        updated_at: "2026-04-17T09:59:00Z",
      },
    ]);
    vi.mocked(builderApi.getSharedReference).mockImplementation(async (kind, id) => {
      if (kind === "team_topology" && id === "team-1") {
        return {
          object: {
            kind,
            id,
            title: "Builders",
            subtitle: "Shapes the builder graph.",
            payload: {
              team_id: "team-1",
              space_id: "space-1",
              entry_agent_id: "builder",
              members: ["builder", "reviewer", "qa"],
              nodes: [
                { agent_id: "builder", label: "builder", column: 0, is_entry: true },
                { agent_id: "reviewer", label: "reviewer", column: 1, is_entry: false },
              ],
              edges: [{ from: "builder", to: "reviewer", kind: "delegates_to" }],
            },
          },
          interactions: [],
        };
      }

      return {
        object: {
          kind,
          id,
          title: "Context turn-1",
          subtitle: "builder · 128 token(s)",
          payload: {
            turn_id: "turn-1",
            assembly_name: "builder",
            total_tokens: 128,
            budget: 512,
            warnings: ["Near budget"],
            elements: [
              {
                element_id: "history:0",
                name: "history",
                source: "session",
                token_count: 128,
                priority: 10,
                preview: "Recent build notes",
                child_count: 0,
              },
            ],
            provider_visible: {
              requests: [
                {
                  round: 1,
                  request: {
                    provider: "openai",
                    body: {
                      messages: [{ role: "user", content: "hello" }],
                    },
                  },
                },
              ],
              reasoning: [{ round: 1, content: "private summary" }],
            },
          },
        },
        interactions: [
          {
            id: "event-1",
            event_kind: "selected",
            payload: { element_id: "history:0" },
            created_at: "2026-04-17T10:01:00Z",
          },
        ],
      };
    });
    vi.mocked(builderApi.createSharedReferenceEvent).mockResolvedValue();
    vi.mocked(builderApi.updateTeamTopologyReference).mockResolvedValue({
      object: {
        kind: "team_topology",
        id: "team-1",
        title: "Builders",
        subtitle: "Shapes the builder graph.",
        payload: {
          team_id: "team-1",
          space_id: "space-1",
          entry_agent_id: "reviewer",
          members: ["builder", "reviewer", "qa"],
          nodes: [
            { agent_id: "builder", label: "builder", column: 0, is_entry: false },
            { agent_id: "reviewer", label: "reviewer", column: 1, is_entry: true },
          ],
          edges: [{ from: "reviewer", to: "builder", kind: "delegates_to" }],
        },
      },
      interactions: [
        {
          id: "event-2",
          event_kind: "updated",
          payload: { entry_agent_id: "reviewer" },
          created_at: "2026-04-17T10:02:00Z",
        },
      ],
    });
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    document.body.innerHTML = "";
  });

  it("lists shared objects and records a topology-node selection", async () => {
    component = mount(SharedReferenceStudio, {
      target: document.body,
      props: {
        spaceId: "space-1",
      },
    });

    await flushUi();

    expect(document.body.textContent).toContain("Shared objects");
    expect(document.body.textContent).toContain("Builders");

    (
      Array.from(document.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Select node",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.createSharedReferenceEvent).toHaveBeenCalledWith(
      "team_topology",
      "team-1",
      "selected",
      {
        agent_id: "builder",
        source: "builder-lens",
      },
    );
  });

  it("promotes a topology node and refreshes summaries", async () => {
    const onTeamTopologyUpdated = vi.fn();
    component = mount(SharedReferenceStudio, {
      target: document.body,
      props: {
        spaceId: "space-1",
        onTeamTopologyUpdated,
      },
    });

    await flushUi();

    (
      Array.from(document.querySelectorAll("button")).find(
        (button) => button.textContent?.trim() === "Promote to lead",
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(builderApi.updateTeamTopologyReference).toHaveBeenCalledWith("team-1", {
      entry_agent_id: "reviewer",
      members: ["builder", "reviewer", "qa"],
    });
    expect(onTeamTopologyUpdated).toHaveBeenCalled();
  });

  it("renders provider-visible debugger data for context snapshots", async () => {
    component = mount(SharedReferenceStudio, {
      target: document.body,
      props: {
        spaceId: "space-1",
      },
    });

    await flushUi();

    (
      Array.from(document.querySelectorAll("button")).find((button) =>
        button.textContent?.includes("Context turn-1"),
      ) as HTMLButtonElement
    ).click();
    await flushUi();

    expect(document.body.textContent).toContain("Provider Visible");
    expect(document.body.textContent).toContain("1 request(s)");
    expect(document.body.textContent).toContain("1 reasoning event(s)");
    expect(document.body.textContent).toContain("\"provider\": \"openai\"");
  });
});
