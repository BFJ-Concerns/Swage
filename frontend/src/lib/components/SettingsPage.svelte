<!--
  Settings body pane.

  Renders the inner pane of the settings window. The window chrome (topbar
  with Settings label + × CLOSE, and the parked nav with the page list) lives
  in AppLayout; this component only owns the .settings-pane content.

  Sub-pages are driven by the route. AppLayout renders the page list and
  navigates between /settings/providers, /settings/generation, and
  /settings/automation; this component just reads the tab and shows the
  matching field set.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { discoverModels, fetchSettings, restartGateway, saveSettings } from "../api/settings";
  import { downloadRecoveryExport, exportRecoveryBundle, listRecoveryExports } from "../api/workflows";
  import type { DiscoveredModel, GenerationPresetConfig, ModelSettings, ProviderProtocol, RecoveryExportSummary, RequireApprovalEntry, SettingsResponse } from "../types";

  // Pages added per the Riso workup (`riso-cyanotype.html`, view 06) — only
  // providers, generation, automation, and recovery have working backends. The
  // rest are shells; see the per-page motes below.
  type SettingsTabId =
    | "providers"
    | "generation"
    | "automation"
    | "plugins"
    | "channels"
    | "nodes"
    | "voice"
    | "recovery"
    | "display"
    | "account";

  interface Props {
    tab?: SettingsTabId;
  }

  let { tab = "providers" }: Props = $props();

  // settings is the working copy — bind:value mutates it freely. saveSettings
  // returns a fresh server snapshot which we replace it with. Null until the
  // initial fetch resolves.
  let settings = $state<SettingsResponse | null>(null);
  let selectedProvider = $state("");
  let selectedPreset = $state("");
  let modelQuery = $state("");
  // Add-provider flow is a two-stage chip expansion. addingProvider toggles
  // the kind row; compatibleDraft holds the OpenAI-compatible name/URL while
  // the inline inputs are open (null otherwise).
  let addingProvider = $state(false);
  let compatibleDraft = $state<{ name: string; base_url: string } | null>(null);
  let newModelIdByProvider = $state<Record<string, string>>({});
  let discoveredModels = $state<DiscoveredModel[]>([]);
  let savedProviderIds = $state<Set<string>>(new Set());
  let isPersisting = $state(false);
  let hasInvalidBundlesJson = $state(false);
  let hasInvalidApprovalJson = $state(false);
  let recoveryExports = $state<RecoveryExportSummary[]>([]);
  let isLoadingRecoveryExports = $state(false);
  let activeRecoveryExportId = $state<string | null>(null);
  let status = $state("");
  let error = $state<string | null>(null);

  const PROVIDER_KINDS = [
    { id: "OpenAi", label: "OpenAI", protocol: "openai" },
    { id: "Anthropic", label: "Anthropic", protocol: "anthropic" },
    { id: "Google", label: "Google", protocol: "google" },
    { id: "OpenRouter", label: "OpenRouter", protocol: "openrouter" },
    { id: "OpenAiCompatible", label: "OpenAI compatible", protocol: "openai" },
  ] as const satisfies readonly ProviderKindDefinition[];

  const providerIds = $derived(Object.keys(settings?.providers ?? {}));
  const modelIds = $derived(Object.keys(settings?.models ?? {}));
  const presetIds = $derived(Object.keys(settings?.generation_presets ?? {}));
  const currentPreset = $derived(settings && selectedPreset ? settings.generation_presets[selectedPreset] : null);
  const defaultModel = $derived(settings?.models[settings.default_model] ?? null);
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
      savedProviderIds = new Set(Object.keys(settings.providers));
      selectedProvider = settings.default_provider || providerIds[0] || "";
      selectedPreset = presetIds[0] ?? "";
      if (selectedProvider) await loadModels();
      if (tab === "recovery") await loadRecoveryExports();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load settings.";
    }
  });

  async function loadRecoveryExports(): Promise<void> {
    isLoadingRecoveryExports = true;
    try {
      recoveryExports = (await listRecoveryExports()).exports;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load recovery exports.";
    } finally {
      isLoadingRecoveryExports = false;
    }
  }

  async function createRecoveryExport(): Promise<void> {
    activeRecoveryExportId = "new";
    try {
      await exportRecoveryBundle();
      await loadRecoveryExports();
      status = "Recovery export captured.";
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to create recovery export.";
    } finally {
      activeRecoveryExportId = null;
    }
  }

  async function downloadRecoveryBundle(exportId: string): Promise<void> {
    activeRecoveryExportId = exportId;
    try {
      const bundle = await downloadRecoveryExport(exportId);
      const json = JSON.stringify(bundle, null, 2);
      const filename = `swage-recovery-${exportId}.json`;
      const blob = new Blob([json], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      link.click();
      URL.revokeObjectURL(url);
      status = `Downloaded ${filename}.`;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to download recovery export.";
    } finally {
      activeRecoveryExportId = null;
    }
  }

  function recoveryCountSummary(recoveryExport: RecoveryExportSummary): string {
    const counts = [
      ["benches", recoveryExport.benches],
      ["workspaces", recoveryExport.workspaces],
      ["sessions", recoveryExport.sessions],
      ["secrets", recoveryExport.secrets],
      ["channels", recoveryExport.channel_links],
      ["nodes", recoveryExport.execution_hosts],
      ["jobs", recoveryExport.scheduled_jobs],
      ["heartbeats", recoveryExport.heartbeats],
    ];
    return counts.map(([label, value]) => `${label}: ${value}`).join(" · ");
  }

  function formatRecoverySize(sizeBytes: number): string {
    if (sizeBytes < 1024) return `${sizeBytes} B`;
    const kib = sizeBytes / 1024;
    if (kib < 1024) return `${kib.toFixed(1)} KiB`;
    return `${(kib / 1024).toFixed(1)} MiB`;
  }

  async function loadModels(): Promise<void> {
    if (!settings || !selectedProvider) return;
    const provider = settings.providers[selectedProvider];
    if (!provider) return;
    const discoveryDraft = savedProviderIds.has(selectedProvider)
      ? undefined
      : {
          protocol: provider.protocol,
          base_url: provider.base_url,
          api_key_secret_ref: provider.api_key_secret_ref,
          api_key_env: provider.api_key_env,
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
    settings.default_model ||= id;
    settings.default_provider ||= selectedProvider;
  }

  function providerModels(providerId: string): [string, ModelSettings][] {
    if (!settings) return [];
    return Object.entries(settings.models).filter(([, model]) => model.provider_id === providerId);
  }

  type ProviderKindDefinition = {
    id: ProviderKind;
    label: string;
    protocol: ProviderProtocol;
  };

  type ProviderKind = "OpenAi" | "Anthropic" | "Google" | "OpenRouter" | "OpenAiCompatible";

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
    if (!addingProvider) compatibleDraft = null;
    error = null;
  }

  function pickProviderKind(kind: ProviderKindDefinition): void {
    if (!settings) return;
    // OpenAI-compatible providers need a name and base URL — open the inline
    // inputs instead of creating immediately. Clicking again collapses them.
    if (kind.id === "OpenAiCompatible") {
      compatibleDraft = compatibleDraft ? null : { name: "", base_url: "" };
      error = null;
      return;
    }
    const id = nextProviderId(slugifyProviderName(kind.label));
    settings.providers[id] = {
      protocol: kind.protocol,
      base_url: null,
      api_key_secret_ref: null,
      api_key_env: null,
      has_credential: false,
      api_key: "",
    };
    selectedProvider = id;
    discoveredModels = [];
    addingProvider = false;
    compatibleDraft = null;
    error = null;
  }

  function commitCompatibleProvider(): void {
    if (!settings || !compatibleDraft) return;
    const baseUrl = compatibleDraft.base_url.trim();
    if (!baseUrl) {
      error = "OpenAI-compatible providers need a base URL.";
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
    error = null;
  }

  function removeProvider(id: string): void {
    if (!settings) return;
    for (const modelId of Object.keys(settings.models)) {
      if (settings.models[modelId].provider_id === id) removeModel(modelId);
    }
    delete settings.providers[id];
    if (settings.default_provider === id) settings.default_provider = defaultModel?.provider_id ?? "";
    if (selectedProvider === id) selectedProvider = providerIds.find((providerId) => providerId !== id) ?? "";
  }

  function addManualModel(providerId: string): void {
    if (!settings) return;
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
    if (!settings) return;
    delete settings.models[id];
    if (settings.default_model === id) {
      const replacement = modelIds.find((modelId) => modelId !== id) ?? "";
      settings.default_model = replacement;
      settings.default_provider = replacement ? settings.models[replacement]?.provider_id ?? "" : "";
    }
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

  function linesFromList(values: string[]): string {
    return values.join("\n");
  }

  function listFromLines(value: string): string[] {
    return value
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);
  }

  function bundlesText(): string {
    if (!settings) return "{}";
    return JSON.stringify(settings.tool_permissions.bundles, null, 2);
  }

  function updateBundles(value: string): void {
    if (!settings) return;
    try {
      const parsed = JSON.parse(value) as Record<string, unknown>;
      const bundles: Record<string, string[]> = {};
      for (const [name, tools] of Object.entries(parsed)) {
        if (!Array.isArray(tools) || tools.some((tool) => typeof tool !== "string")) {
          throw new Error("Tool bundles must be a JSON object of string arrays.");
        }
        bundles[name] = tools;
      }
      settings.tool_permissions.bundles = bundles;
      hasInvalidBundlesJson = false;
      error = null;
    } catch (err) {
      hasInvalidBundlesJson = true;
      error = err instanceof Error ? err.message : "Tool bundles must be valid JSON.";
    }
  }

  function approvalRulesText(): string {
    if (!settings) return "[]";
    return JSON.stringify(settings.tool_permissions.require_approval, null, 2);
  }

  function updateApprovalRules(value: string): void {
    if (!settings) return;
    try {
      const parsed = JSON.parse(value) as unknown;
      if (!Array.isArray(parsed)) throw new Error("Approval rules must be a JSON array.");
      for (const entry of parsed) {
        if (typeof entry === "string") continue;
        if (!entry || typeof entry !== "object" || typeof (entry as { tool?: unknown }).tool !== "string") {
          throw new Error("Approval rule objects need a tool string.");
        }
      }
      settings.tool_permissions.require_approval = parsed as RequireApprovalEntry[];
      hasInvalidApprovalJson = false;
      error = null;
    } catch (err) {
      hasInvalidApprovalJson = true;
      error = err instanceof Error ? err.message : "Approval rules must be valid JSON.";
    }
  }

  function normaliseDefaultModel(): void {
    if (!settings) return;
    if (settings.default_model && settings.models[settings.default_model]) {
      settings.default_provider = settings.models[settings.default_model].provider_id;
      return;
    }
    const replacement = Object.keys(settings.models)[0] ?? "";
    settings.default_model = replacement;
    settings.default_provider = replacement ? settings.models[replacement].provider_id : "";
  }

  async function persist(options: { message?: string } = {}): Promise<boolean> {
    if (!settings) return false;
    error = null;
    status = "";
    if (hasInvalidBundlesJson || hasInvalidApprovalJson) {
      error = "Fix the invalid automation JSON before saving.";
      return false;
    }
    for (const [id, preset] of Object.entries(settings.generation_presets)) {
      if (!id.trim() || presetIsEmpty(preset)) {
        error = "Generation presets need a name and at least one parameter.";
        return false;
      }
      // Normalise NaN-from-empty-input to null so the wire format is valid.
      preset.max_tokens = nullableNumber(preset.max_tokens);
      preset.temperature = nullableNumber(preset.temperature);
    }
    settings.hitl.timeout = nullableNumber(settings.hitl.timeout) ?? 0;
    settings.context.timeout = nullableNumber(settings.context.timeout) ?? 0;
    settings.context.budget = nullableNumber(settings.context.budget);
    const providerToRestore = selectedProvider;
    try {
      isPersisting = true;
      normaliseDefaultModel();
      settings = await saveSettings(settings);
      savedProviderIds = new Set(Object.keys(settings.providers));
      if (providerToRestore && settings.providers[providerToRestore]) selectedProvider = providerToRestore;
      // The server never echoes raw credentials; clear the input so the
      // password field returns to the "Stored credential" placeholder.
      for (const provider of Object.values(settings.providers)) provider.api_key = "";
      status =
        options.message ??
        (settings.can_restart
          ? "Saved. Restart the managed gateway to apply changes."
          : "Saved. Restart the gateway manually to apply changes.");
      return true;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to save settings.";
      return false;
    } finally {
      isPersisting = false;
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

  const TAB_TITLES: Record<SettingsTabId, { title: string; sub: string }> = {
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
    plugins: {
      title: "Plugins",
      sub: "Managed and workspace plugin bundles. Plugins extend tools, skills, and lenses available to agents.",
    },
    channels: {
      title: "Channels",
      sub: "External delivery integrations the gateway can route conversations through.",
    },
    nodes: {
      title: "Nodes",
      sub: "Attached execution hosts that the gateway can dispatch sandboxed work to.",
    },
    voice: {
      title: "Voice",
      sub: "Speech transcription and synthesis capabilities offered by the active gateway.",
    },
    recovery: {
      title: "Recovery",
      sub: "Operator-managed export bundles capturing benches, conversations, and configuration for restore.",
    },
    display: {
      title: "Display",
      sub: "How Swage looks. Settings here are platform-level — they apply across all benches and lenses.",
    },
    account: {
      title: "Account",
      sub: "Identity and billing. Sign-in is local-first today; remote identity arrives with the multi-host era.",
    },
  };

  // Non-functional pages — listed once so the shell branch can short-circuit
  // without duplicating the gating logic. See the per-page motes for the gap.
  const SHELL_TABS: ReadonlySet<SettingsTabId> = new Set([
    "plugins",
    "channels",
    "nodes",
    "voice",
    "recovery",
    "display",
    "account",
  ]);

  const isShellTab = $derived(SHELL_TABS.has(tab));
</script>

<div class="settings-pane" aria-label="Settings">
  {#if isShellTab}
    {@const heading = TAB_TITLES[tab]}
    <!--
      Shell pages from the Riso workup (`riso-cyanotype.html`, view 06) that do
      not yet have a working backend. Each page renders the heading and any
      partial functionality the gateway already exposes (read-only operator
      data where it exists), with an explicit non-functional banner so the
      operator knows nothing is going to be persisted.
    -->
    <div class="head">
      <div class="head-text">
        <p class="eyebrow">Settings</p>
        <h1 class="title">{heading.title}</h1>
        <p class="sub">{heading.sub}</p>
      </div>
    </div>

    <div class="shell-banner" role="note">
      <span class="shell-flag">Non-functional</span>
      <span class="shell-body">
        This page is a placeholder for the Riso workup view. No values here are
        saved or applied — the underlying backend hasn't been built yet.
      </span>
    </div>

    {#if tab === "plugins"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        the platform plugin model is accepted in the backlog.

        Wire the Plugins page to a real plugin/extension catalogue. Today the
        gateway exposes per-bench extensions (`/api/benches/{id}/extensions`)
        but no platform-level plugin registry. Surface installed plugins,
        their tools, and an enable/disable toggle.
        Why: the workup view 06 lists Plugins as the first page; today users
        cannot see or manage anything plugin-shaped from settings.
        How to apply: when the gateway gains a plugin registry endpoint,
        replace this shell with a real list bound to that endpoint.
        ---
      -->
      <section class="shell-section">
        <h2 class="shell-section-title">Installed plugins</h2>
        <p class="shell-empty">No platform plugin registry yet. Bench-level extensions live in the Builder lens.</p>
      </section>
    {:else if tab === "channels"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        channel management semantics are accepted in the backlog.

        Render the channel list returned by /api/operator/overview.channels
        and add per-channel route/credential editing. Today the page is a
        shell because the operator endpoint is read-only and there is no
        management API.
        Why: Riso workup view 06 lists Channels as a settings page; the
        operator deck reads them but there is no way to add/configure them.
        How to apply: extend swage-gateway with channel CRUD, then bind here.
        ---
      -->
      <section class="shell-section">
        <h2 class="shell-section-title">Delivery channels</h2>
        <p class="shell-empty">Channel management is read-only via the operator API; this page will configure them once a CRUD endpoint exists.</p>
      </section>
    {:else if tab === "nodes"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        execution-host registration and disconnect semantics are accepted in
        the backlog.

        Show registered execution hosts (operator overview already returns
        nodes). Add register/disconnect flows once the gateway exposes them.
        Why: workup view 06 includes Nodes; today there is no settings-level
        view of attached hosts.
        How to apply: bind to /api/operator/overview.nodes for a read-only
        view first; design a registration flow before building the form.
        ---
      -->
      <section class="shell-section">
        <h2 class="shell-section-title">Attached nodes</h2>
        <p class="shell-empty">Node listing will mirror /api/operator/overview.nodes once this page is wired.</p>
      </section>
    {:else if tab === "voice"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        the voice provider/configuration model is accepted in the backlog.

        Bind to /api/voice/capabilities for read-only state, then add voice
        provider configuration (transcription engine, synthesis voice,
        per-bench overrides). The capabilities endpoint exists; the
        configuration endpoint does not.
        Why: workup view 06 lists Voice; the WorkbenchOperations capability
        strip is the only place voice surfaces today.
        How to apply: once a /api/voice/config endpoint exists, replace this
        shell with the real form.
        ---
      -->
      <section class="shell-section">
        <h2 class="shell-section-title">Voice capabilities</h2>
        <p class="shell-empty">Read-only capability flags live on the voice endpoint; configuration arrives with a future settings backend.</p>
      </section>
    {:else if tab === "recovery"}
      <section class="shell-section">
        <div class="section-heading-row">
          <h2 class="shell-section-title">Recovery exports</h2>
          <div class="opt-row">
            <button type="button" class="btn" onclick={loadRecoveryExports} disabled={isLoadingRecoveryExports || activeRecoveryExportId !== null}>
              {isLoadingRecoveryExports ? "Refreshing" : "Refresh"}
            </button>
            <button type="button" class="btn primary" onclick={createRecoveryExport} disabled={activeRecoveryExportId !== null}>
              {activeRecoveryExportId === "new" ? "Exporting" : "Export now"}
            </button>
          </div>
        </div>
        {#if recoveryExports.length === 0}
          <p class="shell-empty">No recovery exports have been produced by this gateway process.</p>
        {:else}
          <div class="recovery-export-list">
            {#each recoveryExports as recoveryExport (recoveryExport.id)}
              <article class="recovery-export-row">
                <div>
                  <strong>{new Date(recoveryExport.created_at).toLocaleString()}</strong>
                  <span>{recoveryCountSummary(recoveryExport)}</span>
                </div>
                <div class="recovery-export-actions">
                  <span>{formatRecoverySize(recoveryExport.size_bytes)}</span>
                  <button
                    type="button"
                    class="btn"
                    onclick={() => downloadRecoveryBundle(recoveryExport.id)}
                    disabled={activeRecoveryExportId !== null}
                  >
                    {activeRecoveryExportId === recoveryExport.id ? "Preparing" : "Download"}
                  </button>
                </div>
              </article>
            {/each}
          </div>
        {/if}
      </section>
    {:else if tab === "display"}
      <!-- TODO Add persisted display preferences -->
      <!-- Persist theme, density, model display, rendering, and print-mark preferences. -->
      <!-- --- -->
      <!--
        Move the display preferences from per-component locations (theme
        toggle in topbar, density implicit in CSS) into a single settings
        page. Need a persisted preference store first — today theme is the
        only knob and it lives in the topbar component.
        Why: workup view 06 specifies a Display page covering theme,
        density, per-message model, conversation rendering, and print marks.
        How to apply: design a `display_preferences` table in storage,
        expose it via /api/preferences, then bind real toggles here.
        ---
      -->
      <section class="shell-section">
        <h2 class="shell-section-title">Display preferences</h2>
        <div class="shell-grid">
          <div class="shell-row">
            <span class="shell-label">Theme</span>
            <span class="shell-value">Use the topbar toggle until the preferences API lands.</span>
          </div>
          <div class="shell-row">
            <span class="shell-label">Density</span>
            <span class="shell-value">Compact (fixed)</span>
          </div>
          <div class="shell-row">
            <span class="shell-label">Show model on each message</span>
            <span class="shell-value">Off (fixed)</span>
          </div>
          <div class="shell-row">
            <span class="shell-label">Conversation rendering</span>
            <span class="shell-value">Markdown (fixed)</span>
          </div>
          <div class="shell-row">
            <span class="shell-label">Print marks</span>
            <span class="shell-value">Full (fixed)</span>
          </div>
        </div>
      </section>
    {:else if tab === "account"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        the local-vs-remote identity model is accepted in the backlog.

        Account is unscoped. Swage runs locally with no notion of identity;
        a remote identity provider would precede any meaningful account
        page. Until then this stays a shell so the workup nav resolves.
        Why: workup view 06 lists Account but the application has no
        account model yet.
        How to apply: revisit when remote identity / multi-host work begins.
        ---
      -->
      <section class="shell-section">
        <h2 class="shell-section-title">Account</h2>
        <p class="shell-empty">Swage runs locally — there is no account concept yet.</p>
      </section>
    {/if}
  {:else if !settings}
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
        <button type="button" class="btn primary" onclick={() => persist()} disabled={isPersisting}>
          {isPersisting ? "Saving..." : "Save"}
        </button>
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
              <div class="search-row">
                <input
                  type="text"
                  class="text search"
                  value={compatibleDraft.name}
                  oninput={(event) => {
                    if (compatibleDraft) compatibleDraft.name = event.currentTarget.value;
                  }}
                  placeholder="Provider name"
                  autocomplete="off"
                />
                <input
                  type="text"
                  class="text"
                  value={compatibleDraft.base_url}
                  oninput={(event) => {
                    if (compatibleDraft) compatibleDraft.base_url = event.currentTarget.value;
                  }}
                  placeholder="https://api.example.com/v1"
                  autocomplete="off"
                />
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
        <section class="provider-section" aria-label={`Provider ${id}`}>
          <div class="provider-head">
            <div class="provider-head-text">
              <span class="provider-index">
                Provider <span class="ix">{String(ix + 1).padStart(2, "0")}</span>
              </span>
              <h2>{providerDisplayName(id)}</h2>
              <p class="provider-meta-line">
                <span class="meta-protocol">{provider.protocol}</span>
                <span class="meta-key">{id}</span>
                <span class="meta-endpoint">{provider.base_url || "provider default endpoint"}</span>
              </p>
            </div>
            <div class="provider-meta">
              <span class="credential-status" class:set={provider.has_credential}>
                {provider.has_credential ? "Credential stored" : "No credential"}
              </span>
              <button type="button" class="opt opt-cancel" onclick={() => removeProvider(id)}>
                <span aria-hidden="true">×</span>
                <span>Remove</span>
              </button>
            </div>
          </div>

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
    {:else if tab === "automation"}
      <div class="field">
        <div class="key">
          Sandbox
          <small>Global sandbox policy written to config.toml. Saved changes require a gateway restart.</small>
        </div>
        <div class="val">
          <button
            type="button"
            class="opt"
            class:active={settings?.sandbox.enabled}
            onclick={() => {
              if (settings) settings.sandbox.enabled = true;
            }}
          >
            Enabled
          </button>
          <button
            type="button"
            class="opt"
            class:active={!settings?.sandbox.enabled}
            onclick={() => {
              if (settings) settings.sandbox.enabled = false;
            }}
          >
            Disabled
          </button>
        </div>
      </div>

      <div class="field">
        <div class="key">
          Approval defaults
          <small>Timeout and timeout policy for human approval requests without a per-tool override.</small>
        </div>
        <div class="val val-stack">
          <div class="search-row">
            <input
              type="number"
              class="text small"
              bind:value={settings.hitl.timeout}
              min="1"
              placeholder="300"
            />
            <button
              type="button"
              class="opt"
              class:active={settings?.hitl.timeout_policy === "reject"}
              onclick={() => {
                if (settings) settings.hitl.timeout_policy = "reject";
              }}
            >
              Reject
            </button>
            <button
              type="button"
              class="opt"
              class:active={settings?.hitl.timeout_policy === "interrupt"}
              onclick={() => {
                if (settings) settings.hitl.timeout_policy = "interrupt";
              }}
            >
              Interrupt
            </button>
          </div>
        </div>
      </div>

      <div class="field">
        <div class="key">
          Context assembly
          <small>Assembly strategy, assembly timeout, and optional token budget.</small>
        </div>
        <div class="val val-stack">
          <div class="search-row">
            <input
              type="text"
              class="text"
              bind:value={settings.context.assembly}
              placeholder="default"
              autocomplete="off"
            />
            <input
              type="number"
              class="text small"
              bind:value={settings.context.timeout}
              min="1"
              max="60"
              placeholder="5"
            />
            <input
              type="number"
              class="text small"
              value={settings.context.budget ?? ""}
              min="1"
              placeholder="model limit"
              oninput={(event) => {
                const value = (event.currentTarget as HTMLInputElement).value;
                if (settings) settings.context.budget = value ? Number(value) : null;
              }}
            />
          </div>
        </div>
      </div>

      <div class="field">
        <div class="key">
          Tool allow rules
          <small>One rule per line. Use * for broad availability.</small>
        </div>
        <div class="val">
          <textarea
            class="text"
            rows="4"
            value={linesFromList(settings?.tool_permissions.allow ?? [])}
            oninput={(event) => {
              if (settings) settings.tool_permissions.allow = listFromLines((event.currentTarget as HTMLTextAreaElement).value);
            }}
          ></textarea>
        </div>
      </div>

      <div class="field">
        <div class="key">
          Tool deny rules
          <small>One denied tool name or pattern per line.</small>
        </div>
        <div class="val">
          <textarea
            class="text"
            rows="4"
            value={linesFromList(settings?.tool_permissions.deny ?? [])}
            oninput={(event) => {
              if (settings) settings.tool_permissions.deny = listFromLines((event.currentTarget as HTMLTextAreaElement).value);
            }}
          ></textarea>
        </div>
      </div>

      <div class="field">
        <div class="key">
          Tool bundles
          <small>JSON object mapping bundle names to tool arrays.</small>
        </div>
        <div class="val">
          <textarea
            class="text code"
            rows="7"
            value={bundlesText()}
            spellcheck="false"
            oninput={(event) => updateBundles((event.currentTarget as HTMLTextAreaElement).value)}
          ></textarea>
        </div>
      </div>

      <div class="field">
        <div class="key">
          Approval-gated tools
          <small>JSON array of tool names or objects with timeout overrides.</small>
        </div>
        <div class="val">
          <textarea
            class="text code"
            rows="7"
            value={approvalRulesText()}
            spellcheck="false"
            oninput={(event) => updateApprovalRules((event.currentTarget as HTMLTextAreaElement).value)}
          ></textarea>
        </div>
      </div>
    {/if}
  {/if}
</div>

<style>
  /* ---------------------------------------------------------------------------
     Settings pane — body content for the riso settings shell.
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

  /* ---------------------------------------------------------------------------
     Non-functional page shell — used by the workup pages without a backend.
     --------------------------------------------------------------------------- */

  .shell-banner {
    display: flex;
    align-items: stretch;
    gap: 0;
    border: 2px solid var(--ink);
    background: var(--paper);
  }

  .shell-flag {
    display: flex;
    align-items: center;
    padding: 10px 14px;
    background: var(--accent);
    color: var(--paper);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    flex-shrink: 0;
  }

  .shell-body {
    padding: 10px 14px;
    color: var(--ink);
    font-size: 13px;
    line-height: 1.5;
  }

  .shell-section {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 16px 18px;
    border: 1px solid var(--rule-soft);
    background: var(--paper);
  }

  .shell-section-title {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 18px;
    line-height: 1.05;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .section-heading-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }

  .shell-empty {
    margin: 0;
    color: var(--ink-soft);
    font-size: 13px;
    line-height: 1.55;
  }

  .shell-grid {
    display: flex;
    flex-direction: column;
    gap: 0;
  }

  .shell-row {
    display: grid;
    grid-template-columns: minmax(180px, 0.4fr) 1fr;
    gap: 16px;
    padding: 10px 0;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .shell-row:last-child {
    border-bottom: none;
  }

  .shell-label {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .shell-value {
    color: var(--ink);
    font-size: 13px;
    line-height: 1.5;
  }

  .recovery-export-list {
    display: grid;
    gap: 8px;
  }

  .recovery-export-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 14px;
    align-items: center;
    padding: 12px 0;
    border-top: 1px dotted var(--rule-soft);
  }

  .recovery-export-row strong,
  .recovery-export-row span {
    display: block;
  }

  .recovery-export-row strong {
    color: var(--ink);
    font-size: 13px;
    line-height: 1.3;
  }

  .recovery-export-row span {
    color: var(--ink-soft);
    font-size: 12px;
    line-height: 1.45;
  }

  .recovery-export-actions {
    display: flex;
    align-items: center;
    gap: 10px;
    justify-content: flex-end;
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
    justify-content: space-between;
    align-items: flex-start;
    gap: 18px;
    padding-bottom: 12px;
  }

  .provider-head-text {
    min-width: 0;
  }

  .provider-index {
    display: block;
    margin-bottom: 6px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
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
    font-size: 26px;
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
    font-size: 11px;
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

  .provider-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    justify-content: flex-end;
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

    .recovery-export-row {
      grid-template-columns: 1fr;
    }

    .recovery-export-actions {
      justify-content: space-between;
    }
  }
</style>
