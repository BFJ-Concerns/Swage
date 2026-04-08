// Connection store — owns the WebSocket lifecycle and connection status.
//
// Coordinates between the WebSocket client and other stores. Exposes
// connection status for the UI indicator and manages connect/disconnect
// as the active conversation changes.

import { fromStore, get, writable } from "svelte/store";

import type { ConnectionStatus, ServerEvent } from "../types";
import { createWebSocketClient } from "../api/websocket";
import type { WebSocketClient } from "../api/websocket";

export interface ConnectionStore {
  readonly status: ConnectionStatus;
  readonly errorMessage: string | null;
  connectTo(conversationId: string): void;
  disconnect(): void;
  sendMessage(content: string): void;
  cancelTurn(): void;
  onServerEvent(handler: (event: ServerEvent) => void): void;
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
  const wsClient = (options?.wsClientFactory ?? createWebSocketClient)();

  // Back the getter API with Svelte stores so connection state changes re-render
  // components that read them after mount.
  const statusStore = writable<ConnectionStatus>("disconnected");
  const errorMessageStore = writable<string | null>(null);

  const statusState = fromStore(statusStore);
  const errorMessageState = fromStore(errorMessageStore);

  const serverEventHandlers: ((event: ServerEvent) => void)[] = [];

  // ---- Wire up the WS client callbacks ----------------------------------------

  // Route all server events to registered handlers. The connection store is a
  // pure relay here — event interpretation belongs to the streaming store.
  wsClient.onEvent((event) => {
    for (const handler of serverEventHandlers) handler(event);
  });

  wsClient.onStatusChange((newStatus) => {
    const previousStatus = get(statusStore);

    // At this point `previousStatus` holds the previous value — check it before
    // updating so we can detect the reconnecting → connected transition.
    if (previousStatus === "reconnecting" && newStatus === "connected") {
      options?.onReconnect?.();
    }

    if (newStatus === "error") {
      // Surface a human-readable message. The most common cause of "error"
      // before the connection ever opens is an HTTP 409 Conflict rejection
      // (another tab already has this conversation open).
      errorMessageStore.set(
        "Connection failed. This conversation may already be open in another tab.",
      );
    } else {
      errorMessageStore.set(null);
    }

    statusStore.set(newStatus);
  });

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
      wsClient.connect(conversationId);
    },

    disconnect(): void {
      wsClient.disconnect();
    },

    sendMessage(content: string): void {
      wsClient.send({ type: "send_message", content });
    },

    cancelTurn(): void {
      wsClient.send({ type: "cancel" });
    },

    onServerEvent(handler: (event: ServerEvent) => void): void {
      serverEventHandlers.push(handler);
    },
  };
}
