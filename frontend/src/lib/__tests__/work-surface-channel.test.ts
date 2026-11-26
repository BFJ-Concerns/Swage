// The work-surface channel — the socket an open surface listens on. It takes no
// conversation, so a surface stays live whether or not one is selected.

import { describe, it, expect, vi, afterEach } from "vitest";
import { createWorkSurfaceChannel, openSharedWorkSurfaceChannel } from "../api/work-surface";
import type { ServerEvent } from "../types";

class FakeSocket {
  static last: FakeSocket | null = null;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;

  constructor(public url: string) {
    FakeSocket.last = this;
  }

  close(): void {
    this.closed = true;
  }
}

function installFakeSocket(): void {
  FakeSocket.last = null;
  vi.stubGlobal("WebSocket", FakeSocket as unknown as typeof WebSocket);
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("work-surface channel", () => {
  it("dials a socket that names no conversation", () => {
    installFakeSocket();
    const channel = createWorkSurfaceChannel({ wsBaseUrl: "ws://gateway" });
    expect(FakeSocket.last?.url).toBe("ws://gateway/api/work-surface/ws");
    channel.close();
  });

  it("delivers a surface signal from the wire to its subscriber", () => {
    installFakeSocket();
    const channel = createWorkSurfaceChannel({ wsBaseUrl: "ws://gateway" });
    const seen: ServerEvent[] = [];
    channel.onSignal((event) => seen.push(event));

    FakeSocket.last?.onmessage?.({
      data: JSON.stringify({
        type: "surface_updated",
        surface_id: "11111111-1111-4111-8111-111111111111",
        module_id: "22222222-2222-4222-8222-222222222222",
      }),
    });

    expect(seen).toHaveLength(1);
    expect(seen[0].type).toBe("surface_updated");
    channel.close();
  });

  it("survives a malformed or unrecognised frame", () => {
    installFakeSocket();
    const channel = createWorkSurfaceChannel({ wsBaseUrl: "ws://gateway" });
    const seen: ServerEvent[] = [];
    channel.onSignal((event) => seen.push(event));

    FakeSocket.last?.onmessage?.({ data: "not json" });
    FakeSocket.last?.onmessage?.({ data: JSON.stringify({ nope: true }) });
    FakeSocket.last?.onmessage?.({
      data: JSON.stringify({ type: "invented_by_a_newer_server" }),
    });

    expect(seen).toEqual([]);
    channel.close();
  });

  it("redials after the socket drops, and stops once closed", () => {
    vi.useFakeTimers();
    installFakeSocket();
    const channel = createWorkSurfaceChannel({ wsBaseUrl: "ws://gateway" });
    const first = FakeSocket.last;

    first?.onclose?.();
    vi.advanceTimersByTime(1_000);
    expect(FakeSocket.last).not.toBe(first);

    const second = FakeSocket.last;
    channel.close();
    second?.onclose?.();
    vi.advanceTimersByTime(60_000);
    expect(FakeSocket.last).toBe(second);
    vi.useRealTimers();
  });

  it("can notify a list after the first subscription is ready", () => {
    installFakeSocket();
    const channel = createWorkSurfaceChannel({ notifyOnFirstOpen: true });
    const ready = vi.fn();
    channel.onReconnect(ready);
    expect(ready).not.toHaveBeenCalled();
    FakeSocket.last?.onopen?.();
    expect(ready).toHaveBeenCalledOnce();
    channel.close();
  });

  it("notifies subscribers only after a reconnect succeeds", () => {
    vi.useFakeTimers();
    installFakeSocket();
    const channel = createWorkSurfaceChannel({ wsBaseUrl: "ws://gateway" });
    const reconnected = vi.fn();
    channel.onReconnect(reconnected);

    const first = FakeSocket.last!;
    first.onopen?.();
    expect(reconnected).not.toHaveBeenCalled();
    first.onclose?.();
    vi.advanceTimersByTime(1_000);
    FakeSocket.last?.onopen?.();
    expect(reconnected).toHaveBeenCalledTimes(1);
    channel.close();
    vi.useRealTimers();
  });
});

it("shares one socket and removes only the subscription that closes", () => {
  installFakeSocket();
  vi.useFakeTimers();
  const surfaces = openSharedWorkSurfaceChannel();
  const list = openSharedWorkSurfaceChannel();
  try {
    const socket = FakeSocket.last!;
    const surfaceSignal = vi.fn();
    const listSignal = vi.fn();
    const surfaceReconnect = vi.fn();
    const listReconnect = vi.fn();
    surfaces.onSignal(surfaceSignal);
    list.onSignal(listSignal);
    surfaces.onReconnect(surfaceReconnect);
    list.onReconnect(listReconnect);

    const data = JSON.stringify({ type: "session_updated", session_id: "session" });
    socket.onmessage?.({ data });
    expect(surfaceSignal).toHaveBeenCalledOnce();
    expect(listSignal).toHaveBeenCalledOnce();

    list.close();
    expect(socket.closed).toBe(false);
    socket.onmessage?.({ data });
    expect(surfaceSignal).toHaveBeenCalledTimes(2);
    expect(listSignal).toHaveBeenCalledOnce();

    socket.onclose?.();
    vi.advanceTimersByTime(1_000);
    const reconnected = FakeSocket.last!;
    expect(reconnected).not.toBe(socket);
    reconnected.onopen?.();
    expect(surfaceReconnect).toHaveBeenCalledOnce();
    expect(listReconnect).not.toHaveBeenCalled();
    surfaces.close();
    expect(reconnected.closed).toBe(true);
  } finally {
    surfaces.close();
    list.close();
    vi.useRealTimers();
  }
});
