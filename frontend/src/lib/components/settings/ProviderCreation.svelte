<script lang="ts">
  import type { ProviderProtocol } from "../../types";

  export type ProviderKind = "OpenAi" | "OpenAiResponses" | "Anthropic" | "Google" | "OpenRouter" | "OpenAiCompatible" | "Codex";
  export type ProviderKindDefinition = { id: ProviderKind; label: string; protocol: ProviderProtocol };

  interface Props {
    kinds: readonly ProviderKindDefinition[];
    onpick: (kind: ProviderKindDefinition) => void;
    oncompatible: (name: string, baseUrl: string) => string | null;
    onclearerror: () => void;
  }

  let { kinds, onpick, oncompatible, onclearerror }: Props = $props();
  let adding = $state(false);
  let compatibleDraft = $state<{ name: string; baseUrl: string } | null>(null);
  let baseUrlError = $state<string | null>(null);

  function toggle(): void {
    adding = !adding;
    if (!adding) compatibleDraft = null;
    baseUrlError = null;
    onclearerror();
  }

  function pick(kind: ProviderKindDefinition): void {
    if (kind.id === "OpenAiCompatible") {
      compatibleDraft = compatibleDraft ? null : { name: "", baseUrl: "" };
      baseUrlError = null;
      onclearerror();
      return;
    }
    onpick(kind);
    adding = false;
    compatibleDraft = null;
  }

  function connect(): void {
    if (!compatibleDraft) return;
    baseUrlError = oncompatible(compatibleDraft.name, compatibleDraft.baseUrl);
    if (!baseUrlError) {
      adding = false;
      compatibleDraft = null;
    }
  }
</script>

