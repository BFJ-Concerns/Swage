<script lang="ts">
  import { untrack } from "svelte";
  import { listSurfaces } from "../api/surfaces";
  import type { SurfaceSummary } from "../protocol/SurfaceSummary";
  import AgentPicker from "./AgentPicker.svelte";
  import { listEffectiveAgentDefinitions } from "../api/agent-definitions";
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";
  import type { DeliveryTarget } from "../protocol/DeliveryTarget";
  import type { DeliveryRoutingRequest } from "../protocol/DeliveryRoutingRequest";
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
  import { passedOneShot, runUnsettled, UNSETTLED_RUN_REFRESH_MS } from "../one-shot-runs";
  import type { ScheduleCreator } from "../protocol/ScheduleCreator";
  import type { ScheduleSpec } from "../protocol/ScheduleSpec";
  import type { SessionPolicy } from "../protocol/SessionPolicy";
  import type { Topic } from "../types";

  interface Props {
    topicId: string | null;
    sessionId: string | null;
    topics?: Topic[];
  }

  let { topicId, sessionId, topics = [] }: Props = $props();

  let overlapPolicy = $state<"skip" | "queue" | "overlap">("skip");
  let pendingCap = $state(1);

  // Creation form.
  let scheduleMessage = $state("");
  let scheduleKind = $state<"interval" | "cron" | "one_shot">("interval");
  let scheduleEvery = $state("every 1 hours");
  let scheduleCron = $state("0 0 9 * * * *");
  let scheduleAt = $state("");
  let schedulePolicy = $state<"fresh" | "continuation">("fresh");
  let scheduleAgentId = $state("");
  let scheduleTopicId = $state(untrack(() => topicId ?? ""));
  let agentDefinitions = $state<EffectiveAgentDefinitionResponse[]>([]);
  let agentLoadError = $state<string | null>(null);
  let agentsLoading = $state(false);
  let agentLoadGeneration = 0;
  let storeLocalOutput = $state(true);
  let deliveryTargets = $state<DeliveryTarget[]>([]);
  let deliverySurfaces = $state<SurfaceSummary[]>([]);
  let surfaceLoadError = $state<string | null>(null);
  let surfacesLoading = $state(false);
  let surfaceLoadGeneration = 0;

  $effect(() => {
    const continuationId = schedulePolicy === "continuation" ? sessionId : null;
    const generation = ++surfaceLoadGeneration;
    deliverySurfaces = [];
    surfaceLoadError = null;
    surfacesLoading = !!continuationId;
    if (!continuationId) return;
    void listSurfaces(continuationId)
      .then((response) => {
        if (generation === surfaceLoadGeneration) deliverySurfaces = response.surfaces;
      })
      .catch((err: unknown) => {
        if (generation === surfaceLoadGeneration) surfaceLoadError = err instanceof Error ? err.message : "Failed to load conversation surfaces.";
      })
      .finally(() => {
        if (generation === surfaceLoadGeneration) surfacesLoading = false;
      });
  });

  $effect(() => {
    const scope = scheduleTopicId || null;
    const generation = ++agentLoadGeneration;
    scheduleAgentId = "";
    agentDefinitions = [];
    agentLoadError = null;
    agentsLoading = true;
    void listEffectiveAgentDefinitions(scope)
      .then((definitions) => {
        if (generation === agentLoadGeneration) agentDefinitions = definitions;
      })
      .catch((err: unknown) => {
        if (generation === agentLoadGeneration) {
          agentLoadError = err instanceof Error ? err.message : "Failed to load agents.";
        }
      })
      .finally(() => {
        if (generation === agentLoadGeneration) agentsLoading = false;
      });
  });

  function newDeliveryTarget(kind: DeliveryTarget["kind"]): DeliveryTarget {
    switch (kind) {
      case "channel": return { kind, integration: "telegram", account: "", conversation: "" };
      case "surface": return { kind, surface_id: "" };
      case "webhook": return { kind, url: "" };
      case "local_storage": return { kind };
    }
  }

  function buildRouting(): DeliveryRoutingRequest {
    const explicitTargets = deliveryTargets.map((target): DeliveryTarget => {
      switch (target.kind) {
        case "channel": {
          const account = target.account.trim();
          const conversation = target.conversation.trim();
          if (!account || !conversation) throw new Error("Channel delivery needs an account and conversation.");
          return { ...target, account, conversation };
        }
        case "webhook": {
          const url = target.url.trim();
          const parsed = new URL(url);
          if (!["http:", "https:"].includes(parsed.protocol)) throw new Error("Webhook delivery needs an HTTP or HTTPS URL.");
          return { kind: "webhook", url };
        }
        case "surface": {
          if (schedulePolicy !== "continuation") throw new Error("Surface delivery needs a schedule that continues its conversation.");
          const surfaceId = target.surface_id.trim();
          if (!surfaceId) throw new Error("Surface delivery needs a surface ID from this conversation.");
          return { kind: "surface", surface_id: surfaceId };
        }
        case "local_storage": return target;
      }
    });
    return { deliver_to_origin: false, store_local_output: storeLocalOutput, explicit_targets: explicitTargets };
  }
  let gateEnabled = $state(false);
  let gateModel = $state("");
  let gateCriteria = $state("");
  // Files the gate looks at before deciding, one workspace-relative path per
  // line: what makes a criterion about a file decidable.
  let gateObservedFiles = $state("");
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

  // A passed one-shot's card reports its latest run, so its history is
  // re-read while that run has no outcome yet or a manual run of it is in
  // flight. A failed re-read is retried on the same cadence.
  let runRefreshFailures = $state(0);
  $effect(() => {
    void runRefreshFailures;
    const watched = schedules
      .filter(
        (schedule) =>
          passedOneShot(schedule) &&
          (runUnsettled(runsById[schedule.id]?.at(-1)) || actionId === schedule.id),
      )
      .map((schedule) => schedule.id);
    if (watched.length === 0) return;
    const timer = setTimeout(async () => {
      try {
        const histories = await Promise.all(
          watched.map(async (id) => [id, (await fetchScheduleRuns(id)).runs] as const),
        );
        runsById = { ...runsById, ...Object.fromEntries(histories) };
      } catch (err) {
        error = err instanceof Error ? err.message : "Failed to refresh schedule runs.";
        runRefreshFailures += 1;
      }
    }, UNSETTLED_RUN_REFRESH_MS);
    return () => clearTimeout(timer);
  });

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
    const agentId = scheduleAgentId || "gateway";
    return scheduleTopicId
      ? { policy: "fresh", topic_id: scheduleTopicId, agent_id: agentId }
      : { policy: "fresh", agent_id: agentId };
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
    let routing: DeliveryRoutingRequest;
    try {
      spec = buildScheduleSpec();
      session = buildSessionPolicy();
      routing = buildRouting();
    } catch (err) {
      error = err instanceof Error ? err.message : "Schedule details are incomplete.";
      return;
    }

    if (!Number.isInteger(pendingCap) || pendingCap < 1) {
      error = "Pending fire cap must be a positive whole number.";
      return;
    }
    isCreating = true;
    try {
      const created = await createSchedule({
        message,
        overlap_policy: overlapPolicy,
        pending_cap: pendingCap,
        spec,
        session,
        routing,
        gate: gateEnabled
          ? {
              model: gateModel.trim(),
              criteria: gateCriteria.trim() || null,
              observations: gateObservedFiles
                .split("\n")
                .map((line) => line.trim())
                .filter(Boolean)
                .map((path) => ({ kind: "file" as const, path })),
            }
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
    const targetTopic = session.policy === "fresh"
      ? topics.find((topic) => topic.id === session.topic_id)
      : null;
    return session.policy === "fresh"
      ? `fresh · ${session.topic_id ? `topic ${targetTopic?.name ?? session.topic_id}` : "general scope"} · agent ${session.agent_id}`
      : `continues session ${session.session_id}`;
  }

  function describeCreator(creator: ScheduleCreator): string {
    return creator.kind === "agent" ? `agent ${creator.agent_id}` : "you";
  }

  function formatTime(value: string | null): string {
    return value ? new Date(value).toLocaleString() : "—";
  }

  function describeRunState(run: ScheduleRun): string {
    if (run.task_outcome) return "completed with errors";
    return run.state === "completed_empty" ? "completed empty" : run.state;
  }

  function describeRunOutcome(run: ScheduleRun): string {
    if (run.state === "completed_empty") return "No assistant text produced.";
    return "";
  }

  function cardRun(schedule: Schedule): ScheduleRun | undefined {
    return passedOneShot(schedule) ? runsById[schedule.id]?.at(-1) : undefined;
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
    <p>Create schedule saves the schedule and activates it at once.</p>
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
        <span>Run topic</span>
        <select aria-label="Run topic" bind:value={scheduleTopicId}>
          <option value="">No topic — general assistant</option>
          {#each topics as topic (topic.id)}
            <option value={topic.id}>{topic.name}</option>
          {/each}
        </select>
      </label>
      <AgentPicker definitions={agentDefinitions} bind:selectedAgentId={scheduleAgentId} disabled={agentsLoading} />
      {#if agentsLoading}<p class="empty">Loading agents…</p>{/if}
      {#if agentLoadError}<p class="error" role="alert">{agentLoadError}</p>{/if}
    {/if}
    <label>
      <span>While a previous run is active</span>
      <select bind:value={overlapPolicy}>
        <option value="skip">Skip</option>
        <option value="queue">Queue</option>
        <option value="overlap">Allow overlap</option>
      </select>
    </label>
    {#if overlapPolicy === "queue"}
      <label>
        <span>Pending fire cap</span>
        <input type="number" min="1" step="1" bind:value={pendingCap} />
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
      <label>
        <span>Files to look at first</span>
        <textarea
          bind:value={gateObservedFiles}
          rows="2"
          placeholder="inbox/todo.txt — one path per line, relative to the workspace"
        ></textarea>
      </label>
    {/if}
    <fieldset class="delivery-settings">
      <legend>Delivery destinations</legend>
      <label class="gate-toggle">
        <input type="checkbox" bind:checked={storeLocalOutput} aria-label="Keep a local log" />
        <span>Keep a local log</span>
      </label>
      {#each deliveryTargets as target, index}
        <div class="delivery-target">
          <label>
            <span>Destination type</span>
            <select aria-label="Destination type" value={target.kind}
              onchange={(event) => { deliveryTargets[index] = newDeliveryTarget(event.currentTarget.value as DeliveryTarget["kind"]); }}>
              <option value="webhook">Webhook</option>
              <option value="channel">Channel</option>
              <option value="surface" disabled={schedulePolicy !== "continuation"}>Conversation surface</option>
              <option value="local_storage">Local log</option>
            </select>
          </label>
          {#if target.kind === "webhook"}
            <label><span>Webhook URL</span><input type="url" aria-label="Webhook URL" bind:value={target.url} placeholder="https://example.com/results" /></label>
          {:else if target.kind === "channel"}
            <label><span>Channel</span><select aria-label="Channel integration" bind:value={target.integration}>
              <option value="telegram">Telegram</option><option value="dev_dm">Development direct message</option><option value="dev_thread">Development thread</option>
            </select></label>
            <label><span>Account</span><input aria-label="Channel account" bind:value={target.account} /></label>
            <label><span>Conversation</span><input aria-label="Channel conversation" bind:value={target.conversation} /></label>
          {:else if target.kind === "surface"}
            <label><span>Conversation surface</span><select aria-label="Conversation surface" bind:value={target.surface_id} disabled={surfacesLoading}>
              <option value="">Choose a surface</option>
              {#each deliverySurfaces as surface (surface.id)}<option value={surface.id}>{surface.title}</option>{/each}
            </select></label>
            {#if surfaceLoadError}<p class="error" role="alert">{surfaceLoadError}</p>{/if}
            {#if !surfacesLoading && !surfaceLoadError && deliverySurfaces.length === 0}<p class="empty">This conversation has no surfaces yet.</p>{/if}
          {/if}
          <button type="button" onclick={() => { deliveryTargets = deliveryTargets.filter((_, i) => i !== index); }}>Remove destination</button>
        </div>
      {/each}
      <button type="button" onclick={() => { deliveryTargets = [...deliveryTargets, newDeliveryTarget("webhook")]; }}>Add destination</button>
    </fieldset>
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
      {@const latestRun = cardRun(schedule)}
      <article class="schedule-row">
        <div class="schedule-main">
          <strong>{schedule.message.trim().length > 72 ? `${schedule.message.trim().slice(0, 60).replace(/\s+/g, " ")}… · ${schedule.id.slice(-8)}` : schedule.message}</strong>
          {#if schedule.message.trim().length > 72}
            <details><summary>Full brief</summary><p class="full-brief">{schedule.message}</p></details>
          {/if}
          <span class="schedule-sub">
            {describeSpec(schedule.spec)} · {describeSession(schedule.session)}
            {schedule.gate
              ? ` · gate ${schedule.gate.model}${
                  schedule.gate.observations?.length
                    ? ` watching ${schedule.gate.observations.length} file${schedule.gate.observations.length === 1 ? "" : "s"}`
                    : ""
                }`
              : ""}
          </span>
          {#each schedule.gate_warnings ?? [] as warning (warning.kind)}
            <p class="gate-warning" role="note">{warning.message}</p>
          {/each}
        </div>
        <div class="schedule-facts">
          <span class="status {latestRun ? `status-run-${latestRun.state}` : `status-${schedule.status}`}">
            {latestRun ? describeRunState(latestRun) : schedule.status}
          </span>
          <span class="creator creator-{schedule.creator.kind}"
            >by {describeCreator(schedule.creator)}</span
          >
          <span>While a run is active: {schedule.overlap_policy}{schedule.overlap_policy === "queue" ? ` (cap ${schedule.pending_cap})` : ""}</span>
          {#if schedule.status !== "paused"}
            <span>next {formatTime(schedule.next_run_at ?? null)}</span>
          {/if}
        </div>
        <div class="scope-toggle row-actions">
          <button
            type="button"
            onclick={() => runNow(schedule)}
            disabled={actionId === schedule.id}
          >
            {actionId === schedule.id ? "Running…" : passedOneShot(schedule) ? "Run again" : "Run now"}
          </button>
          {#if !passedOneShot(schedule)}
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
          {/if}
        </div>
        <div class="run-history" aria-label={`Run history for ${schedule.message}`}>
          {#each runHistory(schedule.id) as run (run.id)}
            <div class="run-row">
              <div class="run-head">
                <span class="status status-run-{run.task_outcome ? "failed" : run.state}">{describeRunState(run)}</span>
                <span>{run.trigger}</span>
                {#if run.skip_reason}<span>skipped: {run.skip_reason}</span>{/if}
                {#if run.gate}<span>gate {run.gate.verdict}</span>{/if}
              </div>
              {#if run.executed_message !== schedule.message}<p class="run-detail executed-brief">Ran an earlier brief: {run.executed_message}</p>{/if}
              {#if describeRunOutcome(run)}<p class="run-detail">{describeRunOutcome(run)}</p>{/if}
              {#if run.output_text}<p class="run-detail">{run.output_text}</p>{/if}
              {#if run.task_outcome}<p class="run-detail error">{run.task_outcome}</p>{/if}
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
  .delivery-settings, .delivery-target { display: grid; gap: 8px; border: 1px solid var(--rule-soft); padding: 12px; }
  .full-brief { white-space: pre-wrap; overflow-wrap: anywhere; }

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

  .gate-warning {
    margin: 0.25rem 0 0;
    font-size: 0.85rem;
    color: var(--ink-orange);
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
