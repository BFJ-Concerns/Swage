// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { createClassComponent } from "svelte/legacy";
import MessageInput from "../components/MessageInput.svelte";
import MessageDisplay from "../components/MessageDisplay.svelte";
import { createStreamingStore, withConversationAutoCreate } from "../stores/streaming";
import { createConversationStore } from "../stores/conversations";
import type { CreateMaterialRequest } from "../types";

vi.mock("../api/conversations", () => ({
  getConversation: vi.fn(async (id: string) => ({ id, agent_id: "assistant", topic_id: null, messages: [], created_at: "2026-10-15T00:00:00Z", last_activity: "2026-10-15T00:00:00Z" })),
  listBranches: vi.fn(async () => []),
}));

const mounted: (() => void)[] = [];
afterEach(() => {
  mounted.splice(0).forEach((dispose) => dispose());
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
});

// The gateway's materials routes: each session starts with none, a POST
// registers one, and archive removes it from the session's active list.
function materialServer() {
  const created: CreateMaterialRequest[] = [];
  const archived: string[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: string, init?: RequestInit) => {
    if (init?.method === "POST" && url.endsWith("/api/materials")) {
      const body = JSON.parse(init.body as string) as CreateMaterialRequest;
      created.push(body);
      return new Response(JSON.stringify({ ...body, id: `material-${created.length}`, status: "active" }), { status: 200 });
    }
    if (init?.method === "POST" && url.endsWith("/archive")) {
      archived.push(url.split("/").at(-2)!);
      return new Response(JSON.stringify({ status: "archived" }), { status: 200 });
    }
    return new Response(JSON.stringify({ materials: [] }), { status: 200 });
  }));
  return { created, archived };
}

async function setup(fresh = false) {
  const server = materialServer();
  const conversations = createConversationStore();
  if (!fresh) await conversations.selectConversation("session-one");
  const sendMessage = vi.fn();
  const base = createStreamingStore({ conversationStore: conversations, connectionStore: { sendMessage } });
  let finishCreation: (() => void) | null = null;
  const createConversation = vi.fn(async (): Promise<string | null> => {
    // Creation completes when the test says so, as a gateway round trip would.
    await new Promise<void>((resolve) => { finishCreation = resolve; });
    await conversations.selectConversation("session-created");
    input.$set({ sessionId: "session-created" });
    flushSync();
    return "session-created";
  });
  const stream = withConversationAutoCreate(base, {
    hasActiveConversation: () => !!conversations.activeConversation,
    createConversation,
  });
  const input = createClassComponent({ component: MessageInput, target: document.body, props: {
    store: stream, sessionId: fresh ? null : "session-one", modelOptions: ["vision-model"],
  } });
  mounted.push(() => input.$destroy());
  flushSync();
  const finish = async () => {
    await vi.waitFor(() => expect(finishCreation).not.toBeNull());
    finishCreation!();
  };
  return { input, stream, conversations, sendMessage, createConversation, finish, ...server };
}

function choose(files: File[], control = "Attach files") {
  const button = Array.from(document.querySelectorAll("button")).find((candidate) => candidate.textContent?.trim() === control);
  expect(button, `the composer offers ${control}`).toBeDefined();
  const input = document.querySelector<HTMLInputElement>(`input[type="file"][aria-label="${control === "Attach folder" ? "Attach a folder" : "Attach images or files"}"]`)!;
  const openPicker = vi.spyOn(input, "click");
  flushSync(() => button!.click());
  expect(openPicker).toHaveBeenCalledTimes(1);
  Object.defineProperty(input, "files", { configurable: true, value: files });
  flushSync(() => input.dispatchEvent(new Event("change", { bubbles: true })));
}

function inFolder(path: string, body: BlobPart, type = ""): File {
  const file = new File([body], path.split("/").at(-1)!, { type });
  Object.defineProperty(file, "webkitRelativePath", { value: path });
  return file;
}

function paste(file = new File(["picture"], "pasted.png", { type: "image/png" })) {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", { value: { items: [{ kind: "file", type: file.type, getAsFile: () => file }] } });
  flushSync(() => document.querySelector("textarea")!.dispatchEvent(event));
}

