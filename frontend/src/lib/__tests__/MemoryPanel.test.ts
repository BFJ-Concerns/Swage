// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import MemoryPanel from "../components/MemoryPanel.svelte";
import { writable, fromStore } from "svelte/store";
import type { Topic } from "../types";

type MountedComponent = ReturnType<typeof mount>;

const TOPIC_ID = "7d0c6f3e-1111-4a2b-9c3d-000000000001";
const SECOND_TOPIC_ID = "7d0c6f3e-1111-4a2b-9c3d-000000000002";
const topics = [
  { id: TOPIC_ID, name: "Garden", slug: "garden" },
  { id: SECOND_TOPIC_ID, name: "Kitchen", slug: "kitchen" },
] as unknown as Topic[];

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status });
}

// A gateway double: page listings per scope, page bodies by scope and
// reference, and an instruction handler the test supplies.
function gateway(options: {
  general: Array<{ reference: string; summary: string | null }>;
  topic: Array<{ reference: string; summary: string | null }>;
  secondTopic?: Array<{ reference: string; summary: string | null }>;
  bodies: Record<string, string>;
  instruct?: (body: unknown) => Response | Promise<Response>;
}) {
  return vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const url = new URL(String(input), "http://swage.test");
    const topicId = url.searchParams.get("topic_id");
    if (url.pathname === "/api/memory/pages") {
      if (topicId === SECOND_TOPIC_ID) {
        return json({ scope: { kind: "topic", topic_id: topicId }, pages: options.secondTopic ?? [] });
      }
      return topicId
        ? json({ scope: { kind: "topic", topic_id: topicId }, pages: options.topic })
        : json({ scope: { kind: "general" }, pages: options.general });
    }
    if (url.pathname === "/api/memory/page") {
      const reference = url.searchParams.get("reference") ?? "";
      const body = options.bodies[`${topicId ?? "general"}:${reference}`];
      return body === undefined
        ? json({ error: "not found" }, 404)
        : json({ reference, summary: null, body });
    }
    if (url.pathname === "/api/memory/instructions" && init?.method === "POST") {
      return options.instruct?.(JSON.parse(String(init.body))) ?? json({ error: "unexpected" }, 500);
    }
    return json({ error: `unexpected ${url.pathname}` }, 500);
  });
}

function buttonNamed(text: string): HTMLButtonElement | undefined {
  return Array.from(document.querySelectorAll("button")).find((button) => button.textContent?.includes(text));
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => { resolve = settle; });
  return { promise, resolve };
}

function enterInstruction(text: string, topicId?: string) {
  flushSync(() => {
    if (topicId !== undefined) {
      const scope = document.querySelector<HTMLSelectElement>('select[aria-label="Memory to instruct about"]')!;
      scope.value = topicId;
      scope.dispatchEvent(new Event("change"));
    }
    const input = document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Instruction to the curator"]')!;
    input.value = text;
    input.dispatchEvent(new Event("input"));
  });
  flushSync(() => buttonNamed("Send to curator")!.click());
}

