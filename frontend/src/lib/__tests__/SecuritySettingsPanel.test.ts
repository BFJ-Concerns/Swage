// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";
import SecuritySettingsPanel from "../components/settings/SecuritySettingsPanel.svelte";
import * as api from "../api/settings";
import type { SecuritySettingsResponse } from "../protocol/SecuritySettingsResponse";
import type { SaveSecuritySettingsRequest } from "../protocol/SaveSecuritySettingsRequest";

vi.mock("../api/settings", () => ({
  fetchInstallSecuritySettings: vi.fn(), saveInstallSecuritySettings: vi.fn(),
  fetchTopicSecuritySettings: vi.fn(), saveTopicSecuritySettings: vi.fn(),
}));
let component: ReturnType<typeof mount>;
afterEach(() => { unmount(component); document.body.innerHTML = ""; vi.resetAllMocks(); });
async function settle() {
  for (let i = 0; i < 12; i++) await new Promise(resolve => setTimeout(resolve, 0));
}
it.each([false, true])("sandbox-only stale form preserves a permanent approval (topic=%s)", async (topic) => {
  const reading: SecuritySettingsResponse = {
    available_tools: ["shell", "file_write"],
    scope: topic ? { kind: "topic", topic_id: "topic-1" } : { kind: "install" },
    effective: { sandbox_enabled: true, allow_degraded_execution: true, access_gate: "warn", require_approval: ["shell", "file_write"] },
    overrides: { sandbox_enabled: true, allow_degraded_execution: true, access_gate: topic ? undefined : "warn", require_approval: ["shell", "file_write"] },
    relaxed: [],
  };
  const copy = () => JSON.parse(JSON.stringify(reading));
  vi.mocked(api.fetchInstallSecuritySettings).mockImplementation(async () => copy());
  vi.mocked(api.fetchTopicSecuritySettings).mockImplementation(async () => copy());
  const save = vi.fn(async (overrides: SaveSecuritySettingsRequest) => {
    Object.assign(reading.overrides, JSON.parse(JSON.stringify(overrides)));
    Object.assign(reading.effective, reading.overrides);
    return copy();
  });
  vi.mocked(api.saveInstallSecuritySettings).mockImplementation(save);
  vi.mocked(api.saveTopicSecuritySettings).mockImplementation(async (_id, overrides) => save(overrides));
  component = mount(SecuritySettingsPanel, { target: document.body, props: { topicId: topic ? "topic-1" : null } });
  await settle();
  // Another approval action persists its decision after this form has loaded.
  reading.overrides.require_approval = ["file_write"];
  reading.effective.require_approval = ["file_write"];
  const sandbox = document.querySelector<HTMLSelectElement>(`[aria-label="Sandbox for ${topic ? "this topic" : "the install"}"]`)!;
  sandbox.value = "off"; sandbox.dispatchEvent(new Event("change", { bubbles: true }));
  await settle();
  document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  await settle();
  expect(reading.overrides.require_approval, "sandbox-only save restored a permanently approved tool").toEqual(["file_write"]);
  expect(save).toHaveBeenLastCalledWith({ sandbox_enabled: false });
  // A saved form has no pending edits, so a second save sends no overrides.
  document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  await settle();
  expect(save).toHaveBeenLastCalledWith({});
  // A later explicit approval edit still replaces the list, including clearing it.
  document.querySelector<HTMLButtonElement>('[aria-label="Remove file_write approval"]')!.click();
  await settle();
  document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  await settle();
  expect(save).toHaveBeenLastCalledWith({ require_approval: [] });
});

it("sends explicit null only for controls changed to inherit", async () => {
  vi.mocked(api.fetchInstallSecuritySettings).mockResolvedValue({
    available_tools: [], scope: { kind: "install" },
    effective: { sandbox_enabled: false, allow_degraded_execution: true, access_gate: "off", require_approval: [] },
    overrides: { sandbox_enabled: false, allow_degraded_execution: true, access_gate: "off", require_approval: [] }, relaxed: [],
  });
  component = mount(SecuritySettingsPanel, { target: document.body });
  await settle();
  for (const select of document.querySelectorAll<HTMLSelectElement>("select")) {
    select.value = "inherit"; select.dispatchEvent(new Event("change", { bubbles: true }));
  }
  await settle();
  document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
  await settle();
  expect(api.saveInstallSecuritySettings).toHaveBeenCalledWith({
    sandbox_enabled: null, allow_degraded_execution: null, require_approval: null, access_gate: null,
  });
});