function send(text = "") {
  const area = document.querySelector("textarea")!;
  flushSync(() => {
    area.value = text;
    area.dispatchEvent(new Event("input", { bubbles: true }));
    area.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  });
}

async function previews(count: number): Promise<string[]> {
  await vi.waitFor(() => expect(document.querySelectorAll(".attached-image img")).toHaveLength(count));
  return Array.from(document.querySelectorAll<HTMLImageElement>(".attached-image img")).map((image) => image.src);
}

function remove(name: string) {
  flushSync(() => document.querySelector<HTMLButtonElement>(`[aria-label="Remove ${name}"]`)!.click());
}

describe("composer attachments", () => {
  it("sends a chosen image's bytes with the message and clears it once sent", async () => {
    const { sendMessage } = await setup();
    choose([new File(["selected picture"], "chosen.png", { type: "image/png" })]);
    const [url] = await previews(1);
    expect(url).toBe(`data:image/png;base64,${btoa("selected picture")}`);
    send();
    expect(sendMessage).toHaveBeenCalledWith("session-one", "", "vision-model", [{ kind: "image_url", url }], null, expect.any(String));
    expect(document.querySelector(".attached-image")).toBeNull();
  });

  it("takes a pasted image off again before sending", async () => {
    const { sendMessage } = await setup();
    paste();
    await previews(1);
    remove("pasted.png");
    send("text only");
    expect(sendMessage.mock.calls[0]?.[3]).toBeUndefined();
  });

  it("refuses an image that is not PNG or JPEG", async () => {
    const { created } = await setup();
    paste(new File(["gif"], "moving.gif", { type: "image/gif" }));
    await vi.waitFor(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain("moving.gif is not a PNG or JPEG image."));
    expect(created).toEqual([]);
  });

  it("registers a text file as the session's material and takes it off again", async () => {
    const { created, archived, sendMessage } = await setup();
    choose([new File(["# Plan\nLaunch Tuesday"], "plan.md")]);
    await vi.waitFor(() => expect(document.body.textContent).toContain("plan.md · reference for this session"));
    expect(created).toEqual([expect.objectContaining({ scope_kind: "session", session_id: "session-one", title: "plan.md", mime_type: "text/markdown", content_text: "# Plan\nLaunch Tuesday" })]);
    remove("plan.md");
    await vi.waitFor(() => expect(document.body.textContent).not.toContain("plan.md"));
    expect(archived).toEqual(["material-1"]);
    send("What does the plan say?");
    expect(sendMessage.mock.calls[0]?.[3]).toBeUndefined();
  });

  it("keeps a file registered for the previous session out of the next session's list", async () => {
    const { input, conversations } = await setup();
    let answer!: () => void;
    const answered = new Promise<void>((resolve) => { answer = resolve; });
    vi.stubGlobal("fetch", vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === "POST") {
        await answered;
        return new Response(JSON.stringify({ ...JSON.parse(init.body as string), id: "late", status: "active" }), { status: 200 });
      }
      return new Response(JSON.stringify({ materials: [] }), { status: 200 });
    }));
    choose([new File(["notes"], "late.md")]);
    await vi.waitFor(() => expect(vi.mocked(fetch).mock.calls.some(([, init]) => init?.method === "POST")).toBe(true));
    await conversations.selectConversation("session-two");
    input.$set({ sessionId: "session-two" });
    flushSync();
    answer();
    await vi.waitFor(() => expect(document.body.textContent).not.toContain("Attaching…"));
    expect(document.body.textContent).not.toContain("late.md");
  });

  it("registers a folder's text files by their paths and skips the rest", async () => {
    const { created } = await setup();
    choose([
      inFolder("notes/plan.md", "# Plan"),
      inFolder("notes/src/main.rs", "fn main() {}"),
      inFolder("notes/logo.png", new Uint8Array([0x89, 0x50, 0x00, 0x47]), "image/png"),
      inFolder("notes/.git/HEAD", "ref: refs/heads/main"),
    ], "Attach folder");
    await vi.waitFor(() => expect(document.body.textContent).toContain("2 files attached from the folder; 2 skipped"));
    expect(created.map(({ title, mime_type }) => [title, mime_type])).toEqual([
      ["notes/plan.md", "text/markdown"],
      ["notes/src/main.rs", "text/plain"],
    ]);
    expect(created[1].relative_path).toMatch(/^composer\/[^/]+\/notes\/src\/main\.rs$/);
  });

  it("keeps a second unsent image across the creation of the session the first one starts", async () => {
    const { sendMessage, createConversation, finish } = await setup(true);
    paste(new File(["first"], "first.png", { type: "image/png" }));
    const [first] = await previews(1);
    send("the first");
    paste(new File(["second"], "second.png", { type: "image/png" }));
    const [, second] = await previews(2);
    await finish();
    await vi.waitFor(() => expect(sendMessage).toHaveBeenCalledWith("session-created", "the first", "vision-model", [{ kind: "image_url", url: first }], null, expect.any(String)));
    await vi.waitFor(() => expect(document.querySelectorAll(".attached-image img")).toHaveLength(1));
    expect(document.querySelector<HTMLImageElement>(".attached-image img")!.src).toBe(second);
    expect(createConversation).toHaveBeenCalledTimes(1);
  });

  it("creates a session for a file chosen before the first message and keeps a pending image", async () => {
    const { created, finish, sendMessage } = await setup(true);
    paste();
    const [url] = await previews(1);
    choose([new File(["hello"], "note.txt", { type: "text/plain" })]);
    await finish();
    await vi.waitFor(() => expect(document.body.textContent).toContain("note.txt · reference for this session"));
    expect(created[0].session_id).toBe("session-created");
    send("read both");
    expect(sendMessage.mock.calls[0]?.slice(0, 4)).toEqual(["session-created", "read both", "vision-model", [{ kind: "image_url", url }]]);
  });

  it("clears pending images when the composer moves to another session", async () => {
    const { input, sendMessage, conversations } = await setup();
    paste();
    await previews(1);
    await conversations.selectConversation("session-two");
    input.$set({ sessionId: "session-two" });
    flushSync();
    send("another session");
    expect(sendMessage.mock.calls[0]?.slice(0, 1)).toEqual(["session-two"]);
    expect(sendMessage.mock.calls[0]?.[3]).toBeUndefined();
  });
});

