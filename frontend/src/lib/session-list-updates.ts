import { createWorkSurfaceChannel } from "./api/work-surface";
import type { WorkSurfaceChannel } from "./api/work-surface";

/** Re-read a mounted session list on gateway changes and gaps in delivery. */
export function watchSessionList(
  refresh: () => Promise<void>,
  createChannel: () => WorkSurfaceChannel = () => createWorkSurfaceChannel({ notifyOnFirstOpen: true }),
): () => void {
  const channel = createChannel();
  let timer: ReturnType<typeof setTimeout> | undefined;
  let refreshing = false;
  let pending = false;
  let closed = false;

  function scheduleRefresh(): void {
    if (closed || document.hidden) return;
    pending = true;
    if (refreshing || timer !== undefined) return;
    // Coalesce signals emitted together, including a turn's start and input.
    timer = setTimeout(runRefresh, 0);
  }

  async function runRefresh(): Promise<void> {
    timer = undefined;
    if (closed || document.hidden) return;
    pending = false;
    refreshing = true;
    try {
      await refresh();
    } finally {
      refreshing = false;
      if (pending) scheduleRefresh();
    }
  }

  channel.onSignal((event) => {
    if (event.type === "session_updated" || event.type === "lagged") scheduleRefresh();
  });
  channel.onReconnect(scheduleRefresh);
  document.addEventListener("visibilitychange", scheduleRefresh);
  return () => {
    closed = true;
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", scheduleRefresh);
    channel.close();
  };
}
