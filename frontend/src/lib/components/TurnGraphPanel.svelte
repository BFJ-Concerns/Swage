<!--
  The turn graph: the revision that serves, every revision recorded with
  how it ended, what each changed from the one before, and the step list
  of the selected one; an earlier good revision can be made to serve again.
-->
<script lang="ts">
  import { activateTurnGraphRevision, diffTurnGraph, getTurnGraph } from "../api/turnGraph";
  import type { StepFiller } from "../protocol/StepFiller";
  import type { TurnGraphDiff } from "../protocol/TurnGraphDiff";
  import type { TurnGraphRevision } from "../protocol/TurnGraphRevision";

  let active = $state<number | null>(null);
  let revisions = $state<TurnGraphRevision[]>([]);
  let selected = $state<number | null>(null);
  let diffs = $state<Record<number, TurnGraphDiff>>({});
  let loading = $state(false);
  let error = $state<string | null>(null);
  let activating = $state<number | null>(null);
  let loadGeneration = 0;

  const selectedRevision = $derived(revisions.find((revision) => revision.revision === selected) ?? null);

  async function load() {
    const generation = ++loadGeneration;
    loading = true;
    error = null;
    try {
      const response = await getTurnGraph();
      if (generation !== loadGeneration) return;
      active = response.active;
      revisions = response.revisions;
      if (selected === null || !revisions.some((revision) => revision.revision === selected)) {
        selected = response.active;
      }
      const changes: Record<number, TurnGraphDiff> = {};
      for (const revision of response.revisions) {
        if (revision.revision === 0) continue;
        changes[revision.revision] = await diffTurnGraph(revision.revision - 1, revision.revision);
      }
      if (generation !== loadGeneration) return;
      diffs = changes;
    } catch (err: unknown) {
      if (generation !== loadGeneration) return;
      error = err instanceof Error ? err.message : "Failed to load the turn graph.";
    } finally {
      if (generation === loadGeneration) loading = false;
    }
  }

  $effect(() => {
    void load();
  });

  async function activate(revision: number) {
    activating = revision;
    error = null;
    try {
      await activateTurnGraphRevision(revision);
      await load();
    } catch (err: unknown) {
      error = err instanceof Error ? err.message : "Failed to activate the revision.";
    } finally {
      activating = null;
    }
  }

  function fillerLabel(filler: StepFiller): string {
    switch (filler.kind) {
      case "builtin":
        return "the kernel's own";
      case "plugin":
        return `plugin ${filler.name}`;
      case "workflow":
        return `workflow ${filler.name}`;
    }
  }

  function changeSummary(diff: TurnGraphDiff): string {
    const parts: string[] = [];
    if (diff.steps_added.length) parts.push(`${diff.steps_added.length} step${diff.steps_added.length === 1 ? "" : "s"} added`);
    if (diff.steps_removed.length) parts.push(`${diff.steps_removed.length} removed`);
    for (const refilled of diff.steps_refilled) parts.push(`${refilled.id}: ${fillerLabel(refilled.before)} → ${fillerLabel(refilled.after)}`);
    if (diff.steps_reordered.length) parts.push("reordered");
    for (const hook of diff.hooks_attached) parts.push(`hook ${hook.plugin} at ${hook.point} (${hook.mode})`);
    if (diff.hooks_detached.length) parts.push(`${diff.hooks_detached.length} hook${diff.hooks_detached.length === 1 ? "" : "s"} detached`);
    if (diff.hooks_reordered) parts.push("hooks reordered");
    for (const branch of diff.branches_added) parts.push(`branch ${branch.id} from ${branch.from}`);
    if (diff.branches_removed.length) parts.push(`${diff.branches_removed.length} branch${diff.branches_removed.length === 1 ? "" : "es"} removed`);
    return parts.length ? parts.join("; ") : "no change";
  }

  function activationLabel(revision: TurnGraphRevision): string {
    switch (revision.activation) {
      case "active":
        return "Serving";
      case "failed":
        return `Failed: ${revision.failure ?? "did not validate"}`;
      case "superseded":
        return "Superseded";
    }
  }
</script>

