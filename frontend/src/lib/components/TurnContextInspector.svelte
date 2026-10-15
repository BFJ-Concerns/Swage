<!--
  Turn context inspector — renders the assembled-context report recorded for
  one turn: which elements built the model's context, at what token cost,
  with what compaction, warnings, declared omissions, and fallbacks.

  Read-only by design: the report is the audit record of what already
  happened, fetched on open rather than streamed, so what renders is always
  what was durably recorded.
-->
<script lang="ts">
  import {
    fetchTurnContext,
    type TurnContextReport,
  } from "../api/turns";

  interface Props {
    turnId: string;
    // Called when the user dismisses the inspector.
    onClose: () => void;
  }

  let { turnId, onClose }: Props = $props();

  let report = $state<TurnContextReport | null>(null);
  let loadState = $state<"loading" | "loaded" | "absent" | "error">("loading");
  let errorMessage = $state("");

  $effect(() => {
    // Pin the request to the turn that started it: when the user selects
    // another turn before this response lands, the stale result is dropped
    // rather than rendered against the wrong turn.
    const requestedTurnId = turnId;
    loadState = "loading";
    fetchTurnContext(requestedTurnId)
      .then((loaded) => {
        if (requestedTurnId !== turnId) return;
        report = loaded;
        loadState = loaded === null ? "absent" : "loaded";
      })
      .catch((error: unknown) => {
        if (requestedTurnId !== turnId) return;
        errorMessage = error instanceof Error ? error.message : String(error);
        loadState = "error";
      });
  });

  function object(value: unknown): Record<string, unknown> {
    return value !== null && typeof value === "object" && !Array.isArray(value)
      ? value as Record<string, unknown> : {};
  }

  function list(value: unknown): unknown[] {
    return Array.isArray(value) ? value : [];
  }

  // Assignment metadata is not part of the live inspector's reading surface.
  function blind(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(blind);
    if (value !== null && typeof value === "object") {
      return Object.fromEntries(Object.entries(value).filter(([key]) =>
        !/^(variant(_id|_name)?|experiment_assignment|selected_variant)$/i.test(key),
      ).map(([key, child]) => [key, blind(child)]));
    }
    return value;
  }

  function sentEffort(value: unknown): string {
    const control = object(value);
    switch (control.kind) {
      case "level": return String(control.level);
      case "budget": return `${String(control.tokens)} thinking tokens`;
      case "disabled": return "Disabled";
      case "omitted": return "No effort control sent";
      default: return "Not recorded";
    }
  }

  const visible = $derived(object(report?.provider_visible));
  const model = $derived(object(visible.effective_model));
  const effort = $derived(object(visible.effort));
  const requests = $derived(list(visible.requests).map(object));
  const totalTokens = $derived(report?.total_tokens ?? 0);
  const durationMs = $derived(
    report
      ? Math.round(
          report.duration.secs * 1000 + report.duration.nanos / 1_000_000,
        )
      : 0,
  );
</script>

