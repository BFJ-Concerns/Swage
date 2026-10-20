// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { compile } from "svelte/compiler";
import defaultModelSource from "../components/settings/ProviderDefaultModel.svelte?raw";
import modelManagementSource from "../components/settings/ProviderModelManagement.svelte?raw";
import { createClassComponent } from "svelte/legacy";
import { fromStore, writable } from "svelte/store";

import { emptySettings } from "./fixtures/settings";
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
  fetchProviderSecrets: vi.fn(async () => ({ secrets: [] })),
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
  checkSpeechModel: vi.fn(),
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
    vi.mocked(settingsApi.fetchProviderSecrets).mockReset().mockResolvedValue({ secrets: [], managed_backend: { status: "ready", detail: "", key_source: null } });
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
      attachments: payload.attachments ?? emptySettings().attachments,
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
    vi.useRealTimers();
  });

  it.each(["providers", "generation", "automation", "voice"] as const)(
    "shows quiet transient new-turn feedback after saving %s", async (tab) => {
      vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider());
      const save = vi.mocked(settingsApi.saveSettings).getMockImplementation()!;
      vi.mocked(settingsApi.saveSettings).mockImplementation(async (payload) => ({
        ...await save(payload), restart_required: false, can_restart: true,
      }));
      component = mount(SettingsPage, { target: document.body, props: { tab } });
      await flushUi();
      vi.useFakeTimers();
      flushSync(() => {
        if (tab === "providers") {
          const input = document.querySelector<HTMLInputElement>('input[placeholder="model-id"]')!;
          input.value = "added-model";
          input.dispatchEvent(new Event("input", { bubbles: true }));
        } else if (tab === "automation") {
          const input = document.querySelector<HTMLInputElement>('input[placeholder="300"]')!;
          input.value = "301";
          input.dispatchEvent(new Event("input", { bubbles: true }));
        } else if (tab === "voice") {
          const input = document.querySelector<HTMLInputElement>('input[aria-label="Transcription model"]')!;
          input.value = "transcribe";
          input.dispatchEvent(new Event("input", { bubbles: true }));
        }
      });
      if (tab === "providers") flushSync(() => buttonNamed("+Add model").click());
      if (tab === "generation") flushSync(() => buttonNamed("+ New").click());
      await vi.advanceTimersByTimeAsync(700);
      expect(settingsApi.saveSettings).toHaveBeenCalledTimes(1);
      const notice = document.querySelector(".notice");
      expect.soft(notice?.textContent?.trim()).toBe("Saved automatically. Changes apply to new turns.");
      expect.soft(notice?.getAttribute("role")).toBe("status");
      expect.soft(notice?.querySelector("button")).toBeNull();
      if (tab === "automation") {
        const sandbox = [...document.querySelectorAll(".field")].find((field) => field.querySelector(".key")?.textContent?.startsWith("Sandbox"));
        expect.soft(sandbox?.textContent).toContain("Saved changes apply to new turns.");
        expect.soft(sandbox?.textContent).not.toContain("require a gateway restart");
      }
      await vi.advanceTimersByTimeAsync(5000);
      expect(document.querySelector(".notice")).toBeNull();
    },
  );

  function configuredProvider(source: "environment" | "stored" | "none" = "none"): SettingsResponse {
    const settings = emptySettings();
    settings.providers.proxy = { protocol: "openai", display_name: "Homelab proxy", base_url: "https://Proxy.Example/v1", api_key_secret_ref: source === "stored" ? "secret:existing" : null, api_key_env: source === "environment" ? "PROXY_KEY" : null, has_credential: source !== "none" };
    settings.models["proxy:MixedCase-Model"] = { provider_id: "proxy", model_id: "MixedCase-Model", aliases: [], generation_preset: null };
    settings.default_model = "proxy:MixedCase-Model";
    settings.default_provider = "proxy";
    return settings;
  }

  it.each(["environment", "stored", "none"] as const)("reports %s credential source truthfully", async (source) => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider(source));
    vi.mocked(settingsApi.fetchProviderSecrets).mockResolvedValue({ secrets: source === "stored" ? [{ id: "existing", name: "Stored key", reference: "secret:existing", material_kind: "provider_key", owner_kind: "provider", backend_kind: "managed", scope_kind: "global", metadata: {}, status: "active", created_at: "2026-01-01", updated_at: "2026-01-01" }] : [], managed_backend: { status: "ready", detail: "", key_source: null } });
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } });
    await flushUi();
    const header = document.querySelector(".provider-collapse")!;
    expect(header.textContent).toContain(source === "environment" ? "Environment credential" : source === "stored" ? "Stored credential" : "No credential");
    const input = document.querySelector<HTMLInputElement>('input[type="password"]')!;
    expect(input.placeholder).toBe(source === "environment" ? "Resolved from environment" : source === "stored" ? "Stored credential" : "Paste to store");
    if (source === "none") expect(document.body.textContent).toContain("Authenticated turns cannot run");
  });

  it.each(["env", "file"] as const)("renders governed %s credentials without offering pasted rotation", async (backend) => {
    const settings = configuredProvider("stored");
    const reference = "secret://" + backend + "/chosen";
    settings.providers.proxy.api_key_secret_ref = reference;
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(settings);
    vi.mocked(settingsApi.fetchProviderSecrets).mockResolvedValue({ secrets: [{ id: "chosen", name: "Proxy credential", reference, material_kind: "provider_key", owner_kind: "provider", backend_kind: backend, scope_kind: "global", scope_id: null, metadata: {}, status: "active", created_at: "2026-01-01", updated_at: "2026-01-01" }], managed_backend: { status: "ready", detail: "", key_source: null } });
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } });
    await flushUi();
    const source = backend === "env" ? "Environment credential" : "File credential";
    expect(document.querySelector(".provider-collapse")?.textContent).toContain(source);
    expect(document.body.textContent).toContain(backend === "env" ? "Update this credential in its environment variable." : "Update this credential in its source file.");
    expect(document.body.textContent).not.toContain("Paste a new value to store or rotate");
    expect(document.querySelector('input[type="password"]:not(:disabled)')).toBeNull();
    expect(document.querySelector<HTMLInputElement>('input[type="password"]')?.placeholder).toBe(source);
    expect([...document.querySelectorAll(".credential-status")].every((badge) => badge.textContent === source)).toBe(true);
    const select = document.querySelector<HTMLSelectElement>(".provider-body select")!;
    select.value = ""; select.dispatchEvent(new Event("change", { bubbles: true }));
    await flushUi();
    expect(document.querySelector<HTMLInputElement>('input[type="password"]')?.disabled).toBe(false);
  });

  it("keeps managed credential labels and pasted rotation", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider("stored"));
    vi.mocked(settingsApi.fetchProviderSecrets).mockResolvedValue({ secrets: [{ id: "existing", name: "Stored key", reference: "secret:existing", material_kind: "provider_key", owner_kind: "provider", backend_kind: "managed", scope_kind: "global", metadata: {}, status: "active", created_at: "2026-01-01", updated_at: "2026-01-01" }], managed_backend: { status: "ready", detail: "", key_source: null } });
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } });
    await flushUi();
    expect(document.querySelector(".provider-collapse")?.textContent).toContain("Stored credential");
    const input = document.querySelector<HTMLInputElement>('input[type="password"]')!;
    expect(input.disabled).toBe(false);
    expect(input.placeholder).toBe("Stored credential");
    expect(document.body.textContent).toContain("Paste a new value to store or rotate");
    input.value = "replacement-key"; input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    const sent = vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers.proxy;
    expect(sent.api_key_secret_ref).toBe("secret:existing");
    expect(sent.api_key).toBe("replacement-key");
  });

  it.each(["available", "missing", "failed"] as const)("loads the newly stored credential after saving a first key: %s record", async (record) => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider());
    const reference = "secret://managed/00000000-0000-4000-8000-000000000004";
    const managedBackend = { status: "ready", detail: "", key_source: null };
    vi.mocked(settingsApi.fetchProviderSecrets)
      .mockResolvedValueOnce({ secrets: [], managed_backend: managedBackend })
      .mockResolvedValue({ secrets: [{ id: "00000000-0000-4000-8000-000000000004", name: "provider:proxy:api-key", reference, material_kind: "provider_key", owner_kind: "provider", backend_kind: "managed", scope_kind: "global", scope_id: null, metadata: { provider_id: "proxy" }, status: "active", created_at: "2026-01-01", updated_at: "2026-01-01" }], managed_backend: managedBackend });
    if (record === "missing") vi.mocked(settingsApi.fetchProviderSecrets).mockResolvedValue({ secrets: [], managed_backend: managedBackend });
    if (record === "failed") vi.mocked(settingsApi.fetchProviderSecrets).mockRejectedValue(new Error("List unavailable"));
    const save = vi.mocked(settingsApi.saveSettings).getMockImplementation()!;
    vi.mocked(settingsApi.saveSettings).mockImplementation(async (payload) => {
      const response = JSON.parse(JSON.stringify(await save(payload))) as SettingsResponse;
      response.providers.proxy = { ...response.providers.proxy, api_key_secret_ref: reference, api_key_env: null, has_credential: true };
      delete response.providers.proxy.api_key;
      return response;
    });
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } });
    await flushUi();
    expect(settingsApi.fetchProviderSecrets).toHaveBeenCalledTimes(1);
    expect(document.querySelector(".provider-collapse")?.textContent).toContain("No credential");
    const input = document.querySelector<HTMLInputElement>('input[type="password"]')!;
    input.value = "first-key"; input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall(); await flushUi();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls[0][0].providers.proxy.api_key).toBe("first-key");
    if (record !== "available") {
      expect(document.querySelector(".provider-collapse")?.textContent).toContain("Governed credential");
      expect(input.disabled).toBe(true);
      expect(settingsApi.fetchProviderSecrets).toHaveBeenCalledTimes(2);
      if (record === "failed") expect(document.body.textContent).toContain("Could not load stored credentials.");
      const select = document.querySelector<HTMLSelectElement>(".provider-body select")!;
      select.value = ""; select.dispatchEvent(new Event("change", { bubbles: true }));
      await flushUi();
      expect(input.disabled).toBe(false);
      return;
    }
    expect(document.querySelector(".provider-collapse")?.textContent).toContain("Stored credential");
    expect(settingsApi.fetchProviderSecrets).toHaveBeenCalledTimes(2);
    expect(input.value).toBe("");
    expect(input.placeholder).toBe("Stored credential");
    expect(input.disabled).toBe(false);
    vi.mocked(settingsApi.saveSettings).mockClear();
    input.value = "replacement-key"; input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall(); await flushUi();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls[0][0].providers.proxy).toMatchObject({ api_key_secret_ref: reference, api_key: "replacement-key" });
    expect(settingsApi.fetchProviderSecrets).toHaveBeenCalledTimes(2);
  });

  it("keeps the chosen provider name through creation and autosave", async () => {
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } });
    await flushUi();
    buttonContaining("Add provider").click(); await flushUi();
    buttonContaining("OpenAI compatible").click(); await flushUi();
    const name = document.querySelector<HTMLInputElement>("#compatible-name")!;
    name.value = "Homelab proxy"; name.dispatchEvent(new Event("input", { bubbles: true }));
    const url = document.querySelector<HTMLInputElement>("#compatible-base-url")!;
    url.value = "https://Proxy.Example/v1"; url.dispatchEvent(new Event("input", { bubbles: true })); await flushUi();
    buttonContaining("Connect").click(); await flushUi();
    expect(document.querySelector(".provider-head h2")?.textContent).toBe("Homelab proxy");
    await waitForSaveCall();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers["homelab-proxy"].display_name).toBe("Homelab proxy");
  });

  it("shows readable default labels and disables selecting the current model again", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider());
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } }); await flushUi();
    const choice = document.querySelector<HTMLButtonElement>(".provider-settings-surface .field button")!;
    expect(choice.textContent?.trim()).toBe("MixedCase-Model · Homelab proxy");
    expect(document.querySelector<HTMLButtonElement>(".row-actions button")?.disabled).toBe(true);
    expect(document.querySelector(".row-actions button")?.textContent).toBe("Default");
  });

  it("orders provider and model choices deterministically despite input insertion order", async () => {
    const settings = configuredProvider();
    settings.providers = { zulu: { ...settings.providers.proxy, display_name: "Zulu" }, alpha: { ...settings.providers.proxy, display_name: "Alpha" } };
    settings.models = { z: { ...settings.models[settings.default_model], provider_id: "zulu", model_id: "Z" }, a: { ...settings.models[settings.default_model], provider_id: "alpha", model_id: "A" } };
    settings.default_model = "a"; settings.default_provider = "alpha";
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(settings);
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } }); await flushUi();
    expect([...document.querySelectorAll(".provider-head h2")].map((node) => node.textContent)).toEqual(["Alpha", "Zulu"]);
    expect([...document.querySelectorAll(".provider-settings-surface > .field:first-child button")].map((node) => node.textContent?.trim())).toEqual(["A · Alpha", "Z · Zulu"]);
  });

  it("explains the empty state and each protocol choice", async () => {
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } }); await flushUi();
    expect(document.body.textContent).toContain("Swage is running. To send messages, add a provider and a model.");
    buttonContaining("Add provider").click(); await flushUi();
    const options = [...document.querySelectorAll(".add-row .opt-pick")];
    expect(options).toHaveLength(6);
    for (const option of options) expect(option.querySelector("small")?.textContent?.length ?? 0).toBeGreaterThan(15);
    expect(document.querySelector(".add-row")?.textContent).toContain("Responses API");
    expect(document.querySelector(".add-row")?.textContent).toContain("Chat Completions API");
  });

  it("warns on immediate reload and flushes provider edits on navigation", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider());
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } }); await flushUi();
    const input = document.querySelector<HTMLInputElement>('.provider-section input[type="text"]')!;
    input.value = "https://Changed.Example/v1"; input.dispatchEvent(new Event("input", { bubbles: true })); await flushUi();
    const event = new Event("beforeunload", { cancelable: true }); window.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    await unmount(component); component = undefined; await flushUi();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)?.[0].providers.proxy.base_url).toBe("https://Changed.Example/v1");
  });

  it("flushes provider edits when switching settings tabs", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider());
    const page = createClassComponent({ component: SettingsPage, target: document.body, props: { tab: "providers" as "providers" | "agents" } });
    await flushUi();
    try {
      const input = document.querySelector<HTMLInputElement>('.provider-section input[type="text"]')!;
      input.value = "https://TabSwitch.Example/v1";
      input.dispatchEvent(new Event("input", { bubbles: true })); await flushUi();
      page.$set({ tab: "agents" }); await flushUi();
      expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)?.[0].providers.proxy.base_url).toBe("https://TabSwitch.Example/v1");
    } finally { page.$destroy(); }
  });

  it.each(["rejected", "in-flight"])("protects %s provider saves after switching tabs", async (state) => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider());
    if (state === "rejected") vi.mocked(settingsApi.saveSettings).mockRejectedValue(new Error("Save rejected"));
    else vi.mocked(settingsApi.saveSettings).mockImplementation(() => new Promise(() => {}));
    const page = createClassComponent({ component: SettingsPage, target: document.body, props: { tab: "providers" as "providers" | "agents" } });
    await flushUi();
    try {
      const input = document.querySelector<HTMLInputElement>('.provider-section input[type="text"]')!;
      input.value = "https://Unsaved.Example/v1";
      input.dispatchEvent(new Event("input", { bubbles: true }));
      await waitForSaveCall(); await flushUi();
      page.$set({ tab: "agents" }); await flushUi();
      const event = new Event("beforeunload", { cancelable: true });
      window.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(true);
      expect(settingsApi.saveSettings).toHaveBeenCalledTimes(1);
    } finally { page.$destroy(); }
  });

  it("labels pasted credentials as unsaved and preserves edits made during a save", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider("stored"));
    let release!: (value: SettingsResponse) => void;
    vi.mocked(settingsApi.saveSettings).mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } }); await flushUi();
    const key = document.querySelector<HTMLInputElement>('input[type="password"]')!;
    key.value = "replacement-key"; key.dispatchEvent(new Event("input", { bubbles: true })); await flushUi();
    expect(document.body.textContent).toContain("Credential not saved");
    await waitForSaveCall();
    const url = document.querySelector<HTMLInputElement>('.provider-section input[type="text"]')!;
    url.value = "https://Later.Example/v1"; url.dispatchEvent(new Event("input", { bubbles: true })); await flushUi();
    release(configuredProvider("stored")); await flushUi();
    expect(document.querySelector<HTMLInputElement>('.provider-section input[type="text"]')?.value).toBe("https://Later.Example/v1");
    expect(document.querySelector<HTMLInputElement>('input[type="password"]')?.value).toBe("");
  });

  it("keeps rejected credential edits visibly unsaved", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider("environment"));
    vi.mocked(settingsApi.saveSettings).mockRejectedValue(new Error("Save rejected"));
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } }); await flushUi();
    const input = document.querySelector<HTMLInputElement>('input[type="password"]')!;
    input.value = "edited-key"; input.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall(); await flushUi();
    expect(document.body.textContent).toContain("Credential not saved");
    expect(document.body.textContent).toContain("Save rejected");
    expect(document.querySelector(".autosave-state")?.textContent).toContain("Save failed");
    expect(input.value).toBe("edited-key");
    await new Promise((resolve) => setTimeout(resolve, 1600));
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(1);
    input.value = "corrected-key"; input.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 800));
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(2);
    await new Promise((resolve) => setTimeout(resolve, 800));
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(2);
    await unmount(component); component = undefined; await flushUi();
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(2);
  });

  it("binds an existing governed secret by reference without putting its value in the key input", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider("environment"));
    vi.mocked(settingsApi.fetchProviderSecrets).mockResolvedValue({ secrets: [{ id: "chosen", name: "Proxy credential", reference: "secret:chosen", material_kind: "provider_key", owner_kind: "provider", backend_kind: "managed", scope_kind: "global", scope_id: null, metadata: {}, status: "active", created_at: "2026-01-01", updated_at: "2026-01-01", revoked_at: null, revealed_value: null }], managed_backend: { status: "ready", detail: "", key_source: null } });
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } }); await flushUi();
    const select = document.querySelector<HTMLSelectElement>('select[aria-label="Governed credential for Homelab proxy"]');
    expect(select).not.toBeNull();
    select!.value = "secret:chosen"; select!.dispatchEvent(new Event("change", { bubbles: true }));
    await waitForSaveCall(); await flushUi();
    const sent = vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers.proxy;
    expect(sent.api_key_secret_ref).toBe("secret:chosen"); expect(sent.api_key_env).toBeNull(); expect(sent.api_key).toBe("");
    expect(document.querySelector<HTMLInputElement>('input[type="password"]')?.value).toBe("");
  });

  it("offers owned provider secrets alongside global secrets and excludes other scopes", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(configuredProvider("environment"));
    const global = { id: "global", name: "Global key", reference: "secret://managed/00000000-0000-4000-8000-000000000001", material_kind: "provider_key", owner_kind: "provider", backend_kind: "managed", scope_kind: "global", scope_id: null, metadata: {}, status: "active", created_at: "2026-01-01", updated_at: "2026-01-01", revoked_at: null, revealed_value: null };
    const owned = { ...global, id: "owned", name: "Owned key", reference: "secret://file/00000000-0000-4000-8000-000000000002", backend_kind: "file", scope_kind: "provider", scope_id: "proxy" };
    vi.mocked(settingsApi.fetchProviderSecrets).mockResolvedValue({ secrets: [global, owned, { ...owned, id: "other", name: "Other provider key", scope_id: "other" }, { ...owned, id: "revoked", name: "Revoked key", status: "revoked" }], managed_backend: { status: "ready", detail: "", key_source: null } });
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } }); await flushUi();
    const select = document.querySelector<HTMLSelectElement>('select[aria-label="Governed credential for Homelab proxy"]')!;
    const labels = [...select.options].map((option) => option.textContent);
    expect(labels).toContain("Owned key"); expect(labels).toContain("Global key");
    expect(labels).not.toContain("Other provider key"); expect(labels).not.toContain("Revoked key");
    select.value = owned.reference; select.dispatchEvent(new Event("change", { bubbles: true }));
    await waitForSaveCall(); await flushUi();
    const sent = vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].providers.proxy;
    expect(sent.api_key_secret_ref).toBe(owned.reference); expect(sent.api_key_env).toBeNull(); expect(sent.api_key).toBe("");
  });

  it("renders model choices with a font that has lower-case glyphs", () => {
    for (const source of [defaultModelSource, modelManagementSource]) {
      const css = compile(source, { generate: "client", css: "external" }).css!.code;
      const rule = css.match(/\.opt[^{}]*\{([^}]+)\}/)![1];
      expect(rule).toMatch(/font-family:\s*var\(--font-mono\)/);
      expect(rule).toMatch(/text-transform:\s*none/);
    }
  });

  it("labels the provider password input as API key", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue({
      ...emptySettings(),
      providers: { google: {
        protocol: "google", base_url: null, api_key_secret_ref: null, api_key_env: null,
        has_credential: false,
      } },
      default_provider: "google",
    });
    component = mount(SettingsPage, { target: document.body, props: { tab: "providers" } });
    await flushUi();

    const input = document.querySelector<HTMLInputElement>('.provider-section input[type="password"]');
    expect(input).not.toBeNull();
    expect(input?.closest(".field")?.querySelector(".key")?.firstChild?.textContent).toBe("API key");
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

  it.each(["begin", "clear"])("retains a search draft %s while another save finishes", async (action) => {
    const initial = emptySettings();
    if (action === "clear") initial.tool_runtime.search = { provider_type: "searxng", base_url: "https://search.example.org" };
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(initial);
    const save = vi.mocked(settingsApi.saveSettings).getMockImplementation()!;
    let finish!: () => void;
    vi.mocked(settingsApi.saveSettings).mockImplementationOnce(async (payload) => {
      await new Promise<void>((resolve) => { finish = resolve; });
      return save(payload);
    });
    component = mount(SettingsPage, { target: document.body, props: { tab: "automation" } });
    await flushUi();
    buttonNamed("Disabled").click();
    await waitForSaveCall();
    if (action === "begin") document.querySelector<HTMLInputElement>('[aria-label="Enable web search"]')!.click();
    await flushUi();
    const url = document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')!;
    url.value = "";
    url.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    finish();
    await flushUi();
    expect(document.querySelector<HTMLInputElement>('[aria-label="Enable web search"]')!.checked).toBe(true);
    expect(document.querySelector<HTMLInputElement>('[aria-label="Search base URL"]')?.value).toBe("");
    await new Promise((resolve) => setTimeout(resolve, 4100));
    await flushUi();
    expect(document.body.textContent).toContain("Saved provider/model settings require a gateway restart");
  }, 10_000);

  it.each(["restore", "display", "generation"])("clears the obsolete archive notice on %s", async (action) => {
    const tab = writable<"automation" | "display" | "generation">("automation");
    const currentTab = fromStore(tab);
    component = mount(SettingsPage, { target: document.body, props: { get tab() { return currentTab.current; } } });
    await flushUi();
    const delay = document.querySelector<HTMLInputElement>('[aria-label="Archive sessions after days"]')!;
    delay.value = "0";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(document.querySelector(".notice.error")?.textContent).toContain("Fix the archive delay");
    if (action === "restore") {
      delay.value = "7";
      delay.dispatchEvent(new Event("input", { bubbles: true }));
    } else tab.set(action as "display" | "generation");
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(document.querySelector(".notice.error")).toBeNull();
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
  });

  it("allows saving after a tab switch discards an invalid bundle editor", async () => {
    const tab = writable<"automation" | "generation">("automation");
    const currentTab = fromStore(tab);
    component = mount(SettingsPage, { target: document.body, props: { get tab() { return currentTab.current; } } });
    await flushUi();
    const editor = document.querySelector<HTMLTextAreaElement>('[aria-label="Tool bundles"]')!;
    editor.value = "{";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    expect(editor.getAttribute("aria-invalid")).toBe("true");
    buttonNamed("Disabled").click();
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(document.querySelector(".notice.error")?.textContent).toContain("Fix the invalid automation JSON before saving.");
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    tab.set("generation");
    await flushUi();
    tab.set("automation");
    await flushUi();
    const restored = document.querySelector<HTMLTextAreaElement>('[aria-label="Tool bundles"]')!;
    expect(restored.getAttribute("aria-invalid")).toBeNull();
    expect(JSON.parse(restored.value)).toEqual(emptySettings().tool_permissions.bundles);
    expect(document.querySelector(".notice.error"), "discarded JSON drafts must not leave a page warning").toBeNull();
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    const delay = document.querySelector<HTMLInputElement>('[aria-label="Archive sessions after days"]')!;
    delay.value = "14";
    delay.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(1);
    expect(vi.mocked(settingsApi.saveSettings).mock.calls[0][0].sandbox?.enabled).toBe(false);
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
    buttonNamed("Disabled").click();
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings, "an incomplete search draft must not block other settings").toHaveBeenCalled();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].sandbox?.enabled).toBe(false);
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
    buttonNamed("Enabled").click();
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings).toHaveBeenCalled();
    expect(vi.mocked(settingsApi.saveSettings).mock.calls.at(-1)![0].sandbox?.enabled).toBe(true);
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

  it.each(["0", "1.5", ""])("refuses invalid delegation depth %s without saving", async (value) => {
    component = mount(SettingsPage, {
      target: document.body,
      props: { tab: "automation", topicId: null },
    });
    await flushUi();
    const depth = document.querySelector<HTMLInputElement>('input[aria-label="Maximum delegation depth"]')!;
    depth.value = value;
    depth.dispatchEvent(new Event("input", { bubbles: true }));
    await new Promise((resolve) => setTimeout(resolve, 850));
    await flushUi();
    expect(settingsApi.saveSettings).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("Delegation depth must be a whole number of at least 1.");
    expect(document.body.textContent).not.toContain("Saved automatically");
  });

  it("saves the delegation depth from the automation control", async () => {
    component = mount(SettingsPage, { target: document.body, props: { tab: "automation", topicId: null } });
    await flushUi();
    const depth = document.querySelector<HTMLInputElement>('input[aria-label="Maximum delegation depth"]');
    expect(depth, "Automation exposes the delegation limit").not.toBeNull();
    expect(depth!.value).toBe("4");
    depth!.value = "2";
    depth!.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    const [payload] = vi.mocked(settingsApi.saveSettings).mock.calls[0];
    expect(payload.sessions?.max_delegation_depth).toBe(2);
  });

  it("autosaves edited image reference schemes from Automation", async () => {
    component = mount(SettingsPage, { target: document.body, props: { tab: "automation" } });
    await flushUi();
    const field = document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Accepted image reference schemes"]');
    expect(field?.value).toBe("https\ndata");
    field!.value = "https\nhttp\ncustom+image";
    field!.dispatchEvent(new Event("input", { bubbles: true }));
    await waitForSaveCall();
    expect(settingsApi.saveSettings).toHaveBeenCalledWith(expect.objectContaining({ attachments: { accepted_schemes: ["https", "http", "custom+image"] } }));
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
    expect(payload.sessions).toEqual({ archive_after_days: 14, max_delegation_depth: 4 });
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
      .toEqual({ archive_after_days: 14, max_delegation_depth: 4 });
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

  it("checks the saved speech model, shows upstream failure and allows a successful retry", async () => {
    let finishSave: ((settings: SettingsResponse) => void) | undefined;
    const initial = { ...emptySettings(), voice: { speech_model: "old-model" } };
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(initial);
    vi.mocked(settingsApi.saveSettings).mockImplementationOnce(() => new Promise((resolve) => { finishSave = resolve; }));
    vi.mocked(workflowApi.checkSpeechModel)
      .mockRejectedValueOnce(new Error("upstream HTTP 404: speech model not found"))
      .mockResolvedValueOnce();
    component = mount(SettingsPage, { target: document.body, props: { tab: "voice", topicId: null } });
    await flushUi();
    const check = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.trim() === "Check speech model");
    expect(check, "Voice settings must offer a speech check").toBeDefined();
    const speechDraft = document.querySelector<HTMLInputElement>('input[aria-label="Speech model"]')!;
    speechDraft.value = "bad-model";
    speechDraft.dispatchEvent(new Event("input", { bubbles: true }));
    // Leave a dirty draft so the pending autosave must yield to the check's save.
    const saved = { ...initial, voice: { ...initial.voice, speech_model: "bad-model" } };
    check!.click();
    await flushUi();
    await new Promise((resolve) => setTimeout(resolve, 700));
    expect(workflowApi.checkSpeechModel).not.toHaveBeenCalled();
    expect(settingsApi.saveSettings).toHaveBeenCalledTimes(1);
    expect(check!.disabled).toBe(true);
    expect(settingsApi.saveSettings).toHaveBeenCalledWith(expect.objectContaining({ voice: saved.voice }));
    finishSave!(saved);
    await flushUi();
    expect(workflowApi.checkSpeechModel).toHaveBeenCalledTimes(1);
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("HTTP 404: speech model not found");
    expect(check!.disabled).toBe(false);
    check!.click();
    await flushUi();
    expect(workflowApi.checkSpeechModel).toHaveBeenCalledTimes(2);
    expect(document.body.textContent).toContain("Speech model bad-model is working.");
    expect(document.querySelector('[role="alert"]')).toBeNull();
    const speech = document.querySelector<HTMLInputElement>('input[aria-label="Speech model"]')!;
    speech.value = "other-model";
    speech.dispatchEvent(new Event("input", { bubbles: true }));
    await flushUi();
    expect(document.body.textContent).not.toContain("bad-model is working");
  });

  it("does not check speech when saving the settings fails", async () => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue({ ...emptySettings(), voice: { speech_model: "tts-1" } });
    vi.mocked(settingsApi.saveSettings).mockRejectedValueOnce(new Error("settings could not be saved"));
    component = mount(SettingsPage, { target: document.body, props: { tab: "voice", topicId: null } });
    await flushUi();
    const check = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.trim() === "Check speech model");
    expect(check, "Voice settings must offer a speech check").toBeDefined();
    check!.click();
    await flushUi();
    expect(workflowApi.checkSpeechModel).not.toHaveBeenCalled();
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("settings could not be saved");
    expect(check!.disabled).toBe(false);
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
