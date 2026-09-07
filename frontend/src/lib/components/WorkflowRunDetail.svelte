<!--
  Workflow run detail — renders a run's journal as the account of what its
  workers were asked to do and what came back: the prompt and options each
  call was dispatched under, the provider rounds every attempt of it ran,
  what it cost or why that is not known, and how it stopped.

  Read-only by design: the journal is the record of what already happened,
  fetched on open rather than streamed, so what renders is always what was
  durably recorded. Absence is rendered as absence — a call whose usage was
  never reported reads "not recorded" with its reason, never as zero spend.
-->
<script lang="ts">
  import { fetchWorkflowRun } from "../api/workflow-runs";
  import type { WorkflowRunDetailResponse } from "../protocol/WorkflowRunDetailResponse";
  import type { WorkflowAgentCallDetail } from "../protocol/WorkflowAgentCallDetail";
  import type { WorkflowCallUsage } from "../protocol/WorkflowCallUsage";
  import type { WorkflowWorkerTermination } from "../protocol/WorkflowWorkerTermination";
  import type { WorkflowProviderRound } from "../protocol/WorkflowProviderRound";

  interface Props {
    runId: string;
  }

  let { runId }: Props = $props();

  let detail = $state<WorkflowRunDetailResponse | null>(null);
  let loadState = $state<"loading" | "loaded" | "absent" | "error">("loading");
  let errorMessage = $state("");

  $effect(() => {
    // Pin the request to the run that started it, so a response arriving
    // after the user moved on is dropped rather than rendered against the
    // wrong run.
    const requestedRunId = runId;
    loadState = "loading";
    fetchWorkflowRun(requestedRunId)
      .then((loaded) => {
        if (requestedRunId !== runId) return;
        detail = loaded;
        loadState = loaded === null ? "absent" : "loaded";
      })
      .catch((error: unknown) => {
        if (requestedRunId !== runId) return;
        errorMessage = error instanceof Error ? error.message : String(error);
        loadState = "error";
      });
  });

  const agentCalls = $derived(
    (detail?.journal ?? []).filter((entry) => entry.agent_call !== null),
  );

  // Absence and zero are different facts, and the wording keeps them apart:
  // a call nobody reported usage for is not a call that spent nothing.
  function usageLabel(usage: WorkflowCallUsage): string {
    if (usage.availability === "known") {
      return `${usage.input_tokens} in / ${usage.output_tokens} out`;
    }
    return usage.reason === "provider_reported_no_usage"
      ? "Not recorded — the provider reported no usage"
      : "Not recorded — the call never settled";
  }

  function terminationLabel(termination: WorkflowWorkerTermination): string {
    switch (termination.termination) {
      case "outstanding":
        return "Still working";
      case "settled":
        return "Finished";
      case "failed":
        return `Failed — ${termination.message}`;
      case "cut_short":
        return termination.cause === "run_cancelled"
          ? "Cut short — the run was cancelled"
          : "Cut short";
      case "unsettled":
        return "Never settled";
    }
  }

  // A round with no end time is not automatically one still waiting: a round
  // whose attempt was interrupted has no end time either, and the two read
  // differently, so the recorded outcome decides the wording.
  function roundLabel(round: WorkflowProviderRound): string {
    switch (round.outcome.outcome) {
      case "responded":
        return round.outcome.tool_calls === 0
          ? "responded"
          : `responded with ${round.outcome.tool_calls} tool call(s)`;
      case "failed":
        return `failed — ${round.outcome.message}`;
      case "outstanding":
        return "no response yet";
      case "interrupted":
        return "interrupted — the attempt stopped before a response came back";
    }
  }

  // The last attempt is the one the call ended on; the earlier ones are the
  // attempts an interruption cut off before it.
  function resolvedLabel(call: WorkflowAgentCallDetail): string {
    const resolved = call.dispatch?.attempts.at(-1);
    if (!resolved) return "not yet resolved";
    return [resolved.model ?? "default model", resolved.effort ?? "default effort"].join(
      ", ",
    );
  }
</script>

