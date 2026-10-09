// Connection store — owns the WebSocket lifecycle and connection status.
//
// Coordinates between the WebSocket client and other stores. Exposes
// connection status for the UI indicator and manages connect/disconnect
// as the active conversation changes.

import { fromStore, writable } from "svelte/store";

import type { ConnectionStatus, ServerEvent } from "../types";
import type { MessageAttachment } from "../protocol/MessageAttachment";
import { createWebSocketClient } from "../api/websocket";
import type { WebSocketClient } from "../api/websocket";

export interface ConnectionStore {
  readonly status: ConnectionStatus;
  readonly errorMessage: string | null;
  connectTo(conversationId: string): void;
  /** Mark whether a conversation needs its client retained off-screen. */
  setStreamActive?(conversationId: string, active: boolean): void;
  /** Release the selected client if it is not retained by an active stream. */
  deselect?(): void;
  disconnect(): void;
  /**
   * Send a message on the conversation's socket. `threadId` names the thread
   * the turn runs on; absent, the gateway runs it on the root thread.
   */
  sendMessage(
    conversationId: string,
    content: string,
    selectedModelId?: string | null,
    attachments?: MessageAttachment[],
    threadId?: string | null,
    clientId?: string,
  ): void;
  cancelTurn(conversationId: string): void;
  onServerEvent(handler: (event: ServerEvent, conversationId: string) => void): void;
}

export interface ConnectionStoreOptions {
  // Injectable factory for testing — defaults to createWebSocketClient().
  wsClientFactory?: () => WebSocketClient;
  // Called when a dropped connection successfully reconnects. The owner is
  // carried through so an off-screen conversation is reconciled rather than
  // whichever conversation happens to be selected at that moment.
  onReconnect?: (conversationId: string) => void;
}

export function createConnectionStore(
  options?: ConnectionStoreOptions,
): ConnectionStore {
  // Back the getter API with Svelte stores so connection state changes re-render
  // components that read them after mount.
  const statusStore = writable<ConnectionStatus>("disconnected");
  const errorMessageStore = writable<string | null>(null);

  const statusState = fromStore(statusStore);
  const errorMessageState = fromStore(errorMessageStore);

  const serverEventHandlers: ((event: ServerEvent, conversationId: string) => void)[] = [];
  const clients = new Map<string, WebSocketClient>();
  const statuses = new Map<string, ConnectionStatus>();
  const streamingConversations = new Set<string>();
  let selectedConversationId: string | null = null;

  function releaseInactiveClients(): void {
    for (const [conversationId, client] of clients) {
      if (conversationId === selectedConversationId || streamingConversations.has(conversationId)) {
        continue;
      }
      client.disconnect();
      clients.delete(conversationId);
      statuses.delete(conversationId);
    }
  }

  function clientFor(conversationId: string): WebSocketClient {
    const existing = clients.get(conversationId);
    if (existing) return existing;
    const client = (options?.wsClientFactory ?? createWebSocketClient)();
    clients.set(conversationId, client);
    client.onEvent((event) => {
      for (const handler of serverEventHandlers) handler(event, conversationId);
    });
    client.onStatusChange((newStatus) => {
      const wasReconnecting = statuses.get(conversationId) === "reconnecting";
      statuses.set(conversationId, newStatus);
      if (wasReconnecting && newStatus === "connected") {
        options?.onReconnect?.(conversationId);
      }
      if (conversationId !== selectedConversationId) return;
      errorMessageStore.set(
        newStatus === "error"
          ? "Connection failed. This conversation may already be open in another tab."
          : null,
      );
      statusStore.set(newStatus);
    });
    return client;
  }

  // ---- Public interface --------------------------------------------------------

  return {
    get status(): ConnectionStatus {
      return statusState.current;
    },

    get errorMessage(): string | null {
      return errorMessageState.current;
    },

    connectTo(conversationId: string): void {
      // Clear any stale error from a previous attempt before connecting.
      errorMessageStore.set(null);
      selectedConversationId = conversationId;
      const status = statuses.get(conversationId);
      const existing = clients.get(conversationId);
      if (status === "connected" || status === "reconnecting") {
        statusStore.set(status);
        releaseInactiveClients();
        return;
      }
      if (existing && status !== "error" && status !== "disconnected") {
        releaseInactiveClients();
        return;
      }
      clientFor(conversationId).connect(conversationId);
      releaseInactiveClients();
    },

    setStreamActive(conversationId: string, active: boolean): void {
      if (active) {
        streamingConversations.add(conversationId);
      } else {
        streamingConversations.delete(conversationId);
      }
      releaseInactiveClients();
    },

    deselect(): void {
      selectedConversationId = null;
      errorMessageStore.set(null);
      statusStore.set("disconnected");
      releaseInactiveClients();
    },

    disconnect(): void {
      for (const client of clients.values()) client.disconnect();
      clients.clear();
      statuses.clear();
      streamingConversations.clear();
      selectedConversationId = null;
      statusStore.set("disconnected");
    },

    sendMessage(
      conversationId: string,
      content: string,
      selectedModelId?: string | null,
      attachments?: MessageAttachment[],
      threadId?: string | null,
      clientId?: string,
    ): void {
      clientFor(conversationId).send({
        type: "send_message",
        content,
        ...(clientId ? { client_id: clientId } : {}),
        ...(attachments && attachments.length > 0 ? { attachments } : {}),
        ...(threadId ? { thread_id: threadId } : {}),
        ...(selectedModelId ? { selected_model_id: selectedModelId } : {}),
      });
    },

    cancelTurn(conversationId: string): void {
      clientFor(conversationId).send({ type: "cancel" });
    },

    onServerEvent(handler: (event: ServerEvent, conversationId: string) => void): void {
      serverEventHandlers.push(handler);
    },
  };
}
