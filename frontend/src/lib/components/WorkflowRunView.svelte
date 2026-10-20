<!--
  A workflow run's own surface: what the run was, how it ended, where every
  worker it dispatched stood at that moment, and — for a run that never
  returned — the completed work still recoverable from it.

  A run still going says so; the account appears only once the run has ended,
  because it is the record of an ending rather than a progress reading.
-->
<script lang="ts">
  import { cancelWorkflowRun, fetchWorkflowRun } from "../api/workflow-runs";
  import { formatRunTime } from "../run-times";
  import { conversationPath } from "../router";
  import type { WorkflowRunDetailResponse } from "../protocol/WorkflowRunDetailResponse";
  import type { WorkflowRunEnding } from "../protocol/WorkflowRunEnding";
  import type { WorkflowWorkerAccount } from "../protocol/WorkflowWorkerAccount";

  interface Props {
    onNavigate?: (path: string) => void;
    detail: WorkflowRunDetailResponse | null;
    loadState: "loading" | "loaded" | "absent" | "error";
    errorMessage: string;
  }

  let { detail, loadState, errorMessage, onNavigate }: Props = $props();

  let cancellingRunId = $state<string | null>(null);
  let cancelError = $state("");
  async function cancel() {
    if (!run || cancelling) return;
    const runId = run.id;
    cancellingRunId = runId;
    let requested = false;
    cancelError = "";
    try {
      await cancelWorkflowRun(runId);
      requested = true;
      const refreshed = await fetchWorkflowRun(runId);
      if (run?.id === runId && refreshed) detail = refreshed;
    } catch (error) {
      if (run?.id === runId) {
        cancelError = error instanceof Error ? error.message : String(error);
        if (!requested && cancellingRunId === runId) cancellingRunId = null;
      }
    }
  }

  const run = $derived(detail?.run ?? null);
  const cancelling = $derived(cancellingRunId === run?.id && (run?.state === "running" || run?.state === "parked"));
  const account = $derived(run?.terminal_account ?? null);

  // The four standings, in the order a reader wants them: what came back,
  // what broke, what the ending stopped, and what nothing ever settled.
  const partitions = $derived(
    account
      ? ([
          ["Done", account.workers.done],
          ["Failed", account.workers.failed],
          ["Cut short", account.workers.cut_short],
          ["Unsettled", account.workers.unsettled],
        ] as [string, WorkflowWorkerAccount[]][])
      : [],
  );

  function describeEnding(ending: WorkflowRunEnding): string {
    switch (ending.ending) {
      case "returned":
        return "Returned";
      case "threw":
        return `Threw — ${ending.error.split("\n")[0]}`;
      case "author_error":
        return `Author error — ${ending.error}`;
      case "cancelled":
        return "Cancelled";
      case "wound_down":
        return `Wound down — ${ending.error}`;
      case "interrupted_without_resume":
        return `Interrupted without resume — ${ending.reason}`;
    }
  }

  function describeTermination(worker: WorkflowWorkerAccount): string {
    if (!worker.termination) return "settled on its own account";
    switch (worker.termination.termination) {
      case "outstanding":
        return "still working";
      case "settled":
        return "settled on its own account";
      case "failed":
        return worker.termination.message;
      case "cut_short":
        return worker.termination.detail ?? "cut short by the run's ending";
      case "unsettled":
        return "never settled";
    }
  }

  function describeWorker(worker: WorkflowWorkerAccount): string {
    return worker.label ?? `worker ${worker.seq}`;
  }

  // A worker dispatched again after an interruption is marked where it
  // stands: its interrupted attempt's tool calls have unknown outcomes.
  function describeRedispatch(worker: WorkflowWorkerAccount): string | null {
    return worker.redispatched_after_interruption
      ? "redispatched after an interruption — the interrupted attempt's tool calls have unknown outcomes and may have been applied twice"
      : null;
  }

  const placements = $derived(
    [...new Set(partitions.flatMap(([, workers]) => workers.map((worker) => worker.placement ?? null)))],
  );
  const waits = $derived((detail?.journal ?? []).filter((entry) =>
    entry.status === "started" && ["timer", "await_event"].includes(entry.kind),
  ));

  function awaitedKinds(request: unknown): string {
    if (request && typeof request === "object" && "kinds" in request && Array.isArray(request.kinds)) {
      return request.kinds.filter((kind): kind is string => typeof kind === "string").join(", ");
    }
    return "an event";
  }

  function navigate(event: MouseEvent, path: string) {
    if (onNavigate && !event.ctrlKey && !event.metaKey && !event.shiftKey && !event.altKey && event.button === 0) {
      event.preventDefault();
      onNavigate(path);
    }
  }

  const redispatchedAny = $derived(
    account
      ? Object.values(account.workers).some((workers) =>
          workers.some((worker) => worker.redispatched_after_interruption),
        )
      : false,
  );

  function describeInterruption(interruption: { detected_at: string; run_state: string }): string {
    return interruption.run_state === "parked"
      ? `the gateway stopped while this run was parked; its wait outlived the gateway`
      : `the gateway stopped while this run was executing; the calls it had in flight were cut off`;
  }

  function renderOutput(output: unknown): string {
    return typeof output === "string" ? output : JSON.stringify(output);
  }

