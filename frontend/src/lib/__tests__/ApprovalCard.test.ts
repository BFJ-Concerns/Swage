// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import ApprovalCard from "../components/ApprovalCard.svelte";
import { setOperatorBaseUrl } from "../api/operator";

let component: ReturnType<typeof mount> | undefined;

afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

describe("approval denial", () => {
  it("sends the user's denial as the rejection reason", async () => {
    const fetch = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetch);
    setOperatorBaseUrl("");
    component = mount(ApprovalCard, {
      target: document.body,
      props: {
        approval: {
          approval_id: "approval-1",
          turn_id: "turn-1",
          agent_id: "agent",
          kind: "tool",
          tool_name: "shell",
          prompt: "Allow shell?",
          timeout_at: "2099-01-01T00:00:00Z",
          timeout_policy: "reject",
        },
      },
    });
    flushSync();

    const deny = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Deny",
    );
    expect(deny).toBeDefined();
    deny!.click();

    await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce());
    const [url, options] = fetch.mock.calls[0];
    expect(url).toBe("/api/approvals/approval-1");
    expect(options.method).toBe("POST");
    const decision = JSON.parse(options.body);
    expect(decision.decision).toBe("rejected");
    expect(decision.reason).toBe("denied by the user");
    expect(decision.reason).not.toContain("approval card");
  });
});
