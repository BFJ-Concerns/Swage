<script lang="ts">
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
    onmanualadd: (providerId: string, modelId: string) => string | null;
    onremove: (id: string) => void;
    ondefault: (id: string) => void;
    onselect: (id: string) => void;
    onquery: (id: string, query: string) => void;
    onrefresh: (id: string) => void;
    ondiscoveradd: (model: DiscoveredModel) => void;
  }

  let { providerId, providerName, models, defaultModel, codexUsable, selected, query, discoveredModels, addedModelIds, isPersisting, onmanualadd, onremove, ondefault, onselect, onquery, onrefresh, ondiscoveradd }: Props = $props();
  let manualModelId = $state("");

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
          <label class="model-effort"><span aria-hidden="true">Default effort</span><select aria-label={`Default effort for ${modelKey}`} bind:value={model.default_effort}><option value={undefined}>No effort</option><option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option><option value="max">Maximum</option></select></label>
          {#if defaultModel === modelKey}<span class="fact-strong">Default</span>{/if}
        </div>
        <div class="opt-row row-actions">
          <button type="button" class="opt" class:active={defaultModel === modelKey} disabled={!codexUsable} onclick={() => ondefault(modelKey)}>Use</button>
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
