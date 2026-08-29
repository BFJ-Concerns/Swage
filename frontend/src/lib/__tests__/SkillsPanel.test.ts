// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, tick, unmount } from "svelte";
import SkillsPanel from "../components/SkillsPanel.svelte";
import type { SkillItem } from "../protocol/SkillItem";

type MountedComponent = ReturnType<typeof mount>;

function skill(name: string, overrides: Partial<SkillItem> = {}): SkillItem {
  return { id: null, name, display_name: null, description: "", level: "global", tools: [], always_active: false, shipped: false, ...overrides };
}

describe("SkillsPanel", () => {
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
    component = mount(SkillsPanel, { target: document.body, props: { spaceId: "space-1" } });
    await tick();

    expect(document.body.textContent).toContain("Loading skills");
    rejectRequest?.(new Error("catalogue unavailable"));
    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')).not.toBeNull());
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("catalogue unavailable");
  });

  it("lists resolved skills, shows shipped detail, and performs no mutation", async () => {
    const shipped = skill("research", { id: "shipped-space-1-research", display_name: "Research", description: "Gather primary sources.", level: "space", tools: ["web_search", "file_read"], always_active: true, shipped: true });
    const selected = skill("notes", { display_name: "Notes", description: "Maintain project notes.", tools: ["file_write"] });
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({ skills: [shipped, selected] }), { status: 200 }));
    component = mount(SkillsPanel, { target: document.body, props: { spaceId: "space id/1" } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("Research"));

    expect(fetchSpy).toHaveBeenCalledWith("/api/skills?space_id=space%20id%2F1", { method: "GET" });
    const research = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.includes("Research"));
    const notes = Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.includes("Notes"));
    expect(research?.querySelector(".shipped")?.textContent).toBe("Shipped");
    expect(notes?.querySelector(".shipped")).toBeNull();
    flushSync(() => research?.click());
    const shippedDetail = document.querySelector('[aria-label="Research details"]');
    expect(shippedDetail?.textContent).toContain("Shipped with Swage");
    expect(shippedDetail?.querySelector(".shipped")?.textContent).toBe("Shipped");
    flushSync(() => notes?.click());
    const detail = document.querySelector('[aria-label="Notes details"]');
    expect(detail?.textContent).toContain("Maintain project notes.");
    expect(detail?.textContent).toContain("file_write");
    expect(detail?.textContent).toContain("Available on invocation");
    expect(detail?.textContent).toContain("Operator-defined");
    expect(detail?.querySelector("button, input, select, textarea")).toBeNull();
    expect(detail?.textContent).not.toContain("variant");
  });

  it("shows an empty result without a selected detail", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({ skills: [] }), { status: 200 }));
    component = mount(SkillsPanel, { target: document.body, props: { spaceId: "space-1" } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("No skills are available"));

    expect(document.querySelector("article")).toBeNull();
  });
});
