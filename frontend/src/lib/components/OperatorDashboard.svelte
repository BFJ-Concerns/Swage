<script lang="ts">
  import { onMount } from "svelte";
  import {
    approvePendingAction,
    fetchOperatorOverview,
    fetchScheduleRuns,
    pauseSchedule,
    rejectPendingAction,
    resumeSchedule,
  } from "../api/operator";
  import type { OperatorOverview } from "../types";
  import StatusBadge from "./StatusBadge.svelte";
  import OperatorHistoryPane from "./OperatorHistoryPane.svelte";
  import type { Schedule } from "../protocol/Schedule";
  import type { ScheduleRun } from "../protocol/ScheduleRun";

  let overview = $state<OperatorOverview | null>(null);
  let isLoading = $state(true);
  let errorMessage = $state<string | null>(null);
  let activeActionKey = $state<string | null>(null);
  let openScheduleId = $state<string | null>(null);
  let scheduleRuns = $state<Record<string, ScheduleRun[]>>({});
  let showHistory = $state(false);
  const approvalsIdle = $derived((overview?.approvals.length ?? 0) === 0);

  function describeSpec(schedule: Schedule): string {
    if (schedule.spec.kind === "interval") return schedule.spec.every;
    if (schedule.spec.kind === "cron") return `cron ${schedule.spec.cron}`;
    return `once at ${new Date(schedule.spec.at).toLocaleString()}`;
  }

  function describeSession(schedule: Schedule): string {
    if (schedule.session.policy === "fresh") {
      return `fresh session · runs as ${schedule.session.agent_id}`;
    }
    return `continues session ${schedule.session.session_id.slice(0, 8)}`;
  }

  function describeCreator(creator: Schedule["creator"]): string {
    return creator.kind === "agent" ? `agent ${creator.agent_id}` : "you";
  }

  function describeRunOutcome(run: ScheduleRun): string {
    if (run.state === "completed_empty") {
      return "No assistant text produced.";
    }
    if (run.state === "skipped") {
      return run.skip_reason === "gate"
        ? (run.gate?.reasoning ?? "gate decided no work was needed")
        : "a previous run was still executing";
    }
    if (run.state === "failed") return run.error_message ?? "failed";
    return run.output_text ?? "";
  }

  function describeRunState(run: ScheduleRun): string {
    return run.state === "completed_empty" ? "completed empty" : run.state;
  }

  async function toggleScheduleRuns(scheduleId: string): Promise<void> {
    if (openScheduleId === scheduleId) {
      openScheduleId = null;
      return;
    }
    await runAction(`runs-${scheduleId}`, async () => {
      const response = await fetchScheduleRuns(scheduleId);
      scheduleRuns = { ...scheduleRuns, [scheduleId]: response.runs.toReversed() };
      openScheduleId = scheduleId;
    });
  }

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
        Sessions, approvals, automations, channels, and devices in one place.
      </p>
    </div>
    <div class="header-actions">
      <button class="refresh-btn history-btn" onclick={() => (showHistory = !showHistory)}>
        {showHistory ? "Close history" : "View history"}
      </button>
      <button class="refresh-btn" onclick={loadOverview} disabled={isLoading}>
        {isLoading ? "Refreshing…" : "Refresh"}
      </button>
    </div>
  </header>

  {#if showHistory}<OperatorHistoryPane />{/if}

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
          {overview.schedules.schedules.length}
        </strong>
        <span class="metric-meta">schedules</span>
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
                  {session.topic_name} ·
                  {session.message_preview ?? "No messages yet"}
                </div>
              </div>
              <span class="timestamp">{new Date(session.last_activity).toLocaleString()}</span>
            </div>
          {/each}
        </div>
      </article>

      <!-- With nothing pending, Approvals shrinks to a full-width strip so
           Schedules can take the whole row instead of sharing it with an empty pane. -->
      <article
        class="panel"
        class:panel-wide={approvalsIdle}
        class:panel-compact={approvalsIdle}
      >
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

      <article class="panel" class:panel-wide={approvalsIdle}>
        <div class="panel-head">
          <div>
            <p class="panel-kicker">Schedules</p>
            <h2>Pause, resume, and inspect scheduled work</h2>
          </div>
          <span class="panel-count">{overview.schedules.schedules.length}</span>
        </div>
        <div class="stack">
          {#if overview.schedules.schedules.length === 0}
            <p class="empty-copy">Nothing is scheduled.</p>
          {:else}
            {#each overview.schedules.schedules as schedule (schedule.id)}
              <section class="control-card">
                <div class="primary-line">
                  <span>{schedule.message}</span>
                  <StatusBadge
                    register={schedule.status === "active" ? "calm" : "not-yet"}
                    label={schedule.status}
                  />
                </div>
                <p class="detail-copy">
                  {describeSpec(schedule)} · {describeSession(schedule)}
                  {#if schedule.gate}
                    · gated by {schedule.gate.model}
                  {/if}
                  · created by {describeCreator(schedule.creator)}
                </p>
                {#if schedule.status !== "paused" && schedule.next_run_at}
                  <p class="detail-copy">
                    next run {new Date(schedule.next_run_at).toLocaleString()}
                  </p>
                {/if}
                <div class="action-row">
                  {#if schedule.status === "active"}
                    <button
                      class="action-btn neutral"
                      onclick={() =>
                        runAction(`pause-${schedule.id}`, () => pauseSchedule(schedule.id))}
                      disabled={actionBusy(`pause-${schedule.id}`)}
                    >
                      {actionBusy(`pause-${schedule.id}`) ? "Pausing…" : "Pause"}
                    </button>
                  {:else}
                    <button
                      class="action-btn positive"
                      onclick={() =>
                        runAction(`resume-${schedule.id}`, () => resumeSchedule(schedule.id))}
                      disabled={actionBusy(`resume-${schedule.id}`)}
                    >
                      {actionBusy(`resume-${schedule.id}`) ? "Resuming…" : "Resume"}
                    </button>
                  {/if}
                  <button
                    class="action-btn neutral"
                    onclick={() => toggleScheduleRuns(schedule.id)}
                    disabled={actionBusy(`runs-${schedule.id}`)}
                  >
                    {openScheduleId === schedule.id ? "Hide runs" : "Runs"}
                  </button>
                </div>
                {#if openScheduleId === schedule.id}
                  <div class="run-history">
                    {#each scheduleRuns[schedule.id] ?? [] as run (run.id)}
                      <div class="run-entry">
                        <div class="primary-line">
                          <StatusBadge
                            register={run.state === "completed" || run.state === "completed_empty"
                              ? "calm"
                              : run.state === "failed"
                                ? "wants-a-human"
                                : "not-yet"}
                            label={describeRunState(run)}
                          />
                          <span class="mono">{run.trigger}</span>
                          {#if run.skip_reason}
                            <span class="mono">skipped: {run.skip_reason}</span>
                          {/if}
                          <span class="timestamp">
                            {new Date(run.created_at).toLocaleString()}
                          </span>
                        </div>
                        {#if run.gate}
                          <p class="detail-copy">
                            gate ({run.gate.model}): {run.gate.verdict} — {run.gate.reasoning}
                          </p>
                        {/if}
                        {#if describeRunOutcome(run)}
                          <p class="detail-copy">{describeRunOutcome(run)}</p>
                        {/if}
                      </div>
                    {:else}
                      <p class="empty-copy">No runs recorded yet.</p>
                    {/each}
                  </div>
                {/if}
              </section>
            {/each}
          {/if}
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
            <p class="panel-kicker">Devices</p>
            <h2>Attached devices</h2>
          </div>
          <span class="panel-count">{overview.devices.length}</span>
        </div>
        <div class="ledger">
          {#if overview.devices.length === 0}
            <p class="empty-copy">
              No devices are attached. To attach one, have the device redeem a one-time pairing code
              from this gateway, then run <code>swage device connect</code> on it;
              the Registration and pairing section of the documentation walks through both steps.
            </p>
          {/if}
          {#each overview.devices as device (device.device_id)}
            <div class="ledger-row">
              <div>
                <div class="primary-line">
                  <span>{device.label ?? device.device_id}</span>
                  <StatusBadge
                    register={device.state === "connected" ? "calm" : "wants-a-human"}
                    label={device.state}
                  />
                </div>
                <div class="secondary-line">
                  {device.kind} · {device.workspace_root}
                </div>
              </div>
              <span class="timestamp">
                {device.last_seen_at
                  ? new Date(device.last_seen_at).toLocaleString()
                  : "local device"}
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

  .header-actions { display: flex; gap: 8px; flex-wrap: wrap; }

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
    color: color-mix(in srgb, var(--paper) 85%, transparent);
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

  .panel-compact {
    padding-block: 10px;
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

  .run-history {
    margin-top: 8px;
    padding-left: 12px;
    border-left: 2px solid var(--rule-soft);
    display: grid;
    gap: 8px;
  }

  .run-entry {
    display: grid;
    gap: 2px;
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

  .empty-copy code {
    text-transform: none;
    letter-spacing: normal;
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
