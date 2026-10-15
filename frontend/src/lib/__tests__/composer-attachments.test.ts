// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, tick, unmount } from "svelte";
import { createClassComponent } from "svelte/legacy";
import MessageInput from "../components/MessageInput.svelte";
import MessageDisplay from "../components/MessageDisplay.svelte";
import { createStreamingStore, withConversationAutoCreate } from "../stores/streaming";
import { createConversationStore } from "../stores/conversations";
import type { CreateMaterialRequest, MaterialRecord } from "../types";

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

async function setup(fresh = false) {
  const conversations = createConversationStore();
  if (!fresh) await conversations.selectConversation("session-one");
  const sendMessage = vi.fn();
  const base = createStreamingStore({ conversationStore: conversations, connectionStore: { sendMessage, cancelTurn: vi.fn() } });
  const createConversation = vi.fn(async (): Promise<string | null> => {
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
  return { input, stream, conversations, sendMessage, createConversation };
}

function choose(file: File) {
  const input = document.querySelector<HTMLInputElement>('input[type="file"]');
  expect(input, "the composer must offer file selection").not.toBeNull();
  const button = Array.from(document.querySelectorAll("button")).find((candidate) => candidate.textContent?.trim() === "Attach");
  expect(button, "file selection must have a visible control").toBeDefined();
  const openPicker = vi.spyOn(input!, "click");
  flushSync(() => button!.click());
  expect(openPicker).toHaveBeenCalledTimes(1);
  openPicker.mockRestore();
  Object.defineProperty(input!, "files", { configurable: true, value: [file] });
  flushSync(() => input!.dispatchEvent(new Event("change", { bubbles: true })));
}

function paste(file = new File(["picture"], "pasted.png", { type: "image/png" })) {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", { value: { items: [{ kind: "file", type: file.type, getAsFile: () => file }] } });
  flushSync(() => document.querySelector("textarea")!.dispatchEvent(event));
  return event;
}

function send(text = "") {
  const area = document.querySelector("textarea")!;
  flushSync(() => {
    area.value = text;
    area.dispatchEvent(new Event("input", { bubbles: true }));
    area.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  });
}

async function preview() {
  await vi.waitFor(() => expect(document.querySelector(".attached-image img")).not.toBeNull());
  return document.querySelector<HTMLImageElement>(".attached-image img")!.src;
}

function materialServer() {
  const requests: CreateMaterialRequest[] = [];
  vi.stubGlobal("fetch", vi.fn(async (_url: string, init: RequestInit) => {
    const body = JSON.parse(init.body as string) as CreateMaterialRequest;
    requests.push(body);
    return new Response(JSON.stringify({ ...body, id: "registered-material", status: "active" } satisfies Partial<MaterialRecord>), { status: 200 });
  }));
  return requests;
}

describe("composer attachments through the send stores", () => {
  it("selects an image and sends its bytes beside an image-only message", async () => {
    const { sendMessage } = await setup();
    choose(new File(["selected picture"], "chosen.png", { type: "image/png" }));
    const url = await preview();
    expect(url).toBe(`data:image/png;base64,${btoa("selected picture")}`);
    expect(document.body.textContent).toContain("This session");
    send();
    await vi.waitFor(() => expect(sendMessage).toHaveBeenCalledWith("session-one", "", "vision-model", [{ kind: "image_url", url }], null, expect.any(String)));
    expect(document.querySelector(".attached-image")).toBeNull();
  });

  it("pastes a preview and removes it before sending text", async () => {
    const { sendMessage } = await setup();
    expect(paste().defaultPrevented).toBe(true);
    await preview();
    flushSync(() => document.querySelector<HTMLButtonElement>('[aria-label="Remove pasted.png"]')!.click());
    expect(document.querySelector(".attached-image")).toBeNull();
    send("text only");
    expect(sendMessage.mock.calls[0]?.[3]).toBeUndefined();
  });

  it("carries a pasted image through the queued-send path", async () => {
    const { sendMessage } = await setup();
    send("first turn");
    paste();
    const url = await preview();
    send("queued picture");
    expect(sendMessage).toHaveBeenLastCalledWith("session-one", "queued picture", "vision-model", [{ kind: "image_url", url }], null, expect.any(String));
  });

  it("carries an image through session auto-creation", async () => {
    const { sendMessage, createConversation } = await setup(true);
    paste();
    const url = await preview();
    send("new session picture");
    await vi.waitFor(() => expect(sendMessage).toHaveBeenCalledWith("session-created", "new session picture", "vision-model", [{ kind: "image_url", url }], null, expect.any(String)));
    expect(createConversation).toHaveBeenCalledTimes(1);
  });

  it("registers Markdown content for this session and shows the returned material", async () => {
    const { sendMessage } = await setup();
    const requests = materialServer();
    choose(new File(["# Plan\nLaunch Tuesday"], "plan.md", { type: "" }));
    await vi.waitFor(() => expect(document.body.textContent).toContain("plan.md · Attached to this session · Reference only"));
    expect(requests).toEqual([expect.objectContaining({ scope_kind: "session", session_id: "session-one", title: "plan.md", mime_type: "text/markdown", content_text: "# Plan\nLaunch Tuesday" })]);
    expect(requests[0].topic_id).toBeUndefined();
    send("What does plan.md say?");
    expect(sendMessage.mock.calls[0]?.[3]).toBeUndefined();
  });

  it("creates a session for a file before the first turn without losing a pending image", async () => {
    const { sendMessage, createConversation } = await setup(true);
    const requests = materialServer();
    paste();
    const url = await preview();
    choose(new File(["hello"], "note.txt", { type: "text/plain" }));
    await vi.waitFor(() => expect(requests).toHaveLength(1));
    await tick();
    await vi.waitFor(() => expect(document.body.textContent).toContain("note.txt · Attached"));
    send("read both");
    await vi.waitFor(() => expect(sendMessage.mock.calls[0]?.[3]).toEqual([{ kind: "image_url", url }]));
    expect(requests[0].session_id).toBe("session-created");
    expect(createConversation).toHaveBeenCalledTimes(1);
  });

  it("shows the provider refusal in the conversation after an image send", async () => {
    const { stream, conversations, sendMessage } = await setup();
    const display = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: stream } });
    mounted.push(() => { void unmount(display); });
    paste();
    await preview();
    send();
    expect(sendMessage.mock.calls[0]?.[3]?.[0].kind).toBe("image_url");
    // The transport boundary supplies the gateway's refusal event; the real
    // streaming store and conversation renderer must make it visible.
    flushSync(() => stream.handleServerEvent({ type: "error", code: "validation_error", message: "the selected provider does not accept image attachments" }, "session-one"));
    expect((document.querySelector('[role="alert"]')?.textContent ?? "")).toContain("the selected provider does not accept image attachments");
  });

  it("refuses an unsupported pasted image visibly instead of leaving unexplained emptiness", async () => {
    const { sendMessage } = await setup();
    paste(new File(["gif"], "moving.gif", { type: "image/gif" }));
    await vi.waitFor(() => expect((document.querySelector('[role="alert"]')?.textContent ?? "")).toContain("Choose a PNG, JPEG, text or Markdown file."));
    send();
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it("clears pending images when the composer switches sessions", async () => {
    const { input, sendMessage, conversations } = await setup();
    paste();
    await preview();
    await conversations.selectConversation("session-two");
    input.$set({ sessionId: "session-two" });
    flushSync();
    send("another session");
    expect(sendMessage.mock.calls[0]?.[0]).toBe("session-two");
    expect(sendMessage.mock.calls[0]?.[3]).toBeUndefined();
    expect(document.querySelector(".attached-image")).toBeNull();
  });  // Review fix 1
  it("clears an image after switching sessions during failed file creation", async () => {
    const { input, conversations, createConversation, sendMessage } = await setup(true);
    let finish!: (id: string | null) => void;
    createConversation.mockImplementationOnce(() => new Promise<string | null>((resolve) => { finish = resolve; }));
    paste();
    await preview();
    choose(new File(["note"], "note.txt", { type: "text/plain" }));
    await vi.waitFor(() => expect(createConversation).toHaveBeenCalledTimes(1));
    await conversations.selectConversation("session-two");
    input.$set({ sessionId: "session-two" });
    flushSync();
    finish(null);
    await vi.waitFor(() => expect(document.body.textContent).not.toContain("Attaching…"));
    expect(document.querySelector(".attached-image")).toBeNull();
    send("new session text");
    expect(sendMessage.mock.calls[0]?.[3]).toBeUndefined();
  });

  // Review fix 2
  it("keeps the image draft after failed creation and sends it on retry", async () => {
    const { createConversation, sendMessage, stream } = await setup(true);
    createConversation.mockResolvedValueOnce(null);
    paste();
    const url = await preview();
    send();
    await vi.waitFor(() => expect(stream.turnError?.code).toBe("conversation_create_failed"));
    flushSync();
    expect(document.querySelector(".attached-image img")).not.toBeNull();
    send();
    await vi.waitFor(() => expect(sendMessage).toHaveBeenCalledWith("session-created", "", "vision-model", [{ kind: "image_url", url }], null, expect.any(String)));
    await vi.waitFor(() => expect(document.querySelector(".attached-image")).toBeNull());
  });

  it("names unsent images when session creation fails", async () => {
    const { createConversation, stream } = await setup(true);
    createConversation.mockResolvedValueOnce(null);
    paste();
    await preview();
    send();
    await vi.waitFor(() => expect(stream.turnError?.message).toContain("1 image"));
  });

  // Review fix 3
  it.each(["request", "partial", "refused", "live"])("retries the original images for a %s failure", async (kind) => {
    const { conversations, stream, sendMessage } = await setup();
    const attachments = [{ kind: "image_url" as const, url: "data:image/png;base64,cGljdHVyZQ==" }];
    const request = { role: "user" as const, origin: "user" as const, content: "", attachments, timestamp: "2026-10-15T00:00:00Z", turn_id: "turn", turn_state: kind === "request" ? "failed" as const : "completed" as const };
    conversations.replaceActiveConversationIfCurrent!({ ...conversations.activeConversation!, messages: [request, ...(kind === "partial" ? [{ role: "assistant" as const, content: "Partial", timestamp: "2026-10-15T00:00:01Z", turn_id: "turn", turn_state: "failed" as const }] : kind === "refused" ? [{ role: "user" as const, origin: "platform" as const, content: "Refused", timestamp: "2026-10-15T00:00:01Z", turn_id: "turn", turn_state: "refused" as const }] : [])] });
    if (kind === "live") stream.handleServerEvent({ type: "error", code: "validation_error", message: "Cannot accept image" }, "session-one");
    const display = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: stream } });
    mounted.push(() => { void unmount(display); });
    flushSync();
    const button = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find((candidate) => candidate.textContent?.trim() === (kind === "refused" ? "Retry request" : "Retry"));
    expect(button).toBeDefined();
    flushSync(() => button!.click());
    expect(sendMessage.mock.calls.at(-1)?.[3]).toEqual(attachments);
    expect(sendMessage.mock.calls.at(-1)?.[1]).toBe("");
  });

  // Review fix 4
  it("shows each optimistic image in the sent user message", async () => {
    const { conversations, stream } = await setup();
    const display = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: stream } });
    mounted.push(() => { void unmount(display); });
    paste();
    await preview();
    send();
    flushSync();
    expect(document.querySelector('[aria-label="Your message"]')?.textContent).toContain("Image attached");
  });

  it("shows each recorded image after reloading the conversation", async () => {
    const { getConversation } = await import("../api/conversations");
    const { conversations, stream } = await setup();
    vi.mocked(getConversation).mockResolvedValueOnce({ ...conversations.activeConversation!, messages: [{ id: "saved", thread_id: "root", role: "user", origin: "user", content: "", timestamp: "2026-10-15T00:00:00Z", tool_calls: [], attachments: [{ kind: "image_url", url: "data:image/png;base64,b25l" }, { kind: "image_url", url: "data:image/png;base64,dHdv" }] }] });
    await conversations.selectConversation("session-one");
    const display = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: stream } });
    mounted.push(() => { void unmount(display); });
    flushSync();
    expect(document.querySelector('[aria-label="Your message"]')?.textContent?.match(/Image attached/g)).toHaveLength(2);
  });


  it("shows an image announced as a recorded message", async () => {
    const { conversations, stream } = await setup();
    conversations.appendRecordedMessage({ id: "announced", thread_id: "root", role: "user", origin: "user", content: "", timestamp: "2026-10-15T00:00:00Z", tool_calls: [], attachments: [{ kind: "image_url", url: "data:image/png;base64,b25l" }] }, "session-one");
    const display = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: stream } });
    mounted.push(() => { void unmount(display); });
    flushSync();
    expect(document.querySelector('[aria-label="Your message"]')?.textContent).toContain("Image attached");
  });
  // Review fix 5
  it("describes preview images as pending delivery", async () => {
    await setup();
    paste();
    await preview();
    expect(document.querySelector(".attached-image")?.textContent).toContain("Will be sent with your message");
    expect(document.querySelector(".attached-image")?.textContent).not.toContain("Model can view");
  });

});
