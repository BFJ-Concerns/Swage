<!--
  Riso/cyanotype shell.

  When no bench is selected the top bar collapses and the body becomes a
  full-width bench picker. Switching benches goes through BenchSelector,
  which resets the layout via onBenchChange.

  WorkbenchRail (the right rail) was dropped per Surfaces.md — shared
  reference objects now sit on the module stage like any other module.

  Surface switching: the lens descriptor still exposes ui_modules, but the
  shell has no per-lens surface tabs. We honour the lens's default_surface
  and place the corresponding module on the stage.
  TODO(debt): reconcile lens.ui_modules with the single-surface-per-lens
  design once lens definitions are reshaped. Tracked: future UI cleanup.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import ConversationList from "./ConversationList.svelte";
  import BenchSelector from "./BenchSelector.svelte";
  import BenchPicker from "./BenchPicker.svelte";
  import BenchSettingsPage from "./BenchSettingsPage.svelte";
  import MessageDisplay from "./MessageDisplay.svelte";
  import MessageInput from "./MessageInput.svelte";
  import ApprovalCard from "./ApprovalCard.svelte";
  import ConnectionIndicator from "./ConnectionIndicator.svelte";
  import SharedReferenceStudio from "./SharedReferenceStudio.svelte";
  import SettingsPage from "./SettingsPage.svelte";
  import ThemeToggle from "./ThemeToggle.svelte";
  import { listTeams } from "../api/builder";
  import type { DisplayPreferences } from "../display-preferences";
  import type { BenchSettingsTab, Route } from "../router";
  import type { ThemeMode } from "../theme";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type {
    AppLens,
    BuilderMode,
    LensDescriptor,
    LensStageModulePlacement,
    LensSurface,
    TeamSummary,
  } from "../types";
  import type { ConversationStore } from "../stores/conversations";
  import type { ConnectionStore } from "../stores/connection";
  import type { StreamingStore } from "../stores/streaming";
  import type { BenchStore } from "../stores/benches";

  type BuilderDeckModule = typeof import("./BuilderDeck.svelte");
  type OperatorDashboardModule = typeof import("./OperatorDashboard.svelte");

  interface Props {
    conversationStore: ConversationStore;
    connectionStore: ConnectionStore;
    streamingStore: StreamingStore;
    benchStore?: BenchStore;
    onBenchChange?: () => void;
    onRetry: () => void;
    activeLens: AppLens;
    availableLenses: LensDescriptor[];
    stageModules?: LensStageModulePlacement[];
    activeSurface: LensSurface;
    onSelectLens: (next: AppLens) => void | Promise<void>;
    onRefreshLenses: (preferredSurface?: LensSurface) => void | Promise<void>;
    onSelectSurface: (next: LensSurface) => void;
    desktopRuntime?: DesktopRuntimeInfo | null;
    themeMode?: ThemeMode;
    displayPreferences?: DisplayPreferences;
    onSelectTheme?: (mode: ThemeMode) => void;
    activeModelContextWindow?: number | null;
    modelOptions?: string[];
    defaultModelId?: string | null;
    route?: Route;
    onNavigate?: (path: string) => void;
  }

  // activeSurface and onSelectSurface are kept in Props for API compatibility
  // with App.svelte but the new shell ignores them: the lens determines what
  // sits on the stage. Each lens picks one layout.
  let {
    conversationStore,
    connectionStore,
    streamingStore,
    benchStore,
    onBenchChange,
    onRetry,
    activeLens,
    availableLenses,
    stageModules = [],
    onSelectLens,
    onRefreshLenses,
    desktopRuntime,
    themeMode = "system",
    displayPreferences,
    onSelectTheme,
    activeModelContextWindow = null,
    modelOptions = [],
    defaultModelId = null,
    route,
    onNavigate,
  }: Props = $props();

  // Lazy-load stage modules so a builder-only crate isn't pulled in for
  // assistant-lens users. The dynamic import resolves once on first use.
  let builderDeckModule = $state<Promise<BuilderDeckModule> | null>(null);
  let operatorDashboardModule = $state<Promise<OperatorDashboardModule> | null>(null);
  let stageTeams = $state<TeamSummary[]>([]);
  let stageTeamError = $state<string | null>(null);
  let lastStageTeamBenchId = "";

  const activeLensDescriptor = $derived(
    availableLenses.find((lens) => lens.id === activeLens) ?? null,
  );
  const benches = $derived(benchStore?.benches ?? []);
  const currentBench = $derived(benchStore?.currentBench ?? null);
  const recentBenches = $derived(benchStore?.recentBenches ?? []);
  const conversationCount = $derived(conversationStore.conversations.length);
  const activeConversation = $derived(conversationStore.activeConversation);
  const settingsTab = $derived(route?.settingsTab ?? null);
  const settingsBenchSlug = $derived(route?.settingsBenchSlug ?? null);
  const settingsBenchTab = $derived(route?.settingsBenchTab ?? null);
  const settingsMode = $derived(settingsTab !== null || settingsBenchSlug !== null);
  const hasPlacedStageModules = $derived(stageModules.length > 0);
  const settingsBench = $derived(
    settingsBenchSlug
      ? benches.find((bench) => bench.slug === settingsBenchSlug) ?? null
      : null,
  );
  let now = $state(Date.now());

  // Settings sub-pages — drives both the URL the page list links to and the
  // ix counter shown beside each entry. Pages whose backends don't exist yet
  // render shells (see SettingsPage.svelte) so the nav looks complete and the
  // routes resolve.
  const SETTINGS_PAGES: ReadonlyArray<{
    id: NonNullable<typeof settingsTab>;
    label: string;
    ix: string;
    functional: boolean;
  }> = [
    { id: "providers", label: "Providers", ix: "—", functional: true },
    { id: "generation", label: "Generation", ix: "—", functional: true },
    { id: "automation", label: "Automation", ix: "—", functional: true },
    { id: "plugins", label: "Plugins", ix: "—", functional: false },
    { id: "channels", label: "Channels", ix: "—", functional: false },
    { id: "nodes", label: "Nodes", ix: "—", functional: false },
    { id: "voice", label: "Voice", ix: "—", functional: false },
    { id: "recovery", label: "Recovery", ix: "—", functional: true },
    { id: "display", label: "Display", ix: "—", functional: true },
    { id: "account", label: "Account", ix: "—", functional: false },
  ];
  const BENCH_SETTINGS_PAGES: ReadonlyArray<{
    id: BenchSettingsTab;
    label: string;
    ix: string;
  }> = [
    { id: "overview", label: "Overview", ix: "01" },
    { id: "workspaces", label: "Workspaces", ix: "02" },
    { id: "sessions", label: "Sessions", ix: "03" },
  ];

  const BUILDER_MODE_STORAGE_KEY = "swage.builder.activeMode";
  const BUILDER_MODE_OPTIONS: ReadonlyArray<{
    num: string;
    mode: BuilderMode;
    name: string;
    hint: string;
  }> = [
    { num: "01", mode: "team", name: "Team", hint: "topology + flow" },
    { num: "02", mode: "agent", name: "Agent", hint: "role + tools" },
    { num: "03", mode: "context", name: "Context", hint: "assembly" },
    { num: "04", mode: "lens", name: "Lens", hint: "layout + modules" },
    { num: "05", mode: "skill", name: "Skill", hint: "trigger + content" },
    { num: "06", mode: "tool", name: "Tool", hint: "schema + handler" },
  ];

  // × CLOSE returns to the active conversation if one was open before the
  // user entered settings; otherwise drops back to the bench picker (root).
  function closeSettings(): void {
    const id = activeConversation?.id;
    onNavigate?.(id ? `/conversations/${id}` : "/");
  }

  // Stage surface — the module placed by the active lens. The conversation
  // surface is always present, so we ignore the special-cased "conversation"
  // value here and only treat "builder" / "operator" as stage modules.
  const stageSurface = $derived<LensSurface | null>(
    activeLensDescriptor && activeLensDescriptor.default_surface !== "conversation"
      ? activeLensDescriptor.default_surface
      : null,
  );

  const hasStage = $derived(stageSurface !== null || hasPlacedStageModules);
  // TODO Add lens-level layout dominance.
  // Lenses need an explicit layout placement so conversation-dominant and
  // stage-dominant views are data-driven instead of inferred from module
  // presence.
  // ---
  const isStageDominant = $derived(false);
  const isConversationDominant = $derived(hasStage);

  // Stamp shown on the right of the topbar. The bench portion is real; the
  // state cycles through the streaming store. Team-live and approval states
  // are real too, but timed state durations need a separate aggregator.
  const stampState = $derived<{ label: string; tone: "idle" | "busy" | "alert" }>(
    streamingStore.isStreaming
      ? { label: "Working", tone: "busy" }
      : streamingStore.turnError
        ? { label: "Retrying", tone: "alert" }
        : { label: "Idle", tone: "idle" },
  );
  const turnElapsedSeconds = $derived(
    streamingStore.turnStartedAt
      ? Math.max(0, Math.floor((now - Date.parse(streamingStore.turnStartedAt)) / 1000))
      : 0,
  );
  const stampStateLabel = $derived(
    streamingStore.isStreaming
      ? `Working ${formatDuration(turnElapsedSeconds)}`
      : stampState.label,
  );
  const tokenUsage = $derived(
    (streamingStore.lastInputTokens ?? 0) + (streamingStore.lastOutputTokens ?? 0),
  );
  const tokenBudgetLabel = $derived(
    activeModelContextWindow ? activeModelContextWindow.toLocaleString() : "budget unknown",
  );
  const tokenUsageLabel = $derived(
    tokenUsage > 0
      ? `${tokenUsage.toLocaleString()} / ${tokenBudgetLabel}`
      : `— / ${tokenBudgetLabel}`,
  );

  onMount(() => {
    const storedBuilderMode = window.localStorage.getItem(BUILDER_MODE_STORAGE_KEY);
    if (isBuilderMode(storedBuilderMode)) {
      activeBuilderMode = storedBuilderMode;
    }
    const interval = window.setInterval(() => {
      now = Date.now();
    }, 1000);
    return () => window.clearInterval(interval);
  });

  function formatDuration(seconds: number): string {
    const minutes = Math.floor(seconds / 60);
    const remainingSeconds = seconds % 60;
    return `${minutes}:${remainingSeconds.toString().padStart(2, "0")}`;
  }

  async function openRecentBench(id: string): Promise<void> {
    if (!benchStore) return;
    await benchStore.selectBench(id);
    onBenchChange?.();
  }

  // The Builder lens carries a caret. Clicking the lens body activates it;
  // clicking the caret toggles the persisted mode menu.
  let builderModeOpen = $state(false);
  let activeBuilderMode = $state<BuilderMode>("team");

  function isBuilderLens(id: AppLens): boolean {
    return /builder/i.test(id);
  }

  function toggleBuilderMode(event: Event): void {
    event.preventDefault();
    event.stopPropagation();
    builderModeOpen = !builderModeOpen;
  }

  function isBuilderMode(value: string | null): value is BuilderMode {
    return BUILDER_MODE_OPTIONS.some((option) => option.mode === value);
  }

  function selectBuilderMode(mode: BuilderMode): void {
    activeBuilderMode = mode;
    builderModeOpen = false;
  }

  async function openLensCreation(): Promise<void> {
    const builderLens = availableLenses.find((lens) => isBuilderLens(lens.id));
    if (!builderLens) return;
    builderModeOpen = false;
    await onSelectLens(builderLens.id);
  }

  $effect(() => {
    window.localStorage.setItem(BUILDER_MODE_STORAGE_KEY, activeBuilderMode);
  });

  $effect(() => {
    if (stageSurface === "builder" && !builderDeckModule) {
      builderDeckModule = import("./BuilderDeck.svelte");
    }
    if (stageSurface === "operator" && !operatorDashboardModule) {
      operatorDashboardModule = import("./OperatorDashboard.svelte");
    }
  });

  $effect(() => {
    const benchId = currentBench?.id ?? "";
    const needsTeams = stageModules.some((module) => module.kind === "active_team_topology");
    if (!benchId || !needsTeams || lastStageTeamBenchId === benchId) return;
    lastStageTeamBenchId = benchId;
    void loadStageTeams(benchId);
  });

  async function loadStageTeams(benchId: string): Promise<void> {
    try {
      stageTeams = await listTeams(benchId);
      stageTeamError = null;
    } catch (error) {
      stageTeamError =
        error instanceof Error ? error.message : "Failed to load stage team topology.";
    }
  }
