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

<style>
  .field { display: grid; grid-template-columns: 220px 1fr; gap: 24px; border-top: 1px solid var(--rule-soft); padding: 14px 0; align-items: baseline; }
  .key { font-family: var(--font-mono); font-weight: 500; font-size: 11px; line-height: 1.5; letter-spacing: 0.18em; text-transform: uppercase; }
  .key small { display: block; margin-top: 4px; font-family: var(--font-body); font-weight: 400; font-size: 11px; line-height: 1.4; letter-spacing: 0; text-transform: none; color: var(--ink-soft); }
  .val { display: flex; gap: 10px; flex-wrap: wrap; }
  .opt { position: relative; display: inline-flex; align-items: center; gap: 8px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); padding: 6px 12px; font-family: var(--font-display); font-weight: 400; font-size: 14px; line-height: 1; letter-spacing: 0.04em; text-transform: uppercase; cursor: pointer; isolation: isolate; }
  .opt:hover:not(:disabled):not(.active) { background: color-mix(in srgb, var(--ink) 8%, transparent); }
  .opt.active { background: var(--ink); color: var(--paper); }
  .opt.active::before { content: ""; position: absolute; inset: 0; background: var(--accent); transform: translate(2px, 2px); z-index: -1; mix-blend-mode: var(--shadow-blend); opacity: var(--shadow-opacity); pointer-events: none; }
  .empty { margin: 0; display: inline-flex; align-items: center; gap: 12px; font-family: var(--font-mono); font-weight: 500; font-size: 10px; letter-spacing: 0.18em; text-transform: uppercase; color: var(--ink-soft); }
  .empty::before { content: ""; flex: 0 0 22px; height: 1px; background: var(--rule-soft); }
  @media (max-width: 760px) { .field { grid-template-columns: 1fr; gap: 10px; } }
</style>
