<script lang="ts">
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";

  interface Props {
    definitions: EffectiveAgentDefinitionResponse[];
    selectedAgentId?: string;
    disabled?: boolean;
  }

  let {
    definitions,
    selectedAgentId = $bindable(""),
    disabled = false,
  }: Props = $props();

  const builtin = $derived(definitions.find((definition) => definition.is_builtin));
</script>

<label class="agent-picker">
  <span>Agent</span>
  <select bind:value={selectedAgentId} aria-label="Agent" {disabled}>
    <option value="">{builtin?.display_name ?? "Swage default"}</option>
    {#each definitions.filter((definition) => definition.id !== builtin?.id && definition.selectable) as definition (definition.id)}
      <option value={definition.id}>{definition.display_name}</option>
    {/each}
  </select>
</label>

<style>
  .agent-picker { display: flex; align-items: center; gap: .4rem; color: var(--ink-soft); font-family: var(--font-mono); font-size: .72rem; }
  select { max-width: min(34vw, 18rem); border: 1px solid var(--rule-soft); background: var(--paper); color: var(--ink-soft); font: inherit; padding: 3px 6px; cursor: pointer; }
</style>
