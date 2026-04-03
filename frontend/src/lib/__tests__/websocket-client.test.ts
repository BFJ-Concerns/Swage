/**
 * Tests for the WebSocket client.
 *
 * WebSocket is mocked with a controlled stub so tests do not need a live
 * server. All timer-based behaviour (reconnection backoff) is driven via
 * vitest's fake timers.
 *
 * Scenarios covered:
 *   - connect() opens a WebSocket to the correct URL
 *   - open event → "connected" status
 *   - send() serialises ClientEvent to JSON
 *   - incoming messages dispatched to handlers
 *   - unknown event types ignored silently (forward-compat with Phase 04)
 *   - disconnect() closes cleanly, no reconnect
 *   - unexpected close after open → "reconnecting" + backoff retry
 *   - close before open (handshake rejection / 409) → "error", no retry
 *   - exponential backoff between reconnect attempts
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import type { ServerEvent } from "../types";
import { createWebSocketClient } from "../api/websocket";

// ---------------------------------------------------------------------------
// Mock WebSocket
// ---------------------------------------------------------------------------

// Plain objects stand in for CloseEvent/MessageEvent so the tests run in a
// standard node environment without needing jsdom.

class MockWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  /** Ordered list of all instances created during a test — newest is last. */
  static instances: MockWebSocket[] = [];

  static reset() {
    MockWebSocket.instances = [];
  }

  url: string;
  readyState = MockWebSocket.CONNECTING;
  sentMessages: string[] = [];

  onopen: ((e: unknown) => void) | null = null;
  onclose: ((e: unknown) => void) | null = null;
  onerror: ((e: unknown) => void) | null = null;
  onmessage: ((e: unknown) => void) | null = null;

  constructor(url: string) {
    this.url = url;
    MockWebSocket.instances.push(this);
  }

  send(data: string): void {
    this.sentMessages.push(data);
  }

  close(code = 1000, _reason = ""): void {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.({ code, wasClean: code === 1000 });
  }

  // ---- Test simulation helpers ----

  /** Simulate the server accepting the connection. */
  simulateOpen(): void {
    this.readyState = MockWebSocket.OPEN;
    this.onopen?.({});
  }

  /** Simulate a valid server message. */
  simulateMessage(data: unknown): void {
    this.onmessage?.({ data: JSON.stringify(data) });
  }

  /** Simulate a raw (potentially malformed) message. */
  simulateRawMessage(raw: string): void {
    this.onmessage?.({ data: raw });
  }

  /** Simulate an unexpected connection drop after being open. */
  simulateUnexpectedClose(code = 1006): void {
    this.readyState = MockWebSocket.CLOSED;
    this.onclose?.({ code, wasClean: false });
  }

  /** Simulate an error event (always followed by close in real WS). */
  simulateError(): void {
    this.onerror?.({});
  }
}

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

beforeEach(() => {
  // Replace the global WebSocket with the mock before each test.
  vi.stubGlobal("WebSocket", MockWebSocket);
  MockWebSocket.reset();
  vi.useFakeTimers();
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

// Convenience: get the most recently created MockWebSocket instance.
function latestWs(): MockWebSocket {
  return MockWebSocket.instances[MockWebSocket.instances.length - 1];
}

// ---------------------------------------------------------------------------
// connect()
// ---------------------------------------------------------------------------

describe("connect", () => {
  it("creates a WebSocket to the correct URL", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    client.connect("conv-abc");

    expect(MockWebSocket.instances).toHaveLength(1);
    expect(MockWebSocket.instances[0].url).toBe(
      "ws://localhost:3000/api/conversations/conv-abc/ws",
    );
  });

  it("emits 'connected' status when the socket opens", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));

    client.connect("conv-abc");
    latestWs().simulateOpen();

    expect(statuses).toContain("connected");
  });

  it("replaces an existing connection when called again", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    client.connect("conv-1");
    latestWs().simulateOpen();

    client.connect("conv-2");

    // Old socket should be closed; new one created for conv-2.
    expect(MockWebSocket.instances).toHaveLength(2);
    expect(latestWs().url).toContain("conv-2");
  });
});

// ---------------------------------------------------------------------------
// send()
// ---------------------------------------------------------------------------

