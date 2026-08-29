// The work-surface channel — the socket an open page listens on. It takes no
// conversation, so a surface stays live whether or not one is selected.

import { describe, it, expect, vi, afterEach } from "vitest";
import { createWorkSurfaceChannel } from "../api/work-surface";
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

  it("delivers a page signal from the wire to its subscriber", () => {
    installFakeSocket();
    const channel = createWorkSurfaceChannel({ wsBaseUrl: "ws://gateway" });
    const seen: ServerEvent[] = [];
    channel.onSignal((event) => seen.push(event));

    FakeSocket.last?.onmessage?.({
      data: JSON.stringify({
        type: "page_updated",
        page_id: "11111111-1111-4111-8111-111111111111",
        module_id: "22222222-2222-4222-8222-222222222222",
      }),
    });

    expect(seen).toHaveLength(1);
    expect(seen[0].type).toBe("page_updated");
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
});
