<script lang="ts">
  import type { DiscoveredModel, ModelSettings, ProviderSettings } from "../../types";
  import type { ProviderSignInState } from "../../protocol/ProviderSignInState";
  import CodexProviderAuthentication from "./CodexProviderAuthentication.svelte";
  import ProviderModelManagement from "./ProviderModelManagement.svelte";

  interface Props {
    id: string; index: number; provider: ProviderSettings; displayName: string;
    models: [string, ModelSettings][]; defaultModel: string; saved: boolean;
    signInState: ProviderSignInState | undefined; deviceCode: { code: string; verificationUrl: string } | null;
    selected: boolean; query: string; discoveredModels: DiscoveredModel[]; addedModelIds: Set<string>; isPersisting: boolean;
    onremoveprovider: (id: string) => void; onmanualadd: (id: string, model: string) => string | null;
    onremovemodel: (id: string) => void; ondefault: (id: string) => void; onselect: (id: string) => void;
    onquery: (id: string, query: string) => void; onrefresh: (id: string) => void; ondiscoveradd: (model: DiscoveredModel) => void;
    onbegin: (id: string, method: "browser" | "device_code") => void; oncheck: (id: string) => void; oncopy: () => void; onend: (id: string) => void;
  }

  let { id, index, provider, displayName, models, defaultModel, saved, signInState, deviceCode, selected, query, discoveredModels, addedModelIds, isPersisting, onremoveprovider, onmanualadd, onremovemodel, ondefault, onselect, onquery, onrefresh, ondiscoveradd, onbegin, oncheck, oncopy, onend }: Props = $props();
  let collapsed = $state(false);
  let manualModelId = $state("");
  const codexUsable = $derived(provider.protocol !== "codex" || signInState?.status === "signed_in");
</script>

