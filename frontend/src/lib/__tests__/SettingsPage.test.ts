// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import SettingsPage from "../components/SettingsPage.svelte";
import type { SettingsPayload, SettingsResponse } from "../types";

import * as settingsApi from "../api/settings";

vi.mock("../api/settings", () => ({
  discoverModels: vi.fn(),
  fetchSettings: vi.fn(),
  restartGateway: vi.fn(),
  saveSettings: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 12; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
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
    restart_required: false,
    can_restart: false,
  };
}

describe("SettingsPage", () => {
  let component: MountedComponent | undefined;

  beforeEach(() => {
    vi.mocked(settingsApi.fetchSettings).mockResolvedValue(emptySettings());
    vi.mocked(settingsApi.saveSettings).mockImplementation(async (payload: SettingsPayload) => ({
      providers: payload.providers,
      default_provider: payload.default_provider,
      models: payload.models,
      default_model: payload.default_model,
      generation_presets: payload.generation_presets,
      restart_required: true,
      can_restart: false,
    }));
    vi.mocked(settingsApi.discoverModels).mockResolvedValue({
      provider_id: "openai",
      models: [{ id: "gpt-4.1-mini", name: "gpt-4.1-mini", context_window: null }],
    });
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
    buttonNamed("Save").click();
    await flushUi();

    expect(settingsApi.saveSettings).toHaveBeenCalledWith(
      expect.objectContaining({
        default_model: "openai:gpt-4.1-mini",
        default_provider: "openai",
      }),
    );
  });
});
