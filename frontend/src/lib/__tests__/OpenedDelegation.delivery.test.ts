import { expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import ConversationChannel from "../components/ConversationChannel.svelte";
import { setBaseUrl, getConversation } from "../api/conversations";
import { createWebSocketClient } from "../api/websocket";
import { createConnectionStore } from "../stores/connection";
import { createConversationStore } from "../stores/conversations";
import { createStreamingStore } from "../stores/streaming";

// Launched by stopping_an_open_delegated_child_settles_its_rendered_view in
// the gateway integration suite, which owns the live server and delegation.
const gateway = import.meta.env.VITE_SWAGE_TEST_GATEWAY_URL;
it.skipIf(!gateway)("settles an opened child from gateway cancellation delivery", async () => {
  const id = import.meta.env.VITE_SWAGE_TEST_CHILD_ID!;
  setBaseUrl(gateway!);
  const conversations = createConversationStore();
  const connection = createConnectionStore({
    wsClientFactory: () => createWebSocketClient({ wsBaseUrl: gateway!.replace("http", "ws") }),
  });
  const streaming = createStreamingStore({ conversationStore: conversations, connectionStore: connection });
  connection.onServerEvent((event, owner) => streaming.handleServerEvent(event, owner));
  await conversations.selectConversation(id);
  connection.connectTo(id);
  const component = mount(ConversationChannel, {
    target: document.body,
    props: {
      conversationStore: conversations, connectionStore: connection, streamingStore: streaming,
      selectedAgentId: "researcher", onRetry: () => {},
    },
  });
  try {
    await vi.waitFor(() => expect(connection.status).toBe("connected"));
    flushSync();
    expect(document.querySelector('.message-display [aria-live="polite"]')?.textContent).toBe("Generating response");
    expect(document.querySelector(".message.assistant.streaming")).not.toBeNull();
    const stop = document.querySelector<HTMLButtonElement>(".action.stop");
    expect(stop).not.toBeNull();
    flushSync(() => stop!.click());

    // Establish actual backend cancellation independently of the view. This
    // HTTP read never feeds the store, refreshes it or supplies a socket event.
    await vi.waitFor(async () => {
      const recorded = await getConversation(id);
      expect(recorded.messages[0].turn_state).toBe("cancelled");
    }, { timeout: 5000 });
    await vi.waitFor(() => {
      flushSync();
      expect(document.querySelector(".action.stop"), "Stop must clear from actual gateway delivery").toBeNull();
      expect(document.querySelector(".message.assistant.streaming")).toBeNull();
      expect(document.querySelector('.message-display [aria-live="polite"]')?.textContent).not.toBe("Generating response");
      expect(conversations.activeConversation?.messages[0].turn_state).toBe("cancelled");
    }, { timeout: 5000 });
  } finally {
    connection.disconnect();
    unmount(component);
    setBaseUrl("");
    document.body.innerHTML = "";
  }
}, 15000);
