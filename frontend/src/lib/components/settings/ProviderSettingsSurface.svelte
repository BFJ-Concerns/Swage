<script lang="ts">
  import { onMount } from "svelte";
  import { discoverModels, fetchProviderSignIn, signOutProvider, startProviderSignIn } from "../../api/settings";
  import type { DiscoveredModel, ModelSettings, ProviderProtocol, SettingsResponse } from "../../types";
  import type { ProviderSignInState } from "../../protocol/ProviderSignInState";

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
  // Add-provider flow is a two-stage chip expansion. addingProvider toggles
  // the kind row; compatibleDraft holds the OpenAI-compatible name/URL while
  // the inline inputs are open (null otherwise).
  let addingProvider = $state(false);
  let compatibleDraft = $state<{ name: string; base_url: string } | null>(null);
  // Inline validation for the compatible Base URL field. Reserved slot
  // rendered as role="alert" so client-side validation has somewhere to write
  // (UI audit f-6). Null/empty means no error to show.
  let compatibleBaseUrlError = $state<string | null>(null);
  let newModelIdByProvider = $state<Record<string, string>>({});
  let discoveredModels = $state<DiscoveredModel[]>([]);
  let collapsedProviderIds = $state<Set<string>>(new Set());
  let signInByProvider = $state<Record<string, ProviderSignInState>>({});
  let deviceCode = $state<string | null>(null);

  const PROVIDER_KINDS = [
    { id: "OpenAi", label: "OpenAI", protocol: "openai" },
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

  async function loadModels(): Promise<void> {
    if (!selectedProvider) return;
    const provider = settings.providers[selectedProvider];
    if (!provider) return;
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
      signInByProvider[id] = await fetchProviderSignIn(id);
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to read provider sign-in state.";
    }
  }

  async function beginSignIn(id: string, method: "browser" | "device_code"): Promise<void> {
    try {
      const response = await startProviderSignIn(id, method);
      if (response.method === "browser") {
        window.open(response.auth_url, "_blank", "noopener,noreferrer");
      } else {
        deviceCode = `${response.user_code} — ${response.verification_url}`;
        window.open(response.verification_url, "_blank", "noopener,noreferrer");
      }
      signInByProvider[id] = { status: "signing_in", account: null };
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to start provider sign-in.";
    }
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
    };
    settings.default_model ||= id;
    settings.default_provider ||= selectedProvider;
  }

  function providerModels(providerId: string): [string, ModelSettings][] {
    if (!settings) return [];
    return Object.entries(settings.models).filter(([, model]) => model.provider_id === providerId);
  }

  function isProviderCollapsed(id: string): boolean {
    return collapsedProviderIds.has(id);
  }

  function toggleProviderCollapsed(id: string): void {
    const next = new Set(collapsedProviderIds);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    collapsedProviderIds = next;
  }

  type ProviderKindDefinition = {
    id: ProviderKind;
    label: string;
    protocol: ProviderProtocol;
  };

  type ProviderKind = "OpenAi" | "Anthropic" | "Google" | "OpenRouter" | "OpenAiCompatible" | "Codex";

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

  function toggleAddProvider(): void {
    addingProvider = !addingProvider;
    if (!addingProvider) {
      compatibleDraft = null;
      compatibleBaseUrlError = null;
    }
    error = null;
  }

  function pickProviderKind(kind: ProviderKindDefinition): void {

    // OpenAI-compatible providers need a name and base URL — open the inline
    // inputs instead of creating immediately. Clicking again collapses them.
    if (kind.id === "OpenAiCompatible") {
      compatibleDraft = compatibleDraft ? null : { name: "", base_url: "" };
      compatibleBaseUrlError = null;
      error = null;
      return;
    }
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
    addingProvider = false;
    compatibleDraft = null;
    compatibleBaseUrlError = null;
    error = null;
  }

  function commitCompatibleProvider(): void {
    if (!settings || !compatibleDraft) return;
    const baseUrl = compatibleDraft.base_url.trim();
    if (!baseUrl) {
      compatibleBaseUrlError = "Enter a base URL so Swage knows where to send requests.";
      return;
    }
    if (!/^https?:\/\//i.test(baseUrl)) {
      compatibleBaseUrlError = "Enter a URL starting with http:// or https://";
      return;
    }
    const baseId = slugifyProviderName(compatibleDraft.name) || "openai-compatible";
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
    addingProvider = false;
    compatibleDraft = null;
    compatibleBaseUrlError = null;
    error = null;
  }

  function removeProvider(id: string): void {

    for (const modelId of Object.keys(settings.models)) {
      if (settings.models[modelId].provider_id === id) removeModel(modelId);
    }
    delete settings.providers[id];
    if (settings.default_provider === id) settings.default_provider = defaultModel?.provider_id ?? "";
    if (selectedProvider === id) selectedProvider = providerIds.find((providerId) => providerId !== id) ?? "";
  }

  function addManualModel(providerId: string): void {

    const modelId = (newModelIdByProvider[providerId] ?? "").trim();
    if (!modelId) {
      error = "Model ID is required.";
      return;
    }
    const id = `${providerId}:${modelId}`;
    if (settings.models[id]) {
      error = `Model '${id}' already exists.`;
      return;
    }
    settings.models[id] = {
      provider_id: providerId,
      model_id: modelId,
      aliases: [],
      context_window: null,
      generation_preset: null,
    };
    settings.default_model ||= id;
    settings.default_provider ||= providerId;
    newModelIdByProvider[providerId] = "";
    error = null;
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

      <div class="field">
        <div class="key">
          Default model
          <small>The model selects the provider. There is no separate provider default.</small>
        </div>
        <div class="val">
          {#each modelIds as id (id)}
            <button
              type="button"
              class="opt opt-with-x"
              class:active={settings.default_model === id}
              onclick={() => {
                if (settings) {
                  settings.default_model = id;
                  settings.default_provider = settings.models[id].provider_id;
                }
              }}
            >
              {id}
            </button>
          {:else}
            <p class="empty">No models configured. Add a provider, then add a model under it.</p>
          {/each}
        </div>
      </div>

      <div class="field">
        <div class="key">
          Add provider
          <small>Pick a kind. Swage generates the internal config key.</small>
        </div>
        <div class="val val-stack">
          {#if !addingProvider}
            <div class="opt-row">
              <button type="button" class="opt opt-anchor" onclick={toggleAddProvider}>
                <span class="anchor-mark" aria-hidden="true">+</span>
                <span>Add provider</span>
              </button>
            </div>
          {:else}
            <div class="opt-row add-row">
              {#each PROVIDER_KINDS as kind (kind.id)}
                <button
                  type="button"
                  class="opt opt-pick"
                  class:active={kind.id === "OpenAiCompatible" && !!compatibleDraft}
                  onclick={() => pickProviderKind(kind)}
                >
                  <span class="pick-mark" aria-hidden="true">+</span>
                  <span>{kind.label}</span>
                </button>
              {/each}
              <button type="button" class="opt opt-cancel" onclick={toggleAddProvider}>
                <span aria-hidden="true">×</span>
                <span>Cancel</span>
              </button>
            </div>
            {#if compatibleDraft}
              <!--
                OpenAI-compatible provider draft. Each field has a persistent
                visible <label> (placeholders vanish once the user types, and
                screen readers name the field only by placeholder here), a
                surviving helper line for Base URL, and a reserved role="alert"
                slot for client-side validation. UI audit f-6.
              -->
              <div class="search-row compatible-form">
                <div class="compatible-field">
                  <label class="key" for="compatible-name">Provider name</label>
                  <input
                    id="compatible-name"
                    type="text"
                    class="text search"
                    value={compatibleDraft.name}
                    oninput={(event) => {
                      if (compatibleDraft) compatibleDraft.name = event.currentTarget.value;
                    }}
                    placeholder="My OpenAI proxy"
                    autocomplete="off"
                  />
                </div>
                <div class="compatible-field">
                  <label class="key" for="compatible-base-url">Base URL</label>
                  <input
                    id="compatible-base-url"
                    type="text"
                    class="text"
                    value={compatibleDraft.base_url}
                    oninput={(event) => {
                      if (compatibleDraft) compatibleDraft.base_url = event.currentTarget.value;
                      if (compatibleBaseUrlError) compatibleBaseUrlError = null;
                    }}
                    placeholder="https://api.example.com/v1"
                    autocomplete="off"
                    aria-describedby="compatible-base-url-help compatible-base-url-error"
                  />
                  <small id="compatible-base-url-help" class="field-help">
                    Override the provider's canonical endpoint.
                  </small>
                  <p
                    id="compatible-base-url-error"
                    class="field-error"
                    role="alert"
                  >{compatibleBaseUrlError ?? ""}</p>
                </div>
                <button type="button" class="opt opt-pick" onclick={commitCompatibleProvider}>
                  <span class="pick-mark" aria-hidden="true">+</span>
                  <span>Connect</span>
                </button>
              </div>
            {/if}
          {/if}
        </div>
      </div>

      {#each providerIds as id, ix (id)}
        {@const provider = settings.providers[id]}
        {@const modelsForProvider = providerModels(id)}
        {@const isCollapsed = isProviderCollapsed(id)}
        <section class="provider-section" aria-label={`Provider ${id}`}>
          <!--
            The whole header bar is the collapse trigger — one large click
            target with a rotating chevron — instead of a low-contrast "Collapse"
            text button competing with four other text fragments in the same
            row. Remove is demoted to a low-emphasis icon button (sibling, never
            nested inside the collapse button). aria-expanded is preserved. The
            credential chip becomes a status with a check. UI audit f-22.
          -->
          <div class="provider-head">
            <button
              type="button"
              class="provider-collapse"
              aria-expanded={!isCollapsed}
              aria-label={`Provider ${providerDisplayName(id)} — ${isCollapsed ? "expand" : "collapse"}`}
              onclick={() => toggleProviderCollapsed(id)}
            >
              <span class="chevron" aria-hidden="true">{isCollapsed ? "▸" : "▾"}</span>
              <span class="provider-head-text">
                <span class="provider-index">
                  Provider <span class="ix">{String(ix + 1).padStart(2, "0")}</span>
                </span>
                <h2>{providerDisplayName(id)}</h2>
                <p class="provider-meta-line">
                  <span class="meta-protocol">{provider.protocol}</span>
                  <span class="meta-key">{id}</span>
                  <span class="meta-endpoint">{provider.base_url || "provider default endpoint"}</span>
                </p>
              </span>
              <span class="credential-status" class:set={provider.has_credential || signInByProvider[id]?.status === "signed_in"}>
                {provider.protocol === "codex"
                  ? signInByProvider[id]?.status === "signed_in" ? "✓ Signed in" : "Signed out"
                  : provider.has_credential ? "✓ Credential" : "No credential"}
              </span>
            </button>
            <button
              type="button"
              class="provider-remove"
              aria-label={`Remove provider ${providerDisplayName(id)}`}
              title="Remove provider"
              onclick={() => removeProvider(id)}
            >
              <span aria-hidden="true">×</span>
            </button>
          </div>

          {#if !isCollapsed}
            <div class="provider-body">
              {#if provider.protocol !== "codex"}
              <div class="field">
                <div class="key">
                  Base URL
                  <small>Override the provider's canonical endpoint when needed.</small>
                </div>
                <div class="val">
                  <input
                    type="text"
                    class="text"
                    bind:value={provider.base_url}
                    placeholder="Provider default"
                    autocomplete="off"
                  />
                </div>
              </div>
              <div class="field">
                <div class="key">
                  API key
                  <small>Paste a new value to store or rotate this provider's credential.</small>
                </div>
                <div class="val key-val">
                  <input
                    type="password"
                    class="text"
                    bind:value={provider.api_key}
                    placeholder={provider.has_credential ? "Stored credential" : "Paste to store"}
                    autocomplete="off"
                  />
                  <span class="credential-status" class:set={provider.has_credential}>
                    {provider.has_credential ? "Stored" : "Empty"}
                  </span>
                </div>
              </div>
              {:else}
                <div class="field">
                  <div class="key">
                    ChatGPT account
                    <small>Codex manages the browser or device-code sign-in and refreshes it automatically.</small>
                  </div>
                  <div class="val val-stack">
                    {#if signInByProvider[id]?.status === "signed_in"}
                      <span class="credential-status set">
                        Signed in{signInByProvider[id]?.account?.email ? ` as ${signInByProvider[id].account.email}` : ""}
                      </span>
                      <button type="button" class="secondary" onclick={() => endSignIn(id)}>Sign out</button>
                    {:else}
                      <div class="opt-row">
                        <button type="button" class="secondary" disabled={!savedProviderIds.has(id)} onclick={() => beginSignIn(id, "browser")}>Sign in with browser</button>
                        <button type="button" class="secondary" disabled={!savedProviderIds.has(id)} onclick={() => beginSignIn(id, "device_code")}>Use device code</button>
                        <button type="button" class="secondary" disabled={!savedProviderIds.has(id)} onclick={() => loadSignInState(id)}>Refresh state</button>
                      </div>
                      {#if !savedProviderIds.has(id)}<small>Save settings before signing in.</small>{/if}
                      {#if deviceCode}<code>{deviceCode}</code>{/if}
                    {/if}
                  </div>
                </div>
              {/if}

              <div class="field">
                <div class="key">
                  Models
                  <small>Configured model IDs for this provider. The active default is marked.</small>
                </div>
                <div class="val val-stack">
                  {#each modelsForProvider as [modelKey, model] (modelKey)}
                    <div class="model-row provider-model-row">
                      <div class="model-main">
                        <span class="model-id">{model.model_id}</span>
                        <span class="model-sub">{modelKey}</span>
                      </div>
                      <div class="model-facts">
                        <span>{model.context_window ? `${model.context_window.toLocaleString()} ctx` : "context unknown"}</span>
                        <span>{model.generation_preset ?? "provider generation defaults"}</span>
                        {#if settings.default_model === modelKey}
                          <span class="fact-strong">Default</span>
                        {/if}
                      </div>
                      <div class="opt-row row-actions">
                        <button
                          type="button"
                          class="opt"
                          class:active={settings.default_model === modelKey}
                          onclick={() => {
                            if (!settings) return;
                            settings.default_model = modelKey;
                            settings.default_provider = id;
                          }}
                        >
                          Use
                        </button>
                        <button type="button" class="opt opt-warn" onclick={() => removeModel(modelKey)}>Remove</button>
                      </div>
                    </div>
                  {:else}
                    <p class="empty">No models configured for this provider.</p>
                  {/each}

                  <div class="search-row">
                    <input
                      type="text"
                      class="text search"
                      value={newModelIdByProvider[id] ?? ""}
                      oninput={(event) => (newModelIdByProvider[id] = event.currentTarget.value)}
                      placeholder="model-id"
                      aria-label={`Add model by id for ${providerDisplayName(id)}`}
                      autocomplete="off"
                    />
                    <button type="button" class="opt opt-pick" onclick={() => addManualModel(id)}>
                      <span class="pick-mark" aria-hidden="true">+</span>
                      <span>Add model</span>
                    </button>
                  </div>
                </div>
              </div>

              <div class="field">
                <div class="key">
                  Discover
                  <small>Fetch known models for this provider, then add the ones Swage should use.</small>
                </div>
                <div class="val val-stack">
                  <div class="search-row">
                    <input
                      type="text"
                      class="text search"
                      value={selectedProvider === id ? modelQuery : ""}
                      onfocus={() => selectProvider(id)}
                      oninput={(event) => {
                        selectedProvider = id;
                        modelQuery = event.currentTarget.value;
                        loadModels();
                      }}
                      placeholder="Search models"
                      autocomplete="off"
                    />
                    <button
                      type="button"
                      class="opt"
                      disabled={isPersisting}
                      onclick={() => {
                        selectedProvider = id;
                        loadModels();
                      }}
                    >
                      Refresh
                    </button>
                  </div>
                  {#if selectedProvider === id}
                    <div class="opt-grid">
                      {#each discoveredModels as model (model.id)}
                        {@const added = addedModelIds.has(model.id)}
                        <button
                          type="button"
                          class="opt"
                          class:active={added}
                          disabled={added}
                          onclick={() => addDiscoveredModel(model)}
                          title={added ? "Already added" : `Add ${model.name}`}
                        >
                          <span class="opt-label">{model.name}</span>
                          <span class="opt-tag">{added ? "Added" : "Add"}</span>
                        </button>
                      {:else}
                        <p class="empty">No discovered models to show.</p>
                      {/each}
                    </div>
                  {/if}
                </div>
              </div>
            </div>
          {/if}
        </section>
      {:else}
        <div class="field">
          <div class="key">
            No providers
            <small>Swage will start without messaging until a model/provider pair exists.</small>
          </div>
          <div class="val">
            <p class="empty">Add a provider above. Nothing is assumed.</p>
          </div>
        </div>
      {/each}

<style>
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

  .val-stack {
    flex-direction: column;
    align-items: stretch;
    gap: 12px;
  }

  .key-val {
    align-items: center;
  }

  /* Chip — bordered Bebas pill. Active chip flips to ink with the offset
     orange shadow underneath (the riso "second pass"). */
  .opt {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    padding: 6px 12px;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 14px;
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    cursor: pointer;
    isolation: isolate;
  }

  .opt:hover:not(:disabled):not(.active) {
    background: rgba(14, 59, 102, 0.08);
  }

  .opt.active {
    background: var(--ink);
    color: var(--paper);
  }

  .opt.active::before {
    content: "";
    position: absolute;
    inset: 0;
    background: var(--accent);
    transform: translate(2px, 2px);
    z-index: -1;
    mix-blend-mode: var(--shadow-blend);
    opacity: var(--shadow-opacity);
    pointer-events: none;
  }

  .opt:disabled {
    cursor: default;
    opacity: 0.85;
  }

  .opt-warn {
    border-color: var(--accent);
    color: var(--accent);
  }

  .opt-warn:hover:not(:disabled) {
    background: var(--accent);
    color: var(--paper);
  }

  /* Anchor — the "Add provider" entry. A ticket-stub chip: a printer's
     mark on the left in mono, the action label in display on the right,
     divided by a hairline. Reads as a deliberate stamp rather than a
     dashed afterthought. The riso shadow shows on hover. */
  .opt-anchor {
    padding: 7px 16px 7px 7px;
    gap: 12px;
    border-width: 1.5px;
  }

  .opt-anchor .anchor-mark {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    margin: -1px 0;
    border: 1px solid currentColor;
    color: var(--ink-orange);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 14px;
    line-height: 1;
  }

  .opt-anchor:hover:not(:disabled):not(.active) {
    background: var(--paper);
  }

  /* Riso second-pass shadow on hover — proper blended offset rather than
     a solid box-shadow, so it composes with the page like the rest of
     the riso surfaces. */
  .opt-anchor:hover:not(:disabled):not(.active)::before {
    content: "";
    position: absolute;
    inset: 0;
    background: var(--accent);
    transform: translate(var(--offset), var(--offset));
    z-index: -1;
    mix-blend-mode: var(--shadow-blend);
    opacity: var(--shadow-opacity);
    pointer-events: none;
  }

  .opt-anchor:hover:not(:disabled) .anchor-mark {
    color: var(--accent);
  }

  /* Pick chip — mono "+" prefix as its own glyph cell. Used by the kind
     chips, the inline Connect, and the per-provider Add model. The mark
     is mono so it sits visually beneath the Bebas label. */
  .opt-pick {
    gap: 8px;
    padding-left: 10px;
  }

  .opt-pick .pick-mark {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 13px;
    line-height: 1;
    color: var(--ink-orange);
    letter-spacing: 0;
  }

  .opt-pick.active .pick-mark,
  .opt-pick:hover:not(:disabled):not(.active) .pick-mark {
    color: inherit;
  }

  /* Quiet cancel — outline removed, sits beside the chip row as a soft
     dismiss. Hover surfaces the orange so the affordance still reads. */
  .opt-cancel {
    gap: 6px;
    border-color: transparent;
    color: var(--ink-soft);
    padding-left: 10px;
    padding-right: 10px;
  }

  .opt-cancel:hover:not(:disabled) {
    background: transparent;
    color: var(--ink-orange);
  }

  /* Stagger reveal — when the kind row expands, chips bloom left-to-right.
     Each :nth-child carries its own delay; the cancel sits at the end. */
  .add-row > .opt {
    animation: chip-in 220ms cubic-bezier(0.4, 0, 0.2, 1) backwards;
  }

  .add-row > .opt:nth-child(1) { animation-delay: 0ms; }
  .add-row > .opt:nth-child(2) { animation-delay: 35ms; }
  .add-row > .opt:nth-child(3) { animation-delay: 70ms; }
  .add-row > .opt:nth-child(4) { animation-delay: 105ms; }
  .add-row > .opt:nth-child(5) { animation-delay: 140ms; }
  .add-row > .opt:nth-child(6) { animation-delay: 200ms; }

  @keyframes chip-in {
    from {
      opacity: 0;
      transform: translateX(-6px);
    }
    to {
      opacity: 1;
      transform: translateX(0);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .add-row > .opt {
      animation: none;
    }
  }

  .opt-tag {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 9px;
    letter-spacing: 0.12em;
    padding: 2px 6px;
    border: 1px solid currentColor;
  }

  .opt.active .opt-tag {
    border-color: var(--paper);
  }

  /* Inputs — flat 1px border, paper background, body face. */
  .text {
    width: 100%;
    max-width: 480px;
    padding: 9px 11px;
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-body);
    font-size: 13px;
    line-height: 1.4;
  }

  .text.search {
    max-width: 320px;
  }

  .text:focus {
    outline: none;
    background: rgba(240, 106, 58, 0.06);
  }

  .credential-status {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    padding: 3px 8px;
    border: 1px solid var(--ink-soft);
    color: var(--ink-soft);
  }

  .credential-status.set {
    border-color: var(--accent);
    color: var(--accent);
  }

  .search-row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .opt-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 8px;
    max-height: 280px;
    overflow: auto;
    padding-right: 4px;
  }

  .opt-grid .opt {
    justify-content: space-between;
  }

  /* Empty caption — print stamp typography, framed by hairline rules.
     Replaces the bland italic gray default; a quiet but composed
     "field unused" mark that fits the riso vocabulary. */
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

  .provider-section {
    border-top: 2px solid var(--ink);
    padding-top: 18px;
    position: relative;
  }

  /* Registration crosshair — small print mark hung off the section's
     top-left. A wink at riso/print, never required for affordance. */
  .provider-section::before {
    content: "+";
    position: absolute;
    top: -10px;
    left: -2px;
    width: 14px;
    height: 14px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--paper);
    color: var(--ink-orange);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 14px;
    line-height: 1;
  }

  .provider-head {
    display: flex;
    align-items: stretch;
    gap: 4px;
    padding-bottom: 12px;
  }

  /* The collapse trigger occupies the whole header bar — one large click
     target. Borders, background, and the rotating chevron carry the
     affordance; aria-expanded announces state (UI audit f-22). */
  .provider-collapse {
    flex: 1;
    min-width: 0;
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 12px;
    background: transparent;
    border: none;
    padding: 6px 4px;
    margin: 0;
    text-align: left;
    font: inherit;
    color: inherit;
    cursor: pointer;
    border-radius: 0;
  }

  .provider-collapse:hover {
    background: var(--rule-faint);
  }

  .provider-collapse:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .chevron {
    font-family: var(--font-mono);
    font-size: 14px;
    line-height: 1;
    color: var(--ink-soft);
    transition: transform 120ms ease;
  }

  .provider-head-text {
    min-width: 0;
  }

  /* Credential chip promoted to a status — the set variant carries a check and
     a positive tint so it reads as a state, not a label. */
  .provider-collapse .credential-status {
    justify-self: end;
  }

  .credential-status.set {
    border-color: var(--ink);
    color: var(--ink);
    background: rgba(47, 122, 58, 0.14);
  }

  /* Remove is demoted to a low-emphasis icon button — sibling of the collapse
     trigger, never nested (UI audit f-22). */
  .provider-remove {
    flex-shrink: 0;
    align-self: center;
    background: transparent;
    border: 1px solid transparent;
    color: var(--ink-soft);
    font-size: 16px;
    line-height: 1;
    padding: 4px 8px;
    cursor: pointer;
    opacity: 0.6;
    transition: opacity 120ms ease, color 120ms ease, border-color 120ms ease;
  }

  .provider-remove:hover,
  .provider-remove:focus-visible {
    opacity: 1;
    color: var(--error);
    border-color: var(--rule-soft);
  }

  .provider-remove:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .provider-index {
    display: block;
    margin-bottom: 6px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    line-height: 1;
    letter-spacing: 0.22em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .provider-index .ix {
    color: var(--ink-orange);
  }

  .provider-head h2 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: var(--font-display-3);
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .provider-meta-line {
    margin: 8px 0 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    font-family: var(--font-mono);
    font-size: var(--font-label-sm);
    color: var(--ink-soft);
    word-break: break-word;
  }

  .provider-meta-line span {
    display: inline-flex;
    align-items: center;
    gap: 14px;
  }

  .provider-meta-line span + span::before {
    content: "·";
    color: var(--rule-soft);
  }

  .provider-meta-line .meta-protocol {
    color: var(--ink);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  /* Compatible-provider draft form — persistent visible labels, helper text,
     and an inline validation slot live in their own column structure so the
     search-row layout (input + Connect button on one line) still works. */
  .compatible-form {
    flex-wrap: wrap;
  }

  .compatible-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 220px;
    flex: 1;
  }

  .compatible-field .key {
    /* Reuse the field-key look so the compatible form matches the per-provider
       field rows below it. */
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .field-help {
    font-family: var(--font-body);
    font-size: var(--font-label-sm);
    color: var(--ink-soft);
  }

  .field-error {
    margin: 0;
    font-family: var(--font-mono);
    font-size: var(--font-label-sm);
    color: var(--error);
    min-height: 0;
  }

  .field-error:empty {
    display: none;
  }

  /* Per-model row inside Model compatibility — id label on the left, chip
     row of presets on the right. Wraps cleanly on narrow viewports. */
  .model-row {
    display: grid;
    grid-template-columns: minmax(160px, 240px) 1fr;
    gap: 14px;
    align-items: center;
    padding: 8px 0;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .model-row:last-child {
    border-bottom: none;
  }

  .model-id {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--ink);
    word-break: break-all;
  }

  .provider-model-row {
    grid-template-columns: minmax(180px, 1fr) minmax(180px, 1fr) auto;
    align-items: start;
  }

  .model-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .model-sub,
  .model-facts {
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.4;
    color: var(--ink-soft);
  }

  .model-facts {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .model-facts span {
    padding: 2px 6px;
    border: 1px solid var(--rule-soft);
  }

  .model-facts .fact-strong {
    border-color: var(--accent);
    color: var(--accent);
  }

  .row-actions {
    justify-content: flex-end;
  }

  .opt-row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  @media (max-width: 760px) {
    .field {
      grid-template-columns: 1fr;
      gap: 10px;
    }

    .model-row {
      grid-template-columns: 1fr;
    }
  }
</style>
