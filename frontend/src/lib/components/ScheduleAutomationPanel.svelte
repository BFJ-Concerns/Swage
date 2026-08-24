<script lang="ts">
  import { createSchedule } from "../api/workflows";
  import {
    fetchScheduleRuns,
    listSchedules,
    pauseSchedule,
    resumeSchedule,
    triggerSchedule,
  } from "../api/operator";
  import type { Schedule } from "../protocol/Schedule";
  import type { ScheduleRun } from "../protocol/ScheduleRun";
  import type { ScheduleCreator } from "../protocol/ScheduleCreator";
  import type { ScheduleSpec } from "../protocol/ScheduleSpec";
  import type { SessionPolicy } from "../protocol/SessionPolicy";
  import type { Space } from "../types";

  interface Props {
    spaceId: string | null;
    sessionId: string | null;
    spaces?: Space[];
  }

  let { spaceId, sessionId, spaces = [] }: Props = $props();

  // Creation form.
  let scheduleMessage = $state("");
  let scheduleKind = $state<"interval" | "cron" | "one_shot">("interval");
  let scheduleEvery = $state("every 1 hours");
  let scheduleCron = $state("0 0 9 * * * *");
  let scheduleAt = $state("");
  let schedulePolicy = $state<"fresh" | "continuation">("fresh");
  let scheduleAgentId = $state("gateway");
  let gateEnabled = $state(false);
  let gateModel = $state("");
  let gateCriteria = $state("");
  let isCreating = $state(false);

  // Listing.
  let schedules = $state<Schedule[]>([]);
  let runsById = $state<Record<string, ScheduleRun[]>>({});
  let actionId = $state<string | null>(null);

  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  $effect(() => {
    void loadSchedules();
  });

  async function loadSchedules(): Promise<void> {
    try {
      const response = await listSchedules();
      schedules = response.schedules;
      const histories = await Promise.all(
        schedules.map(async (schedule) => {
          const runs = await fetchScheduleRuns(schedule.id);
          return [schedule.id, runs.runs] as const;
        }),
      );
      runsById = Object.fromEntries(histories);
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load schedules.";
    }
  }

  function buildScheduleSpec(): ScheduleSpec {
    if (scheduleKind === "interval") {
      const every = scheduleEvery.trim();
      if (!every) throw new Error("Interval schedules need an 'every N minutes' cadence.");
      return { kind: "interval", every };
    }
    if (scheduleKind === "cron") {
      const cron = scheduleCron.trim();
      if (!cron) throw new Error("Cron schedules need a cron expression.");
      return { kind: "cron", cron };
    }
    const at = scheduleAt.trim();
    if (!at) throw new Error("One-off schedules need a moment to fire at.");
    // datetime-local inputs omit the zone; treat the value as local time.
    return { kind: "one_shot", at: new Date(at).toISOString() };
  }

  function buildSessionPolicy(): SessionPolicy {
    if (schedulePolicy === "continuation") {
      if (!sessionId) {
        throw new Error("Open a conversation to schedule work that continues it.");
      }
      return { policy: "continuation", session_id: sessionId };
    }
    if (!spaceId) throw new Error("Select a space before scheduling work.");
    const agentId = scheduleAgentId.trim();
    if (!agentId) throw new Error("Fresh-session schedules need a target agent.");
    return { policy: "fresh", space_id: spaceId, agent_id: agentId };
  }

  async function createAutomationSchedule(): Promise<void> {
    error = null;
    notice = null;
    const message = scheduleMessage.trim();
    if (!message) {
      error = "Scheduled work needs a prompt.";
      return;
    }
    // A ticked gate with no model is an incomplete choice, not a silent
    // "no gate" — surface it before the request goes out.
    if (gateEnabled && !gateModel.trim()) {
      error = "Enter a gate model, or turn the gate check off.";
      return;
    }

    let spec: ScheduleSpec;
    let session: SessionPolicy;
    try {
      spec = buildScheduleSpec();
      session = buildSessionPolicy();
    } catch (err) {
      error = err instanceof Error ? err.message : "Schedule details are incomplete.";
      return;
    }

    isCreating = true;
    try {
      const created = await createSchedule({
        message,
        spec,
        session,
        routing: null,
        gate: gateEnabled
          ? { model: gateModel.trim(), criteria: gateCriteria.trim() || null }
          : null,
      });
      scheduleMessage = "";
      notice = created.next_run_at
        ? `Schedule created — next run ${new Date(created.next_run_at).toLocaleString()}.`
        : `Schedule created: ${created.status}.`;
      await loadSchedules();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to create schedule.";
    } finally {
      isCreating = false;
    }
  }

  async function runNow(schedule: Schedule): Promise<void> {
    error = null;
    notice = null;
    actionId = schedule.id;
    try {
      const response = await triggerSchedule(schedule.id);
      notice = `Ran "${schedule.message}" — ${describeRunState(response.run)}.`;
      await loadSchedules();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to run the schedule.";
    } finally {
      actionId = null;
    }
  }

  async function setPaused(schedule: Schedule, paused: boolean): Promise<void> {
    error = null;
    notice = null;
    actionId = schedule.id;
    try {
      if (paused) {
        await pauseSchedule(schedule.id);
      } else {
        await resumeSchedule(schedule.id);
      }
      await loadSchedules();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to update the schedule.";
    } finally {
      actionId = null;
    }
  }

  function describeSpec(spec: ScheduleSpec): string {
    if (spec.kind === "interval") return spec.every;
    if (spec.kind === "cron") return `cron ${spec.cron}`;
    return `once at ${new Date(spec.at).toLocaleString()}`;
  }

  function describeSession(session: SessionPolicy): string {
    const targetSpace = session.policy === "fresh"
      ? spaces.find((space) => space.id === session.space_id)
      : null;
    return session.policy === "fresh"
      ? `fresh · space ${targetSpace?.name ?? session.space_id} · agent ${session.agent_id}`
      : `continues session ${session.session_id}`;
  }

  function describeCreator(creator: ScheduleCreator): string {
    return creator.kind === "agent" ? `agent ${creator.agent_id}` : "you";
  }

  function formatTime(value: string | null): string {
    return value ? new Date(value).toLocaleString() : "—";
  }

  function describeRunState(run: ScheduleRun): string {
    return run.state === "completed_empty" ? "completed empty" : run.state;
  }

  function describeRunOutcome(run: ScheduleRun): string {
    if (run.state === "completed_empty") return "No assistant text produced.";
    return "";
  }

  // Newest run first.
  function runHistory(scheduleId: string): ScheduleRun[] {
    return [...(runsById[scheduleId] ?? [])].reverse();
  }
