// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import SettingsPage from "../components/SettingsPage.svelte";
import type { SettingsPayload, SettingsResponse } from "../types";

import * as settingsApi from "../api/settings";
import * as workflowApi from "../api/workflows";

vi.mock("../api/settings", () => ({
  discoverModels: vi.fn(),
  fetchSettings: vi.fn(),
  restartGateway: vi.fn(),
  saveSettings: vi.fn(),
}));

vi.mock("../api/workflows", () => ({
  downloadRecoveryExport: vi.fn(),
  exportRecoveryBundle: vi.fn(),
  listRecoveryExports: vi.fn(),
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
    restart_required: false,
    can_restart: false,
  };
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
      tool_permissions: payload.tool_permissions,
      hitl: payload.hitl,
      sandbox: payload.sandbox,
      context: payload.context,
      pricing: payload.pricing ?? emptySettings().pricing,
      observability: payload.observability ?? emptySettings().observability,
      communication: payload.communication ?? emptySettings().communication,
      restart_required: true,
      can_restart: false,
    }));
    vi.mocked(settingsApi.discoverModels).mockResolvedValue({
      provider_id: "openai",
      models: [{ id: "gpt-4.1-mini", name: "gpt-4.1-mini", context_window: null }],
    });
    vi.mocked(workflowApi.listRecoveryExports).mockResolvedValue({ exports: [] });
    vi.mocked(workflowApi.exportRecoveryBundle).mockResolvedValue({});
    vi.mocked(workflowApi.downloadRecoveryExport).mockResolvedValue({});
  });

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.innerHTML = "";
    vi.clearAllMocks();
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
            benches: 1,
            workspaces: 2,
            sessions: 3,
            secrets: 1,
            channel_links: 1,
            execution_hosts: 1,
            scheduled_jobs: 0,
            heartbeats: 1,
          },
        ],
      })
      .mockResolvedValueOnce({
        exports: [
          {
            id: "export-2",
            created_at: "2026-05-26T10:05:00Z",
            size_bytes: 512,
            benches: 1,
            workspaces: 0,
            sessions: 0,
            secrets: 0,
            channel_links: 0,
            execution_hosts: 0,
            scheduled_jobs: 0,
            heartbeats: 0,
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
});