describe("send", () => {
  it("serialises a send_message event as JSON", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    client.connect("conv-abc");
    latestWs().simulateOpen();

    client.send({ type: "send_message", content: "Hello, Swage" });

    // sentMessages[0] is the automatic tools subscribe sent on open;
    // sentMessages[1] is the explicit send_message call.
    const sent = latestWs().sentMessages;
    expect(sent).toHaveLength(2);
    expect(JSON.parse(sent[1])).toEqual({
      type: "send_message",
      content: "Hello, Swage",
    });
  });

  it("serialises a cancel event as JSON", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    client.connect("conv-abc");
    latestWs().simulateOpen();

    client.send({ type: "cancel" });

    // sentMessages[0] is the automatic tools subscribe; sentMessages[1] is cancel.
    const sent = latestWs().sentMessages;
    expect(JSON.parse(sent[sent.length - 1])).toEqual({ type: "cancel" });
  });

  it("does not throw when called before the socket is open", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    client.connect("conv-abc");
    // Intentionally do NOT call simulateOpen.

    expect(() => client.send({ type: "cancel" })).not.toThrow();
    expect(latestWs().sentMessages).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// Server event dispatch
// ---------------------------------------------------------------------------

describe("server event dispatch", () => {
  it("passes token events to registered handlers", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const received: ServerEvent[] = [];
    client.onEvent((e) => received.push(e));

    client.connect("conv-abc");
    latestWs().simulateOpen();
    latestWs().simulateMessage({ type: "token", content: "Hello" });

    expect(received).toHaveLength(1);
    expect(received[0]).toEqual({ type: "token", content: "Hello" });
  });

  it("passes turn_complete events to handlers", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const received: ServerEvent[] = [];
    client.onEvent((e) => received.push(e));

    client.connect("conv-abc");
    latestWs().simulateOpen();
    latestWs().simulateMessage({ type: "turn_complete", turn_id: "t-42" });

    expect(received[0]).toEqual({ type: "turn_complete", turn_id: "t-42" });
  });

  it("passes output_schema_failed events to handlers", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const received: ServerEvent[] = [];
    client.onEvent((e) => received.push(e));

    client.connect("conv-abc");
    latestWs().simulateOpen();
    latestWs().simulateMessage({
      type: "output_schema_failed",
      turn_id: "t-42",
      attempts: 3,
      errors: [{ path: "/answer", message: "is required" }],
    });

    expect(received[0]).toEqual({
      type: "output_schema_failed",
      turn_id: "t-42",
      attempts: 3,
      errors: [{ path: "/answer", message: "is required" }],
    });
  });

  it("ignores unknown event types silently (forward-compat for future events)", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const received: ServerEvent[] = [];
    client.onEvent((e) => received.push(e));

    client.connect("conv-abc");
    latestWs().simulateOpen();

    // A hypothetical future event type — must not throw or dispatch.
    expect(() =>
      latestWs().simulateMessage({ type: "future_event", payload: "x" }),
    ).not.toThrow();
    expect(received).toHaveLength(0);
  });

  it("surfaces malformed JSON frames as an error event", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const statuses: string[] = [];
    const received: ServerEvent[] = [];
    client.onStatusChange((status) => statuses.push(status));
    client.onEvent((event) => received.push(event));

    client.connect("conv-abc");
    latestWs().simulateOpen();

    expect(() => latestWs().simulateRawMessage("not-json{{")).not.toThrow();
    expect(statuses).toContain("error");
    expect(received).toContainEqual({
      type: "error",
      code: "invalid_frame",
      message: "Received a malformed WebSocket frame from the gateway.",
    });
  });
});

// ---------------------------------------------------------------------------
// disconnect()
// ---------------------------------------------------------------------------

describe("disconnect", () => {
  it("emits 'disconnected' status on clean close", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));

    client.connect("conv-abc");
    latestWs().simulateOpen();
    client.disconnect();

    expect(statuses).toContain("disconnected");
  });

  it("does not schedule any reconnection after a clean disconnect", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));

    client.connect("conv-abc");
    latestWs().simulateOpen();
    client.disconnect();

    // Advance well past any backoff window.
    vi.advanceTimersByTime(60_000);

    expect(MockWebSocket.instances).toHaveLength(1);
    expect(statuses).not.toContain("reconnecting");
  });
});

// ---------------------------------------------------------------------------
// Reconnection
// ---------------------------------------------------------------------------

