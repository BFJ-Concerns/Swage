// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import TopicSettingsPage from "../components/TopicSettingsPage.svelte";
import * as topicsApi from "../api/topics";
import * as surfacesApi from "../api/surfaces";
import type { SecuritySettingsResponse } from "../protocol/SecuritySettingsResponse";
import type { Topic } from "../types";

vi.mock("../api/topics", () => ({
  attachTopicWorkspace: vi.fn(),
  detachTopicWorkspace: vi.fn(),
  relinkTopicWorkspace: vi.fn(),
  listTopicSessions: vi.fn(),
  listTopicWorkspaces: vi.fn(),
  updateTopicWorkspaceTrust: vi.fn(),
  updateTopic: vi.fn(),
}));

vi.mock("../api/surfaces", () => ({
  addTopicDefaultSurface: vi.fn(),
  listLibrary: vi.fn(),
  listTopicDefaultSurfaces: vi.fn(),
  removeTopicDefaultSurface: vi.fn(),
}));

vi.mock("../api/settings", () => ({
  fetchTopicSecuritySettings: vi.fn(),
  saveTopicSecuritySettings: vi.fn(),
}));
import * as settingsApi from "../api/settings";

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
        agent_id: "gateway",
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

  it("shows the topic's security checks with what stands relaxed and saves its own overrides", async () => {
    vi.mocked(settingsApi.fetchTopicSecuritySettings).mockResolvedValue({
      available_tools: ["shell", "file_write"],
      scope: { kind: "topic", topic_id: "topic-1" },
      effective: { sandbox_enabled: false, allow_degraded_execution: false,
      access_gate: "token", require_approval: ["shell", "file_write"] },
      overrides: {},
      relaxed: [
        { check: "sandbox", relaxed_by: { kind: "install" }, detail: "The per-turn sandbox is off." },
      ],
    });
    vi.mocked(settingsApi.saveTopicSecuritySettings).mockResolvedValue({
      available_tools: ["shell", "file_write"],
      scope: { kind: "topic", topic_id: "topic-1" },
      effective: { sandbox_enabled: true, allow_degraded_execution: false,
      access_gate: "token", require_approval: ["shell", "file_write"] },
      overrides: { sandbox_enabled: true },
      relaxed: [],
    });
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "security" } });
    await flushUi();

    expect(document.body.textContent).toContain("Sandbox — relaxed by the install");
    expect(document.body.textContent).toContain("The per-turn sandbox is off.");
    expect(document.body.textContent).toContain("shell, file_write");

    const sandbox = document.querySelector('select[aria-label="Sandbox for this topic"]') as HTMLSelectElement;
    sandbox.value = "on";
    sandbox.dispatchEvent(new Event("change", { bubbles: true }));
    document.querySelector("form")?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();

    expect(settingsApi.saveTopicSecuritySettings).toHaveBeenCalledWith("topic-1", {
      sandbox_enabled: true,
      allow_degraded_execution: undefined,
      require_approval: undefined,
    });
    expect(document.body.textContent).toContain("Every shipped check is in force for this topic.");
  });

  it("saves unconfined fallback and reloads its persisted override", async () => {
    const reading: SecuritySettingsResponse = {
      available_tools: [], scope: { kind: "topic", topic_id: topic.id },
      effective: { sandbox_enabled: true, allow_degraded_execution: false, access_gate: "token", require_approval: [] },
      overrides: {}, relaxed: [],
    };
    vi.mocked(settingsApi.fetchTopicSecuritySettings).mockImplementation(async () => JSON.parse(JSON.stringify(reading)));
    vi.mocked(settingsApi.saveTopicSecuritySettings).mockImplementation(async (_id, overrides) => {
      reading.overrides = JSON.parse(JSON.stringify(overrides));
      reading.effective.allow_degraded_execution = overrides.allow_degraded_execution ?? false;
      return JSON.parse(JSON.stringify(reading));
    });
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "security" } });
    await flushUi();
    const confinement = document.querySelector<HTMLSelectElement>('[aria-label="Sandbox confinement for this topic"]')!;
    confinement.value = "allow_degraded";
    confinement.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi();
    document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(vi.mocked(settingsApi.saveTopicSecuritySettings)).toHaveBeenLastCalledWith(topic.id, {
      sandbox_enabled: undefined, allow_degraded_execution: true, require_approval: undefined,
    });
    unmount(component);
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "security" } });
    await flushUi();
    expect(document.querySelector<HTMLSelectElement>('[aria-label="Sandbox confinement for this topic"]')?.value).toBe("allow_degraded");
    expect(document.querySelector('[aria-label="Effective security checks"]')?.textContent).toContain("Allow unconfined fallback");
  });

  it("edits a topic approval timeout while preserving policy, and removes a tool", async () => {
    const reading: SecuritySettingsResponse = {
      available_tools: ["shell", "file_write"], scope: { kind: "topic" as const, topic_id: topic.id },
      effective: { sandbox_enabled: true, allow_degraded_execution: false, access_gate: "token" as const,
        require_approval: [{ tool: "shell", timeout: 12, timeout_policy: "interrupt" as const }] },
      overrides: { require_approval: [{ tool: "shell", timeout: 12, timeout_policy: "interrupt" }] }, relaxed: [],
    };
    vi.mocked(settingsApi.fetchTopicSecuritySettings).mockResolvedValue(reading);
    vi.mocked(settingsApi.saveTopicSecuritySettings).mockImplementation(async (_id, overrides) => {
      reading.overrides = JSON.parse(JSON.stringify(overrides));
      reading.effective = { ...reading.effective, ...reading.overrides };
      return JSON.parse(JSON.stringify(reading));
    });
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "security" } });
    await flushUi();
    const mode = document.querySelector<HTMLSelectElement>('[aria-label="Approval prompts for this topic"]')!;
    mode.value = "own"; mode.dispatchEvent(new Event("change", { bubbles: true })); await flushUi();
    expect(document.querySelector('[aria-label="Timeout for shell"]')).not.toBeNull();
    const timeout = document.querySelector<HTMLInputElement>('[aria-label="Timeout for shell"]')!;
    timeout.value = "27"; timeout.dispatchEvent(new Event("change", { bubbles: true })); await flushUi();
    document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(settingsApi.saveTopicSecuritySettings).toHaveBeenLastCalledWith(topic.id, {
      sandbox_enabled: undefined, allow_degraded_execution: undefined,
      require_approval: [{ tool: "shell", timeout: 27, timeout_policy: "interrupt" }],
    });
    const picker = document.querySelector<HTMLSelectElement>('[aria-label="Tool to require approval"]')!;
    picker.value = "file_write"; picker.dispatchEvent(new Event("change", { bubbles: true })); await flushUi();
    Array.from(document.querySelectorAll("button")).find(button => button.textContent?.trim() === "Add tool")!.click();
    await flushUi();
    document.querySelector<HTMLButtonElement>('[aria-label="Remove file_write approval"]')!.click(); await flushUi();
    document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(vi.mocked(settingsApi.saveTopicSecuritySettings).mock.calls.at(-1)![1].require_approval)
      .toEqual([{ tool: "shell", timeout: 27, timeout_policy: "interrupt" }]);
    unmount(component);
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "security" } });
    await flushUi();
    expect(document.querySelector<HTMLInputElement>('[aria-label="Timeout for shell"]')?.value).toBe("27");
    document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(vi.mocked(settingsApi.saveTopicSecuritySettings).mock.calls.at(-1)![1].require_approval)
      .toEqual([{ tool: "shell", timeout: 27, timeout_policy: "interrupt" }]);
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

  it("relinks a missing folder without detaching its attachment, including on refusal", async () => {
    const missing = {
      id: "workspace-1", topic_id: "topic-1", label: "Source", path: "/work/source",
      canonical_path: "/work/source", is_missing: true, permissions: ["read", "write"],
      is_trusted: true, is_default: true, created_at: "2026-05-26T10:00:00Z",
    };
    vi.mocked(topicsApi.listTopicWorkspaces).mockResolvedValue([missing]);
    vi.mocked(topicsApi.relinkTopicWorkspace).mockRejectedValueOnce(new Error("folder does not exist"))
      .mockResolvedValueOnce({ ...missing, path: "/work/moved", canonical_path: "/work/moved", is_missing: false });
    component = mount(TopicSettingsPage, { target: document.body, props: { topic, tab: "workspaces" } });
    await flushUi();
    const input = document.querySelector<HTMLInputElement>('input[aria-label="Replacement path for Source"]')!;
    input.value = "/work/absent";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    const relink = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.trim() === "Relink")!;
    relink.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flushUi();
    expect(topicsApi.relinkTopicWorkspace).toHaveBeenCalledWith("topic-1", "workspace-1", "/work/absent");
    expect(document.body.textContent).toContain("folder does not exist");
    expect(topicsApi.detachTopicWorkspace).not.toHaveBeenCalled();
    expect(topicsApi.attachTopicWorkspace).not.toHaveBeenCalled();

    input.value = "/work/moved";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    relink.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await flushUi();
    expect(topicsApi.relinkTopicWorkspace).toHaveBeenCalledWith("topic-1", "workspace-1", "/work/moved");
    expect(topicsApi.listTopicWorkspaces).toHaveBeenCalledTimes(2);
    expect(topicsApi.detachTopicWorkspace).not.toHaveBeenCalled();
  });

  it("designates a library surface as a topic default and saves the topic's tags", async () => {
    const standup = {
      id: "entry-1",
      name: "Standup",
      description: "Daily standup notes — use at the start of a working day.",
      tags: ["standup"],
      origin_topic_deleted: false,
      updated_at: "2026-09-15T10:00:00Z",
    };
    vi.mocked(surfacesApi.listLibrary).mockResolvedValue({ entries: [standup] });
    vi.mocked(surfacesApi.listTopicDefaultSurfaces).mockResolvedValue({ entries: [] });
    vi.mocked(surfacesApi.addTopicDefaultSurface).mockResolvedValue({ entries: [standup] });
    vi.mocked(topicsApi.updateTopic).mockResolvedValue({ ...topic, tags: ["ops", "daily"] });

    component = mount(TopicSettingsPage, {
      target: document.body,
      props: { topic, tab: "surfaces" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("No default surfaces are designated.");
    const libraryRow = document.querySelector('[aria-label="Surface library"] .record-row');
    expect(libraryRow?.textContent).toContain("use at the start of a working day");
    (libraryRow?.querySelector("button") as HTMLButtonElement).click();
    await flushUi();

    expect(surfacesApi.addTopicDefaultSurface).toHaveBeenCalledWith("topic-1", "entry-1");
    expect(document.querySelector('[aria-label="Default surfaces"]')?.textContent).toContain("Standup");
    expect(document.querySelector('[aria-label="Surface library"]')?.textContent).not.toContain("Make default");

    const tags = document.querySelector<HTMLInputElement>('input[aria-label="Topic tags"]');
    if (!tags) throw new Error("tags input missing");
    tags.value = "ops, daily";
    tags.dispatchEvent(new Event("input", { bubbles: true }));
    document.querySelector("form.tags-form")?.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();

    expect(topicsApi.updateTopic).toHaveBeenCalledWith("topic-1", { tags: ["ops", "daily"] });
    expect(document.body.textContent).toContain("Saved.");
  });
});
