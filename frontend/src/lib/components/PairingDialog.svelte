<!--
  The pairing prompt a client sees when the gateway, bound beyond loopback,
  refuses it for want of its token. The person types the one-time code the
  operator issued on the gateway's machine; redeeming it sets the cookie the
  gateway reads from then on, and the page reloads into a paired session.
-->
<script lang="ts">
  import { pairClient } from "../protocol/client";
  import { markPaired, pairingRequired } from "../stores/access";

  let code = $state("");
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function pair(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const trimmed = code.trim();
    if (!trimmed || busy) return;
    busy = true;
    error = null;
    try {
      await pairClient({ pairing_code: trimmed });
      markPaired();
      window.location.reload();
    } catch (err) {
      error = err instanceof Error ? err.message : "Pairing failed.";
    } finally {
      busy = false;
    }
  }
</script>

{#if $pairingRequired}
  <div class="pairing-backdrop" role="dialog" aria-modal="true" aria-labelledby="pairing-title">
    <form class="pairing-dialog" onsubmit={pair}>
      <h2 id="pairing-title">Pair this device</h2>
      <p>
        This gateway is reachable over the network and admits paired devices only.
        On the machine the gateway runs on, run <code>swage gateway pair</code> and
        enter the code it prints here within ten minutes. Pairing is remembered: you
        will not be asked again on this device.
      </p>
      <label>
        Pairing code
        <input
          class="text"
          aria-label="Pairing code"
          bind:value={code}
          placeholder="0000-0000"
          autocomplete="one-time-code"
          disabled={busy}
        />
      </label>
      {#if error}<p class="pairing-error" role="alert">{error}</p>{/if}
      <button type="submit" disabled={busy || !code.trim()}>Pair</button>
    </form>
  </div>
{/if}

<style>
  .pairing-backdrop {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--ink) 70%, transparent);
    z-index: 50;
  }
  .pairing-dialog {
    display: grid;
    gap: 0.75rem;
    max-width: 28rem;
    padding: 1.5rem;
    background: var(--paper);
    color: var(--ink);
    border-radius: 0.5rem;
  }
  .pairing-dialog label {
    display: grid;
    gap: 0.25rem;
  }
  .pairing-error {
    color: var(--error);
  }
</style>
