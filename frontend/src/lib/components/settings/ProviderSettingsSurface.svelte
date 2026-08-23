<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { discoverModels, fetchProviderSignIn, signOutProvider, startProviderSignIn } from "../../api/settings";
  import type { DiscoveredModel, ModelSettings, SettingsResponse } from "../../types";
  import type { ProviderSignInState } from "../../protocol/ProviderSignInState";
  import ProviderConfiguration from "./ProviderConfiguration.svelte";
  import ProviderCreation, { type ProviderKindDefinition } from "./ProviderCreation.svelte";
  import ProviderDefaultModel from "./ProviderDefaultModel.svelte";

  interface Props {
    settings: SettingsResponse;
    savedProviderIds: Set<string>;
    isPersisting: boolean;
    error: string | null;
  }

  let {
    settings = $bindable(),
    savedProviderIds,
    isPersisting,
    error = $bindable(),
  }: Props = $props();
  let selectedProvider = $state("");
  let modelQuery = $state("");
  let discoveredModels = $state<DiscoveredModel[]>([]);
  let signInByProvider = $state<Record<string, ProviderSignInState>>({});
  let deviceCode = $state<{ code: string; verificationUrl: string } | null>(null);
  const signInPolls = new Map<string, ReturnType<typeof setTimeout>>();

  const PROVIDER_KINDS = [
    { id: "OpenAi", label: "OpenAI", protocol: "openai" },
    { id: "OpenAiResponses", label: "OpenAI Responses", protocol: "openai-responses" },
    { id: "Anthropic", label: "Anthropic", protocol: "anthropic" },
    { id: "Google", label: "Google", protocol: "google" },
    { id: "OpenRouter", label: "OpenRouter", protocol: "openrouter" },
    { id: "OpenAiCompatible", label: "OpenAI compatible", protocol: "openai" },
    { id: "Codex", label: "Codex", protocol: "codex" },
  ] as const satisfies readonly ProviderKindDefinition[];
  const providerIds = $derived(Object.keys(settings.providers));
  const modelIds = $derived(Object.keys(settings.models));
  const defaultModel = $derived(settings.models[settings.default_model] ?? null);
  // Discovered models that have already been added under this provider — used
  // to mark the chip as ADDED rather than silently overwriting on re-click.
  const addedModelIds = $derived(
    settings && selectedProvider
      ? new Set(
          Object.values(settings.models)
            .filter((model) => model.provider_id === selectedProvider)
            .map((model) => model.model_id),
        )
      : new Set<string>(),
  );


  onMount(() => {
    selectedProvider = settings.default_provider || providerIds[0] || "";
    if (selectedProvider) void loadModels();
    if (selectedProvider) void loadSignInState(selectedProvider);
  });

  onDestroy(() => {
    for (const poll of signInPolls.values()) clearTimeout(poll);
  });

  async function loadModels(): Promise<void> {
    if (!selectedProvider) return;
    const provider = settings.providers[selectedProvider];
    if (!provider) return;
    if (provider.protocol === "codex" && savedProviderIds.has(selectedProvider) && !codexModelsUsable(selectedProvider)) {
      discoveredModels = [];
      return;
    }
    const discoveryDraft = savedProviderIds.has(selectedProvider)
      ? undefined
      : {
          protocol: provider.protocol,
          base_url: provider.base_url,
          api_key_secret_ref: provider.api_key_secret_ref,
          api_key_env: provider.api_key_env,
          ...(provider.protocol === "codex" ? {
            command: provider.command,
            state_dir: provider.state_dir,
          } : {}),
        };
    try {
      const response = await discoverModels(selectedProvider, modelQuery, discoveryDraft);
      discoveredModels = response.models;
      error = null;
    } catch (err) {
      // Discovery failures are non-fatal — leave the chip row empty and
      // surface the error in the notice band so the user knows why.
      discoveredModels = [];
      error = err instanceof Error ? err.message : "Failed to discover models.";
    }
  }

  function selectProvider(id: string): void {
    selectedProvider = id;
    modelQuery = "";
    loadModels();
    void loadSignInState(id);
  }

  async function loadSignInState(id: string): Promise<void> {
    if (settings.providers[id]?.protocol !== "codex" || !savedProviderIds.has(id)) return;
    try {
      const state = await fetchProviderSignIn(id);
      signInByProvider[id] = state;
      if (state.status === "signed_in" && selectedProvider === id) void loadModels();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to read provider sign-in state.";
    }
  }

  function pollSignInState(id: string, remaining = 60): void {
    const previous = signInPolls.get(id);
    if (previous) clearTimeout(previous);
    if (remaining <= 0 || signInByProvider[id]?.status !== "signing_in") {
      signInPolls.delete(id);
      return;
    }
    signInPolls.set(id, setTimeout(async () => {
      await loadSignInState(id);
      pollSignInState(id, remaining - 1);
    }, 2_000));
  }

  function codexModelsUsable(id: string): boolean {
    return signInByProvider[id]?.status === "signed_in";
  }

  async function beginSignIn(id: string, method: "browser" | "device_code"): Promise<void> {
    try {
      const response = await startProviderSignIn(id, method);
      if (response.method === "browser") {
        window.open(response.auth_url, "_blank", "noopener,noreferrer");
      } else {
        deviceCode = { code: response.user_code, verificationUrl: response.verification_url };
        window.open(response.verification_url, "_blank", "noopener,noreferrer");
      }
      signInByProvider[id] = { status: "signing_in", account: null };
      pollSignInState(id);
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to start provider sign-in.";
    }
  }

  async function copyDeviceCode(): Promise<void> {
    if (deviceCode) await navigator.clipboard.writeText(deviceCode.code);
  }

  async function endSignIn(id: string): Promise<void> {
    try {
      signInByProvider[id] = await signOutProvider(id);
      deviceCode = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to sign out provider.";
    }
  }

  function addDiscoveredModel(model: DiscoveredModel): void {

    const id = `${selectedProvider}:${model.id}`;
    if (settings.models[id]) return; // Already added — chip shows ADDED.
    settings.models[id] = {
      provider_id: selectedProvider,
      model_id: model.id,
      aliases: [],
      context_window: model.context_window,
      generation_preset: null,
      default_effort: undefined,
    };
    settings.default_model ||= id;
    settings.default_provider ||= selectedProvider;
  }

  function providerModels(providerId: string): [string, ModelSettings][] {
    if (!settings) return [];
    return Object.entries(settings.models).filter(([, model]) => model.provider_id === providerId);
  }

  function slugifyProviderName(value: string): string {
    return value
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "");
  }

  function nextProviderId(baseId: string): string {
    if (!settings || !settings.providers[baseId]) return baseId;
    let suffix = 2;
    let candidate = `${baseId}-${suffix}`;
    while (settings.providers[candidate]) {
      suffix += 1;
      candidate = `${baseId}-${suffix}`;
    }
    return candidate;
  }

  function providerDisplayName(id: string): string {
    if (!settings) return id;
    const provider = settings.providers[id];
    if (!provider) return id;
    if (provider.protocol === "openai" && provider.base_url) return "OpenAI compatible";
    return PROVIDER_KINDS.find((kind) => kind.protocol === provider.protocol && kind.id !== "OpenAiCompatible")?.label ?? id;
  }

  function pickProviderKind(kind: ProviderKindDefinition): void {
    const id = nextProviderId(slugifyProviderName(kind.label));
    settings.providers[id] = {
      protocol: kind.protocol,
      base_url: kind.id === "OpenRouter" ? "https://openrouter.ai/api/v1" : null,
      api_key_secret_ref: null,
      api_key_env: null,
      has_credential: false,
      api_key: "",
      command: kind.id === "Codex" ? "codex" : null,
      state_dir: null,
    };
    selectedProvider = id;
    discoveredModels = [];
    error = null;
  }

  function commitCompatibleProvider(name: string, value: string): string | null {
    const baseUrl = value.trim();
    if (!baseUrl) {
      return "Enter a base URL so Swage knows where to send requests.";
    }
    if (!/^https?:\/\//i.test(baseUrl)) {
      return "Enter a URL starting with http:// or https://";
    }
    const baseId = slugifyProviderName(name) || "openai-compatible";
    const id = nextProviderId(baseId);
    settings.providers[id] = {
      protocol: "openai",
      base_url: baseUrl,
      api_key_secret_ref: null,
      api_key_env: null,
      has_credential: false,
      api_key: "",
    };
    selectedProvider = id;
    discoveredModels = [];
    error = null;
    return null;
  }

  function removeProvider(id: string): void {

    for (const modelId of Object.keys(settings.models)) {
      if (settings.models[modelId].provider_id === id) removeModel(modelId);
    }
    delete settings.providers[id];
    if (settings.default_provider === id) settings.default_provider = defaultModel?.provider_id ?? "";
    if (selectedProvider === id) selectedProvider = providerIds.find((providerId) => providerId !== id) ?? "";
  }

  function addManualModel(providerId: string, value: string): string | null {
    const modelId = value.trim();
    if (!modelId) {
      error = "Model ID is required.";
      return error;
    }
    const id = `${providerId}:${modelId}`;
    if (settings.models[id]) {
      error = `Model '${id}' already exists.`;
      return error;
    }
    settings.models[id] = {
      provider_id: providerId,
      model_id: modelId,
      aliases: [],
      context_window: null,
      generation_preset: null,
      default_effort: undefined,
    };
    settings.default_model ||= id;
    settings.default_provider ||= providerId;
    error = null;
    return null;
  }

  function setDefaultModel(id: string): void {
    settings.default_model = id;
    settings.default_provider = settings.models[id].provider_id;
  }

  function updateModelQuery(id: string, query: string): void {
    selectedProvider = id;
    modelQuery = query;
    void loadModels();
  }

  function refreshModels(id: string): void {
    selectedProvider = id;
    void loadModels();
  }

  function removeModel(id: string): void {

    delete settings.models[id];
    if (settings.default_model === id) {
      const replacement = modelIds.find((modelId) => modelId !== id) ?? "";
      settings.default_model = replacement;
      settings.default_provider = replacement ? settings.models[replacement]?.provider_id ?? "" : "";
    }
  }
