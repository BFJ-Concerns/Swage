// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import MemoryPanel from "../components/MemoryPanel.svelte";
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
  instruct?: (body: unknown) => Response;
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
    component = mount(MemoryPanel, { target: document.body, props: { topics, reference: null } });
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
      props: { topics, reference: "general/people/alice" },
    });

    await vi.waitFor(() =>
      expect(document.querySelector('[aria-label="Memory page general/people/alice"]')?.textContent)
        .toContain("Alice is the operator's sister"),
    );
    expect(document.body.textContent).not.toContain("Ben works in Leeds.");
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
    component = mount(MemoryPanel, { target: document.body, props: { topics, reference: null } });
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
    expect(changed?.querySelectorAll("article")).toHaveLength(2);
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
    component = mount(MemoryPanel, { target: document.body, props: { topics: [], reference: null } });
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
});
