// Connection store — owns the WebSocket lifecycle and connection status.
//
// Coordinates between the WebSocket client and other stores. Exposes
// connection status for the UI indicator and manages connect/disconnect
// as the active conversation changes.

import { fromStore, get, writable } from "svelte/store";

import type { ConnectionStatus, ServerEvent } from "../types";
import type { MessageAttachment } from "../protocol/MessageAttachment";
import { createWebSocketClient } from "../api/websocket";
import type { WebSocketClient } from "../api/websocket";

export interface ConnectionStore {
  readonly status: ConnectionStatus;
  readonly errorMessage: string | null;
  connectTo(conversationId: string): void;
  disconnect(): void;
  disconnectAll?(): void;
  sendMessage(
    conversationId: string,
    content: string,
    selectedModelId?: string | null,
    attachments?: MessageAttachment[],
  ): void;
  cancelTurn(conversationId: string): void;
  onServerEvent(handler: (event: ServerEvent, conversationId: string) => void): void;
}

export interface ConnectionStoreOptions {
  // Injectable factory for testing — defaults to createWebSocketClient().
  wsClientFactory?: () => WebSocketClient;
  // Called when a dropped connection successfully reconnects. Typically bound
  // to conversationStore.refreshActiveConversation() at wiring time so the
  // active conversation is re-fetched from HTTP after any state drift.
  onReconnect?: () => void;
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
  let selectedConversationId: string | null = null;

  function clientFor(conversationId: string): WebSocketClient {
    const existing = clients.get(conversationId);
    if (existing) return existing;
    const client = (options?.wsClientFactory ?? createWebSocketClient)();
    clients.set(conversationId, client);
    client.onEvent((event) => {
      for (const handler of serverEventHandlers) handler(event, conversationId);
    });
    client.onStatusChange((newStatus) => {
      statuses.set(conversationId, newStatus);
      if (conversationId !== selectedConversationId) return;
      const previousStatus = get(statusStore);
      if (previousStatus === "reconnecting" && newStatus === "connected") {
        options?.onReconnect?.();
      }
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
      if (status === "connected" || status === "reconnecting") {
        statusStore.set(status);
        return;
      }
      clientFor(conversationId).connect(conversationId);
    },

    disconnect(): void {
      if (!selectedConversationId) return;
      clients.get(selectedConversationId)?.disconnect();
      clients.delete(selectedConversationId);
      statuses.delete(selectedConversationId);
      selectedConversationId = null;
      statusStore.set("disconnected");
    },

    disconnectAll(): void {
      for (const client of clients.values()) client.disconnect();
      clients.clear();
      statuses.clear();
      selectedConversationId = null;
      statusStore.set("disconnected");
    },

    sendMessage(
      conversationId: string,
      content: string,
      selectedModelId?: string | null,
      attachments?: MessageAttachment[],
    ): void {
      clientFor(conversationId).send({
        type: "send_message",
        content,
        ...(attachments && attachments.length > 0 ? { attachments } : {}),
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
