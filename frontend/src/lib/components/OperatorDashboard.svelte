<script lang="ts">
  import { onMount } from "svelte";
  import {
    approvePendingAction,
    disableHeartbeat,
    enableHeartbeat,
    fetchOperatorOverview,
    pauseScheduledJob,
    rejectPendingAction,
    resumeScheduledJob,
  } from "../api/operator";
  import type { OperatorOverview } from "../types";

  let overview = $state<OperatorOverview | null>(null);
  let isLoading = $state(true);
  let errorMessage = $state<string | null>(null);
  let activeActionKey = $state<string | null>(null);

  async function loadOverview(): Promise<void> {
    isLoading = true;
    errorMessage = null;

    try {
      overview = await fetchOperatorOverview();
    } catch (error) {
      errorMessage =
        error instanceof Error ? error.message : "Failed to load operator overview";
    } finally {
      isLoading = false;
    }
  }

  async function runAction(
    actionKey: string,
    task: () => Promise<void>,
  ): Promise<void> {
    activeActionKey = actionKey;
    errorMessage = null;

    try {
      await task();
      await loadOverview();
    } catch (error) {
      errorMessage =
        error instanceof Error ? error.message : "Operator action failed";
    } finally {
      activeActionKey = null;
    }
  }

  function actionBusy(actionKey: string): boolean {
    return activeActionKey === actionKey;
  }

  function toneForHealth(health: string): string {
    switch (health) {
      case "degraded":
        return "critical";
      case "busy":
        return "busy";
      default:
        return "healthy";
    }
  }

  onMount(async () => {
    await loadOverview();
  });
</script>

