<script lang="ts">
  import { onMount } from "svelte";
  import { fetchAuditEvents, fetchOperatorHistory } from "../api/operator";
  import type { OperatorHistoryResponse } from "../protocol/OperatorHistoryResponse";
  import type { AuditEventView } from "../protocol/AuditEventView";
  import type { AuditEventCursor } from "../protocol/AuditEventCursor";
  import TurnContextInspectorTrigger from "./TurnContextInspectorTrigger.svelte";

  let history = $state<OperatorHistoryResponse | null>(null);
  let errorMessage = $state<string | null>(null);
  let inspectorTarget: HTMLDivElement | undefined = $state();

  // The audit record itself: every event kind the record holds, read by
  // the filters the operator sets and paged older through the cursor.
  const AUDIT_PAGE = 50;
  let auditSessionId = $state("");
  let auditEntityKind = $state("");
  let auditEntityId = $state("");
  let auditEventKind = $state("");
  let auditEvents = $state<AuditEventView[]>([]);
  let auditNextCursor = $state<AuditEventCursor | null>(null);
  let auditError = $state<string | null>(null);
  let auditLoaded = $state(false);

  function formatState(state: unknown): string {
    return JSON.stringify(state, null, 2);
  }

  function auditFilter(before?: AuditEventCursor) {
    return {
      sessionId: auditSessionId.trim() || undefined,
      entityKind: auditEntityKind.trim() || undefined,
      entityId: auditEntityId.trim() || undefined,
      eventKind: auditEventKind.trim() || undefined,
      before,
      limit: AUDIT_PAGE,
    };
  }

  async function loadAudit(): Promise<void> {
    auditError = null;
    try {
      const page = await fetchAuditEvents(auditFilter());
      auditEvents = page.events;
      auditNextCursor = page.next_cursor ?? null;
    } catch (error) {
      auditError = error instanceof Error ? error.message : "Failed to load the audit record";
    } finally {
      auditLoaded = true;
    }
  }

  async function loadOlderAudit(): Promise<void> {
    if (!auditNextCursor) return;
    auditError = null;
    try {
      const page = await fetchAuditEvents(auditFilter(auditNextCursor));
      auditEvents = [...auditEvents, ...page.events];
      auditNextCursor = page.next_cursor ?? null;
    } catch (error) {
      auditError = error instanceof Error ? error.message : "Failed to load the audit record";
    }
  }

  onMount(async () => {
    void loadAudit();
    try {
      history = await fetchOperatorHistory();
    } catch (error) {
      errorMessage = error instanceof Error ? error.message : "Failed to load operator history";
    }
  });
</script>