<section class="provider-section" aria-label={`Provider ${id}`}>
  <div class="provider-head">
    <button type="button" class="provider-collapse" aria-expanded={!collapsed} aria-label={`Provider ${displayName} — ${collapsed ? "expand" : "collapse"}`} onclick={() => collapsed = !collapsed}>
      <span class="chevron" aria-hidden="true">{collapsed ? "▸" : "▾"}</span>
      <span class="provider-head-text"><span class="provider-index">Provider <span class="ix">{String(index + 1).padStart(2, "0")}</span></span><h2>{displayName}</h2><span class="provider-meta-line"><span class="meta-protocol">{provider.protocol}</span><span class="meta-key">{id}</span><span class="meta-endpoint">{provider.base_url || "provider default endpoint"}</span></span></span>
      <span class="credential-status" class:set={provider.has_credential || signInState?.status === "signed_in"}>{provider.protocol === "codex" ? signInState?.status === "signed_in" ? "✓ Signed in" : signInState?.status === "signing_in" ? "Sign-in pending" : signInState?.status === "expired" ? "Sign-in expired" : "Signed out" : provider.has_credential ? "✓ Credential" : "No credential"}</span>
    </button>
    <button type="button" class="provider-remove" aria-label={`Remove provider ${displayName}`} title="Remove provider" onclick={() => onremoveprovider(id)}><span aria-hidden="true">×</span></button>
  </div>
  {#if !collapsed}
    <div class="provider-body">
      {#if provider.protocol !== "codex"}
        <div class="field"><div class="key">Base URL<small>Override the provider's canonical endpoint when needed.</small></div><div class="val"><input type="text" class="text" bind:value={provider.base_url} placeholder="Provider default" autocomplete="off" /></div></div>
        <div class="field"><div class="key">API key<small>Paste a new value to store or rotate this provider's credential.</small></div><div class="val key-val"><input type="password" class="text" bind:value={provider.api_key} placeholder={provider.has_credential ? "Stored credential" : "Paste to store"} autocomplete="off" /><span class="credential-status" class:set={provider.has_credential}>{provider.has_credential ? "Stored" : "Empty"}</span></div></div>
      {:else}
        <CodexProviderAuthentication providerId={id} {saved} state={signInState} {deviceCode} {onbegin} {oncheck} {oncopy} {onend} />
      {/if}
      <ProviderModelManagement providerId={id} providerName={displayName} {models} {defaultModel} {codexUsable} {selected} {query} {discoveredModels} {addedModelIds} {isPersisting} bind:manualModelId {onmanualadd} onremove={onremovemodel} {ondefault} {onselect} {onquery} {onrefresh} {ondiscoveradd} />
    </div>
  {/if}
</section>

<style>
  .provider-section { border-top: 2px solid var(--ink); padding-top: 18px; position: relative; }
  .provider-section::before { content: "+"; position: absolute; top: -10px; left: -2px; width: 14px; height: 14px; display: flex; align-items: center; justify-content: center; background: var(--paper); color: var(--ink-orange); font-family: var(--font-mono); font-weight: 500; font-size: 14px; line-height: 1; }
  .provider-head { display: flex; align-items: stretch; gap: 4px; padding-bottom: 12px; }
  .provider-collapse { flex: 1; min-width: 0; display: grid; grid-template-columns: auto 1fr auto; align-items: center; gap: 12px; background: transparent; border: none; padding: 6px 4px; margin: 0; text-align: left; font: inherit; color: inherit; cursor: pointer; border-radius: 0; }
  .provider-collapse:hover { background: var(--rule-faint); }
  .provider-collapse:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .chevron { font-family: var(--font-mono); font-size: 14px; line-height: 1; color: var(--ink-soft); transition: transform 120ms ease; }
  .provider-head-text { min-width: 0; }
  .credential-status { font-family: var(--font-mono); font-weight: 500; font-size: 10px; letter-spacing: 0.16em; text-transform: uppercase; padding: 3px 8px; border: 1px solid var(--ink-soft); color: var(--ink-soft); }
  .provider-collapse .credential-status { justify-self: end; }
  .credential-status.set { border-color: var(--ink); color: var(--ink); background: color-mix(in srgb, var(--success) 14%, transparent); }
  .provider-remove { flex-shrink: 0; align-self: center; background: transparent; border: 1px solid transparent; color: var(--ink-soft); font-size: 16px; line-height: 1; padding: 4px 8px; cursor: pointer; opacity: 0.6; transition: opacity 120ms ease, color 120ms ease, border-color 120ms ease; }
  .provider-remove:hover, .provider-remove:focus-visible { opacity: 1; color: var(--error); border-color: var(--rule-soft); }
  .provider-remove:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .provider-index { display: block; margin-bottom: 6px; font-family: var(--font-mono); font-weight: 500; font-size: var(--font-meta); line-height: 1; letter-spacing: 0.22em; text-transform: uppercase; color: var(--ink-soft); }
  .provider-index .ix { color: var(--ink-orange); }
  .provider-head h2 { margin: 0; font-family: var(--font-display); font-weight: 400; font-size: var(--font-display-3); line-height: 1; letter-spacing: 0.06em; text-transform: uppercase; }
  .provider-meta-line { margin: 8px 0 0; display: flex; flex-wrap: wrap; gap: 6px 14px; font-family: var(--font-mono); font-size: var(--font-label-sm); color: var(--ink-soft); word-break: break-word; }
  .provider-meta-line span { display: inline-flex; align-items: center; gap: 14px; }
  .provider-meta-line span + span::before { content: "·"; color: var(--rule-soft); }
  .provider-meta-line .meta-protocol { color: var(--ink); letter-spacing: 0.06em; text-transform: uppercase; }
  .field { display: grid; grid-template-columns: 220px 1fr; gap: 24px; border-top: 1px solid var(--rule-soft); padding: 14px 0; align-items: baseline; }
  .key { font-family: var(--font-mono); font-weight: 500; font-size: 11px; line-height: 1.5; letter-spacing: 0.18em; text-transform: uppercase; }
  .key small { display: block; margin-top: 4px; font-family: var(--font-body); font-weight: 400; font-size: 11px; line-height: 1.4; letter-spacing: 0; text-transform: none; color: var(--ink-soft); }
  .val { display: flex; gap: 10px; flex-wrap: wrap; }
  .key-val { align-items: center; }
  .text { width: 100%; max-width: 480px; padding: 9px 11px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); font-family: var(--font-body); font-size: 13px; line-height: 1.4; }
  .text:focus { outline: none; background: color-mix(in srgb, var(--accent) 6%, transparent); }
  @media (max-width: 760px) { .field { grid-template-columns: 1fr; gap: 10px; } }
</style>