</script>

<main class="run">
  {#if loadState === "loading"}
    <p class="notice">Loading run…</p>
  {:else if loadState === "absent"}
    <p class="notice">No run with that id was found.</p>
  {:else if loadState === "error"}
    <p class="notice" role="alert">{errorMessage}</p>
  {:else if run}
    <header>
      <h2>{run.script_name ?? "Inline workflow"}</h2>
      <p class="state">{cancelling ? "Cancelling…" : run.state}</p>
      <p class="identity">Run {run.id}</p>
      {#if run.state === "running" || run.state === "parked"}
        <button onclick={cancel} disabled={cancelling}>{cancelling ? "Cancelling…" : "Cancel"}</button>
      {/if}
      {#if cancelError}<p role="alert">{cancelError}</p>{/if}
      <p>Started {@render recordedTime(run.created_at)}{#if run.completed_at} · Ended {@render recordedTime(run.completed_at)}{/if}</p>
      {#if run.dispatching_session_id}
        <a href={conversationPath(run.dispatching_session_id)} onclick={(event) => navigate(event, conversationPath(run.dispatching_session_id!))}>Launching conversation</a>
      {/if}
    </header>

    {#if run.interruptions.length > 0}
      <section class="interruptions">
        <h3>Interruptions</h3>
        <ul>
          {#each run.interruptions as interruption, index (index)}
            <li class="detail">{@render recordedTime(interruption.detected_at)} — {describeInterruption(interruption)}</li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if account}
      <section class="ending">
        <h3>Ending</h3>
        <p class="ending-line">{describeEnding(account.ending)}</p>
        {#if account.cancellation}
          <p>Cancelled by {account.cancellation.source} at {@render recordedTime(account.cancellation.requested_at)}{account.cancellation.actor ? ` (${account.cancellation.actor})` : ""}{account.cancellation.session_id ? ` in session ${account.cancellation.session_id}` : ""}</p>
        {/if}
        {#if account.ending.ending === "threw" && account.ending.error.includes("\n")}
          <details><summary>Error details</summary><pre>{account.ending.error}</pre></details>
        {/if}
        {#if redispatchedAny}
          <p class="detail disclosure">
            Workers redispatched after an interruption may have repeated their effects.
          </p>
        {/if}
      </section>

      <section class="workers">
        <h3>Workers</h3>
        {#each placements as placement (placement)}
          <div class="placement-group">
            <h4 class="placement">{placement ?? "No placement recorded"}</h4>
            {#each partitions as [standing, workers] (standing)}
              {@const placedWorkers = workers.filter((worker) => (worker.placement ?? null) === placement)}
              {#if placedWorkers.length > 0}
                <div class="standing">
                  <h4>{standing} ({placedWorkers.length})</h4>
                  <ul>
                    {#each placedWorkers as worker (worker.journal_entry_id)}
                      {@const entry = detail?.journal.find((entry) => entry.id === worker.journal_entry_id)}
                      {@const result = entry?.result}
                      {@const sessionId = entry?.agent_call?.session_id ?? entry?.agent_call?.dispatch?.attempts.at(-1)?.session_id}
                      <li>
                        <span class="worker">
                          {#if sessionId}
                            <a href={conversationPath(sessionId)} onclick={(event) => navigate(event, conversationPath(sessionId))}>{describeWorker(worker)}</a>
                          {:else}
                            {describeWorker(worker)}
                          {/if}
                        </span>
                        <span class="detail">{describeTermination(worker)}</span>
                        {#if result?.kind === "agent_call"}<span class="output">{renderOutput(result.output)}</span>{/if}
                        {#if describeRedispatch(worker)}
                          <span class="detail redispatch">{describeRedispatch(worker)}</span>
                        {/if}
                      </li>
                    {/each}
                  </ul>
                </div>
              {/if}
            {/each}
          </div>
        {/each}
        {#each partitions.filter(([, workers]) => workers.length === 0) as [standing] (standing)}
          <div class="standing"><h4>{standing} (0)</h4></div>
        {/each}
      </section>

      {#if account.partial_results && account.partial_results.results.length > 0}
        <section class="partial">
          <h3>Partial results</h3>
          <p class="reason">{account.partial_results.reason}</p>
          <ul>
            {#each account.partial_results.results as result (result.journal_entry_id)}
              <li>
                <span class="worker">{result.label ?? `worker ${result.seq}`}</span>
                <span class="output">{renderOutput(result.output)}</span>
                {#if result.placement && !placements.includes(result.placement)}
                  <span class="placement">{result.placement}</span>
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}
    {:else}
      {#if run.state === "parked"}
        <section class="awaited" aria-label="Awaited work">
          <h3>Waiting for</h3>
          {#if run.wake_at && waits.some((entry) => entry.kind === "timer")}<p>Timer wakes at {@render recordedTime(run.wake_at)}</p>{/if}
          {#each detail?.holds ?? [] as hold (hold.id)}
            {#if hold.state === "open"}
              <p>Decision hold: {hold.content}</p>
              <p>Permitted verdicts: {hold.verdicts.join(", ")}</p>
              <p>An agent answers this decision through its granted tool.</p>
            {/if}
          {/each}
          {#each waits.filter((entry) => entry.kind === "await_event") as wait (wait.id)}
            <p>Awaiting {awaitedKinds(wait.request)}</p>
          {/each}
        </section>
      {/if}
      <p class="notice">This run has not ended yet, so it has no account of its ending.</p>
    {/if}
    {#if run.final_output != null}
      <section class="result" aria-label="Run result">
        <h3>Result</h3>
        {@render outputValue(run.final_output)}
      </section>
    {/if}
  {/if}
</main>

{#snippet recordedTime(stamp: string)}
  <time datetime={stamp} title={stamp}>{formatRunTime(stamp)}</time>
{/snippet}

{#snippet outputValue(value: unknown)}
  {#if Array.isArray(value)}
    <ul>
      {#each value as item, index (index)}
        <li>{@render outputValue(item)}</li>
      {/each}
    </ul>
  {:else if value !== null && typeof value === "object"}
    <dl>
      {#each Object.entries(value) as [key, item] (key)}
        <dt>{key === "gathered" ? "Recorded sources" : key === "failed_sources" ? "Failed sources" : key.replaceAll("_", " ")}</dt>
        <dd>{@render outputValue(item)}</dd>
      {/each}
    </dl>
  {:else}
    <span>{value === null ? "None" : String(value)}</span>
  {/if}
{/snippet}

<style>
  .run {
    padding: 16px;
    background: var(--paper);
    color: var(--ink);
    overflow-y: auto;
  }

  h2 {
    font-family: var(--font-display);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin: 0;
  }

  h3,
  h4 {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-soft);
    margin: 16px 0 4px;
  }

  .state,
  .notice,
  .placement,
  .detail,
  .reason {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-soft);
    margin: 0;
  }

  .ending-line {
    margin: 0;
  }

  .detail,
  .reason {
    text-transform: none;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  li {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px 0;
    border-top: 1px solid var(--rule-faint);
  }

  .worker {
    font-weight: 600;
  }

  .placement {
    text-transform: none;
    word-break: break-all;
  }
</style>
