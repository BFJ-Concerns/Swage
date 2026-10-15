// WebSocket client for real-time conversation streaming.
//
// Manages the WebSocket lifecycle for a single conversation: connect,
// send client events, dispatch server events to callbacks, and handle
// reconnection with exponential backoff.

import {
  KNOWN_SERVER_EVENT_TYPES,
  type ClientEvent,
  type ConnectionStatus,
  type KnownServerEvent,
  type KnownServerEventType,
  type ServerEvent,
} from "../types";

export type ServerEventHandler = (event: ServerEvent) => void;
export type StatusChangeHandler = (status: ConnectionStatus, message?: string) => void;

export interface WebSocketClient {
  connect(conversationId: string): void;
  send(event: ClientEvent): void;
  disconnect(): void;
  onEvent(handler: ServerEventHandler): void;
  onStatusChange(handler: StatusChangeHandler): void;
}

export interface WebSocketClientOptions {
  // Overrides the base URL for WebSocket connections. Defaults to same origin,
  // converting http → ws and https → wss from window.location. Pass an
  // explicit value in tests or non-standard deployments.
  wsBaseUrl?: string;
}

function isObjectRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isKnownServerEventType(type: string): type is KnownServerEventType {
  return (KNOWN_SERVER_EVENT_TYPES as readonly string[]).includes(type);
}

function parseServerEventFrame(raw: unknown): ServerEvent {
  if (!isObjectRecord(raw) || typeof raw.type !== "string") {
    throw new Error("missing event type");
  }

  if (isKnownServerEventType(raw.type)) {
    return raw as KnownServerEvent;
  }

  return {
    type: "unknown",
    event_type: raw.type,
    payload: raw,
  };
}

// ---------------------------------------------------------------------------
// Reconnection backoff constants
// ---------------------------------------------------------------------------

// Initial delay before the first reconnect attempt (ms).
const BACKOFF_BASE_MS = 1_000;

// Ceiling: delay will never exceed this value regardless of attempt count.
const BACKOFF_MAX_MS = 30_000;

// Each failed reconnect multiplies the delay by this factor.
const BACKOFF_FACTOR = 2;

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

