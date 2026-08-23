<script lang="ts">
  interface Props {
    modelIds: string[];
    defaultModel: string;
    onselect: (id: string) => void;
  }

  let { modelIds, defaultModel, onselect }: Props = $props();
</script>

<div class="field">
  <div class="key">
    Default model
    <small>The model selects the provider. There is no separate provider default.</small>
  </div>
  <div class="val">
    {#each modelIds as id (id)}
      <button type="button" class="opt opt-with-x" class:active={defaultModel === id} onclick={() => onselect(id)}>
        {id}
      </button>
    {:else}
      <p class="empty">No models configured. Add a provider, then add a model under it.</p>
    {/each}
  </div>
</div>