</script>

<section class="schedule-automation">
  <div class="panel-heading">
    <h3>Schedules</h3>
    <span>Durable cron, interval, and one-off agent work.</span>
  </div>

  {#if error}
    <p class="feedback error">{error}</p>
  {/if}
  {#if notice}
    <p class="feedback success">{notice}</p>
  {/if}

  <section class="create-card">
    <label>
      <span>Prompt</span>
      <textarea
        bind:value={scheduleMessage}
        rows="3"
        placeholder="Run the review checklist."
      ></textarea>
    </label>
    <label>
      <span>When</span>
      <select bind:value={scheduleKind}>
        <option value="interval">On a cadence</option>
        <option value="cron">On the clock (cron)</option>
        <option value="one_shot">Once, at a moment</option>
      </select>
    </label>
    {#if scheduleKind === "interval"}
      <label>
        <span>Cadence</span>
        <input bind:value={scheduleEvery} placeholder="every 1 hours" />
      </label>
    {:else if scheduleKind === "cron"}
      <label>
        <span>Cron expression</span>
        <input bind:value={scheduleCron} placeholder="0 0 9 * * * *" />
      </label>
    {:else}
      <label>
        <span>Moment</span>
        <input type="datetime-local" bind:value={scheduleAt} />
      </label>
    {/if}
    <div class="scope-toggle" role="tablist" aria-label="Schedule session policy">
      <button
        type="button"
        class:active={schedulePolicy === "fresh"}
        onclick={() => (schedulePolicy = "fresh")}
      >
        Fresh session
      </button>
      <button
        type="button"
        class:active={schedulePolicy === "continuation"}
        disabled={!sessionId}
        onclick={() => (schedulePolicy = "continuation")}
      >
        This conversation
      </button>
    </div>
    {#if schedulePolicy === "fresh"}
      <label>
        <span>Agent</span>
        <input bind:value={scheduleAgentId} placeholder="gateway" />
      </label>
    {/if}
    <label class="gate-toggle">
      <input type="checkbox" bind:checked={gateEnabled} />
      <span>Check with a small model first</span>
    </label>
    {#if gateEnabled}
      <label>
        <span>Gate model</span>
        <input bind:value={gateModel} placeholder="model id or alias" />
      </label>
      <label>
        <span>Run when…</span>
        <textarea
          bind:value={gateCriteria}
          rows="2"
          placeholder="Only when there is something new to act on."
        ></textarea>
      </label>
    {/if}
    <button
      type="button"
      class="primary"
      onclick={createAutomationSchedule}
      disabled={isCreating}
    >
      {isCreating ? "Creating…" : "Create schedule"}
    </button>
  </section>

  <div class="schedule-list" aria-label="Schedules">
    {#each schedules as schedule (schedule.id)}
      <article class="schedule-row">
        <div class="schedule-main">
          <strong>{schedule.message}</strong>
          <span class="schedule-sub">
            {describeSpec(schedule.spec)} · {describeSession(schedule.session)}
            {schedule.gate ? ` · gate ${schedule.gate.model}` : ""}
          </span>
        </div>
        <div class="schedule-facts">
          <span class="status status-{schedule.status}">{schedule.status}</span>
          <span class="creator creator-{schedule.creator.kind}"
            >by {describeCreator(schedule.creator)}</span
          >
          <span>next {formatTime(schedule.next_run_at)}</span>
        </div>
        <div class="scope-toggle row-actions">
          <button
            type="button"
            onclick={() => runNow(schedule)}
            disabled={actionId === schedule.id}
          >
            {actionId === schedule.id ? "Running…" : "Run now"}
          </button>
          {#if schedule.status === "active"}
            <button
              type="button"
              onclick={() => setPaused(schedule, true)}
              disabled={actionId === schedule.id}
            >
              Pause
            </button>
          {:else}
            <button
              type="button"
              onclick={() => setPaused(schedule, false)}
              disabled={actionId === schedule.id}
            >
              Resume
            </button>
          {/if}
        </div>
        <div class="run-history" aria-label={`Run history for ${schedule.message}`}>
          {#each runHistory(schedule.id) as run (run.id)}
            <div class="run-row">
              <div class="run-head">
                <span class="status status-run-{run.state}">{describeRunState(run)}</span>
                <span>{run.trigger}</span>
                {#if run.skip_reason}<span>skipped: {run.skip_reason}</span>{/if}
                {#if run.gate}<span>gate {run.gate.verdict}</span>{/if}
              </div>
              {#if describeRunOutcome(run)}<p class="run-detail">{describeRunOutcome(run)}</p>{/if}
              {#if run.output_text}<p class="run-detail">{run.output_text}</p>{/if}
              {#if run.error_message}<p class="run-detail error">{run.error_message}</p>{/if}
              <span class="run-time">{formatTime(run.created_at)}</span>
            </div>
          {:else}
            <p class="empty">No runs recorded.</p>
          {/each}
        </div>
      </article>
    {:else}
      <p class="empty">No schedules yet.</p>
    {/each}
  </div>
</section>

<style>
  .schedule-automation {
    display: grid;
    gap: 14px;
  }

  .panel-heading {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
  }

  .panel-heading h3 {
    margin: 0;
  }

  .panel-heading span {
    color: var(--ink-soft);
    font-size: 0.85rem;
  }

  .create-card {
    display: grid;
    gap: 10px;
    padding: 14px;
    border-radius: 16px;
    background: color-mix(in srgb, var(--paper) 64%, transparent);
    border: 1px solid color-mix(in srgb, var(--ink) 8%, transparent);
  }

  label {
    display: grid;
    gap: 6px;
  }

  label span {
    font-size: 0.82rem;
    font-weight: 700;
    color: var(--ink-orange);
  }

  input,
  select,
  textarea {
    border-radius: 16px;
    border: 1px solid color-mix(in srgb, var(--ink) 14%, transparent);
    background: color-mix(in srgb, var(--paper) 76%, transparent);
    padding: 12px 14px;
    resize: vertical;
  }

  .scope-toggle {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px;
  }

  .scope-toggle button,
  .primary {
    border-radius: 999px;
    border: 1px solid color-mix(in srgb, var(--ink) 16%, transparent);
    background: color-mix(in srgb, var(--paper) 72%, transparent);
    padding: 9px 14px;
  }

  .scope-toggle button.active {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border-color: color-mix(in srgb, var(--ink-orange) 28%, transparent);
    color: var(--ink-orange);
    font-weight: 700;
  }

  .primary {
    background: var(--ink-orange);
    border-color: color-mix(in srgb, var(--ink-orange) 32%, transparent);
    color: var(--paper);
    font-weight: 700;
  }

  .gate-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .gate-toggle input[type="checkbox"] {
    width: auto;
    padding: 0;
  }

  .schedule-list {
    display: grid;
    gap: 10px;
  }

  .schedule-row {
    display: grid;
    gap: 8px;
    padding: 14px;
    border-radius: 16px;
    background: color-mix(in srgb, var(--paper) 58%, transparent);
    border: 1px solid color-mix(in srgb, var(--ink) 8%, transparent);
  }

  .schedule-main {
    display: grid;
    gap: 2px;
  }

  .schedule-sub,
  .run-time {
    color: var(--ink-soft);
    font-size: 0.82rem;
  }

  .schedule-facts {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 0.85rem;
    color: var(--ink-soft);
  }

  .status {
    text-transform: capitalize;
    font-weight: 700;
  }

  .status-active,
  .status-run-completed,
  .status-run-completed_empty {
    color: var(--success);
  }

  .status-paused {
    color: var(--ink-orange);
  }

  .status-run-failed {
    color: var(--error);
  }

  .creator {
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--paper-deep);
    font-size: 0.78rem;
  }

  /* An agent-made schedule stands out from a user's so the automation
     panel answers "where did this come from" at a glance. */
  .creator-agent {
    background: color-mix(in srgb, var(--accent) 12%, var(--paper));
    color: var(--ink);
  }

  .row-actions {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .run-history {
    display: grid;
    gap: 6px;
    border-top: 1px solid color-mix(in srgb, var(--ink) 8%, transparent);
    padding-top: 8px;
  }

  .run-row {
    display: grid;
    gap: 4px;
    font-size: 0.82rem;
  }

  .run-head {
    display: flex;
    gap: 10px;
    color: var(--ink-soft);
  }

  .run-detail {
    margin: 0;
    color: var(--ink);
    white-space: pre-wrap;
  }

  .run-detail.error {
    color: var(--error);
  }

  .feedback {
    border-radius: 16px;
    padding: 10px 12px;
    font-size: 0.92rem;
    line-height: 1.45;
  }

  .feedback.error {
    background: color-mix(in srgb, var(--error) 12%, transparent);
    color: var(--error);
  }

  .feedback.success {
    background: color-mix(in srgb, var(--success) 12%, transparent);
    color: var(--success);
  }

  .empty {
    color: var(--ink-soft);
    font-size: 0.85rem;
    font-style: italic;
  }
</style>
