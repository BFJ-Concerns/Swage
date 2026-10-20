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

function mockLists(
  codes: () => unknown = () => [{ integration: "telegram", link_code: code, expires_at: expiry }],
  links: () => unknown = () => [link],
) {
  return vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    switch (String(input)) {
      case "/api/channels": return Response.json({ channels: [{
        integration: "telegram", status: "connected", route_count: 1, delivery_count: 7,
        last_delivery_at: null,
      }] });
      case "/api/onboarding/channels": return Response.json({ links: links() });
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
  it("reads statuses, links and Telegram commands with local expiry", async () => {
    const fetchSpy = mockLists();
    openChannels();
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(`/link ${code}`));
    expect(document.querySelector('[aria-label="Channel statuses"]')?.textContent).toContain("connected");
    expect(document.querySelector('[aria-label="Channel statuses"]')?.textContent).toContain("Deliveries7");
    const links = document.querySelector('[aria-label="Channel links"]');
    for (const value of ["assistant-bot", "chat-42", "topic-42", "assistant", "session-42"]) {
      expect(links?.textContent).toContain(value);
    }
    expect(document.querySelector("time")?.getAttribute("datetime")).toBe(expiry);
    expect(document.querySelector("time")?.textContent).toBe(new Date(expiry).toLocaleString());
    expect(document.body.textContent).not.toContain("Non-functional");
    expect(document.querySelector("select")).not.toBeNull();
    expect(fetchSpy.mock.calls).toEqual([
      ["/api/channels", { method: "GET" }],
      ["/api/onboarding/channels", { method: "GET" }],
      ["/api/onboarding/channels/link-codes", { method: "GET" }],
    ]);
  });

  it("re-reads after reopening and removes a previously displayed code on refresh", async () => {
    let listedCodes = [{ integration: "telegram", link_code: code, expires_at: expiry }];
    const fetchSpy = mockLists(() => listedCodes);
    openChannels();
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(`/link ${code}`));
    unmount(component!);
    component = undefined;
    openChannels();
    await vi.waitFor(() => expect(fetchSpy).toHaveBeenCalledTimes(6));
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(`/link ${code}`));
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
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(`/link ${code}`));
    fetchSpy.mockRejectedValueOnce(new Error("channel service unavailable"));
    refresh();
    await vi.waitFor(() => expect(document.querySelector('[aria-label="Channel inventory"] [role="alert"]')?.textContent).toBe("channel service unavailable"));
    expect(document.querySelector("code")).toBeNull();
    refresh();
    await vi.waitFor(() => expect(document.querySelector("code")?.textContent).toBe(`/link ${code}`));
    expect(document.querySelector('[aria-label="Channel inventory"] [role="alert"]')).toBeNull();
  });
});


describe("Channel linking controls", () => {
  it("issues a code for the chosen integration through the gateway", async () => {
    const fetchSpy = mockLists();
    openChannels();
    await vi.waitFor(() => expect(document.querySelector("select")).not.toBeNull());
    const selector = document.querySelector("select")!;
    flushSync(() => { selector.value = "dev_thread"; selector.dispatchEvent(new Event("change", { bubbles: true })); });
    const button = Array.from(document.querySelectorAll("button")).find((b) => b.textContent === "Issue link code");
    expect(button, "Settings must offer code issuance").toBeDefined();
    fetchSpy.mockImplementationOnce(async () => Response.json({ integration: "dev_thread", link_code: "new-thread-code", expires_at: expiry }));
    flushSync(() => button!.click());
    await vi.waitFor(() => expect(fetchSpy.mock.calls.some(([url, options]) =>
      url === "/api/onboarding/channels/link-codes" && options?.method === "POST" &&
      JSON.parse(options.body as string).integration === "dev_thread"
    )).toBe(true));
    await vi.waitFor(() => expect(document.body.textContent).toContain("new-thread-code"));
  });

  it("shows each code with instructions for its own integration", async () => {
    mockLists(() => [
      { integration: "telegram", link_code: "telegram-code", expires_at: expiry },
      { integration: "dev_dm", link_code: "dm-code", expires_at: expiry },
      { integration: "dev_thread", link_code: "thread-code", expires_at: expiry },
    ]);
    openChannels();
    await vi.waitFor(() => expect(document.querySelectorAll('[aria-label="Unredeemed link codes"] article')).toHaveLength(3));
    const articles = Array.from(document.querySelectorAll('[aria-label="Unredeemed link codes"] article'));
    expect(articles[0].querySelector("code")?.textContent).toBe("/link telegram-code");
    expect(articles[0].textContent).toContain("Telegram bot");
    expect(articles[1].textContent).toContain("development direct-message integration");
    expect(articles[2].textContent).toContain("development thread integration");
    for (const article of articles.slice(1)) {
      expect(article.textContent).not.toContain("/link");
      expect(article.textContent).not.toContain("Telegram");
    }
  });

  it("updates distinct colon-containing link tuples independently", async () => {
    let links = [
      { ...link, account: "a:b", conversation: "c", agent_id: "first-agent" },
      { ...link, account: "a", conversation: "b:c", agent_id: "second-agent" },
    ];
    mockLists(() => [], () => links);
    openChannels();
    await vi.waitFor(() => expect(document.querySelectorAll('[aria-label="Channel links"] article')).toHaveLength(2));
    links = [{ ...links[1], agent_id: "updated-second" }, { ...links[0], agent_id: "updated-first" }];
    refresh();
    await vi.waitFor(() => {
      const rows = document.querySelectorAll('[aria-label="Channel links"] article');
      expect(rows).toHaveLength(2);
      expect(rows[0].textContent).toContain("updated-second");
      expect(rows[1].textContent).toContain("updated-first");
      expect(rows[0].textContent).not.toContain("updated-first");
      expect(rows[1].textContent).not.toContain("updated-second");
    });
  });
});
