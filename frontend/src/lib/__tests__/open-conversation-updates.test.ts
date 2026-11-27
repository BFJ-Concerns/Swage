// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";

import type { WorkSurfaceChannel } from "../api/work-surface";
import { watchOpenConversation } from "../open-conversation-updates";
import type { ServerEvent } from "../types";

function fakeChannel() {
  const handlers: ((event: ServerEvent) => void)[] = [];
  const reconnects: (() => void)[] = [];
  const channel: WorkSurfaceChannel & { closed: boolean } = {
    closed: false,
    onSignal: (handler) => { handlers.push(handler); },
    onReconnect: (handler) => { reconnects.push(handler); },
    close() { this.closed = true; },
  };
  return {
    channel,
    signal: (event: ServerEvent) => handlers.forEach((handler) => handler(event)),
    reconnect: () => reconnects.forEach((handler) => handler()),
  };
}

describe("watchOpenConversation", () => {
  it("re-reads the open conversation on its session signal when another tab holds its socket", async () => {
    const fake = fakeChannel();
    const refresh = vi.fn(async () => {});
    let holdsSocket = false;
    const stop = watchOpenConversation(
      { openConversationId: () => "conv-a", holdsSocket: () => holdsSocket, refresh },
      () => fake.channel,
    );

    fake.signal({ type: "session_updated", session_id: "conv-b" });
    await Promise.resolve();
    expect(refresh).not.toHaveBeenCalled();

    fake.signal({ type: "session_updated", session_id: "conv-a" });
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));

    // The tab holding the socket hears the turn itself.
    holdsSocket = true;
    fake.signal({ type: "session_updated", session_id: "conv-a" });
    fake.reconnect();
    await Promise.resolve();
    expect(refresh).toHaveBeenCalledTimes(1);

    stop();
    expect(fake.channel.closed).toBe(true);
  });

  it("catches up after dropped signals and a reconnect", async () => {
    const fake = fakeChannel();
    const refresh = vi.fn(async () => {});
    watchOpenConversation(
      { openConversationId: () => "conv-a", holdsSocket: () => false, refresh },
      () => fake.channel,
    );
    fake.signal({ type: "lagged", dropped: 2 });
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    fake.reconnect();
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(2));
  });
});
