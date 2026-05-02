<!--
  Settings body pane.

  Renders the inner pane of the settings window. The window chrome (topbar
  with Settings label + × CLOSE, and the parked nav with the page list) lives
  in AppLayout — this component only owns the .settings-pane content per the
  riso-cyanotype design (Initiatives/Active/Swage/UI/riso-cyanotype.html,
  Settings — display).

  Sub-pages are driven by the route. AppLayout renders the page list and
  navigates between /settings/providers, /settings/generation, and
  /settings/automation; this component just reads the tab and shows the
  matching field set.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { discoverModels, fetchSettings, restartGateway, saveSettings } from "../api/settings";
  import type { DiscoveredModel, GenerationPresetConfig, ProviderProtocol, SettingsResponse } from "../types";

  interface Props {
    tab?: "providers" | "generation" | "automation";
  }

  let { tab = "providers" }: Props = $props();

  // settings is the working copy — bind:value mutates it freely. saveSettings
  // returns a fresh server snapshot which we replace it with. Null until the
  // initial fetch resolves.
  let settings = $state<SettingsResponse | null>(null);
  let selectedProvider = $state("");
  let selectedPreset = $state("");
  let modelQuery = $state("");
  let discoveredModels = $state<DiscoveredModel[]>([]);
  let status = $state("");
  let error = $state<string | null>(null);

  const PROTOCOLS: ProviderProtocol[] = ["Anthropic", "OpenAi", "OpenRouter", "Google"];

  const providerIds = $derived(Object.keys(settings?.providers ?? {}));
  const modelIds = $derived(Object.keys(settings?.models ?? {}));
  const presetIds = $derived(Object.keys(settings?.generation_presets ?? {}));
  const provider = $derived(settings && selectedProvider ? settings.providers[selectedProvider] : null);
  const currentPreset = $derived(settings && selectedPreset ? settings.generation_presets[selectedPreset] : null);
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

  onMount(async () => {
    try {
      settings = await fetchSettings();
      selectedProvider = settings.default_provider || providerIds[0] || "";
      selectedPreset = presetIds[0] ?? "";
      if (selectedProvider) await loadModels();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load settings.";
    }
  });

  async function loadModels(): Promise<void> {
    if (!selectedProvider) return;
    try {
      const response = await discoverModels(selectedProvider, modelQuery);
      discoveredModels = response.models;
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
  }

  function addDiscoveredModel(model: DiscoveredModel): void {
    if (!settings) return;
    const id = `${selectedProvider}:${model.id}`;
    if (settings.models[id]) return; // Already added — chip shows ADDED.
    settings.models[id] = {
      provider_id: selectedProvider,
      model_id: model.id,
      aliases: [],
      context_window: model.context_window,
      generation_preset: null,
    };
  }

  function removeModel(id: string): void {
    if (!settings) return;
    delete settings.models[id];
    if (settings.default_model === id) settings.default_model = modelIds[0] ?? "";
  }

  function addPreset(): void {
    if (!settings) return;
    let index = presetIds.length + 1;
    let id = `preset-${index}`;
    while (settings.generation_presets[id]) {
      index += 1;
      id = `preset-${index}`;
    }
    settings.generation_presets[id] = { max_tokens: 4096, stop_sequences: [] };
    selectedPreset = id;
  }

  function duplicatePreset(): void {
    if (!settings || !currentPreset) return;
    const id = `${selectedPreset}-copy`;
    settings.generation_presets[id] = structuredClone(currentPreset);
    selectedPreset = id;
  }

  function deletePreset(): void {
    if (!settings || !selectedPreset) return;
    // A preset removal must clear references on models, otherwise saving
    // would fail validation against an unknown preset id.
    for (const model of Object.values(settings.models)) {
      if (model.generation_preset === selectedPreset) model.generation_preset = null;
    }
    delete settings.generation_presets[selectedPreset];
    selectedPreset = Object.keys(settings.generation_presets)[0] ?? "";
  }

  function presetIsEmpty(preset: GenerationPresetConfig): boolean {
    return !preset.max_tokens && preset.temperature == null && preset.stop_sequences.length === 0 && !preset.metadata;
  }

  // <input type="number"> coerces to "" when emptied — Svelte's bind:value
  // gives us NaN. Coerce back to null so the JSON payload matches the schema.
  function nullableNumber(value: number | null | undefined): number | null {
    if (value === null || value === undefined) return null;
    return Number.isFinite(value) ? value : null;
  }

  async function persist(): Promise<void> {
    if (!settings) return;
    error = null;
    status = "";
    for (const [id, preset] of Object.entries(settings.generation_presets)) {
      if (!id.trim() || presetIsEmpty(preset)) {
        error = "Generation presets need a name and at least one parameter.";
        return;
      }
      // Normalise NaN-from-empty-input to null so the wire format is valid.
      preset.max_tokens = nullableNumber(preset.max_tokens);
      preset.temperature = nullableNumber(preset.temperature);
    }
    try {
      settings = await saveSettings(settings);
      // The server never echoes raw credentials; clear the input so the
      // password field returns to the "Stored credential" placeholder.
      for (const provider of Object.values(settings.providers)) provider.api_key = "";
      status = settings.can_restart
        ? "Saved. Restart the managed gateway to apply changes."
        : "Saved. Restart the gateway manually to apply changes.";
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to save settings.";
    }
  }

  async function restart(): Promise<void> {
    try {
      const response = await restartGateway();
      status = response.guidance;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to restart gateway.";
    }
  }

  const TAB_TITLES: Record<NonNullable<Props["tab"]>, { title: string; sub: string }> = {
    providers: {
      title: "Providers",
      sub: "Connection settings for the LLM gateways Swage talks to. Credentials are stored on the server and never returned over the wire.",
    },
    generation: {
      title: "Generation",
      sub: "Reusable parameter sets for sampling — temperature, max tokens, stop sequences. Each model can pick a preset or fall back to the provider default.",
    },
    automation: {
      title: "Automation",
      sub: "Lifecycle hooks and runtime triggers loaded by the gateway at startup.",
    },
  };
</script>

<div class="settings-pane" aria-label="Settings">
  {#if !settings}
    <p class="loading">Loading settings…</p>
  {:else}
    {@const heading = TAB_TITLES[tab]}
    <div class="head">
      <div class="head-text">
        <p class="eyebrow">Settings</p>
        <h1 class="title">{heading.title}</h1>
        <p class="sub">{heading.sub}</p>
      </div>
      <div class="head-actions">
        <button type="button" class="btn primary" onclick={persist}>Save</button>
      </div>
    </div>

    {#if error}
      <div class="notice error" role="alert">
        <span>{error}</span>
        <button type="button" class="notice-dismiss" onclick={() => (error = null)} aria-label="Dismiss">×</button>
      </div>
    {/if}

    {#if status}
      <div class="notice">
        <span>{status}</span>
        {#if settings.can_restart}
          <button type="button" class="notice-action" onclick={restart}>Restart</button>
        {/if}
      </div>
    {:else if settings.restart_required}
      <div class="notice">
        <span>Saved settings require a gateway restart to take effect.</span>
        {#if settings.can_restart}
          <button type="button" class="notice-action" onclick={restart}>Restart</button>
        {/if}
      </div>
    {/if}

    {#if tab === "providers"}
      <div class="field">
        <div class="key">
          Default provider
          <small>Picked when a request doesn't name one explicitly.</small>
        </div>
        <div class="val">
          {#each providerIds as id (id)}
            <button
              type="button"
              class="opt"
              class:active={settings.default_provider === id}
              onclick={() => {
                if (settings) settings.default_provider = id;
              }}
            >
              {id}
            </button>
          {/each}
        </div>
      </div>

      <div class="field">
        <div class="key">
          Provider
          <small>Choose which provider's settings to edit below.</small>
        </div>
        <div class="val">
          {#each providerIds as id (id)}
            <button
              type="button"
              class="opt"
              class:active={selectedProvider === id}
              onclick={() => selectProvider(id)}
            >
              {id}
            </button>
          {/each}
        </div>
      </div>

      {#if provider}
        <div class="field">
          <div class="key">
            Protocol
            <small>Wire format Swage uses to talk to this provider.</small>
          </div>
          <div class="val">
            {#each PROTOCOLS as proto (proto)}
              <button
                type="button"
                class="opt"
                class:active={provider.protocol === proto}
                onclick={() => (provider.protocol = proto)}
              >
                {proto}
              </button>
            {/each}
          </div>
        </div>

        <div class="field">
          <div class="key">
            Base URL
            <small>Override the provider's default endpoint. Leave blank to use the canonical URL.</small>
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
            <small>Stored server-side and never returned. Paste a new value to replace what's there.</small>
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

        <div class="field">
          <div class="key">
            Discovered models
            <small>Live list pulled from the provider. Click to add — added models become available across Swage.</small>
          </div>
          <div class="val val-stack">
            <div class="search-row">
              <input
                type="text"
                class="text search"
                bind:value={modelQuery}
                oninput={loadModels}
                placeholder="Search…"
                autocomplete="off"
              />
              <button type="button" class="btn" onclick={loadModels}>Refresh</button>
            </div>
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
                <p class="empty">
                  No models discovered. {provider.has_credential ? "Try refreshing." : "Set an API key first."}
                </p>
              {/each}
            </div>
          </div>
        </div>

        {#if modelIds.length > 0}
          <div class="field">
            <div class="key">
              Default model
              <small>Used when a request doesn't pin a model.</small>
            </div>
            <div class="val">
              {#each modelIds as id (id)}
                <button
                  type="button"
                  class="opt opt-with-x"
                  class:active={settings.default_model === id}
                  onclick={() => {
                    if (settings) settings.default_model = id;
                  }}
                >
                  <span class="opt-label">{id}</span>
                  <span
                    class="opt-x"
                    role="button"
                    tabindex="0"
                    aria-label={`Remove ${id}`}
                    title="Remove model"
                    onclick={(event) => {
                      event.stopPropagation();
                      removeModel(id);
                    }}
                    onkeydown={(event) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        event.stopPropagation();
                        removeModel(id);
                      }
                    }}
                  >×</span>
                </button>
              {/each}
            </div>
          </div>
        {/if}
      {/if}
    {:else if tab === "generation"}
      <div class="field">
        <div class="key">
          Preset
          <small>Pick a preset to edit, or create a new one.</small>
        </div>
        <div class="val">
          {#each presetIds as id (id)}
            <button
              type="button"
              class="opt"
              class:active={selectedPreset === id}
              onclick={() => (selectedPreset = id)}
            >
              {id}
            </button>
          {/each}
          <button type="button" class="opt opt-add" onclick={addPreset}>+ New</button>
          {#if currentPreset}
            <button type="button" class="opt" onclick={duplicatePreset}>Duplicate</button>
            <button type="button" class="opt opt-warn" onclick={deletePreset}>Delete</button>
          {/if}
        </div>
      </div>

      {#if currentPreset}
        <div class="field">
          <div class="key">
            Max tokens
            <small>Upper bound on generated output length.</small>
          </div>
          <div class="val">
            <input
              type="number"
              class="text small"
              bind:value={currentPreset.max_tokens}
              min="1"
              placeholder="—"
            />
          </div>
        </div>

        <div class="field">
          <div class="key">
            Temperature
            <small>0 is deterministic, 2 is most varied. Provider default if blank.</small>
          </div>
          <div class="val">
            <input
              type="number"
              class="text small"
              bind:value={currentPreset.temperature}
              min="0"
              max="2"
              step="0.1"
              placeholder="—"
            />
          </div>
        </div>

        <div class="field">
          <div class="key">
            Stop sequences
            <small>One per line. Generation halts as soon as any sequence is produced.</small>
          </div>
          <div class="val">
            <textarea
              class="text"
              rows="4"
              value={currentPreset.stop_sequences.join("\n")}
              oninput={(event) => {
                if (!currentPreset) return;
                currentPreset.stop_sequences = (event.currentTarget as HTMLTextAreaElement).value
                  .split("\n")
                  .filter(Boolean);
              }}
              placeholder="One stop sequence per line"
            ></textarea>
          </div>
        </div>
      {:else}
        <div class="field">
          <div class="key">
            No preset selected
            <small>Pick a preset above, or create one.</small>
          </div>
          <div class="val">
            <p class="empty">Models without a preset use the provider's defaults.</p>
          </div>
        </div>
      {/if}

      {#if modelIds.length > 0}
        <div class="field">
          <div class="key">
            Model compatibility
            <small>Bind a preset to each model. None means the provider default.</small>
          </div>
          <div class="val val-stack">
            {#each modelIds as id (id)}
              <div class="model-row">
                <span class="model-id">{id}</span>
                <div class="opt-row">
                  <button
                    type="button"
                    class="opt"
                    class:active={!settings.models[id].generation_preset}
                    onclick={() => {
                      if (settings) settings.models[id].generation_preset = null;
                    }}
                  >
                    None
                  </button>
                  {#each presetIds as presetId (presetId)}
                    <button
                      type="button"
                      class="opt"
                      class:active={settings.models[id].generation_preset === presetId}
                      onclick={() => {
                        if (settings) settings.models[id].generation_preset = presetId;
                      }}
                    >
                      {presetId}
                    </button>
                  {/each}
                </div>
              </div>
            {/each}
          </div>
        </div>
      {/if}
    {:else}
      <div class="field">
        <div class="key">
          Lifecycle hooks
          <small>Loaded by the gateway at startup. A runtime registry view will land alongside the hook authoring UI.</small>
        </div>
        <div class="val">
          <span class="opt active">Startup-loaded</span>
        </div>
      </div>

      <div class="field">
        <div class="key">
          Restart required
          <small>Set when configuration changes haven't yet been applied to the running gateway.</small>
        </div>
        <div class="val">
          <span class="opt" class:active={settings.restart_required}>
            {settings.restart_required ? "Yes" : "No"}
          </span>
        </div>
      </div>
    {/if}
  {/if}
</div>

<style>
  /* ---------------------------------------------------------------------------
     Settings pane — body content for the riso settings shell.
     Mirrors .rs .settings-pane in
     Documents/Notes/Other Notes/Initiatives/Active/Swage/UI/riso-cyanotype.html.
     --------------------------------------------------------------------------- */

  .settings-pane {
    height: 100%;
    padding: 28px 36px;
    display: flex;
    flex-direction: column;
    gap: 18px;
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-body);
    overflow: auto;
    isolation: isolate;
  }

  .loading {
    margin: auto;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  /* Page heading + Save action — display title sits next to the primary CTA. */
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    gap: 24px;
    flex-wrap: wrap;
  }

  .head-text {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 60ch;
  }

  .eyebrow {
    margin: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .title {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 36px;
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .sub {
    margin: 0;
    font-size: 13px;
    color: var(--ink-soft);
  }

  .head-actions {
    display: flex;
    gap: 8px;
  }

  /* Notices — flat orange band with cream text, matches the not-found banner
     in App.svelte so platform-level messages share one visual register. */
  .notice {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 10px 14px;
    background: var(--accent);
    color: var(--paper);
    border: 2px solid var(--ink);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1.4;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .notice.error {
    /* Same band, but tagged as an alert by the role attribute. The riso
       palette only carries one accent — staying on it keeps the print
       conceit honest rather than introducing a third ink for errors. */
    background: var(--ink);
    color: var(--paper);
    border-color: var(--accent);
  }

  .notice-action,
  .notice-dismiss {
    background: var(--paper);
    color: var(--ink);
    border: 1px solid var(--paper);
    padding: 4px 10px;
    font-family: var(--font-display);
    font-size: 12px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .notice-dismiss {
    padding: 0 8px;
    font-family: var(--font-mono);
    font-size: 14px;
  }

  /* Field row — 220px label column + value column, hairline separator above. */
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

  .opt-add {
    border-style: dashed;
    color: var(--accent);
  }

  .opt-warn {
    border-color: var(--accent);
    color: var(--accent);
  }

  .opt-warn:hover:not(:disabled) {
    background: var(--accent);
    color: var(--paper);
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

  .opt-with-x {
    padding-right: 6px;
  }

  .opt-x {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    margin-left: 6px;
    border: 1px solid currentColor;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1;
    cursor: pointer;
    user-select: none;
  }

  .opt-x:hover {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
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

  .text.small {
    max-width: 160px;
  }

  .text.search {
    max-width: 320px;
  }

  .text:focus {
    outline: none;
    background: rgba(240, 106, 58, 0.06);
  }

  textarea.text {
    resize: vertical;
    min-height: 90px;
    font-family: var(--font-mono);
    font-size: 12px;
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

  .empty {
    margin: 0;
    font-style: italic;
    color: var(--ink-soft);
    font-size: 12.5px;
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

  .opt-row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  /* Action buttons — riso flat-rect, primary fills with fluoro orange. */
  .btn {
    background: var(--paper);
    color: var(--ink);
    border: 1px solid var(--ink);
    padding: 8px 16px;
    font-family: var(--font-display);
    font-size: 14px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .btn:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
  }

  .btn.primary {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
  }

  .btn.primary:hover:not(:disabled) {
    background: var(--ink);
    border-color: var(--ink);
  }

  .btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  @media (max-width: 760px) {
    .settings-pane {
      padding: 20px 18px;
    }

    .field {
      grid-template-columns: 1fr;
      gap: 10px;
    }

    .model-row {
      grid-template-columns: 1fr;
    }
  }
</style>