<section class="run-detail" aria-label="Workflow run">
  <header>
    <h2>Workers in this run</h2>
  </header>

  {#if loadState === "loading"}
    <p class="status">Loading the recorded run…</p>
  {:else if loadState === "absent"}
    <p class="status">No run with that id was found.</p>
  {:else if loadState === "error"}
    <p class="status error">{errorMessage}</p>
  {:else if agentCalls.length === 0}
    <p class="status">This run dispatched no workers.</p>
  {:else}
    <ul class="calls">
      {#each agentCalls as entry (entry.id)}
        {@const call = entry.agent_call!}
        <li class="call">
          <h3>{entry.label ?? call.dispatch?.agent_definition_id ?? "Worker"}</h3>

          {#if call.dispatch}
            <p class="prompt">{call.dispatch.prompt}</p>
            <dl>
              <dt>Asked for</dt>
              <dd>
                {call.dispatch.requested_model ?? "no model named"},
                {call.dispatch.requested_effort ?? "no effort named"}
              </dd>
              <dt>Ran with</dt>
              <dd>{resolvedLabel(call)}</dd>
              {#if call.dispatch.attempts.length > 1}
                <dt>Attempts</dt>
                <dd class="attempts">
                  {call.dispatch.attempts.length} — an earlier attempt was
                  interrupted and the call dispatched again
                </dd>
              {/if}
              {#if call.dispatch.placement}
                <dt>Placed at</dt>
                <dd>
                  {call.dispatch.placement.path}
                  ({call.dispatch.placement.can_write ? "writable" : "read-only"})
                </dd>
              {/if}
              <dt>Granted</dt>
              <dd>
                {call.dispatch.grants === null
                  ? "narrowed nothing"
                  : call.dispatch.grants.join(", ") || "no tools"}
              </dd>
            </dl>
          {:else}
            <p class="status">
              No dispatch record — this call was journalled before the record
              existed.
            </p>
          {/if}

          <dl>
            <dt>Spend</dt>
            <dd class="usage">{usageLabel(call.usage)}</dd>
            <dt>Stopped</dt>
            <dd class="termination">{terminationLabel(call.termination)}</dd>
          </dl>

          {#if call.provider_rounds === null}
            <p class="status">Provider rounds are not carried here.</p>
          {:else if call.provider_rounds.length === 0}
            <p class="status">No provider request was made.</p>
          {:else}
            <table class="rounds">
              <thead>
                <tr>
                  <th>Attempt</th>
                  <th>Round</th>
                  <th>Started</th>
                  <th>Ended</th>
                  <th>Outcome</th>
                </tr>
              </thead>
              <tbody>
                {#each call.provider_rounds as round (`${round.attempt}-${round.round}`)}
                  <tr>
                    <td class="numeric">{round.attempt}</td>
                    <td class="numeric">{round.round}</td>
                    <td>{round.started_at}</td>
                    <td>{round.completed_at ?? "—"}</td>
                    <td>{roundLabel(round)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .run-detail {
    padding: 1rem;
    overflow-y: auto;
    font-size: 0.875rem;
  }

  h2 {
    font-size: 1rem;
    margin: 0;
  }

  h3 {
    font-size: 0.875rem;
    margin-block: 0.75rem 0.25rem;
  }

  .status {
    color: var(--ink-soft);
  }

  .error {
    color: var(--error);
  }

  .calls {
    list-style: none;
    padding: 0;
    margin: 0;
  }

  .call {
    border-bottom: var(--rule);
    padding-block: 0.5rem;
  }

  .prompt {
    color: var(--ink-soft);
    margin-block: 0.25rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.125rem 0.5rem;
    margin-block: 0.25rem;
  }

  dt {
    color: var(--ink-soft);
  }

  dd {
    margin: 0;
  }

  table.rounds {
    width: 100%;
    border-collapse: collapse;
    margin-block: 0.5rem;
  }

  th,
  td {
    text-align: left;
    padding: 0.25rem 0.5rem;
    border-bottom: var(--rule);
  }

  .numeric {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
</style>
