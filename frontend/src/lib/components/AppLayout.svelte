<!--
  Riso/cyanotype shell.

  Layout follows the design spec in
  Documents/Notes/Other Notes/Initiatives/Active/Swage/UI/riso-cyanotype.html:

    .window
      ┌──────────────┬───────────────────────────────────────────────┐
      │              │  topbar — lens tabs (collapsed when no bench) │
      │   nav        ├───────────────────────────────────────────────┤
      │ (280px wide) │  body — conversation pane and optional stage  │
      └──────────────┴───────────────────────────────────────────────┘

  When no bench is selected the top bar collapses and the body becomes a
  full-width bench picker. Switching benches goes through BenchSelector,
  which resets the layout via onBenchChange.

  WorkbenchRail (the right rail) was dropped per Surfaces.md — shared
  reference objects now sit on the module stage like any other module.

  Surface switching: the lens descriptor still exposes ui_modules, but the
  riso design does not have per-lens surface tabs. We honour the lens's
  default_surface and place the corresponding module on the stage.
  TODO(debt): reconcile lens.ui_modules with the single-surface-per-lens
  design once lens definitions are reshaped. Tracked: future UI cleanup.
-->
<script lang="ts">
  import ConversationList from "./ConversationList.svelte";
  import BenchSelector from "./BenchSelector.svelte";
  import BenchPicker from "./BenchPicker.svelte";
  import MessageDisplay from "./MessageDisplay.svelte";
  import MessageInput from "./MessageInput.svelte";
  import ConnectionIndicator from "./ConnectionIndicator.svelte";
  import SettingsPage from "./SettingsPage.svelte";
  import ThemeToggle from "./ThemeToggle.svelte";
  import type { Route } from "../router";
  import type { ThemeMode } from "../theme";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type { AppLens, LensDescriptor, LensSurface } from "../types";
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
    activeSurface: LensSurface;
    onSelectLens: (next: AppLens) => void | Promise<void>;
    onRefreshLenses: (preferredSurface?: LensSurface) => void | Promise<void>;
    onSelectSurface: (next: LensSurface) => void;
    desktopRuntime?: DesktopRuntimeInfo | null;
    themeMode?: ThemeMode;
    onSelectTheme?: (mode: ThemeMode) => void;
    route?: Route;
    onNavigate?: (path: string) => void;
  }

  // activeSurface and onSelectSurface are kept in Props for API compatibility
  // with App.svelte but the new shell ignores them: the lens determines what
  // sits on the stage. Each lens picks one layout per Progress.md.
  let {
    conversationStore,
    connectionStore,
    streamingStore,
    benchStore,
    onBenchChange,
    onRetry,
    activeLens,
    availableLenses,
    onSelectLens,
    onRefreshLenses,
    desktopRuntime,
    themeMode = "system",
    onSelectTheme,
    route,
    onNavigate,
  }: Props = $props();

  // Lazy-load stage modules so a builder-only crate isn't pulled in for
  // assistant-lens users. The dynamic import resolves once on first use.
  let builderDeckModule = $state<Promise<BuilderDeckModule> | null>(null);
  let operatorDashboardModule = $state<Promise<OperatorDashboardModule> | null>(null);

  const activeLensDescriptor = $derived(
    availableLenses.find((lens) => lens.id === activeLens) ?? null,
  );
  const currentBench = $derived(benchStore?.currentBench ?? null);
  const conversationCount = $derived(conversationStore.conversations.length);
  const activeConversation = $derived(conversationStore.activeConversation);
  const settingsTab = $derived(route?.settingsTab ?? null);

  // Settings sub-pages — drives both the URL the page list links to and the
  // ix counter shown beside each entry. Kept in this component because the
  // settings nav is part of the window chrome, not the body pane.
  const SETTINGS_PAGES: ReadonlyArray<{
    id: NonNullable<typeof settingsTab>;
    label: string;
    ix: string;
  }> = [
    { id: "providers", label: "Providers", ix: "—" },
    { id: "generation", label: "Generation", ix: "—" },
    { id: "automation", label: "Automation", ix: "—" },
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

  // Stage-dominant layout: conversation moves to the narrow side when a
  // module is on the stage. Per Progress.md, each lens picks one layout —
  // this isn't an on-demand swap.
  const isStageDominant = $derived(stageSurface !== null);

  $effect(() => {
    if (stageSurface === "builder" && !builderDeckModule) {
      builderDeckModule = import("./BuilderDeck.svelte");
    }
    if (stageSurface === "operator" && !operatorDashboardModule) {
      operatorDashboardModule = import("./OperatorDashboard.svelte");
    }
  });
</script>

<div
  class="window"
  class:no-topbar={!currentBench && !settingsTab}
  class:settings-mode={!!settingsTab}
>
  {#if settingsTab}
    <!--
      Settings shell takes the window over completely (per riso-cyanotype.html
      design 03 — Settings — display): topbar collapses to a single label and
      a × CLOSE affordance, nav becomes a parked bench-head plus a vertical
      page list, body is single-column.
    -->
    <header class="topbar settings-topbar">
      <span class="settings-label">Settings · {settingsTab}</span>
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
        <button
          class="lens"
          class:active={activeLens === lens.id}
          role="tab"
          aria-selected={activeLens === lens.id}
          onclick={() => onSelectLens(lens.id)}
        >
          <span class="lens-name">{lens.title.replace(/ lens$/i, "")}</span>
          <!--
            The caret is decorative — the dropdown affordance only applies to
            the builder lens (mode picker). TODO(debt): wire the builder mode
            menu (team / agent / context / lens / skill / tool). Blocked by:
            backend doesn't yet expose the mode dispatcher.
          -->
        </button>
      {/each}
      <!-- '+' to add a new lens. TODO(debt): no backend yet — render but disabled. -->
      <button class="lens-add" disabled title="Add lens (not yet wired)">＋</button>

      <!-- Stamp area: identity / runtime info + theme toggle. Kept minimal
           per Principles.md ("working state stays out of the way"). -->
      <div class="stamp">
        {#if desktopRuntime}
          <span class="stamp-label" title={desktopRuntime.gatewayUrl}>
            {desktopRuntime.managedGateway ? "MANAGED" : "REMOTE"}
          </span>
        {/if}
        {#if onSelectTheme}
          <ThemeToggle mode={themeMode} onSelect={onSelectTheme} />
        {/if}
      </div>
    </header>
  {/if}

  <aside class="nav" class:settings={!!settingsTab} aria-label="Navigation">
    {#if settingsTab}
      <!--
        Settings nav: parked bench-head shows where × CLOSE returns to,
        then a vertical list of settings pages drives the active sub-route.
      -->
      <div class="bench-head settings-head">
        <div class="crumb">
          <span class="return-mark">Return to</span>
        </div>
        <h2 class="name">{currentBench?.name ?? "Swage"}</h2>
        <div class="meta">on close</div>
      </div>
      <div class="settings-pages">
        {#each SETTINGS_PAGES as page (page.id)}
          <button
            type="button"
            class="settings-page"
            class:active={settingsTab === page.id}
            onclick={() => onNavigate?.(`/settings/${page.id}`)}
          >
            <span>{page.label}</span>
            <span class="ix">{page.ix}</span>
          </button>
        {/each}
      </div>
    {:else if currentBench}
      <!-- Bench header — large display name, becomes back-arrow + secondary
           menu in future iterations. -->
      <div class="bench-head">
        <div class="crumb">
          <span class="bench-label">Bench</span>
          <span class="bench-count">{conversationCount} convos</span>
        </div>
        <h2 class="name">{currentBench.name}</h2>
        <!-- TODO(debt): swap BenchSelector dropdown for a back-arrow that
             returns to the bench picker. The picker is not yet a dedicated
             route. Blocked by: bench picker view component. -->
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
      <!-- No bench selected: nav becomes the wordmark + a quick legend.
           Selection happens in the body via BenchPicker; duplicating the
           switcher here would split the user's attention. -->
      <div class="bench-head picker-mode">
        <div class="crumb">
          <span class="bench-label">No bench</span>
        </div>
        <h2 class="name">Swage</h2>
      </div>
      <div class="section picker-legend">
        <p class="eyebrow">Two-stage select</p>
        <p class="legend-body">
          Click a bench card to preview, then open it. Or double-click to skip
          the preview.
        </p>
      </div>
    {/if}
  </aside>

  <main
    class="body"
    class:full={!currentBench || !!settingsTab}
    class:stage-dominant={isStageDominant && !settingsTab}
  >
    {#if settingsTab}
      <SettingsPage tab={settingsTab} />
    {:else if !currentBench}
      <!-- Bench picker view: full card grid with two-stage selection. -->
      <BenchPicker {benchStore} {onBenchChange} />
    {:else}
      <!-- Conversation pane: always present per Surfaces.md.
           The connection indicator only renders when a conversation is in
           scope — without one, no WebSocket is attempted, so a "lost" banner
           would be misleading. -->
      <section class="pane conversation-pane" aria-label="Conversation">
        {#if activeConversation}
          <ConnectionIndicator store={connectionStore} {onRetry} />
        {/if}
        <MessageDisplay store={conversationStore} {streamingStore} />
        <MessageInput store={streamingStore} />
      </section>

      <!-- Stage pane: shows the module placed by the active lens. -->
      {#if stageSurface}
        <section class="pane stage-pane" aria-label="Module stage">
          {#if stageSurface === "builder"}
            {#if builderDeckModule}
              {#await builderDeckModule then module}
                {@const BuilderDeck = module.default}
                <BuilderDeck
                  {benchStore}
                  {desktopRuntime}
                  {activeLens}
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
              <div class="surface-loading">Loading builder…</div>
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
              <div class="surface-loading">Loading operator…</div>
            {/if}
          {/if}
        </section>
      {:else if activeConversation}
        <!-- No module placed by this lens. Render an empty stage state in the
             riso style — preserves the visual rhythm without inventing data. -->
        <section class="pane stage-pane stage-empty" aria-label="Module stage">
          <p class="eyebrow">Module stage</p>
          <p class="stage-empty-body">
            This lens does not place a module on the stage.
          </p>
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
    border: 2px solid var(--ink);
    display: grid;
    grid-template-rows: 48px 1fr;
    grid-template-columns: 280px 1fr;
    grid-template-areas:
      "nav top"
      "nav body";
    font-family: var(--font-body);
    font-size: 13px;
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

  /* Parked bench-head in settings nav: same ink block as the regular nav,
     but the orange registration band underneath is suppressed (the bench
     isn't currently active, just the destination of × CLOSE). */
  .nav.settings .bench-head {
    padding: 14px 22px;
  }

  .nav.settings .bench-head::after {
    display: none;
  }

  .nav.settings .bench-head .crumb {
    margin-bottom: 6px;
  }

  .return-mark {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    letter-spacing: 0.18em;
  }

  /* Decorative pip — orange on cream is below AA so it stays as a pure
     graphic element; the text colour reads against the ink block instead. */
  .return-mark::before {
    content: "";
    width: 8px;
    height: 8px;
    background: var(--accent);
  }

  .nav.settings .bench-head .name {
    font-size: 26px;
    letter-spacing: 0.03em;
  }

  .nav.settings .bench-head .meta {
    margin-top: 4px;
    font-family: var(--font-body);
    font-weight: 500;
    font-size: 11px;
    line-height: 1.5;
    color: rgba(255, 248, 238, 0.7);
  }

  .settings-pages {
    padding: 16px 14px;
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
    font-size: 12px;
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
    font-size: 10px;
    line-height: 1;
    opacity: 0.6;
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
    /* Tells ThemeToggle what colour to paint as the active button text.
       The toggle's .opt.active uses currentColor (ink) as background, so the
       text needs to flip to paper. */
    --toggle-active-fg: var(--paper);
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
    padding: 18px 22px;
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
    border-bottom: 2px solid var(--ink);
  }

  .bench-head .crumb {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    display: flex;
    justify-content: space-between;
    color: rgba(255, 248, 238, 0.7);
    margin-bottom: 10px;
  }

  .bench-head.picker-mode .crumb {
    color: var(--ink-soft);
    justify-content: center;
  }

  .bench-head .name {
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 36px;
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

  /* Stage-dominant lenses (builder, team-running) put the module pane wide
     and the conversation narrow. */
  .body.stage-dominant {
    grid-template-columns: 360px 1fr;
  }

  .body.stage-dominant > .conversation-pane {
    grid-column: 1;
  }

  .body.stage-dominant > .stage-pane {
    grid-column: 2;
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

  .stage-pane.stage-empty {
    /* Empty state when a lens places no module — preserves the visual frame
       without inventing fake content. */
    align-items: flex-start;
    justify-content: flex-start;
    gap: 10px;
    color: var(--ink-soft);
  }

  .stage-empty-body {
    font-family: var(--font-body);
    font-size: 13px;
    color: var(--ink-soft);
    max-width: 36ch;
  }

  .surface-loading,
  .surface-error {
    margin: auto;
    padding: 14px 18px;
    font-family: var(--font-mono);
    font-size: 12px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink-orange);
    border: 1px dashed var(--accent);
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
    .body.stage-dominant {
      grid-template-columns: 1fr;
    }

    .stage-pane {
      display: none;
    }
  }
</style>
