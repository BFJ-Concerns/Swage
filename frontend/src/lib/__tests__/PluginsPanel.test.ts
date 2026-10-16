// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, tick, unmount } from "svelte";
import PluginsPanel from "../components/PluginsPanel.svelte";
import type { PluginSummary } from "../protocol/PluginSummary";

type MountedComponent = ReturnType<typeof mount>;

function plugin(name: string, overrides: Partial<PluginSummary> = {}): PluginSummary {
  return {
    name,
    description: `The ${name} plugin.`,
    version: "1.0.0",
    revision: 1,
    provides: { tools: [{ name: `${name}_tool`, description: `Run ${name}.` }], slots: [], hooks: [] },
    grants: ["tools"],
    secrets: [],
    variant_of: null,
    provenance: "installed",
    revisions: [{ revision: 1, version: "1.0.0", installed_from: "/src/plugin", provenance: "installed", installed_at: "2026-10-16T17:00:00Z", active: true }],
    enabled_for_install: false,
    enabled_topics: [],
    disabled_topics: [],
    service: { state: "serving" },
    profile: "isolate",
    sessions: [],
    ...overrides,
  };
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
}

describe("PluginsPanel", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("shows loading and listing errors", async () => {
    let rejectRequest: ((reason: Error) => void) | undefined;
    vi.spyOn(globalThis, "fetch").mockImplementation(() => new Promise((_, reject) => { rejectRequest = reject; }));
    component = mount(PluginsPanel, { target: document.body, props: { topicId: null } });
    await tick();

    expect(document.body.textContent).toContain("Loading plugins");
    rejectRequest?.(new Error("gateway unavailable"));
    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')).not.toBeNull());
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("gateway unavailable");
  });

  it("lists plugins with what each provides and where it is enabled", async () => {
    const echo = plugin("echo", {
      secrets: [{ name: "api_key", description: "The upstream key." }],
      variant_of: "tools.echo",
      enabled_for_install: true,
      service: { state: "terminated", reason: "heap_limit" },
    });
    const quiet = plugin("quiet", { grants: [], service: { state: "failed", reason: "SyntaxError: bad" }, profile: "process", sessions: ["s1", "s2"] });
    const shipped = plugin("succession-summariser", {
      provides: { tools: [], slots: ["succession"], hooks: [] },
      grants: ["model"],
      provenance: "shipped",
      enabled_for_install: true,
      revisions: [{ revision: 1, version: "1.0.0", installed_from: "/data/generated/shipped-plugins/succession-summariser", provenance: "shipped", installed_at: "2026-10-16T17:00:00Z", active: true }],
    });
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(jsonResponse({ plugins: [echo, quiet, shipped] }));
    component = mount(PluginsPanel, { target: document.body, props: { topicId: null } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("echo"));

    expect(fetchSpy).toHaveBeenCalledWith("/api/plugins", { method: "GET" });
    const detail = document.querySelector('[aria-label="echo details"]');
    expect(detail?.textContent).toContain("echo_tool");
    expect(detail?.textContent).toContain("tools");
    expect(detail?.textContent).toContain("api_key");
    expect(detail?.textContent).toContain("tools.echo");
    expect(detail?.textContent).toContain("Terminated: heap_limit");
    expect(detail?.textContent).toContain("A script in its own isolate");
    expect(detail?.textContent).toContain("revision 1");
    const installToggle = detail?.querySelector<HTMLInputElement>('input[type="checkbox"]');
    expect(installToggle?.checked).toBe(true);
    expect(detail?.textContent).not.toContain("Enabled in this topic");

    const quietButton = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.includes("quiet"));
    expect(quietButton?.textContent).toContain("Failed to load: SyntaxError: bad");
    flushSync(() => quietButton?.click());
    expect(document.querySelector('[aria-label="quiet details"]')?.textContent).toContain("Grants");
    expect(document.querySelector('[aria-label="quiet details"]')?.textContent).toContain("serving 2 sessions");

    // A shipped part sits in the same list with the same controls; only its
    // provenance tells it apart.
    const shippedButton = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.includes("succession-summariser"))!;
    expect(shippedButton.textContent).toContain("Shipped with the gateway");
    flushSync(() => shippedButton.click());
    const shippedDetail = document.querySelector('[aria-label="succession-summariser details"]')!;
    expect(shippedDetail.textContent).toContain("Shipped with the gateway");
    expect(shippedDetail.textContent).toContain("shipped with the gateway");
    expect(shippedDetail.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true);
  });

  it("enables a plugin for the selected topic through the enablement route", async () => {
    const echo = plugin("echo");
    const enabled = plugin("echo", { enabled_topics: ["topic-1"] });
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async (_input, init) => {
      if (init?.method === "PUT") return jsonResponse(enabled);
      return jsonResponse({ plugins: [echo] });
    });
    component = mount(PluginsPanel, { target: document.body, props: { topicId: "topic-1" } });
    await vi.waitFor(() => expect(document.querySelector('[aria-label="echo details"]')).not.toBeNull());

    const toggles = document.querySelectorAll<HTMLInputElement>('[aria-label="echo details"] input[type="checkbox"]');
    expect(toggles).toHaveLength(2);
    expect(toggles[1].checked).toBe(false);
    expect(document.body.textContent).toContain("not enabled");
    toggles[1].checked = true;
    toggles[1].dispatchEvent(new Event("change", { bubbles: true }));
    await vi.waitFor(() => expect(document.body.textContent).toContain("enabled by this topic"));

    const put = fetchSpy.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(put?.[0]).toBe("/api/plugins/echo/enablement");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({ topic_id: "topic-1", enabled: true });
  });

  it("puts the box back when the gateway refuses the change", async () => {
    const echo = plugin("echo");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (_input, init) => {
      if (init?.method === "PUT") {
        return new Response(JSON.stringify({ error: "enablement is read-only here" }), { status: 409, headers: { "Content-Type": "application/json" } });
      }
      return jsonResponse({ plugins: [echo] });
    });
    component = mount(PluginsPanel, { target: document.body, props: { topicId: null } });
    await vi.waitFor(() => expect(document.querySelector('[aria-label="echo details"]')).not.toBeNull());

    const toggle = document.querySelector<HTMLInputElement>('[aria-label="echo details"] input[type="checkbox"]')!;
    expect(toggle.checked).toBe(false);
    toggle.checked = true;
    toggle.dispatchEvent(new Event("change", { bubbles: true }));
    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')).not.toBeNull());
    expect(toggle.checked).toBe(false);
  });

  it("installs a package from a path and selects the new plugin", async () => {
    const installed = plugin("fresh", { revision: 2 });
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async (_input, init) => {
      if (init?.method === "POST") return jsonResponse(installed);
      return jsonResponse({ plugins: [] });
    });
    component = mount(PluginsPanel, { target: document.body, props: { topicId: null } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("No plugins are installed"));

    const input = document.querySelector<HTMLInputElement>('input[type="text"]')!;
    input.value = "/home/ben/plugins/fresh";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    await tick();
    const submit = document.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    expect(submit.disabled).toBe(false);
    flushSync(() => submit.click());
    await vi.waitFor(() => expect(document.querySelector('[aria-label="fresh details"]')).not.toBeNull());

    const post = fetchSpy.mock.calls.find(([, init]) => init?.method === "POST");
    expect(post?.[0]).toBe("/api/plugins");
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({ source_path: "/home/ben/plugins/fresh" });
    expect(document.querySelector('[aria-label="fresh details"]')?.textContent).toContain("revision 2");
  });
});
