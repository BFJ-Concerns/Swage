<!--
  A workflow run's own surface: what the run was, how it ended, where every
  worker it dispatched stood at that moment, and — for a run that never
  returned — the completed work still recoverable from it.

  A run still going says so; the account appears only once the run has ended,
  because it is the record of an ending rather than a progress reading.
-->
<script lang="ts">
  import type { WorkflowRunDetailResponse } from "../protocol/WorkflowRunDetailResponse";
  import type { WorkflowRunEnding } from "../protocol/WorkflowRunEnding";
  import type { WorkflowWorkerAccount } from "../protocol/WorkflowWorkerAccount";

  interface Props {
    detail: WorkflowRunDetailResponse | null;
    loadState: "loading" | "loaded" | "absent" | "error";
    errorMessage: string;
  }

  let { detail, loadState, errorMessage }: Props = $props();

  const run = $derived(detail?.run ?? null);
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
      <p class="state">{run.state}</p>
    </header>

    {#if account}
      <section class="ending">
        <h3>Ending</h3>
        <p class="ending-line">{describeEnding(account.ending)}</p>
        {#if account.cancellation}
          <p>Cancelled by {account.cancellation.source} at {account.cancellation.requested_at}{account.cancellation.actor ? ` (${account.cancellation.actor})` : ""}{account.cancellation.session_id ? ` in session ${account.cancellation.session_id}` : ""}</p>
        {/if}
        {#if account.ending.ending === "threw" && account.ending.error.includes("\n")}
          <details><summary>Engine details</summary><pre>{account.ending.error}</pre></details>
        {/if}
      </section>

      <section class="workers">
        <h3>Workers</h3>
        {#each partitions as [standing, workers] (standing)}
          <div class="standing">
            <h4>{standing} ({workers.length})</h4>
            {#if workers.length > 0}
              <ul>
                {#each workers as worker (worker.journal_entry_id)}
                  <li>
                    <span class="worker">{describeWorker(worker)}</span>
                    <span class="detail">{describeTermination(worker)}</span>
                    {#if worker.placement}
                      <span class="placement">{worker.placement}</span>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </div>
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
                {#if result.placement}
                  <span class="placement">{result.placement}</span>
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}
    {:else}
      <p class="notice">This run has not ended yet, so it has no account of its ending.</p>
    {/if}
  {/if}
</main>

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
    word-break: break-all;
  }
</style>
