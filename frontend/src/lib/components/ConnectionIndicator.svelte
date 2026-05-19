<!--
  Connection status indicator — non-intrusive display of WebSocket state.

  Hidden when connected. Shows as a compact banner for transient states
  (disconnected, reconnecting) and a persistent banner with a Retry button
  for error states that require user action.
-->
<script lang="ts">
  import type { ConnectionStore } from "../stores/connection";

  interface Props {
    // Injected by App.svelte once all stores are wired.
    store?: ConnectionStore;
    // Called when the user clicks Retry. The parent binds this to
    // connectionStore.connectTo(activeConversationId) at wiring time.
    onRetry?: () => void;
  }

  let { store, onRetry }: Props = $props();
</script>

{#if store && store.status !== "connected"}
  <div
    class="banner"
    class:disconnected={store.status === "disconnected"}
    class:reconnecting={store.status === "reconnecting"}
    class:error={store.status === "error"}
    role="status"
    aria-live="polite"
  >
    {#if store.status === "reconnecting"}
      <span class="dot pulsing" aria-hidden="true"></span>
      <span class="message">Reconnecting…</span>
    {:else if store.status === "disconnected"}
      <span class="dot" aria-hidden="true"></span>
      <span class="message">Connection lost</span>
    {:else if store.status === "error"}
      <span class="dot" aria-hidden="true"></span>
      <span class="message">
        {store.errorMessage ?? "Connection error"}
      </span>
      {#if onRetry}
        <button class="retry-btn" onclick={onRetry}>Retry</button>
      {/if}
    {/if}
  </div>
{/if}

<style>
  /*
    Riso alert banner — a fluoro-orange-on-paper strip sitting at the top of
    the conversation pane. Per Surfaces.md ("Connection lost" goes in the
    conversation surface) and Progress.md ("Failures bind to the right scope
    — pane banners for failures that affect everything in the pane.").
  */
  .banner {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 22px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    flex-shrink: 0;
    background: var(--accent);
    color: var(--paper);
  }

  /* All three states share the same fluoro orange treatment — the dot's
     animation distinguishes "reconnecting" from the static failure states. */
  .banner.disconnected,
  .banner.reconnecting,
  .banner.error {
    background: var(--accent);
    color: var(--paper);
    border-bottom: 2px solid var(--ink);
  }

  /* Status block on the left — solid ink against the orange band. */
  .dot {
    width: 8px;
    height: 8px;
    background: var(--ink);
    flex-shrink: 0;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.3; }
  }

  .pulsing {
    animation: pulse 1.4s ease-in-out infinite;
  }

  .message {
    flex: 1;
  }

  .retry-btn {
    padding: 4px 12px;
    font-family: var(--font-display);
    font-size: 14px;
    line-height: 1;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    background: var(--ink);
    color: var(--paper);
    border: none;
    cursor: pointer;
  }

  .retry-btn:hover {
    background: var(--paper);
    color: var(--ink);
  }
</style>