</script>

<div
  class="window"
  class:no-topbar={!currentBench && !settingsMode}
  class:settings-mode={settingsMode}
>
  {#if settingsMode}
    <!--
      Settings shell takes the window over completely: topbar collapses to a
      single label and a × CLOSE affordance, nav becomes a parked bench-head
      plus a vertical page list, body is single-column.
    -->
    <header class="topbar settings-topbar">
      <span class="settings-label">
        {#if settingsBenchSlug}
          Bench settings · {settingsBench?.name ?? settingsBenchSlug}
        {:else}
          Settings · {settingsTab}
        {/if}
      </span>
      <button
        type="button"
        class="settings-close"
        onclick={closeSettings}
        title="Close settings"
      >
        × Close
      </button>
    </header>
  {:else if currentBench}
    <!--
      Top bar: lens tabs. Each tab is a live lens with a close affordance and
      a `+` to add another. When no bench is selected the bar drops out.
    -->
    <header class="topbar" role="tablist" aria-label="Lenses">
      {#each availableLenses as lens (lens.id)}
        {@const builder = isBuilderLens(lens.id)}
        <div class="lens-wrap" class:active={activeLens === lens.id}>
          <button
            class="lens"
            class:active={activeLens === lens.id}
            class:has-caret={builder}
            role="tab"
            aria-selected={activeLens === lens.id}
            onclick={() => onSelectLens(lens.id)}
          >
            <span class="lens-name">{lens.title.replace(/ lens$/i, "")}</span>
          </button>
          {#if builder}
            <button
              type="button"
              class="lens-caret"
              class:active={activeLens === lens.id}
              aria-haspopup="menu"
              aria-expanded={builderModeOpen}
              onclick={(event) => toggleBuilderMode(event)}
              title="Builder mode menu"
            >▾</button>
          {/if}
        </div>
      {/each}

      <button
        type="button"
        class="lens-add"
        disabled={!availableLenses.some((lens) => isBuilderLens(lens.id))}
        onclick={openLensCreation}
        title="Open Builder lens creation"
      >＋</button>

      {#if builderModeOpen && isBuilderLens(activeLens)}
        <div class="builder-pop" role="menu" aria-label="Builder mode">
          <div class="builder-pop-banner">Builder mode</div>
          {#each BUILDER_MODE_OPTIONS as item (item.mode)}
            <button
              type="button"
              class="builder-pop-item"
              class:active={activeBuilderMode === item.mode}
              role="menuitemradio"
              aria-checked={activeBuilderMode === item.mode}
              onclick={() => selectBuilderMode(item.mode)}
            >
              <span class="builder-pop-num">{item.num}</span>
              <span class="builder-pop-name">{item.name}</span>
              <span class="builder-pop-hint">{item.hint}</span>
            </button>
          {/each}
        </div>
      {/if}

      <!-- Stamp area: bench + runtime state. The bench/state portion is real;
           the runtime label (MANAGED/REMOTE) and timer are partial — see the
           mote on `.stamp-state-timer`. -->
      <div class="stamp">
        {#if currentBench}
          <span class="stamp-segment stamp-bench">Bench · {currentBench.name}</span>
          <span class="stamp-divider" aria-hidden="true">·</span>
          <span class="stamp-segment stamp-state" data-tone={stampState.tone}>
            {stampStateLabel}
          </span>
        {/if}
        {#if desktopRuntime}
          <span
            class="stamp-runtime"
            title={desktopRuntime.gatewayUrl}
          >{desktopRuntime.managedGateway ? "MANAGED" : "REMOTE"}</span>
        {/if}
        {#if onSelectTheme}
          <ThemeToggle mode={themeMode} onSelect={onSelectTheme} />
        {/if}
      </div>
    </header>
  {/if}

  <aside class="nav" class:settings={settingsMode} aria-label="Navigation">
    {#if settingsMode}
      <!--
        Settings nav: the parked bench-head is the wayfinding target × CLOSE
        returns to. It's a real link back to the active conversation (or the
        picker if none was open), so the block reads as navigation rather than
        a label that looks like a breadcrumb. UI audit f-16: the old "Return
        to / on close" fragment was a hanging, non-interactive label.
      -->
      <button
        type="button"
        class="bench-head settings-head return-link"
        onclick={closeSettings}
        aria-label={`Close settings and return to ${activeConversation?.id ? "conversation" : "bench picker"}`}
        title="Close settings"
      >
        <span class="return-mark">← Close</span>
        <h2 class="name">{settingsBench?.name ?? currentBench?.name ?? "Swage"}</h2>
      </button>
      <div class="settings-pages">
        {#if settingsBenchSlug}
          {#each BENCH_SETTINGS_PAGES as page (page.id)}
            <button
              type="button"
              class="settings-page"
              class:active={(settingsBenchTab ?? "overview") === page.id}
              onclick={() => onNavigate?.(`/settings/bench/${settingsBenchSlug}/${page.id}`)}
            >
              <span>{page.label}</span>
              <span class="ix">{page.ix}</span>
            </button>
          {/each}
        {:else}
          <!--
            Functional pages lead; non-functional ones collapse into a muted
            "Coming soon" group below, so they stay visible as a roadmap but
            stop competing for scan attention with working entries. UI audit
            f-5: the previous flat list interleaved five SHELL chips with the
            five working pages, reading as "half-finished product".
          -->
          {#each SETTINGS_PAGES.filter((page) => page.functional) as page (page.id)}
            <button
              type="button"
              class="settings-page"
              class:active={settingsTab === page.id}
              onclick={() => onNavigate?.(`/settings/${page.id}`)}
              title={page.label}
            >
              <span>{page.label}</span>
              <span class="ix">{page.ix}</span>
            </button>
          {/each}
          <div class="settings-coming-soon" role="separator" aria-label="Coming soon">
            <span class="coming-soon-label">Coming soon</span>
          </div>
          <div class="settings-coming-soon-grid">
            {#each SETTINGS_PAGES.filter((page) => !page.functional) as page (page.id)}
              <button
                type="button"
                class="settings-page shell"
                class:active={settingsTab === page.id}
                onclick={() => onNavigate?.(`/settings/${page.id}`)}
                title={`${page.label} (not yet wired)`}
              >
                <span>{page.label}</span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    {:else if currentBench}
      <!--
        Bench header. The back-arrow returns to the bench picker — a clear
        wayfinding target that the dropdown-only model lacked. The Switch
        dropdown stays for fast switching without leaving the current bench
        (UI audit f-7).
      -->
      <div class="bench-head">
        <button
          type="button"
          class="back-to-picker"
          aria-label="Back to bench picker"
          title="Back to bench picker"
          onclick={() => {
            benchStore?.closeBench();
            onNavigate?.("/");
          }}
        >
          <span aria-hidden="true">←</span>
        </button>
        <div class="crumb">
          <span class="bench-label">Bench</span>
          <span class="bench-count">{conversationCount} convos</span>
        </div>
        <h2 class="name">{currentBench.name}</h2>
        <BenchSelector {benchStore} {onBenchChange} />
      </div>

      <!-- Conversation list section. -->
      <div class="section">
        <div class="key">
          <span>Conversations</span>
          <span class="count">{conversationCount}</span>
        </div>
        <ConversationList store={conversationStore} />
      </div>

      <!-- Footer: global settings entry point and account placeholder. -->
      <div class="footer">
        <button
          class="footer-item"
          class:active={settingsTab !== null}
          onclick={() => onNavigate?.("/settings/providers")}
        >
          Settings
        </button>
        <button class="footer-item" disabled title="Account (not yet wired)">Account</button>
      </div>
    {:else}
      <!-- No bench selected: nav shows a "Recent" list plus a quick legend.
           Selection still happens in the body via BenchPicker; the nav's
           recent list is a fast lane for re-opening recent work. -->
      <div class="bench-head picker-mode">
        <div class="crumb">
          <span class="bench-label">Choose a bench</span>
        </div>
        <h2 class="name">Swage</h2>
      </div>
      <div class="section">
        <div class="key">
          <span>Recent</span>
        </div>
        {#if recentBenches.length > 0}
          <div class="recent-list">
            {#each recentBenches as bench (bench.id)}
              <button
                type="button"
                class="recent-bench"
                onclick={() => openRecentBench(bench.id)}
              >
                <span>{bench.name}</span>
                <span class="recent-meta">{bench.slug}</span>
              </button>
            {/each}
          </div>
        {:else}
          <p class="recent-shell-body">Open a bench once and it will stay here.</p>
        {/if}
      </div>
      <div class="section picker-legend">
        <p class="eyebrow">Two-stage select</p>
        <p class="legend-body">
          Click a bench card to preview, then open it. Or double-click to skip
          the preview.
        </p>
      </div>
      <div class="footer">
        <button
          class="footer-item"
          class:active={settingsTab !== null}
          onclick={() => onNavigate?.("/settings/providers")}
        >
          Settings
        </button>
        <button class="footer-item" disabled title="Account (not yet wired)">Account</button>
      </div>
    {/if}
  </aside>

  <main
    class="body"
    class:full={!currentBench || settingsMode}
    class:conversation-dominant={isConversationDominant && !settingsMode}
    class:stage-dominant={isStageDominant && !settingsMode}
  >
    {#if settingsBenchSlug}
      <BenchSettingsPage bench={settingsBench} tab={settingsBenchTab ?? "overview"} />
    {:else if settingsTab}
      <SettingsPage tab={settingsTab} />
    {:else if !currentBench}
      <!-- Bench picker view: full card grid with two-stage selection. -->
      <BenchPicker
        {benchStore}
        {onBenchChange}
        onOpenBenchSettings={(bench) => onNavigate?.(`/settings/bench/${bench.slug}`)}
      />
    {:else}
      <!-- Conversation pane: always present per Surfaces.md.
           The connection indicator only renders when a conversation is in
           scope — without one, no WebSocket is attempted, so a "lost" banner
           would be misleading. -->
      <section class="pane conversation-pane" aria-label="Conversation">
        <!-- The pane head carries the conversation's own running stat — token
             usage against the model budget. Live/working state is not repeated
             here: the topbar stamp and the streaming cursor already show it. -->
        <header class="pane-head">
          <span class="pane-label">Conversation</span>
          <span class="pane-index">tokens · {tokenUsageLabel}</span>
        </header>
        {#if activeConversation}
          <ConnectionIndicator store={connectionStore} {onRetry} />
        {/if}
        <MessageDisplay store={conversationStore} {streamingStore} {displayPreferences} />
        {#if streamingStore.pendingApproval}
          <!-- Approval replaces the input box. -->
          <ApprovalCard approval={streamingStore.pendingApproval} />
        {:else}
          <MessageInput
            store={streamingStore}
            modelOptions={modelOptions}
            selectedModelId={activeConversation?.selected_model_id ?? defaultModelId}
          />
        {/if}
      </section>

      <!-- Stage pane: shows the module placed by the active lens. -->
      {#if stageSurface}
        <section class="pane stage-pane" aria-label="Module stage">
          <header class="pane-head">
            <span class="pane-label">Stage</span>
            <span class="pane-index">
              {stageSurface === "builder" ? "Builder" : "Operator"}
            </span>
          </header>
          {#if stageSurface === "builder"}
            {#if builderDeckModule}
              {#await builderDeckModule then module}
                {@const BuilderDeck = module.default}
                <BuilderDeck
                  {benchStore}
                  {desktopRuntime}
                  {activeLens}
                  {activeBuilderMode}
                  {availableLenses}
                  {onSelectLens}
                  {onRefreshLenses}
                  showSharedReferences={false}
                  showOperatorDeck={false}
                />
              {:catch error}
                <div class="surface-error" role="alert">
                  {error instanceof Error ? error.message : "Failed to load builder."}
                </div>
              {/await}
            {:else}
              <div class="surface-skeleton" role="status" aria-label="Loading builder">
                <div class="sk sk-line sk-line-lg"></div>
                <div class="sk sk-line"></div>
                <div class="sk sk-line"></div>
                <div class="sk sk-line sk-line-sm"></div>
              </div>
            {/if}
          {:else if stageSurface === "operator"}
            {#if operatorDashboardModule}
              {#await operatorDashboardModule then module}
                {@const OperatorDashboard = module.default}
                <OperatorDashboard />
              {:catch error}
                <div class="surface-error" role="alert">
                  {error instanceof Error ? error.message : "Failed to load operator."}
                </div>
              {/await}
            {:else}
              <div class="surface-skeleton" role="status" aria-label="Loading operator">
                <div class="sk sk-line sk-line-lg"></div>
                <div class="sk sk-line"></div>
                <div class="sk sk-line sk-line-sm"></div>
              </div>
            {/if}
          {/if}
        </section>
      {:else if hasPlacedStageModules}
        <section class="pane stage-pane stage-shell" aria-label="Module stage">
          <header class="pane-head">
            <span class="pane-label">Stage</span>
            <span class="pane-index">{stageModules.length} module(s)</span>
          </header>
          <div class="stage-modules">
            {#each stageModules as module (module.id)}
              {#if module.kind === "shared_reference" && currentBench}
                <article class="stage-module" data-tone="auto">
                  <header class="stage-module-head">
                    <h3>{module.title}</h3>
                    <span class="stage-module-tag">Live</span>
                  </header>
                  <SharedReferenceStudio benchId={currentBench.id} />
                </article>
              {:else if module.kind === "active_team_topology"}
                <article class="stage-module" data-tone="streaming">
                  <header class="stage-module-head">
                    <h3>{module.title}</h3>
                    <span class="stage-module-tag">Live</span>
                  </header>
                  {#if stageTeamError}
                    <p class="stage-module-body error">{stageTeamError}</p>
                  {:else}
                    <div class="stage-team-list">
                      {#each stageTeams.filter((team) => module.team_ids.length === 0 || module.team_ids.includes(team.id) || module.team_ids.includes(team.name)) as team (team.id)}
                        <article class="stage-team-card">
                          <strong>{team.name}</strong>
                          <span>{team.agents?.length ?? team.members.length} agent(s)</span>
                          <span>entry · {team.entry_agent_id}</span>
                        </article>
                      {:else}
                        <p class="stage-module-body">
                          No matching team topology is available for this lens yet.
                        </p>
                      {/each}
                    </div>
                  {/if}
                </article>
              {/if}
            {/each}
          </div>
        </section>
      {/if}
    {/if}
  </main>
</div>

<style>
  /* ---------------------------------------------------------------------------
     Window — outer frame.
     --------------------------------------------------------------------------- */

  .window {
    width: 100%;
    height: 100%;
    background: var(--paper);
    color: var(--ink);
    border: var(--border-strong);
    display: grid;
    grid-template-rows: 48px 1fr;
    /* Fluid chrome: the nav shrinks to content rather than claiming a fixed
       280px allotment. It yields before the conversation pane (the primary
       surface) does. UI audit f-21. */
    grid-template-columns: clamp(220px, 18vw, 280px) 1fr;
    grid-template-areas:
      "nav top"
      "nav body";
    font-family: var(--font-body);
    font-size: var(--font-body-sm);
    /* The translucent orange offset shadow needs the window above its trail. */
    isolation: isolate;
    position: relative;
  }

  .window.no-topbar {
    grid-template-rows: 1fr;
    grid-template-areas: "nav body";
  }

  .window.no-topbar .topbar {
    display: none;
  }

  /* ---------------------------------------------------------------------------
     Settings shell — overrides for the settings-mode window.
     The topbar collapses to a label + close, the nav swaps the conversation
     list for a settings page list, and the body runs full width.
     --------------------------------------------------------------------------- */

  .settings-topbar {
    grid-area: top;
    display: flex;
    align-items: stretch;
    background: var(--ink);
    color: var(--paper);
    border-bottom: 2px solid var(--ink);
  }

  .settings-label {
    flex: 1;
    display: flex;
    align-items: center;
    padding: 0 24px;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 22px;
    line-height: 1;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .settings-close {
    padding: 0 22px;
    display: flex;
    align-items: center;
    background: transparent;
    color: var(--paper);
    border: none;
    border-left: 1px solid rgba(255, 248, 238, 0.3);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .settings-close:hover {
    background: var(--accent);
  }

  /* Parked bench-head in settings nav: now a real link back to the
     conversation/picker rather than a label that looks like a breadcrumb.
     Same ink block as the regular nav, but the orange registration band
     underneath is suppressed (the bench isn't currently active, just the
     destination of × CLOSE). UI audit f-16. */
  .nav.settings .bench-head {
    padding: 12px 16px;
    width: 100%;
    text-align: left;
    background: var(--ink);
    color: var(--paper);
    cursor: pointer;
    border: none;
    font: inherit;
  }

  .nav.settings .bench-head:hover {
    background: var(--ink-soft);
  }

  .nav.settings .bench-head:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .nav.settings .bench-head::after {
    display: none;
  }

  .return-mark {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: rgba(255, 248, 238, 0.7);
    margin-bottom: 6px;
  }

  .nav.settings .bench-head .name {
    font-size: var(--font-display-2);
    letter-spacing: 0.03em;
  }

  /* Back-to-picker affordance in the active bench header — a clear wayfinding
     target that the dropdown-only model lacked. UI audit f-7. */
  .back-to-picker {
    flex-shrink: 0;
    background: transparent;
    border: 1px solid rgba(255, 248, 238, 0.4);
    color: var(--paper);
    width: 28px;
    height: 28px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
    margin-bottom: 8px;
    transition: border-color 120ms ease, background 120ms ease;
  }

  .back-to-picker:hover,
  .back-to-picker:focus-visible {
    border-color: var(--paper);
    background: rgba(255, 248, 238, 0.12);
  }

  .back-to-picker:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .settings-pages {
    padding: 12px 14px;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
    background: var(--paper);
  }

  .settings-page {
    position: relative;
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 10px 14px;
    background: transparent;
    color: var(--ink);
    border: 1px solid transparent;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-label-md);
    line-height: 1;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    cursor: pointer;
    isolation: isolate;
  }

  .settings-page:hover:not(.active) {
    background: rgba(14, 59, 102, 0.08);
  }

  .settings-page.active {
    background: var(--ink);
    color: var(--paper);
  }

  .settings-page.active::before {
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

  .settings-page .ix {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    line-height: 1;
    opacity: 0.6;
  }

  /* Coming-soon group — functional pages lead; non-functional ones collapse
     into a muted footer so the roadmap stays visible without competing for
     scan attention. UI audit f-5. */
  .settings-coming-soon {
    margin: 14px 0 4px;
    padding-top: 10px;
    border-top: 1px dotted var(--rule-soft);
  }

  .coming-soon-label {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
    opacity: 0.7;
  }

  .settings-coming-soon-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    padding-top: 4px;
  }

  .settings-coming-soon-grid .settings-page {
    /* Low-emphasis entries — they read as a roadmap, not as peers of the
       working pages above. */
    padding: 4px 0;
    color: var(--ink-soft);
    opacity: 0.7;
    letter-spacing: 0.1em;
  }

  .settings-coming-soon-grid .settings-page:hover:not(.active) {
    background: transparent;
    opacity: 1;
    color: var(--ink);
  }

  /* ---------------------------------------------------------------------------
     Top bar — lens tabs.
     --------------------------------------------------------------------------- */

  .topbar {
    grid-area: top;
    display: flex;
    border-bottom: 2px solid var(--ink);
    position: relative;
  }

  /* Hairline along the panel edge, drawn over the lens backgrounds so it
     reads on cream the same way the lens-to-lens dividers do. */
  .topbar::before {
    content: "";
    position: absolute;
    top: 0;
    bottom: 2px;
    left: 0;
    width: 1px;
    background: var(--rule-soft);
    z-index: 1;
    pointer-events: none;
  }

  .lens {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 22px;
    border-right: 1px solid var(--rule-soft);
    font-family: var(--font-display);
    font-size: 18px;
    line-height: 1;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink);
    background: transparent;
    cursor: pointer;
    position: relative;
  }

  .lens:hover {
    background: rgba(14, 59, 102, 0.08);
  }

  .lens.active {
    background: var(--ink);
    color: var(--paper);
  }

  .lens.active::before {
    content: "";
    width: 8px;
    height: 8px;
    background: var(--accent);
    margin-right: 6px;
    display: inline-block;
  }

  .lens-add {
    padding: 0 16px;
    display: flex;
    align-items: center;
    font-family: var(--font-display);
    font-size: 22px;
    line-height: 1;
    color: var(--ink);
    background: transparent;
    cursor: pointer;
  }

  .lens-add:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .stamp {
    margin-left: auto;
    padding: 0 22px;
    display: flex;
    align-items: center;
    gap: 14px;
    border-left: 1px solid var(--rule-soft);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    /* Tells ThemeToggle what colour to paint as the active button text.
       The toggle's .opt.active uses currentColor (ink) as background, so the
       text needs to flip to paper. */
    --toggle-active-fg: var(--paper);
  }

  .stamp-segment {
    color: var(--ink);
    letter-spacing: 0.16em;
  }

  .stamp-bench {
    color: var(--ink);
  }

  .stamp-divider {
    color: var(--ink-soft);
  }

  .stamp-state {
    padding: 2px 6px;
    border: 1px solid var(--ink);
  }

  .stamp-state[data-tone="busy"] {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
  }

  .stamp-state[data-tone="alert"] {
    background: var(--paper);
    color: var(--ink-orange);
    border-color: var(--accent);
  }

  .stamp-runtime {
    color: var(--ink-soft);
    text-transform: uppercase;
  }

  /* Caret as a separate button beside the Builder lens. The split lets
     clicking the lens body activate it without opening the
     menu, while the caret toggles the menu independently. */
  .lens-wrap {
    display: flex;
    align-items: stretch;
    border-right: 1px solid var(--rule-soft);
  }

  .lens-wrap .lens {
    border-right: none;
  }

  .lens-caret {
    display: flex;
    align-items: center;
    padding: 0 10px;
    background: transparent;
    color: var(--ink);
    border: none;
    border-left: 1px dotted var(--rule-soft);
    font-family: var(--font-display);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
  }

  .lens-caret.active {
    background: var(--ink);
    color: var(--paper);
  }

  .lens-caret:hover {
    background: rgba(14, 59, 102, 0.08);
  }

  .lens-caret.active:hover {
    background: var(--ink);
    color: var(--paper);
    filter: brightness(1.1);
  }

  /* Builder mode dropdown. */
  .builder-pop {
    position: absolute;
    top: 48px;
    left: 22px;
    width: 240px;
    background: var(--paper);
    border: 2px solid var(--ink);
    z-index: 20;
    isolation: isolate;
    box-shadow: 4px 4px 0 var(--accent);
  }

  .builder-pop-banner {
    padding: 6px 10px;
    background: var(--accent);
    color: var(--paper);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .builder-pop-item {
    display: grid;
    grid-template-columns: 28px 1fr auto;
    gap: 8px;
    align-items: center;
    width: 100%;
    padding: 8px 10px;
    border: none;
    border-bottom: 1px dotted var(--rule-soft);
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-body);
    font-size: 12px;
    text-align: left;
    text-transform: none;
    cursor: pointer;
  }

  .builder-pop-item:last-child {
    border-bottom: none;
  }

  .builder-pop-item:hover,
  .builder-pop-item.active {
    background: var(--ink);
    color: var(--paper);
  }

  .builder-pop-item:hover .builder-pop-num,
  .builder-pop-item:hover .builder-pop-hint,
  .builder-pop-item.active .builder-pop-num,
  .builder-pop-item.active .builder-pop-hint {
    color: var(--paper);
  }

  .builder-pop-num {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.14em;
    color: var(--ink-soft);
  }

  .builder-pop-name {
    font-weight: 600;
  }

  .builder-pop-hint {
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.04em;
  }

  /* ---------------------------------------------------------------------------
     Nav — bench head, conversation list, footer.
     --------------------------------------------------------------------------- */

  .nav {
    grid-area: nav;
    border-right: 2px solid var(--ink);
    display: flex;
    flex-direction: column;
    background: var(--paper);
    isolation: isolate;
    min-height: 0;
  }

  .bench-head {
    background: var(--ink);
    color: var(--paper);
    /* Density hierarchy: chrome padding tightens to 12–16px; generous air is
       reserved for content surfaces. UI audit f-21. */
    padding: 12px 16px;
    position: relative;
  }

  /* Orange band along the bottom edge — the "ink registration" stripe. */
  .bench-head:not(.picker-mode)::after {
    content: "";
    position: absolute;
    bottom: -2px;
    left: 0;
    right: 0;
    height: 2px;
    background: var(--accent);
  }

  .bench-head.picker-mode {
    background: var(--paper);
    color: var(--ink);
    text-align: center;
    border-bottom: var(--border-strong);
  }

  .bench-head .crumb {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    letter-spacing: 0.18em;
    text-transform: uppercase;
    display: flex;
    justify-content: space-between;
    color: rgba(255, 248, 238, 0.7);
    margin-bottom: 8px;
  }

  .bench-head.picker-mode .crumb {
    color: var(--ink-soft);
    justify-content: center;
  }

  .bench-head .name {
    font-family: var(--font-display);
    font-weight: 400;
    /* Right-size display type to content: a single bench name doesn't need
       36px. 24–28px is still emphatically Bebas. UI audit f-21. */
    font-size: var(--font-display-2);
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    margin: 0;
  }

  .section {
    padding: 16px 22px;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .section .key {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink);
    display: flex;
    justify-content: space-between;
    border-bottom: 1px solid var(--ink);
    padding-bottom: 8px;
    margin-bottom: 10px;
    flex-shrink: 0;
  }

  .section .key .count {
    background: var(--accent);
    color: var(--paper);
    padding: 2px 6px;
  }

  .footer {
    margin-top: auto;
    border-top: 2px solid var(--ink);
    display: grid;
    grid-template-columns: 1fr 1fr;
    flex-shrink: 0;
  }

  .footer-item {
    padding: 14px;
    font-family: var(--font-display);
    font-size: 16px;
    line-height: 1;
    letter-spacing: 0.08em;
    text-align: center;
    text-transform: uppercase;
    border-right: 1px solid var(--rule-soft);
    color: var(--ink);
    background: transparent;
    cursor: pointer;
  }

  .footer-item:last-child {
    border-right: none;
  }

  .footer-item:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
  }

  .footer-item:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .recent-shell-body {
    margin: 0;
    color: var(--ink-soft);
    font-size: 12.5px;
    line-height: 1.5;
  }

  .recent-list {
    display: grid;
    gap: 8px;
  }

  .recent-bench {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 4px;
    padding: 10px;
    border: 1px solid var(--rule-soft);
    background: transparent;
    color: var(--ink);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .recent-bench:hover {
    background: var(--ink);
    color: var(--paper);
  }

  .recent-meta {
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .recent-bench:hover .recent-meta {
    color: var(--paper);
    opacity: 0.72;
  }

  /* Legend block shown in the nav when no bench is selected. */
  .picker-legend {
    gap: 8px;
  }

  .picker-legend .eyebrow {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
    margin: 0;
  }

  .legend-body {
    margin: 0;
    color: var(--ink-soft);
    font-size: 12.5px;
    line-height: 1.5;
  }

  /* ---------------------------------------------------------------------------
     Body — conversation pane and optional stage.
     --------------------------------------------------------------------------- */

  .body {
    grid-area: body;
    display: grid;
    grid-template-columns: 1fr 360px;
    min-height: 0;
    min-width: 0;
  }

  .body.full {
    grid-template-columns: 1fr;
  }

  /* Conversation-dominant lenses keep the conversation pane wide and park the
     module pane as a narrow rail. */
  .body.conversation-dominant {
    grid-template-columns: minmax(0, 1fr) 320px;
  }

  .body.conversation-dominant > .conversation-pane {
    grid-column: 1;
  }

  .body.conversation-dominant > .stage-pane {
    grid-column: 2;
  }

  /* Stage-dominant lenses put the module pane wide and the conversation narrow. */
  .body.stage-dominant {
    grid-template-columns: 360px minmax(0, 1fr);
  }

  .body.stage-dominant > .conversation-pane {
    grid-column: 1;
  }

  .body.stage-dominant > .stage-pane {
    grid-column: 2;
  }

  :global(body.print-marks-minimal) .topbar::before,
  :global(body.print-marks-minimal) .bench-head::after,
  :global(body.print-marks-off) .topbar::before,
  :global(body.print-marks-off) .bench-head::after,
  :global(body.print-marks-off) .lens.active::before,
  :global(body.print-marks-off) .settings-page.active::before {
    display: none;
  }

  .pane {
    border-left: 2px solid var(--ink);
    padding: 18px 22px;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  .pane:first-child {
    border-left: none;
  }

  .conversation-pane {
    /* The conversation pane runs the full height with the input pinned below
       — children handle their own scrolling. */
    padding: 0;
  }

  .stage-pane {
    background: var(--paper);
  }

  /* Pane heads place a small label + index above each pane. Index expresses
     live state such as "LIVE", "Builder", or a count. */
  .pane-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 10px 22px 8px;
    border-bottom: 1px solid var(--ink);
    flex-shrink: 0;
    background: var(--paper);
  }

  .pane-label {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .pane-index {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  /* Stage modules placed by the active lens runtime. */
  .stage-pane.stage-shell {
    padding: 0;
    gap: 0;
  }

  .stage-modules {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 22px 22px;
    overflow-y: auto;
  }

  .stage-module {
    border: 2px solid var(--ink);
    background: var(--paper);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    position: relative;
  }

  /* Tonal hint on the left edge: auto = ink rule, streaming = orange rule. */
  .stage-module[data-tone="auto"] {
    border-left-width: 4px;
  }

  .stage-module[data-tone="streaming"] {
    border-left: 4px solid var(--accent);
  }

  .stage-module-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    border-bottom: 1px solid var(--ink);
    padding-bottom: 6px;
  }

  .stage-module h3 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 16px;
    line-height: 1.05;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .stage-module-tag {
    padding: 2px 6px;
    background: var(--accent);
    color: var(--paper);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .stage-module-body {
    margin: 0;
    color: var(--ink-soft);
    font-size: 12.5px;
    line-height: 1.55;
  }

  .stage-module-body.error {
    color: var(--ink-orange);
  }

  .stage-team-list {
    display: grid;
    gap: 8px;
  }

  .stage-team-card {
    display: grid;
    gap: 4px;
    padding: 10px 0;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .stage-team-card:last-child {
    border-bottom: none;
  }

  .stage-team-card strong {
    color: var(--ink);
    font-size: 14px;
  }

  .stage-team-card span {
    color: var(--ink-soft);
    font-size: 12px;
    line-height: 1.4;
  }

  .surface-error {
    margin: auto;
    padding: 14px 18px;
    font-family: var(--font-mono);
    font-size: var(--font-label-md);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--error);
    border: 1px dashed var(--error);
  }

  /* Low-fidelity skeleton: the layout paints before the lazy module fills it,
     so the surface reads as "the right thing is loading" rather than a bare
     text fragment. aria-label on the wrapper carries the announcement. */
  .surface-skeleton {
    margin: auto;
    width: 100%;
    max-width: 420px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 16px 18px;
  }

  .sk {
    height: 12px;
    background: var(--paper-deep);
    border-left: 3px solid var(--rule-soft);
    opacity: 0.7;
    animation: sk-pulse 1.4s ease-in-out infinite;
  }

  .sk-line { width: 100%; }
  .sk-line-lg { width: 60%; height: 18px; }
  .sk-line-sm { width: 35%; }

  @keyframes sk-pulse {
    0%, 100% { opacity: 0.55; }
    50% { opacity: 0.85; }
  }

  @media (prefers-reduced-motion: reduce) {
    .sk { animation: none; }
  }

  /* ---------------------------------------------------------------------------
     Narrow viewport — single-column fall-back.
     The 280px nav can't share the screen with a 320+px pane below ~720px wide.
     --------------------------------------------------------------------------- */
  @media (max-width: 720px) {
    .window {
      grid-template-columns: 1fr;
      grid-template-rows: auto auto 1fr;
      grid-template-areas:
        "top"
        "nav"
        "body";
    }

    .window.no-topbar {
      grid-template-rows: auto 1fr;
      grid-template-areas:
        "nav"
        "body";
    }

    .nav {
      border-right: none;
      border-bottom: 2px solid var(--ink);
      max-height: 50vh;
    }

    .body,
    .body.full,
    .body.conversation-dominant,
    .body.stage-dominant {
      grid-template-columns: 1fr;
    }

    .stage-pane {
      display: none;
    }
  }
</style>
