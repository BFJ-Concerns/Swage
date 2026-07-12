<script lang="ts">
  import type { SettingsResponse } from "../../types";

  interface Props { settings: SettingsResponse; }
  let { settings = $bindable() }: Props = $props();
  let selectedPreset = $state(Object.keys(settings.generation_presets)[0] ?? "");
  const presetIds = $derived(Object.keys(settings.generation_presets));
  const modelIds = $derived(Object.keys(settings.models));
  const currentPreset = $derived(selectedPreset ? settings.generation_presets[selectedPreset] : null);

  function changed(): void { settings = { ...settings }; }
  function addPreset(): void {
    let index = presetIds.length + 1;
    let id = `preset-${index}`;
    while (settings.generation_presets[id]) id = `preset-${++index}`;
    settings.generation_presets[id] = { max_tokens: 4096, stop_sequences: [] };
    selectedPreset = id; changed();
  }
  function duplicatePreset(): void {
    if (!currentPreset) return;
    const id = `${selectedPreset}-copy`;
    settings.generation_presets[id] = structuredClone(currentPreset);
    selectedPreset = id; changed();
  }
  function deletePreset(): void {
    for (const model of Object.values(settings.models)) if (model.generation_preset === selectedPreset) model.generation_preset = null;
    delete settings.generation_presets[selectedPreset];
    selectedPreset = Object.keys(settings.generation_presets)[0] ?? ""; changed();
  }
</script>

<div class="field"><div class="key">Preset<small>Pick a preset to edit, or create a new one.</small></div><div class="val">
  {#each presetIds as id (id)}<button type="button" class="opt" class:active={selectedPreset === id} onclick={() => (selectedPreset = id)}>{id}</button>{/each}
  <button type="button" class="opt opt-add" onclick={addPreset}>+ New</button>
  {#if currentPreset}<button type="button" class="opt" onclick={duplicatePreset}>Duplicate</button><button type="button" class="opt opt-warn" onclick={deletePreset}>Delete</button>{/if}
</div></div>
{#if currentPreset}
  <div class="field"><div class="key">Max tokens<small>Upper bound on generated output length.</small></div><div class="val"><input type="number" class="text small" bind:value={currentPreset.max_tokens} oninput={changed} min="1" placeholder="—" /></div></div>
  <div class="field"><div class="key">Temperature<small>0 is deterministic, 2 is most varied. Provider default if blank.</small></div><div class="val"><input type="number" class="text small" bind:value={currentPreset.temperature} oninput={changed} min="0" max="2" step="0.1" placeholder="—" /></div></div>
  <div class="field"><div class="key">Stop sequences<small>One per line. Generation halts as soon as any sequence is produced.</small></div><div class="val"><textarea class="text" rows="4" value={currentPreset.stop_sequences.join("\n")} oninput={(event) => { currentPreset.stop_sequences = event.currentTarget.value.split("\n").filter(Boolean); changed(); }} placeholder="One stop sequence per line"></textarea></div></div>
{:else}
  <div class="field"><div class="key">No preset selected<small>Pick a preset above, or create one.</small></div><div class="val"><p class="empty">Models without a preset use the provider's defaults.</p></div></div>
{/if}
{#if modelIds.length > 0}<div class="field"><div class="key">Model compatibility<small>Bind a preset to each model. None means the provider default.</small></div><div class="val val-stack">
  {#each modelIds as id (id)}<div class="model-row"><span class="model-id">{id}</span><div class="opt-row">
    <button type="button" class="opt" class:active={!settings.models[id].generation_preset} onclick={() => { settings.models[id].generation_preset = null; changed(); }}>None</button>
    {#each presetIds as presetId (presetId)}<button type="button" class="opt" class:active={settings.models[id].generation_preset === presetId} onclick={() => { settings.models[id].generation_preset = presetId; changed(); }}>{presetId}</button>{/each}
  </div></div>{/each}
</div></div>{/if}

<style>
  .field{display:grid;grid-template-columns:220px 1fr;gap:24px;border-top:1px solid var(--rule-soft);padding:14px 0;align-items:baseline}.key{font-family:var(--font-mono);font-weight:500;font-size:11px;line-height:1.5;letter-spacing:.18em;text-transform:uppercase}.key small{display:block;margin-top:4px;font-family:var(--font-body);font-weight:400;font-size:11px;line-height:1.4;letter-spacing:0;text-transform:none;color:var(--ink-soft)}.val{display:flex;gap:10px;flex-wrap:wrap}.val-stack{flex-direction:column;align-items:stretch;gap:12px}.opt{position:relative;display:inline-flex;align-items:center;border:1px solid var(--ink);background:var(--paper);color:var(--ink);padding:6px 12px;font-family:var(--font-display);font-size:14px;text-transform:uppercase;cursor:pointer}.opt.active{background:var(--ink);color:var(--paper)}.opt-add,.opt-warn{color:var(--accent)}.text{width:100%;max-width:480px;padding:9px 11px;border:1px solid var(--ink);background:var(--paper);color:var(--ink);font-family:var(--font-body);font-size:13px}.text.small{max-width:160px}textarea.text{resize:vertical;min-height:90px;font-family:var(--font-mono);font-size:12px}.empty{margin:0;color:var(--ink-soft)}.model-row{display:grid;grid-template-columns:minmax(160px,240px) 1fr;gap:14px;align-items:center;padding:8px 0;border-bottom:1px dotted var(--rule-soft)}.model-id{font-family:var(--font-mono);font-size:12px;word-break:break-all}.opt-row{display:flex;gap:8px;flex-wrap:wrap}@media(max-width:760px){.field,.model-row{grid-template-columns:1fr}}
</style>
