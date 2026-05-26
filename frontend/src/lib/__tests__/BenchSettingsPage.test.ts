// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import BenchSettingsPage from "../components/BenchSettingsPage.svelte";
import * as benchesApi from "../api/benches";
import type { Bench } from "../types";

vi.mock("../api/benches", () => ({
  listBenchSessions: vi.fn(),
  listBenchWorkspaces: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

const bench: Bench = {
  id: "bench-1",
  name: "Research",
  slug: "research",
  description: "Research bench",
  is_default: false,
  created_at: "2026-05-26T10:00:00Z",
};

describe("BenchSettingsPage", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(benchesApi.listBenchWorkspaces).mockResolvedValue([
      {
        id: "workspace-1",
        bench_id: "bench-1",
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
    vi.mocked(benchesApi.listBenchSessions).mockResolvedValue([
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

  it("loads bench-scoped data for the overview", async () => {
    component = mount(BenchSettingsPage, {
      target: document.body,
      props: { bench, tab: "overview" },
    });
    await flushUi();

    expect(benchesApi.listBenchWorkspaces).toHaveBeenCalledWith("bench-1");
    expect(benchesApi.listBenchSessions).toHaveBeenCalledWith("bench-1");
    expect(document.body.textContent).toContain("Research");
    expect(document.body.textContent).toContain("Workspaces 1");
    expect(document.body.textContent).toContain("Sessions 1");
  });

  it("renders bench workspaces without global settings controls", async () => {
    component = mount(BenchSettingsPage, {
      target: document.body,
      props: { bench, tab: "workspaces" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("Source");
    expect(document.body.textContent).toContain("/work/source");
    expect(document.body.textContent).not.toContain("Providers");
  });
});
