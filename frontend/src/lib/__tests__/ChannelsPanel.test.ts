// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import SettingsPage from "../components/SettingsPage.svelte";

vi.mock("../api/settings", () => ({
  fetchSettings: vi.fn(async () => { throw new Error("settings unavailable"); }),
  restartGateway: vi.fn(),
  saveSettings: vi.fn(),
}));

const code = "ab12cd34-5678-90ab-cdef-1234567890ab";
const expiry = "2030-07-10T12:34:56Z";
const link = {
  integration: "telegram", account: "assistant-bot", conversation: "chat-42",
  topic_id: "topic-42", agent_id: "assistant", session_id: "session-42",
  home_target: null, linked_at: "2030-07-09T10:00:00Z", updated_at: "2030-07-09T10:00:00Z",
};

function mockLists(codes: () => unknown = () => [{ link_code: code, expires_at: expiry }]) {
  return vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    switch (String(input)) {
      case "/api/channels": return Response.json({ channels: [{
        integration: "telegram", status: "connected", route_count: 1, delivery_count: 7,
        last_delivery_at: null,
      }] });
      case "/api/onboarding/channels": return Response.json({ links: [link] });
      case "/api/onboarding/channels/link-codes": return Response.json(codes());
      default: throw new Error(`Unexpected request: ${String(input)}`);
    }
  });
}

let component: ReturnType<typeof mount> | undefined;
afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

function openChannels() {
  component = mount(SettingsPage, { target: document.body, props: { tab: "channels" } });
}

function refresh() {
  const button = Array.from(document.querySelectorAll("button")).find((b) => b.textContent === "Refresh");
  flushSync(() => button?.click());
}

describe("Channels settings", () => {
  it("reads statuses, links and verbatim codes with local expiry through GET only", async () => {
    const fetchSpy = mockLists();
    openChannels();
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(code));
    expect(document.querySelector('[aria-label="Channel statuses"]')?.textContent).toContain("connected");
    expect(document.querySelector('[aria-label="Channel statuses"]')?.textContent).toContain("Deliveries7");
    const links = document.querySelector('[aria-label="Channel links"]');
    for (const value of ["assistant-bot", "chat-42", "topic-42", "assistant", "session-42"]) {
      expect(links?.textContent).toContain(value);
    }
    expect(document.querySelector("time")?.getAttribute("datetime")).toBe(expiry);
    expect(document.querySelector("time")?.textContent).toBe(new Date(expiry).toLocaleString());
    expect(document.body.textContent).not.toContain("Non-functional");
    expect(document.querySelector("form, input, textarea, select")).toBeNull();
    expect(fetchSpy.mock.calls).toEqual([
      ["/api/channels", { method: "GET" }],
      ["/api/onboarding/channels", { method: "GET" }],
      ["/api/onboarding/channels/link-codes", { method: "GET" }],
    ]);
  });

  it("re-reads after reopening and removes a previously displayed code on refresh", async () => {
    let listedCodes = [{ link_code: code, expires_at: expiry }];
    const fetchSpy = mockLists(() => listedCodes);
    openChannels();
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(code));
    unmount(component!);
    component = undefined;
    openChannels();
    await vi.waitFor(() => expect(fetchSpy).toHaveBeenCalledTimes(6));
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(code));
    listedCodes = [];
    refresh();
    await vi.waitFor(() => expect(document.body.textContent).toContain("No unredeemed link codes."));
    expect(document.querySelector("code")).toBeNull();
    expect(document.body.textContent).not.toContain(code);
    expect(fetchSpy).toHaveBeenCalledTimes(9);
  });

  it("reports a listing failure and retries without showing stale codes", async () => {
    const fetchSpy = mockLists();
    openChannels();
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(code));
    fetchSpy.mockRejectedValueOnce(new Error("channel service unavailable"));
    refresh();
    await vi.waitFor(() => expect(document.querySelector('[aria-label="Channel inventory"] [role="alert"]')?.textContent).toBe("channel service unavailable"));
    expect(document.querySelector("code")).toBeNull();
    refresh();
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(code));
    expect(document.querySelector('[aria-label="Channel inventory"] [role="alert"]')).toBeNull();
  });
});
