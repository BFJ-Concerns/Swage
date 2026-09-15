// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import TopicSettingsPage from "../components/TopicSettingsPage.svelte";
import * as topicsApi from "../api/topics";
import type { Topic } from "../types";

vi.mock("../api/topics", () => ({
  attachTopicWorkspace: vi.fn(),
  detachTopicWorkspace: vi.fn(),
  listTopicSessions: vi.fn(),
  listTopicWorkspaces: vi.fn(),
  updateTopicWorkspaceTrust: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

const topic: Topic = {
  id: "topic-1",
  name: "Research",
  slug: "research",
  description: "Research topic",
  is_default: false,
  created_at: "2026-05-26T10:00:00Z",
};

describe("TopicSettingsPage", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(topicsApi.listTopicWorkspaces).mockResolvedValue([
      {
        id: "workspace-1",
        topic_id: "topic-1",
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
    vi.mocked(topicsApi.listTopicSessions).mockResolvedValue([
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

  it("loads topic-scoped data for the overview", async () => {
    component = mount(TopicSettingsPage, {
      target: document.body,
      props: { topic, tab: "overview" },
    });
    await flushUi();

    expect(topicsApi.listTopicWorkspaces).toHaveBeenCalledWith("topic-1");
    expect(topicsApi.listTopicSessions).toHaveBeenCalledWith("topic-1");
    expect(document.body.textContent).toContain("Research");
    expect(document.body.textContent).toContain("Workspaces 1");
    expect(document.body.textContent).toContain("Sessions 1");
  });

  it("renders topic workspaces without global settings controls", async () => {
    component = mount(TopicSettingsPage, {
      target: document.body,
      props: { topic, tab: "workspaces" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("Source");
    expect(document.body.textContent).toContain("/work/source");
    expect(document.body.textContent).toContain("Trusted workspace content can supply instructions");
    expect(document.body.textContent).toContain("Revoke trust");
    expect(document.body.textContent).toContain("Detach");
    expect(document.body.textContent).not.toContain("Providers");
  });

  it("attaches a workspace from settings and reloads the displayed attachments", async () => {
    vi.mocked(topicsApi.attachTopicWorkspace).mockResolvedValue({
      id: "workspace-2", topic_id: "topic-1", label: "New source", path: "/work/new", canonical_path: "/work/new", is_missing: false, permissions: ["read"], is_trusted: false, is_default: false, created_at: "2026-05-26T10:00:00Z",
    });
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "workspaces" } });
    await flushUi();
    const inputs = document.querySelectorAll("input");
    (inputs[0] as HTMLInputElement).value = "/work/new";
    inputs[0].dispatchEvent(new Event("input", { bubbles: true }));
    document.querySelector("form")?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(topicsApi.attachTopicWorkspace).toHaveBeenCalledWith("topic-1", { path: "/work/new", label: undefined, permissions: ["read", "write"], is_trusted: false, set_as_default: false });
    expect(topicsApi.listTopicWorkspaces).toHaveBeenCalledTimes(2);
  });

  it("uses the first attached workspace as the working folder and honours access choices", async () => {
    vi.mocked(topicsApi.listTopicWorkspaces).mockResolvedValue([]);
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "workspaces" } });
    await flushUi();
    const path = document.querySelector("input") as HTMLInputElement;
    path.value = "/work/first";
    path.dispatchEvent(new Event("input", { bubbles: true }));
    const checkboxes = document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect(checkboxes[0].checked).toBe(true);
    expect(checkboxes[1].checked).toBe(false);
    checkboxes[0].click();
    checkboxes[1].click();
    document.querySelector("form")?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(topicsApi.attachTopicWorkspace).toHaveBeenCalledWith("topic-1", {
      path: "/work/first", label: undefined, permissions: ["read"], is_trusted: true, set_as_default: true,
    });
  });

  it("does not attach while the workspace list is unknown, so an existing working folder is never displaced", async () => {
    vi.mocked(topicsApi.listTopicWorkspaces).mockRejectedValue(new Error("workspace list unavailable"));
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "workspaces" } });
    await flushUi();
    expect(document.body.textContent).toContain("workspace list unavailable");
    const path = document.querySelector("input") as HTMLInputElement;
    path.value = "/work/orphan";
    path.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    const attach = document.querySelector('button[type="submit"]') as HTMLButtonElement;
    expect(attach.disabled).toBe(true);
    document.querySelector("form")?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(topicsApi.attachTopicWorkspace).not.toHaveBeenCalled();
  });

  it("resets the access and trust choices once a workspace is attached", async () => {
    vi.mocked(topicsApi.attachTopicWorkspace).mockResolvedValue({
      id: "workspace-2", topic_id: "topic-1", label: "Second", path: "/work/second", canonical_path: "/work/second", is_missing: false, permissions: ["read"], is_trusted: true, is_default: false, created_at: "2026-05-26T10:00:00Z",
    });
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "workspaces" } });
    await flushUi();
    const path = document.querySelector("input") as HTMLInputElement;
    path.value = "/work/second";
    path.dispatchEvent(new Event("input", { bubbles: true }));
    const checkboxes = document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    checkboxes[0].click();
    checkboxes[1].click();
    document.querySelector("form")?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(topicsApi.attachTopicWorkspace).toHaveBeenCalledWith("topic-1", expect.objectContaining({ permissions: ["read"], is_trusted: true }));
    const after = document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect(after[0].checked).toBe(true);
    expect(after[1].checked).toBe(false);
  });

  it("revokes trust through the workspace mutation API and reloads", async () => {
    vi.mocked(topicsApi.updateTopicWorkspaceTrust).mockResolvedValue({
      id: "workspace-1", topic_id: "topic-1", label: "Source", path: "/work/source", canonical_path: "/work/source", is_missing: false, permissions: ["read", "write"], is_trusted: false, is_default: true, created_at: "2026-05-26T10:00:00Z",
    });
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "workspaces" } });
    await flushUi();
    Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.includes("Revoke trust"))?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flushUi();
    expect(topicsApi.updateTopicWorkspaceTrust).toHaveBeenCalledWith("topic-1", "workspace-1", { is_trusted: false });
    expect(topicsApi.listTopicWorkspaces).toHaveBeenCalledTimes(2);
  });

  it("detaches a workspace through the workspace mutation API and reloads", async () => {
    vi.mocked(topicsApi.detachTopicWorkspace).mockResolvedValue();
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "workspaces" } });
    await flushUi();
    Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.includes("Detach"))?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flushUi();
    expect(topicsApi.detachTopicWorkspace).toHaveBeenCalledWith("topic-1", "workspace-1");
    expect(topicsApi.listTopicWorkspaces).toHaveBeenCalledTimes(2);
  });
});
