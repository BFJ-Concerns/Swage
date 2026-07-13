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
export type StatusChangeHandler = (status: ConnectionStatus) => void;

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

  // True once the current socket instance reaches OPEN state. Reset each time
  // attemptConnect() creates a fresh socket. A close event that arrives before
  // wasConnected becomes true indicates a handshake rejection (e.g. HTTP 409
  // Conflict — duplicate tab). We surface this as "error" and do not retry.
  let wasConnected = false;

  // Set to true immediately before calling ws.close() when the caller
  // explicitly disconnects (via disconnect() or connect() replacing an existing
  // socket). Prevents the close handler from scheduling a reconnect.
  let intentionalClose = false;

  // ---- Helpers ----------------------------------------------------------------

  function resolveWsBase(): string {
    if (options?.wsBaseUrl !== undefined) return options.wsBaseUrl;
    // Derive from the current page origin so relative-style routing works.
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${protocol}//${window.location.host}`;
  }

  function emitStatus(status: ConnectionStatus): void {
    for (const handler of statusHandlers) handler(status);
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

    wasConnected = false;
    const url = `${resolveWsBase()}/api/conversations/${conversationId}/ws`;
    ws = new WebSocket(url);

    ws.onopen = () => {
      wasConnected = true;
      // Backoff delay is NOT reset here — it only resets when the caller
      // explicitly calls connect() again. Keeping it growing means repeated
      // open+drop cycles (e.g. a flapping connection) back off progressively
      // rather than hammering the server at the base rate on every recovery.
      emitStatus("connected");

      // Subscribe to the live observability channels the conversation surface
      // renders inline: tools for call cards, retry for provider retry chips.
      ws!.send(JSON.stringify({ type: "subscribe", channels: ["tools", "retry"] }));
    };

    ws.onmessage = (event: MessageEvent) => {
      try {
        const parsed = parseServerEventFrame(JSON.parse(event.data as string));
        for (const handler of eventHandlers)
          handler(parsed);
      } catch {
        emitStatus("error");
        const frameError: ServerEvent = {
          type: "error",
          code: "invalid_frame",
          message: "Received a malformed WebSocket frame from the gateway.",
        };
        for (const handler of eventHandlers) handler(frameError);
      }
    };

    ws.onerror = () => {
      // The browser always follows onerror with onclose. Status update
      // happens there so we don't double-emit.
    };

    ws.onclose = () => {
      if (intentionalClose) {
        emitStatus("disconnected");
        return;
      }

      if (!wasConnected) {
        // The socket closed before it ever opened — this is a handshake
        // rejection (HTTP 409 Conflict, 404, etc.). Do not retry; surface as
        // a permanent error so the UI can prompt the user.
        emitStatus("error");
        return;
      }

      // Established connection dropped unexpectedly — schedule a reconnect.
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
        intentionalClose = true;
        ws.close(1000, "switching conversation");
        intentionalClose = false;
        ws = null;
      }

      conversationId = id;
      intentionalClose = false;
      reconnectDelay = BACKOFF_BASE_MS;
      attemptConnect();
    },

    send(event: ClientEvent): void {
      // Guard against CONNECTING / CLOSING / CLOSED states — callers should
      // not need to track readyState themselves.
      if (ws !== null && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(event));
      }
    },

    disconnect(): void {
      intentionalClose = true;

      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }

      if (ws !== null) {
        ws.close(1000, "client disconnect");
        ws = null;
      }

      conversationId = null;
    },

    onEvent(handler: ServerEventHandler): void {
      eventHandlers.push(handler);
    },

    onStatusChange(handler: StatusChangeHandler): void {
      statusHandlers.push(handler);
    },
  };
}
