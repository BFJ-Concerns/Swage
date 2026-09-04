// Work-surface signal channel — the live route for the shared work surfaces.
//
// Separate from the conversation socket by necessity: that socket needs a
// conversation id and admits one connection per conversation, while a page
// changes whenever anyone writes to it and the person watching it may have no
// conversation selected at all. This channel takes no identity, is listen-only,
// and carries signals rather than content — the surface answers a signal by
// reading through the authoritative HTTP path.

import { KNOWN_SERVER_EVENT_TYPES } from "../types";
import type { KnownServerEventType, ServerEvent } from "../types";

export interface WorkSurfaceChannel {
  onSignal(handler: (event: ServerEvent) => void): void;
  onReconnect(handler: () => void): void;
  close(): void;
}

export interface WorkSurfaceChannelOptions {
  // Overrides the base URL. Defaults to same origin, converting http → ws.
  wsBaseUrl?: string;
}

// Reconnection backoff, matching the conversation socket's shape: a dropped
// signal channel costs the surface its liveness, so it retries indefinitely.
const BACKOFF_BASE_MS = 1_000;
const BACKOFF_MAX_MS = 30_000;
const BACKOFF_FACTOR = 2;

function isKnownServerEventType(type: string): type is KnownServerEventType {
  return (KNOWN_SERVER_EVENT_TYPES as readonly string[]).includes(type);
}

export function createWorkSurfaceChannel(
  options?: WorkSurfaceChannelOptions,
): WorkSurfaceChannel {
  const handlers: ((event: ServerEvent) => void)[] = [];
  const reconnectHandlers: (() => void)[] = [];

  let ws: WebSocket | null = null;
  let closed = false;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let reconnectDelay = BACKOFF_BASE_MS;
  let hasConnected = false;

  function resolveWsBase(): string {
    if (options?.wsBaseUrl !== undefined) return options.wsBaseUrl;
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${protocol}//${window.location.host}`;
  }

  function scheduleReconnect(): void {
    if (closed || reconnectTimer !== null) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * BACKOFF_FACTOR, BACKOFF_MAX_MS);
  }

  function connect(): void {
    if (closed) return;
    try {
      ws = new WebSocket(`${resolveWsBase()}/api/work-surface/ws`);
    } catch {
      scheduleReconnect();
      return;
    }

    ws.onopen = () => {
      reconnectDelay = BACKOFF_BASE_MS;
      if (hasConnected) {
        for (const handler of reconnectHandlers) handler();
      }
      hasConnected = true;
    };

    ws.onmessage = (event: MessageEvent) => {
      let parsed: unknown;
      try {
        parsed = JSON.parse(event.data as string);
      } catch {
        // A malformed frame is not actionable here: the channel carries only
        // signals, so the next one recovers the surface.
        return;
      }
      if (
        typeof parsed !== "object" ||
        parsed === null ||
        typeof (parsed as { type?: unknown }).type !== "string"
      ) {
        return;
      }
      const type = (parsed as { type: string }).type;
      if (!isKnownServerEventType(type)) return;
      for (const handler of handlers) handler(parsed as ServerEvent);
    };

    ws.onclose = () => {
      ws = null;
      scheduleReconnect();
    };

    ws.onerror = () => {
      // The browser always follows onerror with onclose, which retries.
    };
  }

  connect();

  return {
    onSignal(handler: (event: ServerEvent) => void): void {
      handlers.push(handler);
    },

    onReconnect(handler: () => void): void {
      reconnectHandlers.push(handler);
    },

    close(): void {
      closed = true;
      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      ws?.close();
      ws = null;
    },
  };
}
