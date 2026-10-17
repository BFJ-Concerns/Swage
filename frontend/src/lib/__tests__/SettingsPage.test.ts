// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";
import { fromStore, writable } from "svelte/store";

import { parseRoute } from "../router";
import SettingsPage from "../components/SettingsPage.svelte";
import type { SecuritySettingsResponse } from "../protocol/SecuritySettingsResponse";
import type { Schedule } from "../protocol/Schedule";
import type { SettingsPayload, SettingsResponse } from "../types";

import * as agentDefinitionsApi from "../api/agent-definitions";
import * as settingsApi from "../api/settings";
import * as skillsApi from "../api/skills";
import * as workflowApi from "../api/workflows";
import * as operatorApi from "../api/operator";
import { makeSchedule } from "./fixtures/schedules";

vi.mock("../api/settings", () => ({
  fetchInstallSecuritySettings: vi.fn(),
  saveInstallSecuritySettings: vi.fn(),
  discoverModels: vi.fn(),
  fetchSettings: vi.fn(),
  restartGateway: vi.fn(),
  saveSettings: vi.fn(),
}));

vi.mock("../api/agent-definitions", () => ({
  listEffectiveAgentDefinitions: vi.fn(),
}));

vi.mock("../api/skills", () => ({
  listSkills: vi.fn(),
}));

vi.mock("../api/workflows", () => ({
  createSchedule: vi.fn(),
  downloadRecoveryExport: vi.fn(),
  exportRecoveryBundle: vi.fn(),
  listRecoveryExports: vi.fn(),
}));

vi.mock("../api/operator", () => ({
  listSchedules: vi.fn(),
  fetchScheduleRuns: vi.fn(),
  triggerSchedule: vi.fn(),
  pauseSchedule: vi.fn(),
  resumeSchedule: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

async function waitForSaveCall(): Promise<void> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    await flushUi();
    if (vi.mocked(settingsApi.saveSettings).mock.calls.length > 0) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error("Timed out waiting for settings autosave.");
}

function buttonNamed(name: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll("button")).find(
    (candidate) => candidate.textContent?.trim() === name,
  );
  if (!button) throw new Error(`Button not found: ${name}\n${document.body.textContent ?? ""}`);
  return button as HTMLButtonElement;
}

function buttonContaining(text: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll("button")).find((candidate) =>
    candidate.textContent?.includes(text),
  );
  if (!button) {
    throw new Error(`Button not found containing: ${text}\n${document.body.textContent ?? ""}`);
  }
  return button as HTMLButtonElement;
}

function clickDiscoveredModel(name: string): void {
  const button = Array.from(document.querySelectorAll("button")).find(
    (candidate) => candidate.title === `Add ${name}`,
  );
  if (!button) {
    throw new Error(`Discovered model button not found: ${name}\n${document.body.textContent ?? ""}`);
  }
  (button as HTMLButtonElement).click();
}

function emptySettings(): SettingsResponse {
  return {
    providers: {},
    default_provider: "",
    models: {},
    default_model: "",
    generation_presets: {},
    tool_runtime: {
      shell_timeout_secs: 30,
      python_execute: {
        enabled: true,
        timeout_secs: 30,
        startup_timeout_secs: 10,
        idle_ttl_secs: 60,
        warm_target: 1,
        max_workers: 1,
        runtime_policy: "bundled_then_system",
        runtime_path: null,
        require_sandbox: true,
      },
      max_output_bytes: 100_000,
      plugin_directory: null,
      search: null,
    },
    tool_permissions: {
      allow: ["*"],
      deny: [],
      bundles: {},
      require_approval: ["shell", "file_edit", "file_write", "python_execute"],
    },
    hitl: {
      timeout: 300,
      timeout_policy: "reject",
    },
    sandbox: {
      enabled: true,
      allow_degraded_execution: false,
    },
    context: {
      assembly: "default",
      timeout: 5,
      budget: null,
    },
    pricing: {
      models: {},
    },
    observability: {
      force_on: false,
    },
    communication: {
      context_transfer: {
        mode: "none",
        allowed_fields: [],
        summary_budget: null,
        targets: {},
      },
      message_ttl_secs: 86_400,
      expiry_interval_secs: 60,
    },
    sessions: { archive_after_days: 7 },
    memory: { curator_idle_minutes: 10 },
    voice: {},
    restart_required: false,
    can_restart: false,
  };
}

function createdSchedule(topicId: string): Schedule {
  return makeSchedule({
    session: { policy: "fresh", topic_id: topicId, agent_id: "gateway" },
    next_run_at: "2026-08-21T10:00:00Z",
    created_at: "2026-08-20T10:00:00Z",
    updated_at: "2026-08-20T10:00:00Z",
  });
}

