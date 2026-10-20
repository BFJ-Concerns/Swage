// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { mount, tick, unmount } from "svelte";
import MessageDisplay from "../components/MessageDisplay.svelte";
import { createConversationStore } from "../stores/conversations";
import { createStreamingStore } from "../stores/streaming";
import { getConversation, listBranches } from "../api/conversations";
import type { ServerEvent } from "../types";

vi.mock("../api/conversations", () => ({ getConversation: vi.fn(), listBranches: vi.fn() }));
vi.mock("../api/turns", () => ({ fetchTurnContext: vi.fn() }));

let component: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (component) await unmount(component);
  component = undefined;
  document.body.replaceChildren();
  vi.resetAllMocks();
});

it.each(["completed", "cancelled", "incomplete"] as const)(
  "renders a recorded %s reply without reloading and keeps one copy on redelivery",
  async (ending) => {
    vi.mocked(getConversation).mockResolvedValue({
      id: "child", agent_id: "gateway", agent_display_name: "Swage default",
      topic_id: "topic-1", root_thread_id: "thread-1",
      created_at: "2026-10-20T20:00:00Z", last_activity: "2026-10-20T20:00:00Z",
      messages: [],
    });
    vi.mocked(listBranches).mockResolvedValue([]);
    const conversations = createConversationStore();
    await conversations.selectConversation("child");
    const streaming = createStreamingStore({ conversationStore: conversations });
    component = mount(MessageDisplay, {
      target: document.body, props: { store: conversations, streamingStore: streaming },
    });
    await tick();
    const reply: ServerEvent = {
      type: "message_appended", session_id: "child",
      message: {
        id: "reply-1", thread_id: "thread-1", turn_id: "turn-1",
        role: "assistant", origin: "assistant", content: "Recorded child reply",
        timestamp: "2026-10-20T20:00:01Z", turn_state: ending, tool_calls: [],
      },
    };
    streaming.handleServerEvent(reply, "child");
    await tick();
    expect(document.querySelector(".assistant-content")?.textContent).toContain("Recorded child reply");
    streaming.handleServerEvent(ending === "cancelled"
      ? { type: "cancelled" }
      : { type: "turn_complete", turn_id: "turn-1", incomplete_reason: ending === "incomplete" ? "length" : undefined }, "child");
    await tick();
    expect(document.querySelectorAll(".assistant-content")).toHaveLength(1);
    expect(document.querySelector(".assistant-content")?.textContent).toContain("Recorded child reply");
    expect(getConversation).toHaveBeenCalledTimes(1);
    await unmount(component);
    component = undefined;
    // The incomplete-turn recorder can announce the same persisted row.
    streaming.handleServerEvent(reply, "child");
    expect(conversations.activeConversation!.messages, "a recorded reply must have one visible identity").toHaveLength(1);
  },
);