<aside class="history-pane" aria-label="Operator history">
  <header>
    <p class="eyebrow">Recorded history</p>
    <h2>Configuration and experiment activity</h2>
    <p>Read-only records from the local audit trail and saved turn context.</p>
  </header>

  <section aria-labelledby="audit-record-heading" class="audit-record">
    <h3 id="audit-record-heading">Audit record</h3>
    <form
      class="audit-filters"
      aria-label="Audit record filters"
      onsubmit={(event) => {
        event.preventDefault();
        void loadAudit();
      }}
    >
      <label><span>Session</span><input bind:value={auditSessionId} placeholder="session id" /></label>
      <label><span>Entity kind</span><input bind:value={auditEntityKind} placeholder="turn, surface, settings…" /></label>
      <label><span>Entity id</span><input bind:value={auditEntityId} placeholder="entity id" /></label>
      <label><span>Event kind</span><input bind:value={auditEventKind} placeholder="configuration_change…" /></label>
      <button type="submit">Apply filters</button>
    </form>
    {#if auditError}
      <p class="history-alert" role="alert">{auditError}</p>
    {:else if !auditLoaded}
      <p class="history-loading">Loading the audit record…</p>
    {:else if auditEvents.length === 0}
      <p class="empty-copy">No events match.</p>
    {:else}
      <div class="entries">
        {#each auditEvents as event (event.id)}
          <article class="history-entry audit-entry">
            <div class="entry-heading">
              <strong>{event.event_kind} · {event.entity_kind} · {event.entity_id}</strong>
              <time datetime={event.created_at}>{new Date(event.created_at).toLocaleString()}</time>
            </div>
            <p>{event.summary} · session <span class="mono">{event.session_id ?? "none"}</span></p>
            <details>
              <summary>Recorded detail</summary>
              <pre>{formatState(event.metadata)}</pre>
            </details>
          </article>
        {/each}
      </div>
      {#if auditNextCursor}
        <button type="button" class="load-older" onclick={() => void loadOlderAudit()}>Load older events</button>
      {/if}
    {/if}
  </section>

  {#if errorMessage}
    <p class="history-alert" role="alert">{errorMessage}</p>
  {:else if !history}
    <p class="history-loading">Loading recorded history…</p>
  {:else}
    <section aria-labelledby="configuration-history-heading">
      <h3 id="configuration-history-heading">Configuration changes</h3>
      {#if history.configuration_changes.length === 0}
        <p class="empty-copy">No configuration changes recorded.</p>
      {:else}
        <div class="entries">
          {#each history.configuration_changes as event (event.id)}
            <article class="history-entry configuration-entry">
              <div class="entry-heading">
                <strong>{event.resource_kind} · {event.resource_id}</strong>
                <time datetime={event.recorded_at}>{new Date(event.recorded_at).toLocaleString()}</time>
              </div>
              <dl>
                <div><dt>Action</dt><dd>{event.action}</dd></div>
                <div><dt>Source</dt><dd>{event.source}</dd></div>
                <div><dt>Session</dt><dd class="mono">{event.session_id ?? "none"}</dd></div>
              </dl>
              <div class="state-grid">
                <section><h4>Prior state</h4><pre>{formatState(event.prior_state)}</pre></section>
                <section><h4>New state</h4><pre>{formatState(event.new_state)}</pre></section>
              </div>
            </article>
          {/each}
        </div>
      {/if}
    </section>

    <section aria-labelledby="variant-history-heading">
      <h3 id="variant-history-heading">Variant selections</h3>
      {#if history.variant_selections.length === 0}
        <p class="empty-copy">No variant selections recorded.</p>
      {:else}
        <div class="entries">
          {#each history.variant_selections as event (event.id)}
            <article class="history-entry variant-entry">
              <div class="entry-heading">
                <strong>{event.location_id} · {event.variant}</strong>
                <time datetime={event.recorded_at}>{new Date(event.recorded_at).toLocaleString()}</time>
              </div>
              <p>{event.reason} · session <span class="mono">{event.session_id ?? "none"}</span></p>
              {#if event.turn_id}<p>Turn <span class="mono">{event.turn_id}</span></p>{/if}
            </article>
          {/each}
        </div>
      {/if}
    </section>

    <section aria-labelledby="assembly-history-heading">
      <h3 id="assembly-history-heading">Assembly decisions</h3>
      {#if history.assembly_reports.length === 0}
        <p class="empty-copy">No assembly reports recorded.</p>
      {:else}
        <div class="entries">
          {#each history.assembly_reports as entry (entry.turn_id)}
            <article class="history-entry assembly-entry">
              <div class="entry-heading">
                <strong>{entry.assembly_name}</strong>
                <time datetime={entry.recorded_at}>{new Date(entry.recorded_at).toLocaleString()}</time>
              </div>
              <p>Turn <span class="mono">{entry.turn_id}</span> · session <span class="mono">{entry.session_id}</span></p>
              <TurnContextInspectorTrigger
                turnId={entry.turn_id}
                label="Inspect turn context"
                presentation="history"
                {inspectorTarget}
              />
            </article>
          {/each}
        </div>
        <div class="turn-context-inspector-outlet" bind:this={inspectorTarget}></div>
      {/if}
    </section>
  {/if}
</aside>

<style>
  .history-pane { border: 2px solid var(--ink); background: var(--paper); padding: 18px; margin-bottom: 18px; }
  header { border-bottom: 1px solid var(--ink); padding-bottom: 12px; }
  h2, h3, h4, p { margin: 0; }
  h3 { margin: 20px 0 10px; font-family: var(--font-display); text-transform: uppercase; letter-spacing: 0.04em; }
  h4 { font: 600 11px var(--font-mono); text-transform: uppercase; letter-spacing: 0.1em; margin-bottom: 6px; }
  .eyebrow { font: 600 11px var(--font-mono); text-transform: uppercase; letter-spacing: 0.16em; color: var(--accent); }
  .entries { display: grid; gap: 10px; }
  .history-entry { border: 1px solid var(--ink); padding: 12px; min-width: 0; }
  .entry-heading { display: flex; justify-content: space-between; gap: 12px; flex-wrap: wrap; }
  time, dt, .mono, .history-loading, .history-alert, .empty-copy { font-family: var(--font-mono); font-size: 11px; }
  dl { display: flex; gap: 16px; flex-wrap: wrap; margin: 10px 0; }
  dl div { display: flex; gap: 6px; } dt { text-transform: uppercase; } dd { margin: 0; }
  .state-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
  pre { overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; background: var(--paper-deep); padding: 10px; font: 11px/1.45 var(--font-mono); margin: 6px 0 0; }
  .history-alert { color: var(--accent); padding-top: 12px; }
  .audit-filters { display: flex; gap: 10px; flex-wrap: wrap; align-items: flex-end; margin-bottom: 10px; }
  .audit-filters label { display: flex; flex-direction: column; gap: 4px; font: 600 11px var(--font-mono); text-transform: uppercase; letter-spacing: 0.08em; }
  .audit-filters input { font: 12px var(--font-mono); padding: 4px 6px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); }
  details summary { font: 600 11px var(--font-mono); text-transform: uppercase; letter-spacing: 0.08em; cursor: pointer; margin-top: 6px; }
  .load-older { margin-top: 10px; }
  .history-loading, .empty-copy { padding: 12px 0; text-transform: uppercase; letter-spacing: 0.08em; }
  @media (max-width: 760px) { .state-grid { grid-template-columns: 1fr; } }
</style>
