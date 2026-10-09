// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import MessageDisplay from "../components/MessageDisplay.svelte";
import { createConversationStore } from "../stores/conversations";
import { createStreamingStore } from "../stores/streaming";
import * as api from "../api/conversations";
import { makeConversationDetail } from "./fixtures/conversations";
import type { MessageView } from "../protocol/MessageView";

vi.mock("../api/conversations", () => ({ getConversation: vi.fn(), listBranches: vi.fn() }));

let component: ReturnType<typeof mount> | undefined;
afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  document.body.replaceChildren();
  vi.resetAllMocks();
});

async function openConversation() {
  vi.mocked(api.getConversation).mockResolvedValue(makeConversationDetail());
  vi.mocked(api.listBranches).mockResolvedValue([]);
  const conversation = createConversationStore();
  await conversation.selectConversation("conv-1");
  const connection = { sendMessage: vi.fn(), cancelTurn: vi.fn() };
  const streaming = createStreamingStore({ conversationStore: conversation, connectionStore: connection });
  component = mount(MessageDisplay, { target: document.body, props: { store: conversation, streamingStore: streaming } });
  flushSync();
  return { conversation, streaming, connection };
}

describe("queued input display", () => {
  it("appends three drained rows with a shared timestamp and follows the reply without remounting", async () => {
    const { streaming } = await openConversation();
    const contents = ["first input", "second input", "third input"];
    const rows: MessageView[] = contents.map((content, index) => ({
      id: `recorded-${index}`, thread_id: "root-1", turn_id: "drained-turn",
      role: "user", origin: "user", content, attachments: [],
      timestamp: "2026-10-04T20:24:30.049508158Z", turn_state: null, tool_calls: [],
    }));
    expect(() => flushSync(() => {
      for (const message of rows) streaming.handleServerEvent({ type: "message_appended", session_id: "conv-1", message });
      streaming.handleServerEvent({ type: "queue_drained", session_id: "conv-1", turn_id: "drained-turn", queued_ids: [] });
      streaming.handleServerEvent({ type: "token", content: "shared reply" }, "conv-1");
    })).not.toThrow();
    expect(Array.from(document.querySelectorAll('[aria-label="Your message"]')).map((row) => row.querySelector(".message-content")?.textContent?.trim())).toEqual(contents);
    expect(document.body.textContent).toContain("shared reply");
    flushSync(() => streaming.handleServerEvent({ type: "turn_complete", turn_id: "drained-turn" }, "conv-1"));
    expect(document.querySelectorAll('[aria-label="Your message"]')).toHaveLength(3);
    expect(document.querySelectorAll('[aria-label="Assistant message"]')).toHaveLength(1);
  });

  it("keeps the first ping visible when the second identical send queues", async () => {
    const { streaming, connection } = await openConversation();
    streaming.startTurn("ping");
    streaming.startTurn("ping");
    flushSync(() => streaming.handleServerEvent({ type: "message_queued", session_id: "conv-1", queued: {
      id: "second-ping", client_id: connection.sendMessage.mock.calls[1][5], content: "ping", origin: "user", attachments: [], queued_at: "2026-10-04T20:25:00Z",
    }}));
    const contents = (label: string) => Array.from(document.querySelectorAll(`[aria-label="${label}"]`)).map(row => row.querySelector(".message-content")?.textContent?.trim());
    expect({ messages: contents("Your message"), queued: contents("Queued message"), labelled: document.body.textContent?.includes("You · queued") })
      .toEqual({ messages: ["ping"], queued: ["ping"], labelled: true });
  });

  it("shows a send once as queued when the page opened during a turn has no local stream", async () => {
    const { conversation, streaming, connection } = await openConversation();
    expect(streaming.isStreaming).toBe(false);
    streaming.startTurn("follow up");
    expect(connection.sendMessage).toHaveBeenCalledWith("conv-1", "follow up", undefined, undefined, null, expect.any(String));
    const queued = { id: "queued-1", client_id: connection.sendMessage.mock.calls[0][5], content: "follow up", origin: "user" as const, attachments: [], queued_at: "2026-10-04T20:25:00Z" };
    flushSync(() => {
      streaming.handleServerEvent({ type: "message_queued", session_id: "conv-1", queued });
      streaming.handleServerEvent({ type: "message_queued", session_id: "conv-1", queued });
    });
    expect(conversation.activeConversation?.messages.filter((row) => row.content === "follow up")).toHaveLength(0);
    expect(document.querySelectorAll('[aria-label="Queued message"]')).toHaveLength(1);
    expect(document.body.textContent?.match(/follow up/g)).toHaveLength(1);
    expect(document.body.textContent).toContain("You · queued");
    flushSync(() => streaming.handleServerEvent({ type: "token", content: "running reply" }, "conv-1"));
    expect(document.body.textContent).toContain("running reply");
  });
});