describe("MemoryPanel", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
    vi.restoreAllMocks();
  });

  it("lists every page of the general scope and of each topic, and opens one to read", async () => {
    gateway({
      general: [
        { reference: "general/profile", summary: "Who the operator is." },
        { reference: "general/people/alice", summary: "Alice, the operator's sister." },
      ],
      topic: [{ reference: "topic/conventions", summary: "How beds are labelled." }],
      secondTopic: [{ reference: "topic/stock", summary: "What the pantry holds." }],
      bodies: { [`${TOPIC_ID}:topic/conventions`]: "Beds are labelled north to south." },
    });
    component = mount(MemoryPanel, { target: document.body, props: { topics, page: null } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("topic/conventions"));

    const general = document.querySelector('[aria-label="General memory pages"]');
    const garden = document.querySelector('[aria-label="Garden memory pages"]');
    expect(general?.textContent).toContain("general/profile");
    expect(general?.textContent).toContain("general/people/alice");
    expect(garden?.textContent).toContain("topic/conventions");
    expect(garden?.textContent).toContain("How beds are labelled.");
    const kitchen = document.querySelector('[aria-label="Kitchen memory pages"]');
    expect(kitchen?.textContent).toContain("topic/stock");
    expect(kitchen?.textContent).not.toContain("topic/conventions");
    expect(garden?.textContent).not.toContain("topic/stock");

    flushSync(() => buttonNamed("topic/conventions")?.click());
    await vi.waitFor(() =>
      expect(document.querySelector('[aria-label="Memory page topic/conventions"]')?.textContent)
        .toContain("Beds are labelled north to south."),
    );
    const page = document.querySelector('[aria-label="Memory page topic/conventions"]');
    expect(page?.querySelector("input, textarea, [contenteditable]")).toBeNull();
  });

  it("opens the page a notice link names", async () => {
    gateway({
      general: [
        { reference: "general/profile", summary: null },
        { reference: "general/people/alice", summary: null },
      ],
      topic: [],
      bodies: {
        "general:general/profile": "Ben works in Leeds.",
        "general:general/people/alice": "Alice is the operator's sister (conversation abc, 2026-10-04).",
      },
    });
    component = mount(MemoryPanel, {
      target: document.body,
      props: { topics, page: { scope: null, reference: "general/people/alice" } },
    });

    await vi.waitFor(() =>
      expect(document.querySelector('[aria-label="Memory page general/people/alice"]')?.textContent)
        .toContain("Alice is the operator's sister"),
    );
    expect(document.body.textContent).not.toContain("Ben works in Leeds.");
  });

  it("opens a notice link's page from the scope it names when two topics share the reference", async () => {
    gateway({
      general: [],
      topic: [{ reference: "topic/conventions", summary: null }],
      secondTopic: [{ reference: "topic/conventions", summary: null }],
      bodies: {
        [`${TOPIC_ID}:topic/conventions`]: "Beds are labelled north to south.",
        [`${SECOND_TOPIC_ID}:topic/conventions`]: "Jars are labelled with the date opened.",
      },
    });
    component = mount(MemoryPanel, {
      target: document.body,
      props: { topics, page: { scope: SECOND_TOPIC_ID, reference: "topic/conventions" } },
    });

    await vi.waitFor(() =>
      expect(document.querySelector('[aria-label="Memory page topic/conventions"]')?.textContent)
        .toContain("Jars are labelled with the date opened."),
    );
    expect(document.body.textContent).not.toContain("Beds are labelled north to south.");
  });

  it("sends an instruction to the curator for the chosen scope and shows its reply and the changed page", async () => {
    let sent: unknown;
    const topicPages = [{ reference: "topic/conventions", summary: null as string | null }];
    gateway({
      general: [{ reference: "general/profile", summary: null }],
      topic: topicPages,
      bodies: { "general:general/profile": "Ben takes Fridays off." },
      instruct: (body) => {
        sent = body;
        topicPages.push({ reference: "topic/harvest", summary: null });
        return json({
          reply: "Removed the note about Fridays.",
          pages_changed: [
            { reference: "topic/conventions", summary: null, body: "Beds are labelled south to north." },
            { reference: "topic/harvest", summary: null, body: "Beans are picked in August." },
          ],
          curator_session_id: "7d0c6f3e-1111-4a2b-9c3d-0000000000aa",
          turn_id: "7d0c6f3e-1111-4a2b-9c3d-0000000000bb",
        });
      },
    });
    component = mount(MemoryPanel, { target: document.body, props: { topics, page: null } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("topic/conventions"));

    const scope = document.querySelector<HTMLSelectElement>('select[aria-label="Memory to instruct about"]');
    const input = document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Instruction to the curator"]');
    expect(scope).not.toBeNull();
    expect(input).not.toBeNull();
    flushSync(() => {
      scope!.value = TOPIC_ID;
      scope!.dispatchEvent(new Event("change"));
      input!.value = "Beds run south to north now";
      input!.dispatchEvent(new Event("input"));
    });
    flushSync(() => buttonNamed("Send to curator")?.click());

    await vi.waitFor(() => expect(document.body.textContent).toContain("Removed the note about Fridays."));
    expect(sent).toEqual({
      scope: { kind: "topic", topic_id: TOPIC_ID },
      instruction: "Beds run south to north now",
    });
    const changed = document.querySelector('[aria-label="Pages the curator changed"]');
    expect(changed?.textContent).toContain("topic/conventions");
    expect(changed?.textContent).toContain("Beds are labelled south to north.");
    expect(changed?.textContent).toContain("Beans are picked in August.");
    // Each changed page is shown under its own reference.
    const articles = Array.from(changed?.querySelectorAll("article") ?? []).map((article) => [
      article.querySelector("code")?.textContent,
      article.querySelector("pre")?.textContent,
    ]);
    expect(articles).toEqual([
      ["topic/conventions", "Beds are labelled south to north."],
      ["topic/harvest", "Beans are picked in August."],
    ]);
    // A page the instruction created joins the listing without a reload.
    await vi.waitFor(() =>
      expect(document.querySelector('[aria-label="Garden memory pages"]')?.textContent).toContain("topic/harvest"),
    );
  });

  it("says when an instruction changed no page, and shows a refused instruction's error", async () => {
    let calls = 0;
    gateway({
      general: [],
      topic: [],
      bodies: {},
      instruct: () => {
        calls += 1;
        return calls === 1
          ? json({ reply: "Nothing matched.", pages_changed: [], curator_session_id: "a", turn_id: "b" })
          : json({ error: "curator pass failed" }, 502);
      },
    });
    component = mount(MemoryPanel, { target: document.body, props: { topics: [], page: null } });
    await vi.waitFor(() => expect(document.body.textContent).toContain("No memory pages yet"));

    const input = document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Instruction to the curator"]')!;
    flushSync(() => {
      input.value = "forget my birthday";
      input.dispatchEvent(new Event("input"));
    });
    flushSync(() => buttonNamed("Send to curator")?.click());
    await vi.waitFor(() => expect(document.body.textContent).toContain("Nothing matched."));
    expect(document.body.textContent).toContain("No page changed.");

    flushSync(() => {
      input.value = "forget my birthday";
      input.dispatchEvent(new Event("input"));
    });
    flushSync(() => buttonNamed("Send to curator")?.click());
    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')).not.toBeNull());
    expect(document.querySelector('[role="alert"]')?.textContent).toMatch(/curator pass failed|502/);
  });
  it.each([200, 502])("ignores an older page read completing with status %s after a newer selection", async (status) => {
    const late = deferred<Response>();
    const fetch = gateway({
      general: [{ reference: "general/profile", summary: null }],
      topic: [{ reference: "topic/conventions", summary: null }],
      bodies: { [`${TOPIC_ID}:topic/conventions`]: "Current selection" },
    });
    const serve = fetch.getMockImplementation()!;
    fetch.mockImplementation((input, init) => {
      const url = new URL(String(input), "http://swage.test");
      return url.pathname === "/api/memory/page" && url.searchParams.get("reference") === "general/profile"
        ? late.promise : serve(input, init);
    });
    component = mount(MemoryPanel, { target: document.body, props: { topics, page: null } });
    await vi.waitFor(() => expect(buttonNamed("general/profile")).toBeDefined());
    flushSync(() => buttonNamed("general/profile")!.click());
    flushSync(() => buttonNamed("topic/conventions")!.click());
    await vi.waitFor(() => expect(document.querySelector(".detail pre")?.textContent).toBe("Current selection"));
    late.resolve(json(status === 200
      ? { reference: "general/profile", body: "Obsolete selection" }
      : { error: "Obsolete read failed" }, status));
    await late.promise;
    await new Promise((resolve) => setTimeout(resolve, 20));
    flushSync();
    expect(document.querySelector(".detail pre")?.textContent).toBe("Current selection");
    expect(document.querySelector('[role="alert"]')).toBeNull();
  });

  it("invalidates an in-flight read when a notice target changes", async () => {
    const late = deferred<Response>();
    const fetch = gateway({
      general: [{ reference: "general/profile", summary: null }],
      topic: [{ reference: "topic/conventions", summary: null }],
      bodies: { [`${TOPIC_ID}:topic/conventions`]: "Notice target" },
    });
    const serve = fetch.getMockImplementation()!;
    fetch.mockImplementation((input, init) => String(input).includes("/api/memory/page?") && String(input).includes("general%2Fprofile")
      ? late.promise : serve(input, init));
    const targetStore = writable({ scope: "general", reference: "general/profile" });
    const target = fromStore(targetStore);
    component = mount(MemoryPanel, {
      target: document.body,
      props: { topics, get page() { return target.current; } },
    });
    await vi.waitFor(() => expect(fetch.mock.calls.some(([input]) => String(input).includes("general%2Fprofile"))).toBe(true));
    flushSync(() => { targetStore.set({ scope: TOPIC_ID, reference: "topic/conventions" }); });
    await vi.waitFor(() => expect(document.querySelector(".detail pre")?.textContent).toBe("Notice target"));
    late.resolve(json({ reference: "general/profile", body: "Obsolete notice" }));
    await late.promise;
    await new Promise((resolve) => setTimeout(resolve, 20));
    flushSync();
    expect(document.querySelector(".detail pre")?.textContent).toBe("Notice target");
  });

  it("keeps successful curation and its open page when the following listing fails", async () => {
    let curated = false;
    const fetch = gateway({
      general: [{ reference: "general/profile", summary: null }], topic: [],
      bodies: { "general:general/profile": "Old profile" },
      instruct: () => {
        curated = true;
        return json({ reply: "Updated successfully", pages_changed: [{ reference: "general/profile", body: "Current profile" }] });
      },
    });
    const serve = fetch.getMockImplementation()!;
    fetch.mockImplementation((input, init) => curated && new URL(String(input), "http://swage.test").pathname === "/api/memory/pages"
      ? Promise.resolve(json({ error: "Listing unavailable" }, 503)) : serve(input, init));
    component = mount(MemoryPanel, { target: document.body, props: { topics: [], page: { scope: "general", reference: "general/profile" } } });
    await vi.waitFor(() => expect(document.querySelector(".detail > article pre")?.textContent).toBe("Old profile"));
    enterInstruction("Update my profile");
    await vi.waitFor(() => expect(document.body.textContent).toContain("Listing unavailable"));
    expect(document.querySelector(".reply")?.textContent).toBe("Updated successfully");
    expect(document.querySelector(".detail > article pre")?.textContent).toBe("Current profile");
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("General");
  });

  it("refreshes an open general page changed by an instruction sent to a topic", async () => {
    let sent: unknown;
    gateway({
      general: [{ reference: "general/profile", summary: null }], topic: [],
      bodies: { "general:general/profile": "Old profile" },
      instruct: (body) => {
        sent = body;
        return json({ reply: "Updated", pages_changed: [{ reference: "general/profile", body: "Current profile" }] });
      },
    });
    component = mount(MemoryPanel, { target: document.body, props: { topics, page: { scope: "general", reference: "general/profile" } } });
    await vi.waitFor(() => expect(document.querySelector(".detail > article pre")?.textContent).toBe("Old profile"));
    enterInstruction("Update my profile", TOPIC_ID);
    await vi.waitFor(() => expect(document.querySelector(".reply")?.textContent).toBe("Updated"));
    expect(sent).toEqual({ scope: { kind: "topic", topic_id: TOPIC_ID }, instruction: "Update my profile" });
    await vi.waitFor(() => expect(document.querySelector(".detail > article pre")?.textContent).toBe("Current profile"));
  });

  it("shows healthy scope listings when another scope fails", async () => {
    const fetch = gateway({
      general: [{ reference: "general/profile", summary: null }],
      topic: [], secondTopic: [{ reference: "topic/stock", summary: null }], bodies: {},
    });
    const serve = fetch.getMockImplementation()!;
    fetch.mockImplementation((input, init) => new URL(String(input), "http://swage.test").searchParams.get("topic_id") === TOPIC_ID
      ? Promise.resolve(json({ error: "Garden unavailable" }, 503)) : serve(input, init));
    component = mount(MemoryPanel, { target: document.body, props: { topics, page: null } });
    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain("Garden unavailable"));
    expect(document.querySelector('[aria-label="General memory pages"]')?.textContent ?? "").toContain("general/profile");
    expect(document.querySelector('[aria-label="Kitchen memory pages"]')?.textContent ?? "").toContain("topic/stock");
    expect(document.body.textContent).not.toContain("No memory pages yet");
  });

  it.each([false, true])("targets the opened page's scope for instructions (notice: %s)", async (notice) => {
    let sent: unknown;
    gateway({
      general: [], topic: [{ reference: "topic/conventions", summary: null }],
      bodies: { [`${TOPIC_ID}:topic/conventions`]: "Topic page" },
      instruct: (body) => { sent = body; return json({ reply: "Done", pages_changed: [] }); },
    });
    component = mount(MemoryPanel, { target: document.body, props: { topics, page: notice ? { scope: TOPIC_ID, reference: "topic/conventions" } : null } });
    await vi.waitFor(() => expect(buttonNamed("topic/conventions")).toBeDefined());
    if (!notice) flushSync(() => buttonNamed("topic/conventions")!.click());
    await vi.waitFor(() => expect(document.querySelector(".detail > article pre")?.textContent).toBe("Topic page"));
    enterInstruction("Change this convention");
    await vi.waitFor(() => expect(sent).toBeDefined());
    expect(sent).toEqual({ scope: { kind: "topic", topic_id: TOPIC_ID }, instruction: "Change this convention" });
  });

  it("keeps a curated page when a read started before the edit finishes later", async () => {
    const late = deferred<Response>();
    const fetch = gateway({
      general: [{ reference: "general/profile", summary: null }], topic: [], bodies: {},
      instruct: () => json({ reply: "Updated", pages_changed: [{ reference: "general/profile", body: "Current profile" }] }),
    });
    const serve = fetch.getMockImplementation()!;
    fetch.mockImplementation((input, init) => new URL(String(input), "http://swage.test").pathname === "/api/memory/page"
      ? late.promise : serve(input, init));
    component = mount(MemoryPanel, { target: document.body, props: { topics: [], page: null } });
    await vi.waitFor(() => expect(buttonNamed("general/profile")).toBeDefined());
    flushSync(() => buttonNamed("general/profile")!.click());
    enterInstruction("Update my profile");
    await vi.waitFor(() => expect(document.querySelector(".reply")?.textContent).toBe("Updated"));
    late.resolve(json({ reference: "general/profile", body: "Before the edit" }));
    await late.promise;
    await new Promise((resolve) => setTimeout(resolve, 20));
    flushSync();
    expect(document.querySelector(".detail > article pre")?.textContent).toBe("Current profile");
  });

  it("returns instruction targeting to general memory when a general page is selected", async () => {
    let sent: unknown;
    gateway({
      general: [{ reference: "general/profile", summary: null }],
      topic: [{ reference: "topic/conventions", summary: null }],
      bodies: { "general:general/profile": "General page", [`${TOPIC_ID}:topic/conventions`]: "Topic page" },
      instruct: (body) => { sent = body; return json({ reply: "Done", pages_changed: [] }); },
    });
    component = mount(MemoryPanel, { target: document.body, props: { topics, page: null } });
    await vi.waitFor(() => expect(buttonNamed("topic/conventions")).toBeDefined());
    // An explicit topic choice predates opening the general page.
    flushSync(() => {
      const scope = document.querySelector<HTMLSelectElement>('select[aria-label="Memory to instruct about"]')!;
      scope.value = TOPIC_ID;
      scope.dispatchEvent(new Event("change"));
      buttonNamed("general/profile")!.click();
    });
    await vi.waitFor(() => expect(document.querySelector(".detail > article pre")?.textContent).toBe("General page"));
    enterInstruction("Update this profile");
    await vi.waitFor(() => expect(sent).toBeDefined());
    expect(sent).toEqual({ scope: { kind: "general" }, instruction: "Update this profile" });
  });

});
