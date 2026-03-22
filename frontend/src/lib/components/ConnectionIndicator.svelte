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
  .banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 16px;
    font-size: 0.8125rem;
    /* Banner sits at the top of the conversation area without blocking content. */
    flex-shrink: 0;
  }

  /* -------------------------------------------------------------------------
     State-specific colours
  ------------------------------------------------------------------------- */

  .banner.disconnected {
    background: #fef3c7;
    color: #92400e;
    border-bottom: 1px solid #fde68a;
  }

  .banner.reconnecting {
    background: #eff6ff;
    color: #1e40af;
    border-bottom: 1px solid #bfdbfe;
  }

  .banner.error {
    background: #fee2e2;
    color: #991b1b;
    border-bottom: 1px solid #fca5a5;
  }

  /* -------------------------------------------------------------------------
     Status dot
  ------------------------------------------------------------------------- */

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .disconnected .dot {
    background: #d97706;
  }

  .reconnecting .dot {
    background: #3b82f6;
  }

  .error .dot {
    background: #dc2626;
  }

  /* Pulse animation for reconnecting state — draws attention without alarming. */
  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50%       { opacity: 0.3; }
  }

  .pulsing {
    animation: pulse 1.4s ease-in-out infinite;
  }

  /* -------------------------------------------------------------------------
     Message and retry button
  ------------------------------------------------------------------------- */

  .message {
    flex: 1;
  }

  .retry-btn {
    padding: 2px 10px;
    font-size: 0.8125rem;
    font-weight: 500;
    border-radius: 4px;
    cursor: pointer;
    background: #fca5a5;
    color: #7f1d1d;
    border: 1px solid #f87171;
  }

  .retry-btn:hover {
    background: #f87171;
    color: #450a0a;
  }
</style>
