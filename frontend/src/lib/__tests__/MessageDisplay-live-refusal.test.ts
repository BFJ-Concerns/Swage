// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { mount, tick, unmount } from "svelte";
import MessageDisplay from "../components/MessageDisplay.svelte";
import { createConversationStore } from "../stores/conversations";
import { createConnectionStore } from "../stores/connection";
import { createStreamingStore } from "../stores/streaming";
import { createWebSocketClient } from "../api/websocket";
import { getConversation, listBranches } from "../api/conversations";

vi.mock("../api/conversations", () => ({ getConversation: vi.fn(), listBranches: vi.fn() }));
vi.mock("../api/turns", () => ({ fetchTurnContext: vi.fn() }));

// Only the transport is controlled; decoding, installation and rendering are real.
class Socket {
  static OPEN = 1;
  static instance: Socket;
  readyState = 1;
  onopen: (() => void) | null = null;
  onclose: ((event: { code: number; wasClean: boolean }) => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  sent: string[] = [];
  constructor() { Socket.instance = this; }
  send(data: string) { this.sent.push(data); }
  close() { this.onclose?.({ code: 1000, wasClean: true }); }
}

let component: ReturnType<typeof mount> | undefined;
let connection: ReturnType<typeof createConnectionStore> | undefined;
afterEach(async () => {
  connection?.disconnect();
  if (component) await unmount(component);
  document.body.replaceChildren();
  vi.unstubAllGlobals();
  vi.resetAllMocks();
});

it("installs a socket refusal account and renders its usable Retry control without refetching", async () => {
  vi.stubGlobal("WebSocket", Socket);
  vi.mocked(getConversation).mockResolvedValue({
    id: "conv-1", agent_id: "gateway", topic_id: "topic-1", root_thread_id: "root-1",
    created_at: "2026-10-04T20:00:00Z", last_activity: "2026-10-04T20:00:00Z",
    messages: [{ role: "user", origin: "user", content: "Read a harmless file.",
      timestamp: "2026-10-04T20:00:00Z", turn_id: "turn-1", thread_id: "root-1" }],
  });
  vi.mocked(listBranches).mockResolvedValue([]);
  const conversations = createConversationStore();
  await conversations.selectConversation("conv-1");
  connection = createConnectionStore({ wsClientFactory: () => createWebSocketClient({ wsBaseUrl: "ws://fixture" }) });
  const streaming = createStreamingStore({ conversationStore: conversations, connectionStore: connection });
  connection.onServerEvent((event, owner) => streaming.handleServerEvent(event, owner));
  connection.connectTo("conv-1");
  Socket.instance.onopen?.();
  component = mount(MessageDisplay, { target: document.body, props: { store: conversations, streamingStore: streaming } });
  await tick();
  expect(document.body.textContent).not.toContain("The model refused this request.");
  expect(document.querySelector("button.branch-link")).toBeNull();

  Socket.instance.onmessage?.({ data: JSON.stringify({
    type: "message_appended", session_id: "conv-1",
    message: { id: "notice-1", thread_id: "root-1", turn_id: "turn-1", role: "user",
      origin: "platform", content: "The model refused this request. You can amend your request or retry it.",
      timestamp: "2026-10-04T20:00:01Z", turn_state: "refused", tool_calls: [] },
  }) });
  await tick();
  expect(document.body.textContent, "the carried account must appear in the already open view").toContain("The model refused this request.");
  const retry = Array.from(document.querySelectorAll<HTMLButtonElement>("button"))
    .find((button) => button.textContent?.includes("Retry"));
  expect(retry, "the live account offers Retry before any reload").toBeDefined();
  expect(retry!.disabled).toBe(false);
  retry!.click();
  expect(Socket.instance.sent.map((frame) => JSON.parse(frame))).toContainEqual({ type: "send_message", content: "Read a harmless file." });
  expect(getConversation).toHaveBeenCalledTimes(1);
});
