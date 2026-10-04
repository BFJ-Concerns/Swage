// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import SettingsPage from "../components/SettingsPage.svelte";
import type { Schedule } from "../protocol/Schedule";
import type { SettingsPayload, SettingsResponse } from "../types";

import * as agentDefinitionsApi from "../api/agent-definitions";
import * as settingsApi from "../api/settings";
import * as skillsApi from "../api/skills";
import * as workflowApi from "../api/workflows";
import * as operatorApi from "../api/operator";
import { makeSchedule } from "./fixtures/schedules";

vi.mock("../api/settings", () => ({
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
      tool_permissions: payload.tool_permissions ?? emptySettings().tool_permissions,
      hitl: payload.hitl ?? emptySettings().hitl,
      sandbox: payload.sandbox,
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
