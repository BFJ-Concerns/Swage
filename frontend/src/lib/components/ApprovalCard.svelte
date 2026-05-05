<!--
  Approval card — replaces the message input while a turn is gated on a
  human-in-the-loop approval. Mirrors riso-cyanotype.html, view 03.

  Allow once / Allow turn / Deny are wired to /api/approvals/:id. The
  turn-scoped action applies only to later approval gates in the same live
  turn; it does not create broader session approval memory.
-->
<script lang="ts">
  import { approvePendingAction, rejectPendingAction } from "../api/operator";
  import type { PendingApproval } from "../stores/streaming";

  interface Props {
    approval: PendingApproval;
  }

  let { approval }: Props = $props();

  let busy = $state<"allow" | "allow-turn" | "deny" | null>(null);
  let error = $state<string | null>(null);

  async function decide(action: "allow" | "allow-turn" | "deny"): Promise<void> {
    busy = action;
    error = null;
    try {
      if (action === "allow") {
        await approvePendingAction(approval.approval_id, "approval card");
      } else if (action === "allow-turn") {
        await approvePendingAction(approval.approval_id, "approval card", "turn");
      } else {
        await rejectPendingAction(approval.approval_id, "approval card");
      }
    } catch (err) {
      error = err instanceof Error ? err.message : "Approval decision failed.";
    } finally {
      busy = null;
    }
  }

  const subjectLabel = $derived(
    approval.tool_name ?? approval.kind ?? "tool call",
  );
</script>

<div class="approval-card" role="dialog" aria-label="Approval required">
  <header class="head">
    <span class="head-label">Approval required</span>
    <span class="head-badge">{subjectLabel}</span>
  </header>
  <p class="body-text">{approval.prompt}</p>

  {#if error}
    <p class="approval-error" role="alert">{error}</p>
  {/if}

  <div class="actions">
    <button
      type="button"
      class="btn primary"
      onclick={() => decide("allow")}
      disabled={busy !== null}
    >
      {busy === "allow" ? "Allowing…" : "Allow once"}
    </button>

    <button
      type="button"
      class="btn"
      onclick={() => decide("allow-turn")}
      disabled={busy !== null}
      title="Allow the rest of this turn"
    >
      {busy === "allow-turn" ? "Allowing…" : "Allow turn"}
    </button>

    <button
      type="button"
      class="btn danger"
      onclick={() => decide("deny")}
      disabled={busy !== null}
    >
      {busy === "deny" ? "Denying…" : "Deny"}
    </button>
  </div>
</div>

<style>
  /*
    Approval card vocabulary mirrors the workup: ink frame, fluoro-orange
    head badge, three buttons stacked on the right. Sits flush with the
    conversation pane edges so the input slot it replaces stays the same
    size.
  */
  .approval-card {
    margin: 12px 22px 18px;
    border: 2px solid var(--ink);
    background: var(--paper);
    display: flex;
    flex-direction: column;
    gap: 10px;
    position: relative;
    isolation: isolate;
  }

  .approval-card::after {
    content: "";
    position: absolute;
    inset: 0;
    border: 2px solid var(--accent);
    transform: translate(var(--offset), var(--offset));
    z-index: -1;
    mix-blend-mode: var(--shadow-blend);
    opacity: var(--shadow-opacity);
    pointer-events: none;
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 8px 14px;
    background: var(--ink);
    color: var(--paper);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
  }

  .head-badge {
    padding: 2px 8px;
    background: var(--accent);
    color: var(--paper);
    border: 1px solid var(--accent);
    font-size: 10px;
    letter-spacing: 0.12em;
  }

  .body-text {
    margin: 0;
    padding: 0 14px;
    font-family: var(--font-body);
    font-size: 13px;
    line-height: 1.55;
    color: var(--ink);
  }

  .approval-error {
    margin: 0;
    padding: 8px 14px;
    background: rgba(240, 106, 58, 0.1);
    border-left: 3px solid var(--accent);
    color: var(--ink-orange);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .actions {
    display: flex;
    gap: 8px;
    padding: 10px 14px 14px;
    flex-wrap: wrap;
  }

  .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 7px 14px;
    font-family: var(--font-display);
    font-size: 14px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    background: var(--paper);
    color: var(--ink);
    border: 1px solid var(--ink);
    cursor: pointer;
  }

  .btn:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
  }

  .btn:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .btn.primary {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
  }

  .btn.primary:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
    border-color: var(--ink);
  }

  .btn.danger {
    color: var(--ink-orange);
    border-color: var(--accent);
  }

  .btn.danger:hover:not(:disabled) {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
  }

</style>