<div class="field">
  <div class="key">
    Add provider
    <small>Pick a kind. Swage generates the internal config key.</small>
  </div>
  <div class="val val-stack">
    {#if !adding}
      <div class="opt-row">
        <button type="button" class="opt opt-anchor" onclick={toggle}>
          <span class="anchor-mark" aria-hidden="true">+</span><span>Add provider</span>
        </button>
      </div>
    {:else}
      <div class="opt-row add-row">
        {#each kinds as kind (kind.id)}
          <button type="button" class="opt opt-pick" class:active={kind.id === "OpenAiCompatible" && !!compatibleDraft} onclick={() => pick(kind)}>
            <span class="pick-mark" aria-hidden="true">+</span><span>{kind.label}</span>
          </button>
        {/each}
        <button type="button" class="opt opt-cancel" onclick={toggle}><span aria-hidden="true">×</span><span>Cancel</span></button>
      </div>
      {#if compatibleDraft}
        <div class="search-row compatible-form">
          <div class="compatible-field">
            <label class="key" for="compatible-name">Provider name</label>
            <input id="compatible-name" type="text" class="text search" bind:value={compatibleDraft.name} placeholder="My OpenAI proxy" autocomplete="off" />
          </div>
          <div class="compatible-field">
            <label class="key" for="compatible-base-url">Base URL</label>
            <input id="compatible-base-url" type="text" class="text" value={compatibleDraft.baseUrl} oninput={(event) => { if (compatibleDraft) compatibleDraft.baseUrl = event.currentTarget.value; baseUrlError = null; }} placeholder="https://api.example.com/v1" autocomplete="off" aria-describedby="compatible-base-url-help compatible-base-url-error" />
            <small id="compatible-base-url-help" class="field-help">Override the provider's canonical endpoint.</small>
            <p id="compatible-base-url-error" class="field-error" role="alert">{baseUrlError ?? ""}</p>
          </div>
          <button type="button" class="opt opt-pick" onclick={connect}><span class="pick-mark" aria-hidden="true">+</span><span>Connect</span></button>
        </div>
      {/if}
    {/if}
  </div>
</div>

<style>
  .field { display: grid; grid-template-columns: 220px 1fr; gap: 24px; border-top: 1px solid var(--rule-soft); padding: 14px 0; align-items: baseline; }
  .key { font-family: var(--font-mono); font-weight: 500; font-size: 11px; line-height: 1.5; letter-spacing: 0.18em; text-transform: uppercase; }
  .key small { display: block; margin-top: 4px; font-family: var(--font-body); font-weight: 400; font-size: 11px; line-height: 1.4; letter-spacing: 0; text-transform: none; color: var(--ink-soft); }
  .val { display: flex; gap: 10px; flex-wrap: wrap; }
  .val-stack { flex-direction: column; align-items: stretch; gap: 12px; }
  .opt { position: relative; display: inline-flex; align-items: center; gap: 8px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); padding: 6px 12px; font-family: var(--font-display); font-weight: 400; font-size: 14px; line-height: 1; letter-spacing: 0.04em; text-transform: uppercase; cursor: pointer; isolation: isolate; }
  .opt:hover:not(:disabled):not(.active) { background: rgba(14, 59, 102, 0.08); }
  .opt.active { background: var(--ink); color: var(--paper); }
  .opt.active::before { content: ""; position: absolute; inset: 0; background: var(--accent); transform: translate(2px, 2px); z-index: -1; mix-blend-mode: var(--shadow-blend); opacity: var(--shadow-opacity); pointer-events: none; }
  .opt-anchor { padding: 7px 16px 7px 7px; gap: 12px; border-width: 1.5px; }
  .opt-anchor .anchor-mark { display: inline-flex; align-items: center; justify-content: center; width: 22px; height: 22px; margin: -1px 0; border: 1px solid currentColor; color: var(--ink-orange); font-family: var(--font-mono); font-weight: 500; font-size: 14px; line-height: 1; }
  .opt-anchor:hover:not(:disabled):not(.active) { background: var(--paper); }
  .opt-anchor:hover:not(:disabled):not(.active)::before { content: ""; position: absolute; inset: 0; background: var(--accent); transform: translate(var(--offset), var(--offset)); z-index: -1; mix-blend-mode: var(--shadow-blend); opacity: var(--shadow-opacity); pointer-events: none; }
  .opt-anchor:hover:not(:disabled) .anchor-mark { color: var(--accent); }
  .opt-pick { gap: 8px; padding-left: 10px; }
  .opt-pick .pick-mark { font-family: var(--font-mono); font-weight: 500; font-size: 13px; line-height: 1; color: var(--ink-orange); letter-spacing: 0; }
  .opt-pick.active .pick-mark, .opt-pick:hover:not(:disabled):not(.active) .pick-mark { color: inherit; }
  .opt-cancel { gap: 6px; border-color: transparent; color: var(--ink-soft); padding-left: 10px; padding-right: 10px; }
  .opt-cancel:hover:not(:disabled) { background: transparent; color: var(--ink-orange); }
  .add-row > .opt { animation: chip-in 220ms cubic-bezier(0.4, 0, 0.2, 1) backwards; }
  .add-row > .opt:nth-child(1) { animation-delay: 0ms; }
  .add-row > .opt:nth-child(2) { animation-delay: 35ms; }
  .add-row > .opt:nth-child(3) { animation-delay: 70ms; }
  .add-row > .opt:nth-child(4) { animation-delay: 105ms; }
  .add-row > .opt:nth-child(5) { animation-delay: 140ms; }
  .add-row > .opt:nth-child(6) { animation-delay: 200ms; }
  .text { width: 100%; max-width: 480px; padding: 9px 11px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); font-family: var(--font-body); font-size: 13px; line-height: 1.4; }
  .text.search { max-width: 320px; }
  .text:focus { outline: none; background: rgba(240, 106, 58, 0.06); }
  .search-row, .opt-row { display: flex; gap: 8px; flex-wrap: wrap; }
  .compatible-form { flex-wrap: wrap; }
  .compatible-field { display: flex; flex-direction: column; gap: 4px; min-width: 220px; flex: 1; }
  .compatible-field .key { font-size: var(--font-meta); color: var(--ink-soft); }
  .field-help { font-family: var(--font-body); font-size: var(--font-label-sm); color: var(--ink-soft); }
  .field-error { margin: 0; font-family: var(--font-mono); font-size: var(--font-label-sm); color: var(--error); min-height: 0; }
  .field-error:empty { display: none; }
  @keyframes chip-in { from { opacity: 0; transform: translateX(-6px); } to { opacity: 1; transform: translateX(0); } }
  @media (prefers-reduced-motion: reduce) { .add-row > .opt { animation: none; } }
  @media (max-width: 760px) { .field { grid-template-columns: 1fr; gap: 10px; } }
</style>