describe("reconnection on unexpected close", () => {
  it("emits 'reconnecting' and creates a new WebSocket after the first backoff", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));

    client.connect("conv-abc");
    latestWs().simulateOpen();
    latestWs().simulateUnexpectedClose();

    expect(statuses).toContain("reconnecting");

    vi.advanceTimersByTime(1_100); // past the 1 000 ms base delay

    expect(MockWebSocket.instances).toHaveLength(2);
  });

  it("uses exponential backoff: second delay is double the first", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });

    // First connection + drop → reconnect after 1 000 ms
    client.connect("conv-abc");
    latestWs().simulateOpen();
    latestWs().simulateUnexpectedClose();

    vi.advanceTimersByTime(1_000);
    expect(MockWebSocket.instances).toHaveLength(2); // second attempt created

    // Second connection + drop → reconnect after 2 000 ms
    latestWs().simulateOpen();
    latestWs().simulateUnexpectedClose();

    vi.advanceTimersByTime(1_000); // only 1 000 ms elapsed — too soon
    expect(MockWebSocket.instances).toHaveLength(2); // not yet

    vi.advanceTimersByTime(1_000); // now at 2 000 ms — fires
    expect(MockWebSocket.instances).toHaveLength(3);
  });

  it("resets backoff delay when connect() is called explicitly", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });

    // Build up delay to 2 000 ms via two automatic reconnect cycles.
    client.connect("conv-abc");
    latestWs().simulateOpen();
    latestWs().simulateUnexpectedClose(); // → 1 000 ms delay, reconnectDelay grows to 2 000
    vi.advanceTimersByTime(1_000); // ws2 reconnects

    latestWs().simulateOpen();
    latestWs().simulateUnexpectedClose(); // → 2 000 ms delay, reconnectDelay grows to 4 000
    vi.advanceTimersByTime(2_000); // ws3 reconnects

    // Explicit connect() resets the backoff to the base delay.
    client.connect("conv-abc");
    expect(MockWebSocket.instances).toHaveLength(4); // explicit connect created ws4

    // Drop ws4 — should now use the base 1 000 ms delay again.
    latestWs().simulateOpen();
    latestWs().simulateUnexpectedClose();
    vi.advanceTimersByTime(1_000);
    expect(MockWebSocket.instances).toHaveLength(5);
  });
});

// ---------------------------------------------------------------------------
// Tools subscription on open
// ---------------------------------------------------------------------------

describe("tools channel subscription", () => {
  it("sends a subscribe message for 'tools' immediately on open", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    client.connect("conv-abc");
    latestWs().simulateOpen();

    const sent = latestWs().sentMessages;
    expect(sent).toHaveLength(1);
    expect(JSON.parse(sent[0])).toEqual({
      type: "subscribe",
      channels: ["tools"],
    });
  });

  it("dispatches context_assembled events to handlers", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const received: ServerEvent[] = [];
    client.onEvent((e) => received.push(e));

    client.connect("conv-abc");
    latestWs().simulateOpen();
    latestWs().simulateMessage({
      type: "context_assembled",
      turn_id: "t-1",
      report: { errors: [] },
    });

    expect(received).toHaveLength(1);
    expect(received[0].type).toBe("context_assembled");
  });

  it("dispatches shutdown events to handlers", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const received: ServerEvent[] = [];
    client.onEvent((e) => received.push(e));

    client.connect("conv-abc");
    latestWs().simulateOpen();
    latestWs().simulateMessage({ type: "shutdown", reason: "idle_timeout" });

    expect(received).toHaveLength(1);
    expect(received[0].type).toBe("shutdown");
  });
});

// ---------------------------------------------------------------------------
// Handshake rejection (covers HTTP 409 Conflict / duplicate tab)
// ---------------------------------------------------------------------------

describe("handshake rejection", () => {
  it("emits 'error' and does not retry when connection never opens", () => {
    const client = createWebSocketClient({ wsBaseUrl: "ws://localhost:3000" });
    const statuses: string[] = [];
    client.onStatusChange((s) => statuses.push(s));

    client.connect("conv-abc");
    // Connection closes before ever opening — typical of an HTTP 409 rejection
    // during the WebSocket upgrade handshake.
    latestWs().simulateUnexpectedClose(1006);

    expect(statuses).toContain("error");
    expect(statuses).not.toContain("reconnecting");

    // No timer should be pending.
    vi.advanceTimersByTime(60_000);
    expect(MockWebSocket.instances).toHaveLength(1);
  });
});
