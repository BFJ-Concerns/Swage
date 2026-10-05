<!--
  Settings body pane.

  Renders the inner pane of the settings window. The window chrome (topbar
  with Settings label + × CLOSE, and the parked nav with the page list) lives
  in AppLayout; this component only owns the .settings-pane content.

  Sub-pages are driven by the route. AppLayout renders the working page list;
  this component just reads the tab and shows the matching field set.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { fetchSettings, restartGateway, saveSettings } from "../api/settings";
  import AutomationSettingsPanel from "./settings/AutomationSettingsPanel.svelte";
  import AgentDefinitionsPanel from "./AgentDefinitionsPanel.svelte";
  import CommandsPanel from "./CommandsPanel.svelte";
  import DisplayPreferencesSettingsPanel from "./settings/DisplayPreferencesSettingsPanel.svelte";
  import GenerationSettingsPanel from "./settings/GenerationSettingsPanel.svelte";
  import InformationalSettingsPanel from "./settings/InformationalSettingsPanel.svelte";
  import ProviderSettingsPanel from "./settings/ProviderSettingsPanel.svelte";
  import RecoverySettingsPanel from "./settings/RecoverySettingsPanel.svelte";
  import MemoryPanel from "./MemoryPanel.svelte";
  import type { MemoryPageAddress } from "../router";
  import SkillsPanel from "./SkillsPanel.svelte";
  import VoiceSettingsPanel from "./settings/VoiceSettingsPanel.svelte";
  import type { GenerationPresetConfig, SettingsResponse, Topic } from "../types";

  // Agents, providers, generation, automation, and recovery have working
  // backends. The remaining partial pages are identified below.
  type SettingsTabId =
    | "providers"
    | "agents"
    | "commands"
    | "skills"
    | "generation"
    | "automation"
    | "plugins"
    | "channels"
    | "devices"
    | "voice"
    | "memory"
    | "recovery"
    | "display"
    | "account";

  interface Props {
    tab?: SettingsTabId;
    topicId?: string | null;
    topics?: Topic[];
    // The memory page a notice link names, opened on the memory tab.
    memoryPage?: MemoryPageAddress | null;
    onSettingsSaved?: () => void | Promise<void>;
  }

  let {
    tab = "providers",
    topicId = null,
    topics = [],
    memoryPage = null,
    onSettingsSaved,
  }: Props = $props();

  // settings is the working copy — bind:value mutates it freely. saveSettings
  // returns a fresh server snapshot which we replace it with. Null until the
  // initial fetch resolves.
  let settings = $state<SettingsResponse | null>(null);
  let savedProviderIds = $state<Set<string>>(new Set());
  let isPersisting = $state(false);
  let hasLoadedSettings = $state(false);
  let lastPersistedSettingsFingerprint = "";
  let lastSavedSearch: SettingsResponse["tool_runtime"]["search"] = null;
  let hasInvalidBundlesJson = $state(false);
  let hasInvalidApprovalJson = $state(false);
  let lastSavedArchiveDelay = $state(7);
  const archiveDelayError = $derived(
    settings && (nullableNumber(settings.sessions.archive_after_days) == null || settings.sessions.archive_after_days < 1)
      ? "Archive delay must be at least 1 day."
      : null,
  );
  let status = $state("");
  let error = $state<string | null>(null);
  const HOT_AUTOSAVE_TABS: ReadonlySet<SettingsTabId> = new Set([
    "providers",
    "generation",
    "automation",
    "voice",
  ]);

  onMount(async () => {
    try {
      settings = await fetchSettings();
      lastSavedArchiveDelay = settings.sessions.archive_after_days;
      lastSavedSearch = settings.tool_runtime.search ? { ...settings.tool_runtime.search } : null;
      savedProviderIds = new Set(Object.keys(settings.providers));
      lastPersistedSettingsFingerprint = settingsFingerprint() ?? "";
      hasLoadedSettings = true;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load settings.";
    }
  });

  function presetIsEmpty(preset: GenerationPresetConfig): boolean {
    return !preset.max_tokens && preset.temperature == null && (preset.stop_sequences ?? []).length === 0 && !preset.metadata;
  }

  // <input type="number"> coerces to "" when emptied — Svelte's bind:value
  // gives us NaN. Coerce back to null so the JSON payload matches the schema.
  function nullableNumber(value: number | null | undefined): number | null {
    if (value === null || value === undefined) return null;
    return Number.isFinite(value) ? value : null;
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

  function searchIsIncomplete(search: SettingsResponse["tool_runtime"]["search"]): boolean {
    return search != null && (!search.provider_type.trim() || !search.base_url.trim());
  }

  function toolRuntimeForSave(runtime: SettingsResponse["tool_runtime"]): SettingsResponse["tool_runtime"] {
    return { ...runtime, search: searchIsIncomplete(runtime.search) ? lastSavedSearch : runtime.search };
  }

  function settingsFingerprint(): string | null {
    if (!settings) return null;
    return JSON.stringify({
      providers: settings.providers,
      default_provider: settings.default_provider,
      models: settings.models,
      default_model: settings.default_model,
      generation_presets: settings.generation_presets,
      tool_runtime: toolRuntimeForSave(settings.tool_runtime),
      tool_permissions: settings.tool_permissions,
      hitl: settings.hitl,
      sandbox: settings.sandbox,
      context: settings.context,
      pricing: settings.pricing,
      observability: settings.observability,
      communication: settings.communication,
      sessions: settings.sessions,
      memory: settings.memory,
      voice: settings.voice,
    });
  }

  async function persist(options: { message?: string } = {}): Promise<boolean> {
    if (!settings) return false;
    error = null;
    status = "";
    if (archiveDelayError) {
      error = "Fix the archive delay in Automation before saving. It must be at least 1 day.";
      return false;
    }
    const archiveDelayChanged = settings.sessions.archive_after_days !== lastSavedArchiveDelay;
    const holdSearchDraft = searchIsIncomplete(settings.tool_runtime.search);
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
    settings.sessions.archive_after_days = Math.min(3650, settings.sessions.archive_after_days);
    settings.memory.curator_idle_minutes = Math.min(10080, Math.max(1, nullableNumber(settings.memory.curator_idle_minutes) ?? 10));
    settings.context.timeout = nullableNumber(settings.context.timeout) ?? 0;
    settings.context.budget = nullableNumber(settings.context.budget);
    try {
      isPersisting = true;
      normaliseDefaultModel();
      const saved = await saveSettings({ ...settings, tool_runtime: toolRuntimeForSave(settings.tool_runtime) });
      const searchDraft = settings.tool_runtime.search;
      settings = saved;
      lastSavedArchiveDelay = saved.sessions.archive_after_days;
      lastSavedSearch = saved.tool_runtime.search ? { ...saved.tool_runtime.search } : null;
      savedProviderIds = new Set(Object.keys(settings.providers));
      // The server never echoes raw credentials; clear the input so the
      // password field returns to the "Stored credential" placeholder.
      for (const provider of Object.values(settings.providers)) provider.api_key = "";
      lastPersistedSettingsFingerprint = settingsFingerprint() ?? "";
      if (holdSearchDraft) settings.tool_runtime.search = searchDraft;
      await onSettingsSaved?.();
      status = archiveDelayChanged
        ? `Archive delay saved as ${saved.sessions.archive_after_days} ${saved.sessions.archive_after_days === 1 ? "day" : "days"}.`
        : options.message ?? "Saved. New turns will use the updated runtime.";
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
    agents: {
      title: "Agent definitions",
      sub: "The effective agent definitions available when starting a conversation in the selected topic.",
    },
    commands: {
      title: "Commands",
      sub: "The slash commands available in the selected topic.",
    },
    skills: {
      title: "Skills",
      sub: "The resolved skills available in the selected topic.",
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
      sub: "Managed and workspace plugin bundles. Plugins extend tools and skills available to agents.",
    },
    channels: {
      title: "Channels",
      sub: "External delivery integrations the gateway can route conversations through.",
    },
    devices: {
      title: "Devices",
      sub: "Attached devices that the gateway can dispatch sandboxed work to.",
    },
    voice: {
      title: "Voice",
      sub: "The models the default provider uses for speech transcription and synthesis.",
    },
    memory: {
      title: "Memory",
      sub: "What the curator remembers from your conversations, in general and for each topic. To change a page, tell the curator.",
    },
    recovery: {
      title: "Recovery",
      sub: "Operator-managed export bundles capturing topics, conversations, and configuration for restore.",
    },
    display: {
      title: "Display",
      sub: "How Swage looks. Settings here are platform-level — they apply across all topics.",
    },
    account: {
      title: "Account",
      sub: "Identity and billing. Sign-in is local-first today; remote identity arrives with the multi-host era.",
    },
  };

  // Pages rendered in the compact settings-shell layout. Some are fully wired
  // frontend surfaces; the non-functional subset gets an explicit banner.
  const SHELL_LAYOUT_TABS: ReadonlySet<SettingsTabId> = new Set([
    "agents",
    "commands",
    "skills",
    "plugins",
    "channels",
    "devices",
    "voice",
    "memory",
    "recovery",
    "display",
    "account",
  ]);
  const NON_FUNCTIONAL_TABS: ReadonlySet<SettingsTabId> = new Set([
    "plugins",
    "channels",
    "devices",
    "account",
  ]);

  const isShellTab = $derived(SHELL_LAYOUT_TABS.has(tab));
  const isNonFunctionalTab = $derived(NON_FUNCTIONAL_TABS.has(tab));
  const shouldAutosaveSettings = $derived(HOT_AUTOSAVE_TABS.has(tab));

  $effect(() => {
    if (!hasLoadedSettings || !shouldAutosaveSettings) return;
    const fingerprint = settingsFingerprint();
    if (!fingerprint || fingerprint === lastPersistedSettingsFingerprint) return;

    const timer = window.setTimeout(() => {
      void persist({ message: "Saved automatically. New turns will use the updated runtime." });
    }, 650);

    return () => window.clearTimeout(timer);
  });
</script>

<div class="settings-pane" aria-label="Settings">
  {#if isShellTab}
    {@const heading = TAB_TITLES[tab]}
    <!--
      Shell pages that do not yet have a working backend. Each page renders the
      heading and any partial functionality the gateway already exposes
      (read-only operator data where it exists), with an explicit
      non-functional banner so the operator knows nothing is going to be
      persisted.
    -->
    <div class="head">
      <div class="head-text">
        <p class="eyebrow">Settings</p>
        <h1 class="title">{heading.title}</h1>
        <p class="sub">{heading.sub}</p>
      </div>
    </div>

    {#if isNonFunctionalTab}
      <div class="shell-banner" role="note">
        <span class="shell-flag">Non-functional</span>
        <span class="shell-body">
          This page is a placeholder. No values here are saved or applied —
          the underlying backend hasn't been built yet.
        </span>
      </div>
    {/if}

    {#if error}
      <div class="notice error" role="alert">
        <span>{error}</span>
        <button type="button" class="notice-dismiss" onclick={() => (error = null)} aria-label="Dismiss">×</button>
      </div>
    {/if}

    {#if status && !archiveDelayError}
      <div class="notice">
        <span>{status}</span>
      </div>
    {/if}

    {#if tab === "agents"}
      <AgentDefinitionsPanel {topicId} />
    {:else if tab === "commands"}
      <CommandsPanel {topicId} />
    {:else if tab === "skills"}
      <SkillsPanel {topicId} />
    {:else if tab === "memory"}
      <MemoryPanel {topics} page={memoryPage} />
    {:else if tab === "plugins"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        Swage has a platform plugin model.

        Wire the Plugins page to a real plugin/extension catalogue. Today the
        there is no platform-level plugin registry. Surface installed plugins,
        their tools, and an enable/disable toggle.
        Why: users cannot see or manage anything plugin-shaped from settings.
        How to apply: when the gateway gains a plugin registry endpoint,
        replace this shell with a real list bound to that endpoint.
        ---
      -->
      <InformationalSettingsPanel {tab} />
    {:else if tab === "channels"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        Swage has channel management semantics.

        Render the channel list returned by /api/operator/overview.channels
        and add per-channel route/credential editing. Today the page is a
        shell because the operator endpoint is read-only and there is no
        management API.
        Why: the operator deck reads channels but there is no way to
        add/configure them.
        How to apply: extend swage-gateway with channel CRUD, then bind here.
        ---
      -->
      <InformationalSettingsPanel {tab} />
    {:else if tab === "devices"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        Swage has device registration and disconnect semantics.

        Show registered devices (operator overview already returns
        devices). Add register/disconnect flows once the gateway exposes them.
        Why: there is no settings-level view of attached devices.
        How to apply: bind to /api/operator/overview.devices for a read-only
        view first; design a registration flow before building the form.
        ---
      -->
      <InformationalSettingsPanel {tab} />
    {:else if tab === "voice"}
      {#if settings}
        <VoiceSettingsPanel bind:settings />
      {/if}
    {:else if tab === "recovery"}
      <RecoverySettingsPanel />
    {:else if tab === "display"}
      <DisplayPreferencesSettingsPanel />
    {:else if tab === "account"}
      <!--
        Debt marker: this visible settings page is intentionally a shell until
        Swage has a local-vs-remote identity model.

        Account is unscoped. Swage runs locally with no notion of identity;
        a remote identity provider would precede any meaningful account
        page. Until then this stays a shell so the settings route resolves.
        Why: the application has no account model yet.
        How to apply: revisit when remote identity / multi-host work begins.
        ---
      -->
      <InformationalSettingsPanel {tab} />
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
        {#if shouldAutosaveSettings}
          <span class="autosave-state" class:saving={isPersisting}>
            {isPersisting ? "Autosaving" : "Autosave"}
          </span>
        {:else}
          <button type="button" class="btn primary" onclick={() => persist()} disabled={isPersisting}>
            {isPersisting ? "Saving..." : "Save"}
          </button>
        {/if}
      </div>
    </div>

    {#if error}
      <div class="notice error" role="alert">
        <span>{error}</span>
        <button type="button" class="notice-dismiss" onclick={() => (error = null)} aria-label="Dismiss">×</button>
      </div>
    {/if}

    {#if status && !archiveDelayError}
      <div class="notice">
        <span>{status}</span>
        {#if settings.can_restart}
          <button type="button" class="notice-action" onclick={restart}>Restart</button>
        {/if}
      </div>
    {:else if settings.restart_required}
      <div class="notice">
        <span>Saved provider/model settings require a gateway restart before new turns can use them.</span>
        {#if settings.can_restart}
          <button type="button" class="notice-action" onclick={restart}>Restart</button>
        {/if}
      </div>
    {/if}

    {#if tab === "providers"}
      <ProviderSettingsPanel
        bind:settings
        {savedProviderIds}
        {isPersisting}
        bind:error
      />
    {:else if tab === "generation"}
      <GenerationSettingsPanel bind:settings />
    {:else if tab === "automation"}
      <AutomationSettingsPanel
        bind:settings
        {archiveDelayError}
        {topicId}
        {topics}
        sessionId={null}
        onValidation={(bundlesInvalid, approvalsInvalid, message) => {
          hasInvalidBundlesJson = bundlesInvalid;
          hasInvalidApprovalJson = approvalsInvalid;
          error = message;
        }}
      />
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
     Non-functional page shell — used by settings pages without a backend.
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

  .autosave-state {
    display: inline-flex;
    align-items: center;
    min-height: 32px;
    padding: 0 12px;
    border: 1px solid var(--ink);
    color: var(--ink-soft);
    background: var(--paper);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .autosave-state.saving {
    color: var(--accent);
    border-color: var(--accent);
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

  }
</style>
