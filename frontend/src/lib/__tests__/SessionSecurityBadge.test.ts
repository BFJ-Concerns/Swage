// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";
import { flushSync } from "svelte";
import SessionSecurityBadge from "../components/SessionSecurityBadge.svelte";
import * as api from "../api/settings";
import { securitySettingsChanged } from "../security-settings";
import type { SecuritySettingsResponse } from "../protocol/SecuritySettingsResponse";
vi.mock("../api/settings", () => ({ fetchConversationSecuritySettings: vi.fn() }));
let component: ReturnType<typeof mount>;
afterEach(() => { if (component) unmount(component); document.body.innerHTML = ""; vi.clearAllMocks(); });
const reading = (relaxed: SecuritySettingsResponse["relaxed"]): SecuritySettingsResponse => ({
  available_tools: [], scope: { kind: "install" }, overrides: {}, relaxed,
  effective: { sandbox_enabled: true, allow_degraded_execution: false, require_approval: [], access_gate: "token" },
});
async function settle() { flushSync(); await new Promise((resolve) => setTimeout(resolve, 0)); flushSync(); }
it("reads a session's attributed relaxations and refreshes after a settings save", async () => {
  vi.mocked(api.fetchConversationSecuritySettings).mockResolvedValueOnce(reading([
    { check: "sandbox", relaxed_by: { kind: "topic", topic_id: "topic-1" }, detail: "Off" },
    { check: "approval_prompts", relaxed_by: { kind: "install" }, detail: "Reduced" },
  ])).mockResolvedValue(reading([]));
  component = mount(SessionSecurityBadge, { target: document.body, props: { conversationId: "session-1" } });
  await settle();
  expect(api.fetchConversationSecuritySettings).toHaveBeenCalledWith("session-1");
  expect(document.body.textContent).toContain("Sandbox — relaxed by this topic");
  expect(document.body.textContent).toContain("Approval prompts — relaxed by the install");
  securitySettingsChanged(); await settle();
  expect(document.querySelector('[aria-label="Session security relaxations"]')).toBeNull();
  expect(api.fetchConversationSecuritySettings).toHaveBeenCalledTimes(2);
});
it("shows a failed reading rather than declaring checks in force", async () => {
  vi.mocked(api.fetchConversationSecuritySettings).mockRejectedValue(new Error("offline"));
  component = mount(SessionSecurityBadge, { target: document.body, props: { conversationId: "session-2" } });
  await settle();
  expect(document.body.textContent).toContain("Security settings unavailable");
});
