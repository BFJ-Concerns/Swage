// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import SpaceSettingsPage from "../components/SpaceSettingsPage.svelte";
import * as spacesApi from "../api/spaces";
import type { Space } from "../types";

vi.mock("../api/spaces", () => ({
  listSpaceSessions: vi.fn(),
  listSpaceWorkspaces: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

const space: Space = {
  id: "space-1",
  name: "Research",
  slug: "research",
  description: "Research space",
  is_default: false,
  created_at: "2026-05-26T10:00:00Z",
};

describe("SpaceSettingsPage", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(spacesApi.listSpaceWorkspaces).mockResolvedValue([
      {
        id: "workspace-1",
        space_id: "space-1",
        label: "Source",
        path: "/work/source",
        canonical_path: "/work/source",
        is_missing: false,
        permissions: ["read", "write"],
        is_trusted: true,
        is_default: true,
        created_at: "2026-05-26T10:00:00Z",
      },
    ]);
    vi.mocked(spacesApi.listSpaceSessions).mockResolvedValue([
      {
        id: "session-1",
        created_at: "2026-05-26T10:00:00Z",
        last_activity: "2026-05-26T10:05:00Z",
        message_preview: "Review the release",
        has_interrupted_turn: false,
      },
    ]);
  });

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.innerHTML = "";
    vi.clearAllMocks();
  });

  it("loads space-scoped data for the overview", async () => {
    component = mount(SpaceSettingsPage, {
      target: document.body,
      props: { space, tab: "overview" },
    });
    await flushUi();

    expect(spacesApi.listSpaceWorkspaces).toHaveBeenCalledWith("space-1");
    expect(spacesApi.listSpaceSessions).toHaveBeenCalledWith("space-1");
    expect(document.body.textContent).toContain("Research");
    expect(document.body.textContent).toContain("Workspaces 1");
    expect(document.body.textContent).toContain("Sessions 1");
  });

  it("renders space workspaces without global settings controls", async () => {
    component = mount(SpaceSettingsPage, {
      target: document.body,
      props: { space, tab: "workspaces" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("Source");
    expect(document.body.textContent).toContain("/work/source");
    expect(document.body.textContent).not.toContain("Providers");
  });
});
