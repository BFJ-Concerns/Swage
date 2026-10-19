<!--
  Workflow run detail — renders a run's journal as the account of what its
  workers were asked to do and what came back: the prompt and options each
  call was dispatched under, the provider rounds every attempt of it ran,
  what it cost or why that is not known, and how it stopped.

  Read-only by design: the journal is the record of what already happened,
  read through the run view's refresh path, so what renders is always what was
  durably recorded. Absence is rendered as absence — a call whose usage was
  never reported reads "not recorded" with its reason, never as zero spend.
-->
<script lang="ts">
  import { conversationPath } from "../router";
  import type { WorkflowJournalEntry } from "../protocol/WorkflowJournalEntry";
  import type { WorkflowRunDetailResponse } from "../protocol/WorkflowRunDetailResponse";
  import type { WorkflowCallUsage } from "../protocol/WorkflowCallUsage";
  import type { WorkflowWorkerTermination } from "../protocol/WorkflowWorkerTermination";
  import type { WorkflowProviderRound } from "../protocol/WorkflowProviderRound";
  import type { WorkflowAgentCallResolution } from "../protocol/WorkflowAgentCallResolution";

  interface Props {
    onNavigate?: (path: string) => void;
    detail: WorkflowRunDetailResponse | null;
    loadState: "loading" | "loaded" | "absent" | "error";
    errorMessage: string;
  }

  let { detail, loadState, errorMessage, onNavigate }: Props = $props();

  const agentCalls = $derived(
    (detail?.journal ?? []).filter((entry) => entry.agent_call !== null),
  );

  // Absence and zero are different facts, and the wording keeps them apart:
  // a call nobody reported usage for is not a call that spent nothing.
  function usageLabel(
    usage: WorkflowCallUsage,
    termination: WorkflowWorkerTermination,
  ): string {
    if (usage.availability === "known") {
      return `${usage.input_tokens} in / ${usage.output_tokens} out`;
    }
    if (usage.reason === "provider_reported_no_usage") {
      return "Not reported — the provider reported no usage";
    }
    switch (termination.termination) {
      case "outstanding":
        return "Not reported — the worker is still in progress";
      case "failed":
        return "Not reported — the worker failed before usage was reported";
      case "cut_short":
        return "Not reported — the worker was cut short before usage was reported";
      case "settled":
        return "Not reported — the worker finished without reporting usage";
      case "unsettled":
        return "Not reported — the call never settled";
    }
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

  // Each attempt after the first says why it was dispatched, in the record's
  // own words — a cause, never an outcome: a fallback attempt was dispatched
  // because the primary failed, and whether it served is the round's
  // result, read from the rounds beside it; an interruption leaves the
  // attempt before it with an unknown outcome.
  function attemptLine(attempt: WorkflowAgentCallResolution, index: number): string {
    const ordinal = `attempt ${index + 1}`;
    switch (attempt.cause.cause) {
      case "first":
        return `${ordinal} — the first dispatch`;
      case "after_interruption":
        return `${ordinal} — dispatched again after a gateway interruption: attempt ${index}'s outcome is unknown and the tool calls it issued may have been applied twice`;
      case "provider_fallback":
        return `${ordinal} — dispatched to the declared fallback because the primary failed (${attempt.cause.primary_failure})`;
    }
  }

  function resolvedLabel(attempt: WorkflowAgentCallResolution): string {
    if (attempt.turn_id == null) return "opened, not yet resolved by a turn";
    return [attempt.model ?? "default model", attempt.effort ?? "default effort"].join(", ");
  }

  function navigate(event: MouseEvent, path: string) {
    if (onNavigate && !event.ctrlKey && !event.metaKey && !event.shiftKey && !event.altKey && event.button === 0) {
      event.preventDefault();
      onNavigate(path);
    }
  }

  function terminationFor(entry: WorkflowJournalEntry): WorkflowWorkerTermination {
    const original = entry.agent_call!.termination;
    const workers = detail?.run.terminal_account?.workers;
    if (original.termination !== "outstanding" || !workers) return original;
    return Object.values(workers).flat().find((worker) => worker.journal_entry_id === entry.id)?.termination ?? original;
  }

  function placementInAccount(path: string): boolean {
    const workers = detail?.run.terminal_account?.workers;
    return workers ? Object.values(workers).some((group) => group.some((worker) => worker.placement === path)) : false;
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
        {@const termination = terminationFor(entry)}
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
              <dd>{call.dispatch.attempts.length ? resolvedLabel(call.dispatch.attempts[call.dispatch.attempts.length - 1]) : "not yet resolved"}</dd>
              {#if call.dispatch.attempts.length > 0}
                <dt>Attempts</dt>
                <dd class="attempts">
                  <ol class="plain">
                    {#each call.dispatch.attempts as attempt, index (index)}
                      <li>
                        <p>{attemptLine(attempt, index)} — {resolvedLabel(attempt)}</p>
                        {#if attempt.session_id}
                          <a href={conversationPath(attempt.session_id)} onclick={(event) => navigate(event, conversationPath(attempt.session_id!))}>Attempt {index + 1} session</a>
                        {/if}
                      </li>
                    {/each}
                  </ol>
                </dd>
              {/if}
              {#if call.dispatch.placement}
                <dt>Placed at</dt>
                <dd>
                  {#if placementInAccount(call.dispatch.placement.path)}
                    Shown in the terminal account
                  {:else}
                    {call.dispatch.placement.path}
                  {/if}
                  ({call.dispatch.placement.can_write ? "writable" : "read-only"})
                </dd>
              {/if}
              <dt>Granted</dt>
              <dd>
                {call.dispatch.grants == null
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

          {#if call.session_id}
            <a href={conversationPath(call.session_id)} onclick={(event) => navigate(event, conversationPath(call.session_id!))}>Worker session</a>
          {/if}
          {#if call.liveness}
            <dl class="liveness">
              <dt>Started</dt><dd>{call.liveness.started_at}</dd>
              <dt>First evidence</dt><dd>{call.liveness.first_evidence_at ?? "Not yet recorded"}</dd>
            </dl>
          {/if}
          <dl>
            <dt>Spend</dt>
            <dd class="usage">{usageLabel(call.usage, termination)}</dd>
            <dt>Stopped</dt>
            <dd class="termination">{terminationLabel(termination)}</dd>
          </dl>

          {#if call.provider_rounds == null}
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
