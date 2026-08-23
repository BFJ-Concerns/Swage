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
      <ProviderModelManagement providerId={id} providerName={displayName} {models} {defaultModel} {codexUsable} {selected} {query} {discoveredModels} {addedModelIds} {isPersisting} {onmanualadd} onremove={onremovemodel} {ondefault} {onselect} {onquery} {onrefresh} {ondiscoveradd} />
    </div>
  {/if}
</section>