describe("SettingsPage", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    window.localStorage.clear();
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(emptySettings());
    vi.mocked(settingsApi.saveSettings).mockImplementation(async (payload: SettingsPayload) => ({
      providers: payload.providers,
      default_provider: payload.default_provider,
      models: payload.models,
      default_model: payload.default_model,
      generation_presets: payload.generation_presets,
      tool_runtime: payload.tool_runtime ?? emptySettings().tool_runtime,
      tool_permissions: { ...emptySettings().tool_permissions, ...payload.tool_permissions },
      hitl: payload.hitl ?? emptySettings().hitl,
      sandbox: emptySettings().sandbox,
      context: payload.context ?? emptySettings().context,
      pricing: payload.pricing ?? emptySettings().pricing,
      observability: payload.observability ?? emptySettings().observability,
      communication: payload.communication ?? emptySettings().communication,
      sessions: payload.sessions ?? emptySettings().sessions,
      memory: payload.memory ?? emptySettings().memory,
      voice: payload.voice ?? emptySettings().voice,
      restart_required: true,
      can_restart: false,
    }));
    vi.mocked(settingsApi.discoverModels).mockResolvedValue({
      provider_id: "openai",
      models: [{ id: "gpt-4.1-mini", name: "gpt-4.1-mini", context_window: null }],
    });
    vi.mocked(agentDefinitionsApi.listEffectiveAgentDefinitions).mockResolvedValue([]);
    vi.mocked(skillsApi.listSkills).mockResolvedValue([]);
    vi.mocked(workflowApi.listRecoveryExports).mockResolvedValue({ exports: [] });
    vi.mocked(workflowApi.exportRecoveryBundle).mockResolvedValue({});
    vi.mocked(workflowApi.downloadRecoveryExport).mockResolvedValue({});
    vi.mocked(operatorApi.listSchedules).mockResolvedValue({ schedules: [] });
  });

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.innerHTML = "";
    vi.clearAllMocks();
  });

  it("omits security fields from an Automation autosave holding an old snapshot", async () => {
    const old = emptySettings();
    old.sandbox.enabled = false;
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(old);
    component = mount(SettingsPage, { target: document.body, props: { tab: "automation" } });
    await flushUi();
    const newer = { available_tools: [], scope: { kind: "install" as const },
      effective: { sandbox_enabled: true, allow_degraded_execution: true, access_gate: "token" as const, require_approval: ["shell"] },
      overrides: { sandbox_enabled: true, allow_degraded_execution: true, require_approval: ["shell"] }, relaxed: [] };
    vi.mocked(settingsApi.saveInstallSecuritySettings).mockResolvedValue(newer);
    await settingsApi.saveInstallSecuritySettings(newer.overrides);
    const editor = document.querySelector<HTMLTextAreaElement>('[aria-label="Tool bundles"]')!;
    editor.value = '{"inspect":["file_read"]}';
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    const body = vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0];
    expect(body).not.toHaveProperty("sandbox");
    expect(body.tool_permissions).not.toHaveProperty("require_approval");
    expect(body.tool_permissions?.bundles).toEqual({ inspect: ["file_read"] });
  });

  it("saves unconfined fallback and reloads its persisted override", async () => {
    const reading: SecuritySettingsResponse = {
      available_tools: [], scope: { kind: "install" },
      effective: { sandbox_enabled: true, allow_degraded_execution: false, access_gate: "token", require_approval: [] },
      overrides: {}, relaxed: [],
    };
    vi.mocked(settingsApi.fetchInstallSecuritySettings).mockImplementation(async () => JSON.parse(JSON.stringify(reading)));
    vi.mocked(settingsApi.saveInstallSecuritySettings).mockImplementation(async (overrides) => {
      reading.overrides = JSON.parse(JSON.stringify(overrides));
      reading.effective.allow_degraded_execution = overrides.allow_degraded_execution ?? false;
      return JSON.parse(JSON.stringify(reading));
    });
    component = mount(SettingsPage, { target: document.body, props: { tab: "security" } });
    await flushUi();
    const confinement = document.querySelector<HTMLSelectElement>('[aria-label="Sandbox confinement for the install"]')!;
    confinement.value = "allow_degraded";
    confinement.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi();
    document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    await flushUi();
    expect(vi.mocked(settingsApi.saveInstallSecuritySettings)).toHaveBeenLastCalledWith({
      sandbox_enabled: undefined, allow_degraded_execution: true, require_approval: undefined, access_gate: undefined,
    });
    unmount(component);
    component = mount(SettingsPage, { target: document.body, props: { tab: "security" } });
    await flushUi();
    expect(document.querySelector<HTMLSelectElement>('[aria-label="Sandbox confinement for the install"]')?.value).toBe("allow_degraded");
    expect(document.querySelector('[aria-label="Effective security checks"]')?.textContent).toContain("Allow unconfined fallback");
  });

  it("edits install security through its own reading and saves picker timeouts and removals", async () => {
    expect(parseRoute("/settings/security").settingsTab).toBe("security");
    const reading: SecuritySettingsResponse = {
      available_tools: ["shell", "file_write"], scope: { kind: "install" as const },
      effective: { sandbox_enabled: true, allow_degraded_execution: false, access_gate: "token" as const, require_approval: [] },
      overrides: {}, relaxed: [],
    };
    vi.mocked(settingsApi.fetchInstallSecuritySettings).mockResolvedValue(reading);
    vi.mocked(settingsApi.saveInstallSecuritySettings).mockImplementation(async (overrides) => {
      reading.overrides = JSON.parse(JSON.stringify(overrides));
      reading.effective = { ...reading.effective, ...reading.overrides };
      return JSON.parse(JSON.stringify(reading));
    });
    component = mount(SettingsPage, { target: document.body, props: { tab: "security" } });
    await flushUi();
    expect(document.querySelector('[aria-label="Sandbox for the install"]')).not.toBeNull();
    expect(document.body.textContent).not.toContain("require a gateway restart");
    const choose = (label: string, value: string) => {
      const select = document.querySelector<HTMLSelectElement>(`[aria-label="${label}"]`)!;
      select.value = value; select.dispatchEvent(new Event("change", { bubbles: true }));
    };
    choose("Sandbox for the install", "off");
    choose("Access gate for the install", "warn");
    choose("Approval prompts for the install", "own");
    await flushUi();
    choose("Tool to require approval", "shell");
    await flushUi(); buttonNamed("Add tool").click(); await flushUi();
    const timeout = document.querySelector<HTMLInputElement>('[aria-label="Timeout for shell"]')!;
    timeout.value = "42"; timeout.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi(); buttonNamed("Save security settings").click(); await flushUi();
    expect(settingsApi.saveInstallSecuritySettings).toHaveBeenLastCalledWith({
      sandbox_enabled: false, allow_degraded_execution: undefined, access_gate: "warn",
      require_approval: [{ tool: "shell", timeout: 42, timeout_policy: null }],
    });
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    choose("Approval prompts for the install", "own"); await flushUi();
    choose("Tool to require approval", "file_write"); await flushUi();
    buttonNamed("Add tool").click(); await flushUi();
    document.querySelector<HTMLButtonElement>('[aria-label="Remove file_write approval"]')!.click();
    await flushUi(); buttonNamed("Save security settings").click(); await flushUi();
    expect(vi.mocked(settingsApi.saveInstallSecuritySettings).mock.calls.at(-1)![0].require_approval)
      .toEqual([{ tool: "shell", timeout: 42, timeout_policy: null }]);
    unmount(component);
    component = mount(SettingsPage, { target: document.body, props: { tab: "security" } });
    await flushUi();
    expect(document.querySelector<HTMLInputElement>('[aria-label="Timeout for shell"]')?.value).toBe("42");
    expect(document.querySelector('[aria-label="Remove file_write approval"]')).toBeNull();
  });

  it("refuses invalid provider URL drafts before autosave and accepts corrections or clearing", async () => {
    const originalUrl = "https://api.example.org/v1";
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue({
      ...emptySettings(),
      providers: { openai: {
        protocol: "openai", base_url: originalUrl, api_key_secret_ref: null,
        api_key_env: null, has_credential: false,
      } },
      default_provider: "openai",
    });
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } });
    await flushUi();
    const input = document.querySelector<HTMLInputElement>('input[placeholder="Provider default"]')!;
    const edit = async (value: string) => {
      input.value = value;
      input.dispatchEvent(new Event("input", { bubbles: true }));
      await flushUi();
    };
    await edit("not-a-url");
    await new Promise((resolve) => setTimeout(resolve, 850));
    expect(document.body.textContent).toContain("Base URL must start with http:// or https://");
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    expect(document.querySelector('.meta-endpoint')?.textContent).toBe(originalUrl);
    input.dispatchEvent(new Event("blur"));
    await flushUi();
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();

    await edit(" HTTP://localhost:8317/v1 ");
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers.openai.base_url).toBe("HTTP://localhost:8317/v1");
    expect(document.body.textContent).not.toContain("Base URL must start with http:// or https://");
    vi.mocked(settingsApi.saveSettings).mockClear();
    await edit("https://replacement.example.org/v1");
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers.openai.base_url).toBe("https://replacement.example.org/v1");
    vi.mocked(settingsApi.saveSettings).mockClear();
    await edit("   ");
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers.openai.base_url).toBeNull();
    expect(document.body.textContent).not.toContain("Base URL must start with http:// or https://");

    vi.mocked(settingsApi.saveSettings).mockClear();
    await edit("https://pending.example.org/v1");
    await edit("ftp://invalid.example.org");
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers.openai.base_url).toBe("https://pending.example.org/v1");
    expect(input.value).toBe("ftp://invalid.example.org");
    expect(document.body.textContent).toContain("Base URL must start with http:// or https://");

    buttonContaining("Add provider").click();
    await flushUi();
    buttonContaining("OpenAI compatible").click();
    await flushUi();
    const connectInput = document.querySelector<HTMLInputElement>('#compatible-base-url')!;
    connectInput.value = "not-a-url";
    connectInput.dispatchEvent(new Event("input", { bubbles: true }));
    buttonContaining("Connect").click();
    await flushUi();
    expect(document.querySelector('#compatible-base-url-error')?.textContent).toBe("Base URL must start with http:// or https://");
    expect(document.querySelectorAll('.provider-section')).toHaveLength(1);
    vi.mocked(settingsApi.saveSettings).mockClear();
    connectInput.value = " HTTPS://proxy.example.org/v1 ";
    connectInput.dispatchEvent(new Event("input", { bubbles: true }));
    buttonContaining("Connect").click();
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers["openai-compatible"].base_url).toBe("HTTPS://proxy.example.org/v1");
  }, 10_000);

  it("clears Tool bundles save status until the next successful save", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    const field = Array.from(document.querySelectorAll(".field")).find(
      (candidate) => candidate.querySelector(".key")?.textContent?.startsWith("Tool bundles"),
    )!;
    const editor = field.querySelector<HTMLTextAreaElement>("textarea")!;
    editor.value = '{"reading":["file_read"]}';
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    await flushUi();
    expect(document.body.textContent).toContain("Saved automatically");
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].tool_permissions?.bundles).toEqual({ reading: ["file_read"] });
    vi.mocked(settingsApi.saveSettings).mockClear();

    editor.value = "{";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    expect(document.body.textContent).not.toContain("Saved automatically");
    await new Promise((resolve) => setTimeout(resolve, 850));
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();

    editor.value = '{"reading":["file_read"]}';
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    expect(document.body.textContent).not.toContain("Saved automatically");
    expect(field.querySelector('[role="alert"]')).toBeNull();
    editor.value = '{"reading":["file_read","file_search"]}';
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    await flushUi();
    expect(document.body.textContent).toContain("Saved automatically");
  });

  it("distinguishes Tool bundles parse errors from shape errors beside the editor", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    const field = Array.from(document.querySelectorAll(".field")).find(
      (candidate) => candidate.querySelector(".key")?.textContent?.startsWith("Tool bundles"),
    )!;
    const editor = field.querySelector<HTMLTextAreaElement>("textarea")!;
    editor.value = '{"reading":["file_read"]}';
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    await flushUi();
    vi.mocked(settingsApi.saveSettings).mockClear();

    editor.value = "{";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    expect(field.querySelector('[role="alert"]')?.textContent ?? "").toContain("Tool bundles: invalid JSON at line 1, column 2");
    expect(editor.getAttribute("aria-invalid")).toBe("true");
    expect(editor.getAttribute("aria-describedby")).toBe(field.querySelector('[role="alert"]')?.id);
    for (const invalidShape of ['{"a":1}', "null"]) {
      editor.value = invalidShape;
      editor.dispatchEvent(new Event("input", { bubbles: true }));
      await flushUi();
      expect(field.querySelector('[role="alert"]')?.textContent ?? "").toBe("Tool bundles: Tool bundles must be a JSON object of string arrays.");
      expect(editor.getAttribute("aria-invalid")).toBe("true");
    }
    await new Promise((resolve) => setTimeout(resolve, 850));
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
  });

  it("remounts Tool bundles with the last valid object after invalid input", async () => {
    let stored = emptySettings();
    const save = vi.mocked(settingsApi.saveSettings).getMockImplementation()!;
    vi.mocked(settingsApi.fetchSettings).mockImplementation(async () => structuredClone(stored));
    vi.mocked(settingsApi.saveSettings).mockImplementation(async (payload) => {
      stored = JSON.parse(JSON.stringify(await save(payload)));
      return structuredClone(stored);
    });
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    const editor = document.querySelector<HTMLTextAreaElement>('[aria-label="Tool bundles"]')!;
    editor.value = '{"reading":["file_read"]}';
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    await flushUi();
    editor.value = "{";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(1);

    unmount(component);
    component = undefined;
    document.body.innerHTML = "";
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    expect(settingsApi.fetchSettings).toHaveBeenCalledTimes(2);
    const restored = document.querySelector<HTMLTextAreaElement>('[aria-label="Tool bundles"]')!;
    expect(JSON.parse(restored.value)).toEqual({ reading: ["file_read"] });
    expect(restored.getAttribute("aria-invalid")).toBeNull();
    expect(document.querySelector('[role="alert"]')).toBeNull();
  });

  it("keeps Tool bundles status clear when an earlier save finishes during invalid input", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    let finishSave!: () => void;
    const save = vi.mocked(settingsApi.saveSettings).getMockImplementation()!;
    vi.mocked(settingsApi.saveSettings).mockImplementationOnce(async (payload) => {
      await new Promise<void>((resolve) => { finishSave = resolve; });
      return save(payload);
    });
    const field = Array.from(document.querySelectorAll(".field")).find(
      (candidate) => candidate.querySelector(".key")?.textContent?.startsWith("Tool bundles"),
    )!;
    const editor = field.querySelector<HTMLTextAreaElement>("textarea")!;
    editor.value = '{"reading":["file_read"]}';
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    editor.value = "{";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    finishSave();
    await flushUi();
    expect(document.body.textContent).not.toContain("Saved automatically");
    expect(field.querySelector('[role="alert"]')?.textContent ?? "").toContain("Tool bundles: invalid JSON");
    expect(editor.value).toBe("{");
  });

  it("waits for the required search fields before autosaving", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    document.querySelector<HTMLInputElement>('[aria-label="Enable web search"]')!.click();
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings, "enabling search must not save an empty endpoint").not.toHaveBeenCalled();
    expect(document.querySelector('[role="alert"]')).toBeNull();

    const url = document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')!;
    expect(url.required).toBe(true);
    url.value = "   ";
    url.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    expect(document.querySelector('[role="alert"]')).toBeNull();

    url.value = "http://127.0.0.1:35123";
    url.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].tool_runtime?.search).toEqual({
      provider_type: "searxng", base_url: "http://127.0.0.1:35123",
    });
    expect(document.querySelector('[role="alert"]')).toBeNull();
  });

  it("saves other settings while retaining an incomplete search draft", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    document.querySelector<HTMLInputElement>('[aria-label="Enable web search"]')!.click();
    const delay = document.querySelector<HTMLInputElement>('[aria-label="Archive sessions after days"]')!;
    delay.value = "8";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings, "an incomplete search draft must not block other settings").toHaveBeenCalled();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].sessions?.archive_after_days).toBe(8);
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].tool_runtime?.search).toBeNull();
    expect(document.querySelector<HTMLInputElement>('[aria-label="Enable web search"]')!.checked).toBe(true);
    expect(document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')!.value).toBe("");
    expect(document.querySelector('[role="alert"]')).toBeNull();

    vi.mocked(settingsApi.saveSettings).mockClear();
    const url = document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')!;
    url.value = "http://127.0.0.1:35123";
    url.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].tool_runtime?.search).toEqual({
      provider_type: "searxng", base_url: "http://127.0.0.1:35123",
    });

    vi.mocked(settingsApi.saveSettings).mockClear();
    document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')!.value = "";
    document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')!.dispatchEvent(new Event("input", { bubbles: true }));
    delay.value = "9";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings).toHaveBeenCalled();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].sessions?.archive_after_days).toBe(9);
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].tool_runtime?.search).toEqual({
      provider_type: "searxng", base_url: "http://127.0.0.1:35123",
    });
    expect(document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')!.value).toBe("");
    expect(document.querySelector('[role="alert"]')).toBeNull();
  });

  it("edits and disables web search through Settings", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    const enable = document.querySelector<HTMLInputElement>('[aria-label="Enable web search"]');
    expect(enable, "Settings must offer web search configuration").not.toBeNull();
    enable!.click();
    await flushUi();
    const type = document.querySelector<HTMLInputElement>('[aria-label="Search provider type"]')!;
    const url = document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')!;
    expect(type.value).toBe("searxng");
    vi.mocked(settingsApi.saveSettings).mockRejectedValueOnce(new Error("unsupported search provider 'brave'; expected one of: searxng"));
    type.value = "brave";
    type.dispatchEvent(new Event("input", { bubbles: true }));
    url.value = "http://127.0.0.1:35123";
    url.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].tool_runtime?.search).toEqual({
      provider_type: "brave", base_url: "http://127.0.0.1:35123",
    });
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("expected one of: searxng");
    vi.mocked(settingsApi.saveSettings).mockClear();
    type.value = "searxng";
    type.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].tool_runtime?.search).toEqual({
      provider_type: "searxng", base_url: "http://127.0.0.1:35123",
    });
    vi.mocked(settingsApi.saveSettings).mockClear();
    document.querySelector<HTMLInputElement>('[aria-label="Enable web search"]')!.click();
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].tool_runtime?.search).toBeNull();
  });

  it("discovers models for a newly-added provider without saving first", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "providers" },
    });
    await flushUi();

    buttonContaining("Add provider").click();
    await flushUi();
    buttonContaining("OpenAI").click();
    await flushUi();
    buttonNamed("Refresh").click();
    await flushUi();

    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    expect(settingsApi.discoverModels).toHaveBeenCalledWith("openai", "", {
      protocol: "openai",
      base_url: null,
      api_key_secret_ref: null,
      api_key_env: null,
    });
    expect(document.body.textContent).not.toContain(
      "Save settings before discovering models for a new provider.",
    );
    expect(document.body.textContent).toContain("gpt-4.1-mini");
  });

  it("adds OpenRouter with its canonical base URL", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "providers" },
    });
    await flushUi();

    buttonContaining("Add provider").click();
    await flushUi();
    buttonContaining("OpenRouter").click();
    await flushUi();
    await waitForSaveCall();

    expect(settingsApi.saveSettings).toHaveBeenCalledWith(
      expect.objectContaining({
        providers: expect.objectContaining({
          openrouter: expect.objectContaining({
            protocol: "openrouter",
            base_url: "https://openrouter.ai/api/v1",
          }),
        }),
      }),
    );
  });

  it("lists, triggers, and downloads recovery exports", async () => {
    const createObjectUrl = vi.fn(() => "blob:swage-recovery");
    const revokeObjectUrl = vi.fn();
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      value: createObjectUrl,
    });
    Object.defineProperty(URL, "revokeObjectURL", {
      configurable: true,
      value: revokeObjectUrl,
    });
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => undefined);

    vi.mocked(workflowApi.listRecoveryExports)
      .mockResolvedValueOnce({
        exports: [
          {
            id: "export-1",
            created_at: "2026-05-26T10:00:00Z",
            size_bytes: 2048,
            topics: 1,
            workspaces: 2,
            sessions: 3,
            secrets: 1,
            channel_links: 1,
            devices: 1,
            schedules: 1,
          },
        ],
      })
      .mockResolvedValueOnce({
        exports: [
          {
            id: "export-2",
            created_at: "2026-05-26T10:05:00Z",
            size_bytes: 512,
            topics: 1,
            workspaces: 0,
            sessions: 0,
            secrets: 0,
            channel_links: 0,
            devices: 0,
            schedules: 0,
          },
        ],
      });
    vi.mocked(workflowApi.downloadRecoveryExport).mockResolvedValue({ version: 1 });

    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "recovery" },
    });
    await flushUi();

    expect(workflowApi.listRecoveryExports).toHaveBeenCalled();
    expect(document.body.textContent).toContain("sessions: 3");
    expect(document.body.textContent).toContain("2.0 KiB");

    buttonNamed("Export now").click();
    await flushUi();

    expect(workflowApi.exportRecoveryBundle).toHaveBeenCalled();
    expect(document.body.textContent).toContain("512 B");

    buttonNamed("Download").click();
    await flushUi();

    expect(workflowApi.downloadRecoveryExport).toHaveBeenCalledWith("export-2");
    expect(createObjectUrl).toHaveBeenCalled();
    expect(revokeObjectUrl).toHaveBeenCalledWith("blob:swage-recovery");
  });

  it("makes the first discovered model the default before saving", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "providers" },
    });
    await flushUi();

    buttonContaining("Add provider").click();
    await flushUi();
    buttonContaining("OpenAI").click();
    await flushUi();
    buttonNamed("Refresh").click();
    await flushUi();
    clickDiscoveredModel("gpt-4.1-mini");
    await flushUi();
    await waitForSaveCall();

    expect(settingsApi.saveSettings).toHaveBeenCalledWith(
      expect.objectContaining({
        default_model: "openai:gpt-4.1-mini",
        default_provider: "openai",
      }),
    );
  });

  it("collapses provider sections", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue({
      ...emptySettings(),
      providers: {
        openai: {
          protocol: "openai",
          base_url: null,
          api_key_secret_ref: null,
          api_key_env: null,
          has_credential: false,
          api_key: "",
        },
      },
      default_provider: "openai",
    });
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "providers" },
    });
    await flushUi();

    expect(document.body.textContent).toContain("Base URL");

    // The whole provider header bar is the collapse trigger (UI audit f-22).
    const collapse = document.querySelector<HTMLButtonElement>(".provider-collapse");
    if (!collapse) throw new Error("Provider collapse trigger not found");
    expect(collapse.getAttribute("aria-expanded")).toBe("true");
    collapse.click();
    await flushUi();

    expect(document.body.textContent).not.toContain("Base URL");
    expect(collapse.getAttribute("aria-expanded")).toBe("false");
  });

  it("preserves a manual model draft while its provider is collapsed", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue({
      ...emptySettings(),
      providers: {
        openai: {
          protocol: "openai",
          base_url: null,
          api_key_secret_ref: null,
          api_key_env: null,
          has_credential: false,
          api_key: "",
        },
      },
      default_provider: "openai",
    });
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "providers" },
    });
    await flushUi();

    const draft = document.querySelector<HTMLInputElement>('input[aria-label="Add model by id for OpenAI"]');
    const collapse = document.querySelector<HTMLButtonElement>(".provider-collapse");
    if (!draft || !collapse) throw new Error("Provider model draft controls not found");
    draft.value = "half-typed-model";
    draft.dispatchEvent(new Event("input", { bubbles: true }));
    collapse.click();
    await flushUi();
    collapse.click();
    await flushUi();

    expect(document.querySelector<HTMLInputElement>('input[aria-label="Add model by id for OpenAI"]')?.value).toBe(
      "half-typed-model",
    );
  });

  it("persists display preferences locally", async () => {
    const preferenceEvents: Event[] = [];
    const onPreferenceChange = (event: Event) => {
      preferenceEvents.push(event);
    };
    window.addEventListener("swage:display-preferences-changed", onPreferenceChange);

    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "display" },
    });
    await flushUi();

    buttonNamed("comfortable").click();
    await flushUi();
    buttonNamed("plain").click();
    await flushUi();

    const stored = JSON.parse(window.localStorage.getItem("swage:display-preferences") ?? "{}");
    expect(stored.density).toBe("comfortable");
    expect(stored.conversation_rendering).toBe("plain");
    expect(document.body.textContent).toContain("Display preferences saved.");
    expect(preferenceEvents).toHaveLength(2);
    window.removeEventListener("swage:display-preferences-changed", onPreferenceChange);
  });

  it("lists effective agent definitions for the selected topic", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "agents", topicId: "selected-topic" },
    });
    await flushUi();

    expect(agentDefinitionsApi.listEffectiveAgentDefinitions).toHaveBeenCalledWith("selected-topic");
  });

  it("loads settings alongside the command catalogue so later mutable tabs are ready", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "commands", topicId: "selected-topic" },
    });
    await flushUi();

    expect(skillsApi.listSkills).toHaveBeenCalledWith("selected-topic");
    expect(settingsApi.fetchSettings).toHaveBeenCalledTimes(1);
    expect(document.body.textContent).toContain("Commands");
  });

  it("describes live schedule activation without a restart", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();

    const subtitle = document.querySelector(".head .sub")?.textContent;
    expect(subtitle).toContain("Schedules become active when created, without a gateway restart.");
    expect(subtitle).not.toContain("at startup");
  });

  it("places the autosave indication with settings rather than schedule creation", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();

    expect(document.querySelector(".settings-pane > .head .autosave-state")).toBeNull();
    expect(document.querySelector(".schedule-automation")?.textContent).not.toContain("Autosave");
    const settings = document.querySelector('[aria-label="Automation settings"]');
    expect(settings?.textContent).toContain("Autosave");
    expect(settings?.querySelector('[aria-label="Archive sessions after days"]')).not.toBeNull();
    expect(settings?.querySelector(".schedule-automation")).toBeNull();
  });

  it("explains that Create schedule saves and activates the schedule", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();

    expect(document.querySelector(".create-card")?.textContent).toContain(
      "Create schedule saves the schedule and activates it at once.",
    );
  });

  it("creates a fresh schedule in the selected topic", async () => {
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(createdSchedule("selected-topic"));
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: "selected-topic" },
    });
    await flushUi();

    const prompt = document.querySelector<HTMLTextAreaElement>(".schedule-automation textarea");
    if (!prompt) throw new Error("Schedule prompt was not rendered");
    prompt.value = "Review the queue.";
    prompt.dispatchEvent(new Event("input", { bubbles: true }));
    buttonNamed("Create schedule").click();
    await flushUi();

    expect(workflowApi.createSchedule).toHaveBeenCalledWith({
      message: "Review the queue.",
      spec: { kind: "interval", every: "every 1 hours" },
      session: { policy: "fresh", topic_id: "selected-topic", agent_id: "gateway" },
      routing: null,
      gate: null,
    });
    expect(document.body.textContent).not.toContain("Select a topic before scheduling work.");
  });

  it("saves the session archive delay from the automation tab", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();

    const delay = document.querySelector<HTMLInputElement>(
      'input[aria-label="Archive sessions after days"]',
    );
    if (!delay) throw new Error("Archive delay input was not rendered");
    expect(delay.value).toBe("7");
    delay.value = "14";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();

    const [payload] = vi.mocked(settingsApi.saveSettings).mock.calls[0];
    expect(payload.sessions).toEqual({ archive_after_days: 14 });
  });

  it("explains the archive delay minimum before editing", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();

    const delay = document.querySelector<HTMLInputElement>(
      'input[aria-label="Archive sessions after days"]',
    )!;
    const hint = document.getElementById(delay.getAttribute("aria-describedby") ?? "");
    expect(hint?.textContent ?? "").toContain("Minimum: 1 day");
  });

  it.each([0, -1])("refuses archive delay %s without changing the saved value", async (days) => {
    // Only the save API can change this persisted snapshot; mounting or
    // reloading reads it without replacing it with the working draft.
    let persisted = structuredClone(emptySettings());
    const save = vi.mocked(settingsApi.saveSettings).getMockImplementation()!;
    vi.mocked(settingsApi.fetchSettings).mockImplementation(async () => structuredClone(persisted));
    vi.mocked(settingsApi.saveSettings).mockImplementation(async (payload) => {
      persisted = structuredClone(await save(payload));
      return structuredClone(persisted);
    });
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();

    const delay = document.querySelector<HTMLInputElement>(
      'input[aria-label="Archive sessions after days"]',
    )!;
    delay.value = String(days);
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    // No blur: input alone triggers the real autosave timer.
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();

    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    expect(delay.value).toBe(String(days));
    expect(delay.getAttribute("aria-invalid")).toBe("true");
    expect(delay.closest(".field")?.querySelector('[role="alert"]')?.textContent)
      .toContain("Archive delay must be at least 1 day");
    expect(document.body.textContent).not.toContain("Saved automatically");
    expect(persisted.sessions.archive_after_days).toBe(7);

    await unmount(component);
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    expect(document.querySelector<HTMLInputElement>(
      'input[aria-label="Archive sessions after days"]',
    )?.value).toBe("7");
  });

  it("announces the saved archive delay after correcting a refused draft", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();

    const delay = document.querySelector<HTMLInputElement>(
      'input[aria-label="Archive sessions after days"]',
    )!;
    delay.value = "0";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    vi.mocked(settingsApi.saveSettings).mockClear();

    delay.value = "14";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    await flushUi();

    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(1);
    expect(vi.mocked(settingsApi.saveSettings).mock.calls[0][0].sessions)
      .toEqual({ archive_after_days: 14 });
    expect(delay.closest(".field")?.querySelector('[role="alert"]')).toBeNull();
    expect(document.body.textContent).toContain("Archive delay saved as 14 days.");

    vi.mocked(settingsApi.saveSettings).mockClear();
    delay.value = "1";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    await flushUi();
    expect(document.body.textContent).toContain("Archive delay saved as 1 day.");
  });

  it("explains why voice autosave is blocked by an invalid archive draft", async () => {
    const tab = writable<"automation" | "voice">("automation");
    const currentTab = fromStore(tab);
    component = mount(SettingsPage, {
      target: document.body,
      props: { get tab() { return currentTab.current; }, topicId: null },
    });
    await flushUi();

    const delay = document.querySelector<HTMLInputElement>(
      'input[aria-label="Archive sessions after days"]',
    )!;
    delay.value = "0";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();

    tab.set("voice");
    await flushUi();
    expect(document.querySelector('input[aria-label="Archive sessions after days"]')).toBeNull();
    const transcription = document.querySelector<HTMLInputElement>(
      'input[aria-label="Transcription model"]',
    )!;
    transcription.value = "whisper-1";
    transcription.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();

    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    expect(transcription.value).toBe("whisper-1");
    expect(document.querySelector('[role="alert"]')?.textContent ?? "")
      .toContain("Fix the archive delay in Automation before saving.");

    tab.set("automation");
    await flushUi();
    const correction = document.querySelector<HTMLInputElement>(
      'input[aria-label="Archive sessions after days"]',
    )!;
    correction.value = "14";
    correction.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    await flushUi();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls[0][0].voice)
      .toEqual({ transcription_model: "whisper-1" });
    expect(document.querySelector('[role="alert"]')).toBeNull();
  });

  it("saves the voice model routes from the voice tab", async () => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "voice", topicId: null },
    });
    await flushUi();

    expect(document.body.textContent).toContain("OpenAI protocol");
    expect(document.body.textContent).not.toContain("Non-functional");
    const transcription = document.querySelector<HTMLInputElement>(
      'input[aria-label="Transcription model"]',
    );
    const speech = document.querySelector<HTMLInputElement>('input[aria-label="Speech model"]');
    if (!transcription || !speech) throw new Error("Voice model inputs were not rendered");
    expect(transcription.value).toBe("");
    transcription.value = " whisper-1 ";
    transcription.dispatchEvent(new Event("input", { bubbles: true }));
    speech.value = "tts-1";
    speech.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();

    const [payload] = vi.mocked(settingsApi.saveSettings).mock.calls[0];
    expect(payload.voice).toEqual({ transcription_model: "whisper-1", speech_model: "tts-1" });
  });

  it("creates a fresh schedule in the general scope when no topic is selected", async () => {
    vi.mocked(workflowApi.createSchedule).mockResolvedValue(makeSchedule());
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();

    const prompt = document.querySelector<HTMLTextAreaElement>(".schedule-automation textarea");
    if (!prompt) throw new Error("Schedule prompt was not rendered");
    prompt.value = "Review the queue.";
    prompt.dispatchEvent(new Event("input", { bubbles: true }));
    buttonNamed("Create schedule").click();
    await flushUi();

    expect(document.body.textContent).not.toContain("Select a topic before scheduling work.");
    const request = vi.mocked(workflowApi.createSchedule).mock.calls[0]?.[0];
    expect(request?.session).toEqual({ policy: "fresh", agent_id: "gateway" });
  });
});
