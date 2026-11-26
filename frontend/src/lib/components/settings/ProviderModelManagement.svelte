<script lang="ts">
  import { SETTINGS_LABELS } from "../../settings-labels";
  import type { DiscoveredModel, ModelSettings } from "../../types";

  interface Props {
    providerId: string;
    providerName: string;
    models: [string, ModelSettings][];
    defaultModel: string;
    codexUsable: boolean;
    selected: boolean;
    query: string;
    discoveredModels: DiscoveredModel[];
    addedModelIds: Set<string>;
    isPersisting: boolean;
    manualModelId: string;
    onmanualadd: (providerId: string, modelId: string) => string | null;
    onremove: (id: string) => void;
    ondefault: (id: string) => void;
    onselect: (id: string) => void;
    onquery: (id: string, query: string) => void;
    onrefresh: (id: string) => void;
    ondiscoveradd: (model: DiscoveredModel) => void;
  }

  let { providerId, providerName, models, defaultModel, codexUsable, selected, query, discoveredModels, addedModelIds, isPersisting, manualModelId = $bindable(), onmanualadd, onremove, ondefault, onselect, onquery, onrefresh, ondiscoveradd }: Props = $props();

  function addManual(): void {
    const error = onmanualadd(providerId, manualModelId);
    if (!error) manualModelId = "";
  }
</script>

<div class="field">
  <div class="key">Models<small>Configured model IDs for this provider. The active default is marked.</small></div>
  <div class="val val-stack">
    {#if !codexUsable}<p class="sign-in-guidance warning">These models cannot be used until the ChatGPT account is signed in.</p>{/if}
    {#each models as [modelKey, model] (modelKey)}
      <div class="model-row provider-model-row">
        <div class="model-main"><span class="model-id">{model.model_id}</span><span class="model-sub">{modelKey}</span></div>
        <div class="model-facts">
          <span>{model.context_window ? `${model.context_window.toLocaleString()} ctx` : "context unknown"}</span>
          <span>{model.generation_preset ?? "provider generation defaults"}</span>
          <label class="model-effort"><span aria-hidden="true">{SETTINGS_LABELS["models.*.default_effort"]}</span><select aria-label={`${SETTINGS_LABELS["models.*.default_effort"]} for ${modelKey}`} bind:value={model.default_effort}><option value={undefined}>No effort</option><option value="off">Off</option><option value="minimal">Minimal</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option><option value="xhigh">Extra high</option><option value="max">Maximum</option></select></label>
          {#if defaultModel === modelKey}<span class="fact-strong">Default</span>{/if}
        </div>
        <div class="opt-row row-actions">
          <button type="button" class="opt" class:active={defaultModel === modelKey} disabled={!codexUsable || defaultModel === modelKey} onclick={() => ondefault(modelKey)}>{defaultModel === modelKey ? "Default" : "Use"}</button>
          <button type="button" class="opt opt-warn" onclick={() => onremove(modelKey)}>Remove</button>
        </div>
      </div>
    {:else}<p class="empty">No models configured for this provider.</p>{/each}
    <div class="search-row">
      <input type="text" class="text search" bind:value={manualModelId} placeholder="model-id" aria-label={`Add model by id for ${providerName}`} autocomplete="off" disabled={!codexUsable} />
      <button type="button" class="opt opt-pick" disabled={!codexUsable} onclick={addManual}><span class="pick-mark" aria-hidden="true">+</span><span>Add model</span></button>
    </div>
  </div>
</div>

<div class="field">
  <div class="key">Discover<small>Fetch known models for this provider, then add the ones Swage should use.</small></div>
  <div class="val val-stack">
    <div class="search-row">
      <input type="text" class="text search" value={selected ? query : ""} onfocus={() => onselect(providerId)} oninput={(event) => onquery(providerId, event.currentTarget.value)} placeholder="Search models" autocomplete="off" disabled={!codexUsable} />
      <button type="button" class="opt" disabled={isPersisting || !codexUsable} onclick={() => onrefresh(providerId)}>Refresh</button>
    </div>
    {#if selected}
      <div class="opt-grid">
        {#each discoveredModels as model (model.id)}
          {@const added = addedModelIds.has(model.id)}
          <button type="button" class="opt" class:active={added} disabled={added || !codexUsable} onclick={() => ondiscoveradd(model)} title={added ? "Already added" : `Add ${model.name}`}><span class="opt-label">{model.name}</span><span class="opt-tag">{added ? "Added" : "Add"}</span></button>
        {:else}<p class="empty">No discovered models to show.</p>{/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .opt { font-family: var(--font-mono); text-transform: none; }
  .opt:disabled { cursor: default; opacity: 0.85; }
  .opt-warn { border-color: var(--accent); color: var(--accent); }
  .opt-warn:hover:not(:disabled) { background: var(--accent); color: var(--paper); }
  .opt-pick { gap: 8px; padding-left: 10px; }
  .opt-pick .pick-mark { font-family: var(--font-mono); font-weight: 500; font-size: 13px; line-height: 1; color: var(--ink-orange); letter-spacing: 0; }
  .opt-pick:hover:not(:disabled):not(.active) .pick-mark { color: inherit; }
  .opt-tag { font-family: var(--font-mono); font-weight: 500; font-size: 9px; letter-spacing: 0.12em; padding: 2px 6px; border: 1px solid currentColor; }
  .opt.active .opt-tag { border-color: var(--paper); }
  .text.search { max-width: 320px; }
  .sign-in-guidance { margin: 0; color: var(--ink-soft); }
  .sign-in-guidance.warning { color: var(--ink-orange); }
  .search-row, .opt-row { display: flex; gap: 8px; flex-wrap: wrap; }
  .opt-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 8px; max-height: 280px; overflow: auto; padding-right: 4px; }
  .opt-grid .opt { justify-content: space-between; }
  .model-row { display: grid; grid-template-columns: minmax(160px, 240px) 1fr; gap: 14px; align-items: center; padding: 8px 0; border-bottom: 1px dotted var(--rule-soft); }
  .model-row:last-child { border-bottom: none; }
  .model-id { font-family: var(--font-mono); font-size: 12px; color: var(--ink); word-break: break-all; }
  .provider-model-row { grid-template-columns: minmax(180px, 1fr) minmax(180px, 1fr) auto; align-items: start; }
  .model-main { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .model-sub, .model-facts { font-family: var(--font-mono); font-size: 10px; line-height: 1.4; color: var(--ink-soft); }
  .model-facts { display: flex; gap: 8px; flex-wrap: wrap; }
  .model-facts span { padding: 2px 6px; border: 1px solid var(--rule-soft); }
  .model-effort { display: inline-flex; align-items: center; gap: 6px; color: var(--ink); }
  .model-facts .model-effort span { padding: 0; border: 0; }
  .model-facts .fact-strong { border-color: var(--accent); color: var(--accent); }
  .row-actions { justify-content: flex-end; }
  @media (max-width: 760px) { .model-row { grid-template-columns: 1fr; } }
</style>
