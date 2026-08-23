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