describe("images in the conversation", () => {
  const image = { kind: "image_url" as const, url: "data:image/png;base64,cGljdHVyZQ==" };

  it("shows an image sent while a turn runs on its queued message", async () => {
    const { conversations, stream } = await setup();
    // The running turn's request is already in the conversation.
    conversations.replaceActiveConversationIfCurrent!({ ...conversations.activeConversation!, messages: [{ role: "user", origin: "user", content: "look at this", timestamp: "2026-10-15T00:00:00Z", turn_id: "turn" }] });
    const display = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: stream } });
    mounted.push(() => { void unmount(display); });
    flushSync(() => stream.handleServerEvent({ type: "message_queued", session_id: "session-one", queued: { id: "held", origin: "user", content: "and this", attachments: [image], queued_at: "2026-10-15T00:00:00Z" } }, "session-one"));
    const queued = document.querySelector('[aria-label="Queued message"]');
    expect(queued?.querySelector<HTMLImageElement>("img")?.src).toBe(image.url);
  });

  it("shows a sent image on the person's message", async () => {
    const { conversations, stream } = await setup();
    const display = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: stream } });
    mounted.push(() => { void unmount(display); });
    paste();
    await previews(1);
    send();
    flushSync();
    expect(document.querySelector('[aria-label="Your message"] img')).not.toBeNull();
  });

  it("resends a failed request's images on Retry", async () => {
    const { conversations, stream, sendMessage } = await setup();
    conversations.replaceActiveConversationIfCurrent!({ ...conversations.activeConversation!, messages: [{ role: "user", origin: "user", content: "", attachments: [image], timestamp: "2026-10-15T00:00:00Z", turn_id: "turn", turn_state: "failed" }] });
    const display = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: stream } });
    mounted.push(() => { void unmount(display); });
    flushSync();
    const retry = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find((button) => button.textContent?.trim() === "Retry");
    flushSync(() => retry!.click());
    expect(sendMessage.mock.calls.at(-1)?.[3]).toEqual([image]);
  });
});