<section class="turn-graph" aria-label="Turn graph">
  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}
  {#if loading && revisions.length === 0}
    <p class="loading">Loading the turn graph…</p>
  {:else}
    <div class="catalogue">
      <div class="catalogue-list" role="list" aria-label="Turn graph revisions">
        {#each revisions as revision (revision.revision)}
          <button
            type="button"
            class:selected={revision.revision === selected}
            aria-pressed={revision.revision === selected}
            onclick={() => (selected = revision.revision)}
          >
            <span class="item-name">Revision {revision.revision}</span>
            <span class="meta" class:warn={revision.activation === "failed"}>{activationLabel(revision)}</span>
            {#if revision.revision > 0 && diffs[revision.revision]}
              <span class="description">{changeSummary(diffs[revision.revision])}</span>
            {:else if revision.revision === 0}
              <span class="description">The shipped graph: every step the kernel's own.</span>
            {/if}
          </button>
        {/each}
      </div>
      {#if selectedRevision}
        <article class="catalogue-detail" aria-label={`Revision ${selectedRevision.revision} details`}>
          <header>
            <div>
              <p class="meta">{activationLabel(selectedRevision)} · {selectedRevision.created_at}</p>
              <h2>Revision {selectedRevision.revision}</h2>
            </div>
            {#if selectedRevision.activation !== "active" && selectedRevision.revision !== active}
              <button
                type="button"
                class="btn"
                disabled={activating !== null}
                onclick={() => void activate(selectedRevision.revision)}
              >
                {activating === selectedRevision.revision ? "Activating…" : "Make it serve"}
              </button>
            {/if}
          </header>
          <dl>
            <div>
              <dt>Steps</dt>
              <dd>
                <ol class="plain">
                  {#each selectedRevision.graph.steps as step, index (index)}
                    <li><code>{step.id}</code> — {step.slot}: {fillerLabel(step.filler)}</li>
                  {/each}
                </ol>
              </dd>
            </div>
            <div>
              <dt>Hooks</dt>
              <dd>
                {#if (selectedRevision.graph.hooks ?? []).length === 0}
                  None
                {:else}
                  <ul class="plain">
                    {#each selectedRevision.graph.hooks ?? [] as hook, index (index)}
                      <li><code>{hook.plugin}</code> at {hook.point} — {hook.mode}</li>
                    {/each}
                  </ul>
                {/if}
              </dd>
            </div>
            <div>
              <dt>Branches</dt>
              <dd>
                {#if (selectedRevision.graph.branches ?? []).length === 0}
                  None
                {:else}
                  <ul class="plain">
                    {#each selectedRevision.graph.branches ?? [] as branch, index (index)}
                      <li><code>{branch.id}</code> from {branch.from}: {branch.steps.map((step) => `${step.slot} by ${fillerLabel(step.filler)}`).join(", ")} — a shadow, recorded and never served</li>
                    {/each}
                  </ul>
                {/if}
              </dd>
            </div>
          </dl>
        </article>
      {/if}
    </div>
  {/if}
</section>

<style>
  .turn-graph { display: flex; flex-direction: column; gap: 18px; }
  .meta { font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; }
  .meta.warn { color: var(--accent); }
  .btn { font-family: var(--font-mono); font-size: 11px; letter-spacing: .12em; text-transform: uppercase; padding: 9px 14px; border: 2px solid var(--ink); background: var(--ink); color: var(--paper); cursor: pointer; }
  .btn:disabled { opacity: .5; cursor: default; }
  .catalogue { display: grid; grid-template-columns: minmax(14rem, 1fr) minmax(20rem, 2fr); gap: 18px; align-items: start; }
  .catalogue-list { display: flex; flex-direction: column; border: 2px solid var(--ink); }
  .catalogue-list button { display: flex; flex-direction: column; align-items: flex-start; gap: 4px; padding: 14px; border: 0; border-bottom: 1px solid var(--ink); background: var(--paper); color: var(--ink); text-align: left; cursor: pointer; }
  .catalogue-list button:last-child { border-bottom: 0; }
  .catalogue-list button.selected { background: var(--ink); color: var(--paper); }
  .item-name { font-family: var(--font-display); font-size: 18px; letter-spacing: .04em; text-transform: uppercase; }
  .description { font-size: 12px; opacity: .75; }
  .catalogue-detail { min-width: 0; border: 2px solid var(--ink); padding: 18px; }
  .catalogue-detail header { display: flex; justify-content: space-between; gap: 16px; align-items: start; border-bottom: 1px solid var(--ink); padding-bottom: 12px; }
  .catalogue-detail h2 { margin: 4px 0 0; font-family: var(--font-display); font-weight: 400; text-transform: uppercase; }
  dl { margin: 0; }
  dl div { display: grid; grid-template-columns: minmax(8rem, 1fr) minmax(0, 2fr); gap: 12px; padding: 12px 0; border-bottom: 1px solid color-mix(in srgb, var(--ink) 25%, transparent); }
  dl div:last-child { border-bottom: 0; padding-bottom: 0; }
  dt { font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; }
  dd, code { margin: 0; overflow-wrap: anywhere; }
  .plain { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 6px; }
  .loading, .error { margin: 0; padding: 16px; border: 2px solid var(--ink); }
  .error { background: var(--ink); color: var(--paper); border-color: var(--accent); }
  @media (max-width: 760px) { .catalogue { grid-template-columns: 1fr; } }
</style>