<section class="ops-deck">
  <header class="ops-header">
    <div>
      <p class="eyebrow">Operator Deck</p>
      <h1>Live control surface for the running gateway</h1>
      <p class="lede">
        Sessions, approvals, automations, channels, and nodes in one place.
      </p>
    </div>
    <button class="refresh-btn" onclick={loadOverview} disabled={isLoading}>
      {isLoading ? "Refreshing…" : "Refresh"}
    </button>
  </header>

  {#if errorMessage}
    <div class="ops-alert" role="alert">{errorMessage}</div>
  {/if}

  {#if isLoading && !overview}
    <div class="loading-state">Pulling operator state from the gateway…</div>
  {:else if overview}
    <div class="metric-strip">
      <article class="metric-card" data-tone={toneForHealth(overview.gateway.health)}>
        <span class="metric-label">Gateway</span>
        <strong class="metric-value">{overview.gateway.health}</strong>
        <span class="metric-meta">
          {overview.gateway.active_turn_count} active turn(s) ·
          {overview.gateway.active_connection_count} live client(s)
        </span>
      </article>
      <article class="metric-card">
        <span class="metric-label">Approvals</span>
        <strong class="metric-value">{overview.approvals.length}</strong>
        <span class="metric-meta">pending human decisions</span>
      </article>
      <article class="metric-card">
        <span class="metric-label">Automation</span>
        <strong class="metric-value">
          {overview.scheduled_jobs.jobs.length + overview.heartbeats.heartbeats.length}
        </strong>
        <span class="metric-meta">scheduled jobs and heartbeats</span>
      </article>
      <article class="metric-card">
        <span class="metric-label">Channels</span>
        <strong class="metric-value">{overview.channels.channels.length}</strong>
        <span class="metric-meta">known integrations</span>
      </article>
    </div>

    <div class="ops-grid">
      <article class="panel panel-wide">
        <div class="panel-head">
          <div>
            <p class="panel-kicker">Sessions</p>
            <h2>Live and recent sessions</h2>
          </div>
          <span class="panel-count">{overview.sessions.length}</span>
        </div>
        <div class="ledger">
          {#each overview.sessions as session (session.id)}
            <div class="ledger-row">
              <div>
                <div class="primary-line">
                  <span class="mono">{session.id}</span>
                  {#if session.has_active_connection}
                    <span class="badge good">connected</span>
                  {/if}
                  {#if session.has_running_turn}
                    <span class="badge busy">running</span>
                  {/if}
                  {#if session.has_interrupted_turn}
                    <span class="badge warn">interrupted</span>
                  {/if}
                </div>
                <div class="secondary-line">
                  {session.bench_name} ·
                  {session.message_preview ?? "No messages yet"}
                </div>
              </div>
              <span class="timestamp">{new Date(session.last_activity).toLocaleString()}</span>
            </div>
          {/each}
        </div>
      </article>

      <article class="panel">
        <div class="panel-head">
          <div>
            <p class="panel-kicker">Approvals</p>
            <h2>Pending decisions</h2>
          </div>
          <span class="panel-count">{overview.approvals.length}</span>
        </div>
        <div class="stack">
          {#if overview.approvals.length === 0}
            <p class="empty-copy">Nothing is waiting for operator approval.</p>
          {:else}
            {#each overview.approvals as approval (approval.id)}
              <section class="control-card">
                <div class="primary-line">
                  <span class="mono">{approval.tool_name ?? approval.kind}</span>
                  <span class="badge warn">{approval.agent_id}</span>
                </div>
                <p class="detail-copy">{approval.prompt}</p>
                <div class="action-row">
                  <button
                    class="action-btn positive"
                    onclick={() =>
                      runAction(`approve-${approval.id}`, () =>
                        approvePendingAction(approval.id, "operator deck approve"),
                      )}
                    disabled={actionBusy(`approve-${approval.id}`)}
                  >
                    {actionBusy(`approve-${approval.id}`) ? "Approving…" : "Approve"}
                  </button>
                  <button
                    class="action-btn negative"
                    onclick={() =>
                      runAction(`reject-${approval.id}`, () =>
                        rejectPendingAction(approval.id, "operator deck reject"),
                      )}
                    disabled={actionBusy(`reject-${approval.id}`)}
                  >
                    {actionBusy(`reject-${approval.id}`) ? "Rejecting…" : "Reject"}
                  </button>
                </div>
              </section>
            {/each}
          {/if}
        </div>
      </article>

      <article class="panel">
        <div class="panel-head">
          <div>
            <p class="panel-kicker">Scheduled jobs</p>
            <h2>Pause or resume recurring work</h2>
          </div>
          <span class="panel-count">{overview.scheduled_jobs.jobs.length}</span>
        </div>
        <div class="stack">
          {#each overview.scheduled_jobs.jobs as job (job.id)}
            <section class="control-card">
              <div class="primary-line">
                <span>{job.message}</span>
                <span class="badge {job.status === 'active' ? 'good' : 'muted'}">{job.status}</span>
              </div>
              <p class="detail-copy mono">
                {JSON.stringify(job.schedule)}
              </p>
              <div class="action-row">
                {#if job.status === "active"}
                  <button
                    class="action-btn neutral"
                    onclick={() =>
                      runAction(`pause-${job.id}`, () => pauseScheduledJob(job.id))}
                    disabled={actionBusy(`pause-${job.id}`)}
                  >
                    {actionBusy(`pause-${job.id}`) ? "Pausing…" : "Pause"}
                  </button>
                {:else}
                  <button
                    class="action-btn positive"
                    onclick={() =>
                      runAction(`resume-${job.id}`, () => resumeScheduledJob(job.id))}
                    disabled={actionBusy(`resume-${job.id}`)}
                  >
                    {actionBusy(`resume-${job.id}`) ? "Resuming…" : "Resume"}
                  </button>
                {/if}
              </div>
            </section>
          {/each}
        </div>
      </article>

      <article class="panel">
        <div class="panel-head">
          <div>
            <p class="panel-kicker">Heartbeats</p>
            <h2>Restart selected runtime activity</h2>
          </div>
          <span class="panel-count">{overview.heartbeats.heartbeats.length}</span>
        </div>
        <div class="stack">
          {#each overview.heartbeats.heartbeats as heartbeat (heartbeat.id)}
            <section class="control-card">
              <div class="primary-line">
                <span>{heartbeat.message}</span>
                <span class="badge {heartbeat.status === 'enabled' ? 'good' : 'muted'}">
                  {heartbeat.status}
                </span>
              </div>
              <p class="detail-copy">
                every {heartbeat.interval_seconds}s
              </p>
              <div class="action-row">
                {#if heartbeat.status === "enabled"}
                  <button
                    class="action-btn neutral"
                    onclick={() =>
                      runAction(`disable-${heartbeat.id}`, () =>
                        disableHeartbeat(heartbeat.id),
                      )}
                    disabled={actionBusy(`disable-${heartbeat.id}`)}
                  >
                    {actionBusy(`disable-${heartbeat.id}`) ? "Stopping…" : "Stop"}
                  </button>
                {:else}
                  <button
                    class="action-btn positive"
                    onclick={() =>
                      runAction(`enable-${heartbeat.id}`, () =>
                        enableHeartbeat(heartbeat.id),
                      )}
                    disabled={actionBusy(`enable-${heartbeat.id}`)}
                  >
                    {actionBusy(`enable-${heartbeat.id}`) ? "Restarting…" : "Restart"}
                  </button>
                {/if}
              </div>
            </section>
          {/each}
        </div>
      </article>

      <article class="panel">
        <div class="panel-head">
          <div>
            <p class="panel-kicker">Channels</p>
            <h2>Observed delivery lanes</h2>
          </div>
          <span class="panel-count">{overview.channels.channels.length}</span>
        </div>
        <div class="ledger">
          {#each overview.channels.channels as channel (channel.integration)}
            <div class="ledger-row">
              <div>
                <div class="primary-line">
                  <span class="mono">{channel.integration}</span>
                  <span class="badge {channel.status === 'active' ? 'good' : 'muted'}">
                    {channel.status}
                  </span>
                </div>
                <div class="secondary-line">
                  {channel.route_count} route(s) · {channel.delivery_count} delivered response(s)
                </div>
              </div>
              <span class="timestamp">
                {channel.last_delivery_at
                  ? new Date(channel.last_delivery_at).toLocaleString()
                  : "no deliveries"}
              </span>
            </div>
          {/each}
        </div>
      </article>

      <article class="panel">
        <div class="panel-head">
          <div>
            <p class="panel-kicker">Nodes</p>
            <h2>Attached execution hosts</h2>
          </div>
          <span class="panel-count">{overview.nodes.length}</span>
        </div>
        <div class="ledger">
          {#each overview.nodes as node (node.host_id)}
            <div class="ledger-row">
              <div>
                <div class="primary-line">
                  <span>{node.label ?? node.host_id}</span>
                  <span class="badge {node.state === 'connected' ? 'good' : 'warn'}">
                    {node.state}
                  </span>
                </div>
                <div class="secondary-line">
                  {node.kind} · {node.base_url ?? node.workspace_root}
                </div>
              </div>
              <span class="timestamp">
                {node.last_seen_at
                  ? new Date(node.last_seen_at).toLocaleString()
                  : "local host"}
              </span>
            </div>
          {/each}
        </div>
      </article>
    </div>
  {/if}
</section>

<style>
  .ops-deck {
    min-height: 100%;
    padding: 28px;
    background:
      radial-gradient(circle at top right, rgba(234, 88, 12, 0.12), transparent 24%),
      radial-gradient(circle at left 20%, rgba(15, 23, 42, 0.08), transparent 22%),
      linear-gradient(180deg, #f6f0e4 0%, #f8f6ef 100%);
    overflow: auto;
  }

  .ops-header {
    display: flex;
    justify-content: space-between;
    gap: 24px;
    align-items: flex-start;
    margin-bottom: 24px;
  }

  .eyebrow,
  .panel-kicker {
    text-transform: uppercase;
    letter-spacing: 0.16em;
    font-size: 0.72rem;
    color: #9a3412;
    margin-bottom: 8px;
    font-family: "Menlo", "Monaco", "Consolas", monospace;
  }

  h1,
  h2 {
    font-family: "Iowan Old Style", "Palatino Linotype", "Book Antiqua", serif;
    font-weight: 600;
    color: #1f2937;
    line-height: 1.05;
  }

  h1 {
    font-size: clamp(2rem, 4vw, 3.4rem);
    max-width: 12ch;
  }

  h2 {
    font-size: 1.18rem;
  }

  .lede,
  .detail-copy,
  .secondary-line,
  .metric-meta,
  .loading-state,
  .empty-copy,
  .timestamp {
    color: #5b6470;
    font-size: 0.92rem;
  }

  .refresh-btn,
  .action-btn {
    border: 1px solid rgba(31, 41, 55, 0.14);
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.82);
    color: #1f2937;
    cursor: pointer;
    transition:
      transform 140ms ease,
      background 140ms ease,
      border-color 140ms ease;
  }

  .refresh-btn:hover,
  .action-btn:hover {
    transform: translateY(-1px);
    background: #fffdf8;
    border-color: rgba(154, 52, 18, 0.35);
  }

  .refresh-btn:disabled,
  .action-btn:disabled {
    cursor: wait;
    transform: none;
    opacity: 0.68;
  }

  .refresh-btn {
    padding: 10px 18px;
    font-weight: 600;
  }

  .ops-alert {
    margin-bottom: 20px;
    padding: 12px 14px;
    border-left: 4px solid #dc2626;
    background: rgba(254, 226, 226, 0.9);
    color: #7f1d1d;
  }

  .metric-strip {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 14px;
    margin-bottom: 20px;
  }

  .metric-card,
  .panel {
    border: 1px solid rgba(31, 41, 55, 0.08);
    background: rgba(255, 252, 248, 0.82);
    backdrop-filter: blur(6px);
    box-shadow: 0 12px 30px rgba(15, 23, 42, 0.05);
  }

  .metric-card {
    padding: 16px;
    border-radius: 22px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .metric-card[data-tone="critical"] {
    border-color: rgba(220, 38, 38, 0.26);
    background: linear-gradient(135deg, rgba(254, 242, 242, 0.95), rgba(255, 251, 235, 0.92));
  }

  .metric-card[data-tone="busy"] {
    border-color: rgba(234, 88, 12, 0.26);
  }

  .metric-card[data-tone="healthy"] {
    border-color: rgba(22, 163, 74, 0.18);
  }

  .metric-label {
    text-transform: uppercase;
    letter-spacing: 0.12em;
    font-size: 0.72rem;
    color: #8a6d4a;
    font-family: "Menlo", "Monaco", "Consolas", monospace;
  }

  .metric-value {
    font-size: 1.6rem;
    color: #111827;
  }

  .ops-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 16px;
  }

  .panel {
    border-radius: 24px;
    padding: 18px;
  }

  .panel-wide {
    grid-column: 1 / -1;
  }

  .panel-head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    margin-bottom: 16px;
  }

  .panel-count {
    font-family: "Menlo", "Monaco", "Consolas", monospace;
    color: #9a3412;
    font-size: 0.88rem;
  }

  .ledger,
  .stack {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .ledger-row,
  .control-card {
    padding: 14px;
    border-radius: 18px;
    border: 1px solid rgba(15, 23, 42, 0.07);
    background: rgba(255, 255, 255, 0.66);
  }

  .ledger-row {
    display: flex;
    justify-content: space-between;
    gap: 16px;
    align-items: center;
  }

  .primary-line {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 4px;
    color: #111827;
    font-weight: 600;
  }

  .mono {
    font-family: "Menlo", "Monaco", "Consolas", monospace;
    font-size: 0.82rem;
  }

  .badge {
    border-radius: 999px;
    padding: 3px 9px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-size: 0.68rem;
    font-family: "Menlo", "Monaco", "Consolas", monospace;
  }

  .badge.good {
    background: #dcfce7;
    color: #166534;
  }

  .badge.busy {
    background: #ffedd5;
    color: #c2410c;
  }

  .badge.warn {
    background: #fef3c7;
    color: #a16207;
  }

  .badge.muted {
    background: #e5e7eb;
    color: #4b5563;
  }

  .action-row {
    display: flex;
    gap: 10px;
    margin-top: 12px;
  }

  .action-btn {
    padding: 8px 14px;
    font-size: 0.86rem;
    font-weight: 600;
  }

  .action-btn.positive {
    color: #166534;
  }

  .action-btn.negative {
    color: #991b1b;
  }

  .action-btn.neutral {
    color: #92400e;
  }

  @media (max-width: 1080px) {
    .metric-strip,
    .ops-grid {
      grid-template-columns: 1fr;
    }

    .ops-header,
    .ledger-row {
      flex-direction: column;
      align-items: flex-start;
    }

    .panel-wide {
      grid-column: auto;
    }
  }
</style>
