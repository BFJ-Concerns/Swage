<script lang="ts">
  import { listEffectiveAgentDefinitions } from "../api/agent-definitions";
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";

  interface Props {
    // The scope whose effective definitions are shown: a topic, or null
    // for the general scope.
    topicId: string | null;
  }

  let { topicId }: Props = $props();
  let definitions = $state<EffectiveAgentDefinitionResponse[]>([]);
  let selectedId = $state("");
  let loading = $state(false);
  let error = $state<string | null>(null);
  let loadGeneration = 0;

  const selected = $derived(definitions.find((definition) => definition.id === selectedId) ?? null);

  $effect(() => {
    const currentTopicId = topicId;
    const generation = ++loadGeneration;
    definitions = [];
    selectedId = "";
    error = null;
    loading = true;
    void listEffectiveAgentDefinitions(currentTopicId)
      .then((loaded) => {
        if (generation !== loadGeneration) return;
        definitions = loaded;
        selectedId = loaded[0]?.id ?? "";
      })
      .catch((err: unknown) => {
        if (generation !== loadGeneration) return;
        error = err instanceof Error ? err.message : "Failed to load agent definitions.";
      })
      .finally(() => {
        if (generation === loadGeneration) loading = false;
      });
  });

  function sourceLabel(definition: EffectiveAgentDefinitionResponse): string {
    if (definition.source_scope === "built_in") return "Built-in default";
    if (definition.source_scope === "topic") return "This topic";
    return "Global";
  }

  function formatted(value: unknown): string {
    return JSON.stringify(value, null, 2);
  }
</script>

<section class="agent-definitions" aria-label="Agent definitions">
  {#if loading}
    <p class="loading">Loading agent definitions…</p>
  {:else if error}
    <p class="error" role="alert">{error}</p>
  {:else if definitions.length === 0}
    <p class="empty">No agent definitions are available for this {topicId ? "topic" : "scope"}.</p>
  {:else}
    <div class="definition-list" role="list" aria-label="Effective agent definitions">
      {#each definitions as definition (definition.id)}
        <button
          type="button"
          class:selected={definition.id === selectedId}
          aria-pressed={definition.id === selectedId}
          onclick={() => (selectedId = definition.id)}
        >
          <span class="definition-name">{definition.display_name}</span>
          <span class="source">{sourceLabel(definition)}</span>
          {#if definition.description}<span class="description">{definition.description}</span>{/if}
        </button>
      {/each}
    </div>

    {#if selected}
      <article class="definition-detail" aria-label={`${selected.display_name} configuration`}>
        <header>
          <div>
            <p class="source">{sourceLabel(selected)}</p>
            <h2>{selected.display_name}</h2>
          </div>
          <code>{selected.id}</code>
        </header>
        <dl>
          <div><dt>Instructions</dt><dd>{selected.instructions || "Not set"}</dd></div>
          <div><dt>Model</dt><dd>{selected.model_id || "Use the configured default"}</dd></div>
          <div><dt>Tool permissions</dt><dd><pre>{selected.tool_permissions == null ? "Not set" : formatted(selected.tool_permissions)}</pre></dd></div>
          <div><dt>Skills</dt><dd>{selected.skills.length ? selected.skills.join(", ") : "None"}</dd></div>
          <div><dt>Output schema</dt><dd><pre>{selected.output_schema == null ? "Not set" : formatted(selected.output_schema)}</pre></dd></div>
          <div><dt>Output retries</dt><dd>{selected.output_max_retries ?? "Not set"}</dd></div>
        </dl>
      </article>
    {/if}
  {/if}
</section>

<style>
  .agent-definitions { display: grid; grid-template-columns: minmax(14rem, 1fr) minmax(20rem, 2fr); gap: 18px; align-items: start; }
  .definition-list { display: flex; flex-direction: column; border: 2px solid var(--ink); }
  .definition-list button { display: flex; flex-direction: column; align-items: flex-start; gap: 4px; padding: 14px; border: 0; border-bottom: 1px solid var(--ink); background: var(--paper); color: var(--ink); text-align: left; cursor: pointer; }
  .definition-list button:last-child { border-bottom: 0; }
  .definition-list button.selected { background: var(--ink); color: var(--paper); }
  .definition-name { font-family: var(--font-display); font-size: 18px; letter-spacing: .04em; text-transform: uppercase; }
  .source { font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; }
  .description { font-size: 12px; opacity: .75; }
  .definition-detail { min-width: 0; border: 2px solid var(--ink); padding: 18px; }
  .definition-detail header { display: flex; justify-content: space-between; gap: 16px; align-items: start; border-bottom: 1px solid var(--ink); padding-bottom: 12px; }
  .definition-detail h2 { margin: 4px 0 0; font-family: var(--font-display); font-weight: 400; text-transform: uppercase; }
  .definition-detail code { overflow-wrap: anywhere; }
  dl { margin: 0; }
  dl div { display: grid; grid-template-columns: minmax(8rem, 1fr) minmax(0, 2fr); gap: 12px; padding: 12px 0; border-bottom: 1px solid color-mix(in srgb, var(--ink) 25%, transparent); }
  dl div:last-child { border-bottom: 0; padding-bottom: 0; }
  dt { font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; }
  dd, pre { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; }
  pre { font-family: var(--font-mono); font-size: 12px; }
  .loading, .empty, .error { grid-column: 1 / -1; margin: 0; padding: 16px; border: 2px solid var(--ink); }
  .error { background: var(--ink); color: var(--paper); border-color: var(--accent); }
  @media (max-width: 760px) { .agent-definitions { grid-template-columns: 1fr; } }
</style>
