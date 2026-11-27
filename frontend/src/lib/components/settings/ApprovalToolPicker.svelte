<script lang="ts">
  import type { RequireApprovalEntry } from "../../protocol/RequireApprovalEntry";
  interface Props {
    tools: string[];
    entries: RequireApprovalEntry[];
    onChange: (entries: RequireApprovalEntry[]) => void;
  }
  let { tools, entries, onChange }: Props = $props();
  let selectedTool = $state("");
  const name = (entry: RequireApprovalEntry) => typeof entry === "string" ? entry : entry.tool;
  const available = $derived(tools.filter((tool) => !entries.some((entry) => name(entry) === tool)));
  function timeout(entry: RequireApprovalEntry, value: string): void {
    const seconds = value === "" ? null : Number(value);
    if (seconds !== null && (!Number.isSafeInteger(seconds) || seconds < 1)) return;
    onChange(entries.map((item) => item === entry
      ? { ...(typeof item === "string" ? { tool: item, timeout_policy: null } : item), timeout: seconds }
      : item));
  }
</script>

<div class="picker" aria-label="Approval-gated tools">
  {#each entries as entry (name(entry))}
    <div class="tool-row">
      <span>{name(entry)}</span>
      <label>Timeout (seconds)
        <input type="number" min="1" step="1" aria-label={`Timeout for ${name(entry)}`}
          value={typeof entry === "string" ? "" : entry.timeout ?? ""} placeholder="Use default"
          onchange={(event) => timeout(entry, event.currentTarget.value)} />
      </label>
      <button type="button" aria-label={`Remove ${name(entry)} approval`} onclick={() => onChange(entries.filter((item) => item !== entry))}>Remove</button>
    </div>
  {/each}
  <div class="tool-row">
    <select aria-label="Tool to require approval" bind:value={selectedTool}>
      <option value="">Choose a tool</option>
      {#each available as tool}<option value={tool}>{tool}</option>{/each}
    </select>
    <button type="button" disabled={!available.includes(selectedTool)} onclick={() => {
      onChange([...entries, selectedTool]); selectedTool = "";
    }}>Add tool</button>
  </div>
</div>

<style>
  .picker { display: grid; gap: 0.75rem; }
  .tool-row { display: flex; flex-wrap: wrap; gap: 0.75rem; align-items: center; }
  label { display: grid; gap: 0.25rem; }
  input { width: 9rem; }
  input, select, button { color: var(--ink); background: var(--paper); border: 1px solid var(--rule); padding: 0.5rem; }
</style>
