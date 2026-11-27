import { openSharedWorkSurfaceChannel } from "./api/work-surface";
import type { WorkSurfaceChannel } from "./api/work-surface";

export interface OpenConversationWatch {
  /** The conversation this tab has open, if any. */
  openConversationId(): string | null;
  /** Whether this tab holds the open conversation's socket. */
  holdsSocket(): boolean;
  /** Re-read the open conversation from the gateway. */
  refresh(): Promise<void>;
}

/**
 * Keep an open conversation current when this tab does not hold its socket:
 * the turn's own events go to the tab that does, so a turn that starts, stops
 * or settles reaches this one only as the gateway's session signal.
 */
export function watchOpenConversation(
  watch: OpenConversationWatch,
  createChannel: () => WorkSurfaceChannel = openSharedWorkSurfaceChannel,
): () => void {
  const channel = createChannel();
  let refreshing = false;
  let pending = false;

  async function refresh(): Promise<void> {
    if (watch.holdsSocket() || watch.openConversationId() === null) return;
    pending = true;
    if (refreshing) return;
    refreshing = true;
    try {
      while (pending) {
        pending = false;
        await watch.refresh().catch(() => {
          // A failed read leaves the conversation as it was shown.
        });
      }
    } finally {
      refreshing = false;
    }
  }

  channel.onSignal((event) => {
    if (
      (event.type === "session_updated" && event.session_id === watch.openConversationId()) ||
      event.type === "lagged"
    ) {
      void refresh();
    }
  });
  channel.onReconnect(() => void refresh());
  return () => channel.close();
}