<aside class="context-inspector" aria-label="Turn context">
  <header>
    <h2>Context for this turn</h2>
    <button type="button" class="close" onclick={onClose} aria-label="Close">
      ×
    </button>
  </header>

  {#if loadState === "loading"}
    <p class="status">Loading the recorded context report…</p>
  {:else if loadState === "absent"}
    <p class="status">
      No context report was recorded for this turn. Reports exist for turns
      that reached context assembly.
    </p>
  {:else if loadState === "error"}
    <p class="status error">{errorMessage}</p>
  {:else if report}
    <p class="summary">
      Assembled by <strong>{report.assembly_name}</strong> — request total:
      {totalTokens} tokens in {durationMs}&nbsp;ms. The
      {report.elements.length} per-element estimates below are non-additive and
      do not sum to the request total.
    </p>

    {#if Object.keys(model).length > 0 || Object.keys(effort).length > 0 || report.budget != null || report.budget_remaining != null}
      <section aria-label="Effective request">
        <h3>Effective request</h3>
        {#if model.model_id}<p>Model: {String(model.model_id)}</p>{/if}
        {#if model.provider_id}<p>Provider: {String(model.provider_id)}</p>{/if}
        {#if effort.requested}<p>Effort: {String(effort.requested)} → {String(effort.clamped_to ?? effort.requested)}{effort.clamped_to ? " (clamped)" : ""}</p>{/if}
        {#if Object.keys(effort).length > 0}<p>Sent effort: {sentEffort(effort.control)}</p>{/if}
        {#if effort.note}<p>{String(effort.note)}</p>{/if}
        {#if report.budget != null}<p>Configured budget: {report.budget} tokens</p>{/if}
        {#if report.budget_remaining != null}<p>Remaining: {report.budget_remaining} tokens</p>{/if}
      </section>
    {/if}

    {#if requests.length > 0}
      <section aria-label="Provider requests">
        <h3>Provider requests</h3>
        {#each requests as request}
          <details>
            <summary>Full request · round {String(request.round)}</summary>
            <pre>{JSON.stringify(blind(request.request), null, 2)}</pre>
          </details>
          {#if list(object(request.request).divergences).length > 0}
            <section aria-label="Request divergences">
              <h4>Request divergences · round {String(request.round)}</h4>
              <pre>{JSON.stringify(blind(object(request.request).divergences), null, 2)}</pre>
            </section>
          {/if}
        {/each}
      </section>
    {/if}

    {#if (report.provider_attempts ?? []).length > 0}
      <section aria-label="Provider attempts">
        <h3>Provider attempts</h3>
        <ol>
          {#each report.provider_attempts as attempt}
            <li>Round {attempt.round}, attempt {attempt.attempt}: {attempt.outcome}
              <div>Started: {new Date(attempt.started_at_ms).toISOString()}</div>
              <div>Ended: {attempt.ended_at_ms == null ? "Outstanding" : new Date(attempt.ended_at_ms).toISOString()}</div>
              {#if attempt.error}<pre>{attempt.error}</pre>{/if}
            </li>
          {/each}
        </ol>
      </section>
    {/if}

    <table class="elements">
      <thead>
        <tr>
          <th scope="col">Element</th>
          <th scope="col">From</th>
          <th scope="col" class="numeric">Estimated tokens</th>
        </tr>
      </thead>
      <tbody>
        {#each report.elements as element (element.name)}
          <tr>
            <td>{element.name}</td>
            <td>
              {element.source}
              {#if element.preview}
                <span class="preview">{element.preview}</span>
              {/if}
              {#if element.full_text !== null && element.full_text !== undefined}
                <details>
                  <summary>Full text · {element.name}</summary>
                  <pre>{element.full_text}</pre>
                </details>
              {/if}
            </td>
            <td class="numeric">{element.token_count}</td>
          </tr>
        {/each}
      </tbody>
    </table>

    {#if report.compaction_applied.length > 0}
      <section>
        <h3>Compaction</h3>
        <ul>
          {#each report.compaction_applied as record}
            <li>
              {record.element} via {record.actual_strategy}{record.actual_strategy !==
              record.requested_strategy
                ? ` (requested ${record.requested_strategy})`
                : ""}: {record.tokens_before} → {record.tokens_after} tokens
              ({record.messages_removed} messages removed)
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if (report.declared_omissions ?? []).length > 0}
      <section>
        <h3>Declared omissions</h3>
        <ul>
          {#each report.declared_omissions ?? [] as omission}
            <li>
              <strong>{omission.element}</strong>
              <span class="reason">{omission.reason}</span>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if report.errors.length > 0}
      <section>
        <h3>Fallbacks</h3>
        <ul>
          {#each report.errors as assemblyError}
            <li class="error">
              {assemblyError.message} — served by
              {assemblyError.fallback_used} instead.
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if report.warnings.length > 0}
      <section>
        <h3>Warnings</h3>
        <ul>
          {#each report.warnings as warning}
            <li>{warning}</li>
          {/each}
        </ul>
      </section>
    {/if}
  {/if}
</aside>

<style>
  pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: 0.8rem;
  }

  summary { cursor: pointer; }

  .context-inspector {
    padding: 1rem;
    border-left: var(--border);
    overflow-y: auto;
    font-size: 0.875rem;
  }

  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  h2 {
    font-size: 1rem;
    margin: 0;
  }

  .close {
    border: none;
    background: none;
    font-size: 1.25rem;
    cursor: pointer;
    line-height: 1;
  }

  .status {
    color: var(--ink-soft);
  }

  .error {
    color: var(--error);
  }

  table.elements {
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

  .reason {
    display: block;
    color: var(--ink-soft);
    font-size: 0.8em;
  }

  .preview {
    display: block;
    color: var(--ink-soft);
    font-size: 0.8em;
  }

  h3 {
    font-size: 0.875rem;
    margin-block: 0.75rem 0.25rem;
  }
</style>
