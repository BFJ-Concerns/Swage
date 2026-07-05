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
  import StatusBadge from "./StatusBadge.svelte";

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
                    <StatusBadge register="calm" label="connected" />
                  {/if}
                  {#if session.has_running_turn}
                    <StatusBadge register="in-motion" label="running" />
                  {/if}
                  {#if session.has_interrupted_turn}
                    <StatusBadge register="wants-a-human" label="interrupted" />
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
                  <StatusBadge register="wants-a-human" label={approval.agent_id} />
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
                <StatusBadge
                  register={job.status === "active" ? "calm" : "not-yet"}
                  label={job.status}
                />
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
                <StatusBadge
                  register={heartbeat.status === "enabled" ? "calm" : "not-yet"}
                  label={heartbeat.status}
                />
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
                  <StatusBadge
                    register={channel.status === "active" ? "calm" : "not-yet"}
                    label={channel.status}
                  />
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
                  <StatusBadge
                    register={node.state === "connected" ? "calm" : "wants-a-human"}
                    label={node.state}
                  />
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
  /*
    Riso/cyanotype operator deck. Two-tone print vocabulary throughout:
      - Bebas Neue display headings, Hanken Grotesk body, DM Mono for labels.
      - Flat ink-bordered cards, no rounding, no shadows.
      - Active/critical badges use the fluoro accent; muted ones stay paper.
  */

  .ops-deck {
    min-height: 100%;
    padding: 24px 28px;
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-body);
    overflow: auto;
  }

  .ops-header {
    display: flex;
    justify-content: space-between;
    gap: 24px;
    align-items: flex-start;
    margin-bottom: 22px;
    border-bottom: 2px solid var(--ink);
    padding-bottom: 18px;
  }

  .eyebrow,
  .panel-kicker,
  .metric-label {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
    margin: 0 0 8px;
  }

  h1 {
    font-family: var(--font-display);
    font-weight: 400;
    font-size: clamp(2rem, 3.8vw, 2.8rem);
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink);
    max-width: 16ch;
    margin: 0;
  }

  h2 {
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 18px;
    line-height: 1.05;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink);
    margin: 0;
  }

  .lede,
  .detail-copy,
  .secondary-line,
  .metric-meta,
  .loading-state,
  .empty-copy,
  .timestamp {
    color: var(--ink-soft);
    font-size: 13px;
    line-height: 1.5;
  }

  .timestamp {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.06em;
    white-space: nowrap;
  }

  /* ---- Top-right refresh ---- */

  .refresh-btn {
    padding: 8px 16px;
    font-family: var(--font-display);
    font-size: 16px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    background: transparent;
    color: var(--ink);
    border: 1px solid var(--ink);
    cursor: pointer;
    flex-shrink: 0;
  }

  .refresh-btn:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
  }

  .refresh-btn:disabled {
    opacity: 0.5;
    cursor: wait;
  }

  /* ---- Alert banner: same vocabulary as ConnectionIndicator ---- */

  .ops-alert {
    margin-bottom: 18px;
    padding: 8px 14px;
    background: var(--accent);
    color: var(--paper);
    border: 2px solid var(--ink);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .loading-state {
    padding: 14px 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
  }

  /* ---- Metric strip ---- */

  .metric-strip {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 12px;
    margin-bottom: 18px;
  }

  .metric-card {
    padding: 14px;
    border: 2px solid var(--ink);
    background: var(--paper);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  /* Tone-tinted cards: critical fills with accent, healthy/busy keep paper. */
  .metric-card[data-tone="critical"] {
    background: var(--accent);
    color: var(--paper);
  }

  .metric-card[data-tone="critical"] .metric-label,
  .metric-card[data-tone="critical"] .metric-meta {
    color: rgba(255, 248, 238, 0.85);
  }

  .metric-card[data-tone="busy"] {
    border-color: var(--accent);
  }

  .metric-value {
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 28px;
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: inherit;
  }

  /* ---- Panels ---- */

  .ops-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 14px;
  }

  .panel {
    padding: 16px 18px;
    border: 2px solid var(--ink);
    background: var(--paper);
  }

  .panel-wide {
    grid-column: 1 / -1;
  }

  .panel-head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
    margin-bottom: 12px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--ink);
  }

  .panel-count {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.16em;
    background: var(--accent);
    color: var(--paper);
    padding: 4px 8px;
    flex-shrink: 0;
  }

  .ledger,
  .stack {
    display: flex;
    flex-direction: column;
    gap: 0;
  }

  .ledger-row {
    padding: 10px 0;
    border-bottom: 1px dotted var(--rule-soft);
    display: flex;
    justify-content: space-between;
    gap: 16px;
    align-items: center;
  }

  .ledger-row:last-child {
    border-bottom: none;
  }

  .control-card {
    padding: 12px 0;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .control-card:last-child {
    border-bottom: none;
  }

  .empty-copy {
    padding: 12px 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .primary-line {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    margin-bottom: 4px;
    color: var(--ink);
    font-family: var(--font-body);
    font-size: 13px;
    font-weight: 600;
  }

  .mono {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.04em;
  }

  /* Status badges now render through StatusBadge.svelte — see it for the
     register → hue treatment. The dashboard maps domain state to a register
     at each call site and never reaches for a colour. */

  /* ---- Action buttons ---- */

  .action-row {
    display: flex;
    gap: 8px;
    margin-top: 10px;
  }

  .action-btn {
    padding: 6px 12px;
    font-family: var(--font-display);
    font-size: 14px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    background: transparent;
    color: var(--ink);
    border: 1px solid var(--ink);
    cursor: pointer;
  }

  .action-btn:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
  }

  .action-btn:disabled {
    opacity: 0.5;
    cursor: wait;
  }

  /* Semantic action variants — colour the border to telegraph intent. */
  .action-btn.positive {
    border-color: var(--ink);
  }

  .action-btn.positive:hover:not(:disabled) {
    background: var(--ink);
  }

  .action-btn.negative {
    border-color: var(--accent);
    color: var(--ink-orange);
  }

  .action-btn.negative:hover:not(:disabled) {
    background: var(--accent);
    color: var(--paper);
  }

  .action-btn.neutral {
    color: var(--ink-soft);
  }

  /* ---- Narrow viewport ---- */
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