export function createWebSocketClient(
  options?: WebSocketClientOptions,
): WebSocketClient {
  let ws: WebSocket | null = null;
  let conversationId: string | null = null;

  const eventHandlers: ServerEventHandler[] = [];
  const statusHandlers: StatusChangeHandler[] = [];

  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let reconnectDelay = BACKOFF_BASE_MS;

  // Frames handed to send() while the socket is still connecting. Flushed in
  // order once the handshake completes, so callers can dispatch immediately
  // after connect() without racing the upgrade (the compose-then-create flow
  // does exactly that). Cleared on connect()/disconnect() so frames never
  // leak into a different conversation's socket.
  let pendingFrames: ClientEvent[] = [];

  // ---- Helpers ----------------------------------------------------------------

  function resolveWsBase(): string {
    if (options?.wsBaseUrl !== undefined) return options.wsBaseUrl;
    // Derive from the current surface origin so relative-style routing works.
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${protocol}//${window.location.host}`;
  }

  function emitStatus(status: ConnectionStatus, message?: string): void {
    for (const handler of statusHandlers) handler(status, message);
  }

  // A frame could not be delivered and will not be retried. Surfaced as a
  // turn error rather than swallowed: the streaming store treats it like any
  // other failed turn, so the UI never shows a working timer for a message
  // that went nowhere.
  function emitDeliveryFailure(): void {
    const deliveryError: ServerEvent = {
      type: "error",
      code: "not_connected",
      message: "Not connected to the conversation — the message was not sent.",
    };
    for (const handler of eventHandlers) handler(deliveryError);
  }

  function scheduleReconnect(): void {
    if (reconnectTimer !== null) clearTimeout(reconnectTimer);

    const delay = reconnectDelay;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      attemptConnect();
    }, delay);

    // Grow delay for the next attempt, capped at the ceiling.
    reconnectDelay = Math.min(reconnectDelay * BACKOFF_FACTOR, BACKOFF_MAX_MS);
  }

  function attemptConnect(): void {
    if (!conversationId) return;

    const url = `${resolveWsBase()}/api/conversations/${conversationId}/ws`;
    const socket = new WebSocket(url);
    ws = socket;

    socket.onopen = () => {
      if (ws !== socket) return;
      // Backoff delay is NOT reset here — it only resets when the caller
      // explicitly calls connect() again. Keeping it growing means repeated
      // open+drop cycles (e.g. a flapping connection) back off progressively
      // rather than hammering the server at the base rate on every recovery.
      emitStatus("connected");

      // Subscribe to the live observability channels the conversation surface
      // renders inline: tools for call cards, retry for provider retry chips.
      socket.send(JSON.stringify({ type: "subscribe", channels: ["tools", "retry"] }));

      // Deliver anything queued while the handshake was in flight.
      const queued = pendingFrames;
      pendingFrames = [];
      for (const frame of queued) socket.send(JSON.stringify(frame));
    };

    socket.onmessage = (event: MessageEvent) => {
      if (ws !== socket) return;
      try {
        let parsed = parseServerEventFrame(JSON.parse(event.data as string));
        if (parsed.type === "error" && parsed.code === "already_connected") {
          parsed = {
            ...parsed,
            message: "This conversation is open in another tab. Switch to that tab to continue, or close it and retry here.",
          };
          if (pendingFrames.length > 0) {
            pendingFrames = [];
            emitDeliveryFailure();
          }
          ws = null;
          socket.close(1000, "connection refused");
          emitStatus("error", parsed.message);
          // Ownership refusal describes this connection, not the running turn.
          return;
        }
        for (const handler of eventHandlers)
          handler(parsed);
      } catch {
        const frameError: ServerEvent = {
          type: "error",
          code: "invalid_frame",
          message: "Received a malformed WebSocket frame from the gateway.",
        };
        for (const handler of eventHandlers) handler(frameError);
      }
    };

    socket.onerror = () => {
      // The browser always follows onerror with onclose. Status update
      // happens there so we don't double-emit.
    };

    socket.onclose = () => {
      // A replaced socket can close after its successor has opened. It no
      // longer owns this client's status or reconnect timer.
      if (ws !== socket) return;
      if (pendingFrames.length > 0) {
        pendingFrames = [];
        emitDeliveryFailure();
      }
      emitStatus("reconnecting");
      scheduleReconnect();
    };
  }

  // ---- Public interface -------------------------------------------------------

  return {
    connect(id: string): void {
      // Cancel any pending reconnect so we don't create a spurious socket
      // after the explicit connect.
      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }

      // Close any existing socket without triggering the reconnect path.
      if (ws !== null) {
        const previous = ws;
        ws = null;
        previous.close(1000, "switching conversation");
      }

      conversationId = id;
      reconnectDelay = BACKOFF_BASE_MS;
      pendingFrames = [];
      attemptConnect();
    },

    send(event: ClientEvent): void {
      // Callers should not need to track readyState themselves: an open
      // socket sends now, a connecting one queues for the open flush, and
      // anything else is a delivery failure surfaced as a turn error.
      if (ws !== null && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(event));
        return;
      }
      if (ws !== null && ws.readyState === WebSocket.CONNECTING) {
        pendingFrames.push(event);
        return;
      }
      emitDeliveryFailure();
    },

    disconnect(): void {
      pendingFrames = [];

      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }

      if (ws !== null) {
        const previous = ws;
        ws = null;
        previous.close(1000, "client disconnect");
      }

      conversationId = null;
      emitStatus("disconnected");
    },

    onEvent(handler: ServerEventHandler): void {
      eventHandlers.push(handler);
    },

    onStatusChange(handler: StatusChangeHandler): void {
      statusHandlers.push(handler);
    },
  };
}
