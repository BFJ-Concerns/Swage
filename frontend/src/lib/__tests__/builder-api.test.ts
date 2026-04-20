// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  applyBuilderProposal,
  createBuilderProposal,
  approveWorkspaceExtension,
  createProjectExtension,
  createTeam,
  listProjectExtensions,
  listTeams,
  updateProjectExtension,
  updateTeam,
} from "../api/builder";

function mockResponse(body: unknown, status = 200): Response {
  return new Response(body === null ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("builder api", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn());
  });

  it("lists teams for a project", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(
      mockResponse({ teams: [{ id: "team-1", name: "core" }] }),
    );

    const teams = await listTeams("project-1");

    expect(teams).toEqual([{ id: "team-1", name: "core" }]);
    expect(fetch).toHaveBeenCalledWith("/api/projects/project-1/teams", {
      method: "GET",
    });
  });

  it("creates and updates teams", async () => {
    vi.mocked(fetch)
      .mockResolvedValueOnce(mockResponse({ id: "team-1", name: "core" }, 201))
      .mockResolvedValueOnce(mockResponse({ id: "team-1", name: "core-updated" }));

    const created = await createTeam("project-1", {
      name: "core",
      description: "",
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
    const updated = await updateTeam("team-1", { description: "updated" });

    expect(created.name).toBe("core");
    expect(updated.name).toBe("core-updated");
  });

  it("lists, creates, updates, and approves project extensions", async () => {
    vi.mocked(fetch)
      .mockResolvedValueOnce(
        mockResponse({
          extensions: [{ name: "builder-guardrails", status: "active" }],
        }),
      )
      .mockResolvedValueOnce(
        mockResponse({ name: "builder-guardrails", status: "active" }, 201),
      )
      .mockResolvedValueOnce(
        mockResponse({ name: "builder-guardrails", status: "active" }),
      )
      .mockResolvedValueOnce(
        mockResponse({ name: "workspace-skill", status: "active" }),
      );

    const listed = await listProjectExtensions("project-1");
    const created = await createProjectExtension("project-1", {
      name: "builder-guardrails",
      display_name: "Builder Guardrails",
      description: "desc",
      requested_tools: ["shell"],
      instructions: "Do the careful thing.",
    });
    const updated = await updateProjectExtension("project-1", "builder-guardrails", {
      display_name: "Builder Guardrails",
      description: "updated",
      requested_tools: ["shell", "file_read"],
      instructions: "Do the careful thing with logs.",
    });
    const approved = await approveWorkspaceExtension("project-1", "workspace-skill");

    expect(listed[0].name).toBe("builder-guardrails");
    expect(created.status).toBe("active");
    expect(updated.status).toBe("active");
    expect(approved.name).toBe("workspace-skill");
  });

  it("creates and applies builder proposals", async () => {
    vi.mocked(fetch)
      .mockResolvedValueOnce(
        mockResponse({
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
        }),
      )
      .mockResolvedValueOnce(
        mockResponse(
          {
            success: true,
            summary: "Applied 1 builder change(s).",
            diagnostics: [],
            applied: [
              {
                kind: "create_team",
                summary: "Created team delivery-core.",
                entity_kind: "team",
                entity_id: "team-1",
              },
            ],
          },
          200,
        ),
      );

    const proposal = await createBuilderProposal(
      "project-1",
      "Create a delivery workspace.",
    );
    const applied = await applyBuilderProposal("project-1", [
      {
        kind: "create_team",
        name: "delivery-core",
        description: "Delivery team",
        entry_agent_id: "default",
        members: ["reviewer"],
      },
    ]);

    expect(proposal.can_apply).toBe(true);
    expect(applied.status).toBe(200);
    expect(applied.body.success).toBe(true);
  });
});
