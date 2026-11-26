<script lang="ts">
  import { SETTINGS_LABELS } from "../../settings-labels";
  interface Props {
    models: Record<string, import("../../types").ModelSettings>;
    providerName: (id: string) => string;
    modelIds: string[];
    defaultModel: string;
    onselect: (id: string) => void;
  }

  let { models, providerName, modelIds, defaultModel, onselect }: Props = $props();
</script>

<div class="field">
  <div class="key">
    {SETTINGS_LABELS["default_model"]}
    <small>The model selects the provider. There is no separate provider default.</small>
  </div>
  <div class="val">
    {#each modelIds as id (id)}
      <button type="button" class="opt opt-with-x" class:active={defaultModel === id} onclick={() => onselect(id)}>
        {models[id].model_id} · {providerName(models[id].provider_id)}
      </button>
    {:else}
      <p class="empty">No models configured. Add a provider, then add a model under it.</p>
    {/each}
  </div>
</div>

<style>
  .opt { font-family: var(--font-mono); text-transform: none; }
</style>