</script>

<div class="provider-settings-surface">
  <ProviderDefaultModel {modelIds} defaultModel={settings.default_model} onselect={setDefaultModel} />
  <ProviderCreation kinds={PROVIDER_KINDS} onpick={pickProviderKind} oncompatible={commitCompatibleProvider} onclearerror={() => error = null} />

{#each providerIds as id, ix (id)}
  {@const provider = settings.providers[id]}
  <ProviderConfiguration
    {id} index={ix} {provider} displayName={providerDisplayName(id)} models={providerModels(id)}
    defaultModel={settings.default_model} saved={savedProviderIds.has(id)} signInState={signInByProvider[id]}
    {deviceCode} selected={selectedProvider === id} query={modelQuery} {discoveredModels} {addedModelIds} {isPersisting}
    onremoveprovider={removeProvider} onmanualadd={addManualModel} onremovemodel={removeModel} ondefault={setDefaultModel}
    onselect={selectProvider} onquery={updateModelQuery} onrefresh={refreshModels} ondiscoveradd={addDiscoveredModel}
    onbegin={beginSignIn} oncheck={loadSignInState} oncopy={copyDeviceCode} onend={endSignIn}
  />
{:else}
  <div class="field"><div class="key">No providers<small>Swage will start without messaging until a model/provider pair exists.</small></div><div class="val"><p class="empty">Add a provider above. Nothing is assumed.</p></div></div>
  {/each}
</div>

<style>
  .provider-settings-surface {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }

  .field {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 24px;
    border-top: 1px solid var(--rule-soft);
    padding: 14px 0;
    align-items: baseline;
  }

  .key {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1.5;
    letter-spacing: 0.18em;
    text-transform: uppercase;
  }

  .key small {
    display: block;
    margin-top: 4px;
    font-family: var(--font-body);
    font-weight: 400;
    font-size: 11px;
    line-height: 1.4;
    letter-spacing: 0;
    text-transform: none;
    color: var(--ink-soft);
  }

  .val {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
  }

  .empty {
    margin: 0;
    display: inline-flex;
    align-items: center;
    gap: 12px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .empty::before {
    content: "";
    flex: 0 0 22px;
    height: 1px;
    background: var(--rule-soft);
  }

  @media (max-width: 760px) {
    .field {
      grid-template-columns: 1fr;
      gap: 10px;
    }
  }
</style>
