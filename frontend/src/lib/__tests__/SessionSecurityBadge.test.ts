// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";
import { flushSync } from "svelte";
import ApprovalCard from "../components/ApprovalCard.svelte";
import SessionSecurityBadge from "../components/SessionSecurityBadge.svelte";
import * as api from "../api/settings";
import { securitySettingsChanged } from "../security-settings";
import type { SecuritySettingsResponse } from "../protocol/SecuritySettingsResponse";
vi.mock("../api/settings", () => ({ fetchConversationSecuritySettings: vi.fn() }));
let approvalCard: ReturnType<typeof mount> | undefined;
let component: ReturnType<typeof mount>;
afterEach(() => { if (component) unmount(component); if (approvalCard) unmount(approvalCard); approvalCard = undefined; vi.unstubAllGlobals(); document.body.innerHTML = ""; vi.clearAllMocks(); });
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

it.each(["Allow permanently", "Allow permanently globally"])("refreshes the mounted badge after %s succeeds", async (label) => {
  vi.mocked(api.fetchConversationSecuritySettings).mockResolvedValueOnce(reading([]))
    .mockResolvedValue(reading([{ check: "approval_prompts", relaxed_by: { kind: "install" }, detail: "Shell no longer requires approval" }]));
  let complete!: (value: unknown) => void;
  const fetch = vi.fn(() => new Promise((resolve) => complete = resolve));
  vi.stubGlobal("fetch", fetch);
  component = mount(SessionSecurityBadge, { target: document.body, props: { conversationId: "session-1" } });
  approvalCard = mount(ApprovalCard, { target: document.body, props: { approval: {
    approval_id: "approval-1", turn_id: "turn-1", agent_id: "agent", kind: "tool", tool_name: "shell",
    prompt: "Allow shell?", timeout_at: "2099-01-01T00:00:00Z", timeout_policy: "reject",
  } } });
  await settle();
  if (label.endsWith("globally")) {
    document.querySelector<HTMLButtonElement>('[title="More permanent approval scopes"]')!.click();
    await settle();
  }
  const button = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.trim() === label)!;
  button.click(); await settle();
  expect(api.fetchConversationSecuritySettings).toHaveBeenCalledTimes(1);
  complete({ ok: true }); await settle();
  expect(api.fetchConversationSecuritySettings).toHaveBeenCalledTimes(2);
  expect(document.body.textContent).toContain("Approval prompts — relaxed by the install");
});
