<!--
  Approval card — replaces the message input while a turn is gated on a
  human-in-the-loop approval.
-->
<script lang="ts">
  import { approvePendingAction, rejectPendingAction } from "../api/operator";
  import type { PendingApproval } from "../stores/streaming";

  interface Props {
    approval: PendingApproval;
  }

  let { approval }: Props = $props();

  type ApprovalAction =
    | "allow"
    | "allow-session"
    | "allow-permanent"
    | "allow-global"
    | "deny";

  let busy = $state<ApprovalAction | null>(null);
  let error = $state<string | null>(null);
  let permanentMenuOpen = $state(false);

  async function decide(action: ApprovalAction): Promise<void> {
    busy = action;
    error = null;
    permanentMenuOpen = false;
    try {
      if (action === "allow") {
        await approvePendingAction(approval.approval_id, "approval card");
      } else if (action === "allow-session") {
        await approvePendingAction(approval.approval_id, "approval card", "session");
      } else if (action === "allow-permanent") {
        await approvePendingAction(
          approval.approval_id,
          "approval card",
          "permanent",
        );
      } else if (action === "allow-global") {
        await approvePendingAction(
          approval.approval_id,
          "approval card",
          "permanent_global",
        );
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
      onclick={() => decide("allow-session")}
      disabled={busy !== null}
      title="Allow matching approvals until the gateway exits"
    >
      {busy === "allow-session" ? "Allowing…" : "Allow for this session"}
    </button>

    <div class="split-action">
      <button
        type="button"
        class="btn split-main"
        onclick={() => decide("allow-permanent")}
        disabled={busy !== null}
        title="Persist this tool as allowed for this bench"
      >
        {busy === "allow-permanent" ? "Allowing…" : "Allow permanently"}
      </button>
      <button
        type="button"
        class="btn split-caret"
        onclick={() => (permanentMenuOpen = !permanentMenuOpen)}
        disabled={busy !== null}
        aria-haspopup="menu"
        aria-expanded={permanentMenuOpen}
        title="More permanent approval scopes"
      >
        ▾
      </button>
      {#if permanentMenuOpen}
        <div class="scope-menu" role="menu">
          <button
            type="button"
            role="menuitem"
            class="menu-item"
            onclick={() => decide("allow-global")}
          >
            {busy === "allow-global"
              ? "Allowing…"
              : "Allow permanently globally"}
          </button>
        </div>
      {/if}
    </div>

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
    Sits flush with the conversation pane edges so the input slot it replaces
    stays the same size.
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

  .split-action {
    position: relative;
    display: inline-flex;
  }

  .split-main {
    border-right: 0;
  }

  .split-caret {
    width: 34px;
    justify-content: center;
    padding-left: 0;
    padding-right: 0;
  }

  .scope-menu {
    position: absolute;
    left: 0;
    bottom: calc(100% + 6px);
    min-width: 236px;
    border: 1px solid var(--ink);
    background: var(--paper);
    z-index: 3;
    box-shadow: 4px 4px 0 var(--accent);
  }

  .menu-item {
    width: 100%;
    padding: 9px 12px;
    border: 0;
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-display);
    font-size: 13px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-align: left;
    text-transform: uppercase;
    cursor: pointer;
  }

  .menu-item:hover {
    background: var(--ink);
    color: var(--paper);
  }
</style>
