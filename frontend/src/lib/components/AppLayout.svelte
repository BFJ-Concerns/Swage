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
  import ConversationList from "./ConversationList.svelte";
  import BenchSelector from "./BenchSelector.svelte";
  import BenchPicker from "./BenchPicker.svelte";
  import MessageDisplay from "./MessageDisplay.svelte";
  import MessageInput from "./MessageInput.svelte";
  import ApprovalCard from "./ApprovalCard.svelte";
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
  // ix counter shown beside each entry. The list mirrors the Riso workup
  // (`riso-cyanotype.html`, view 06): Plugins, Automations, Channels, Nodes,
  // Voice, Recovery, Display, Account. Pages whose backends don't exist yet
  // render shells (see SettingsPage.svelte) — the entries land here so the nav
  // looks complete and the routes resolve.
  //
  // TODO type=issue priority=normal tags=ui,settings
  // type: issue
  // Several settings entries route to non-functional shells (plugins,
  // channels, nodes, voice, recovery, display, account). Promote each to a
  // task once its underlying backend (or, for display, a preferences API)
  // is scoped. The shell pages themselves carry per-page motes describing
  // the gap.
  // ---
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
    { id: "recovery", label: "Recovery", ix: "—", functional: false },
    { id: "display", label: "Display", ix: "—", functional: false },
    { id: "account", label: "Account", ix: "—", functional: false },
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

  // Stamp shown on the right of the topbar — `BENCH · NAME · STATE` per
  // workup view 01/02/03/07. The bench portion is real; the state cycles
  // through the streaming store. Team-live and approval states are real
  // too, but the workup also uses time-on-state ("TEAM LIVE 04:18") which
  // we do not have an aggregator for yet (see mote below).
  const stampState = $derived<{ label: string; tone: "idle" | "busy" | "alert" }>(
    streamingStore.isStreaming
      ? { label: "Working", tone: "busy" }
      : streamingStore.turnError
        ? { label: "Retrying", tone: "alert" }
        : { label: "Idle", tone: "idle" },
  );

  // The Builder lens carries a caret (workup view 05). Clicking the lens body
  // activates it; clicking the caret toggles the mode menu independently. The
  // menu is rendered as a non-functional shell because the gateway has no
  // mode dispatcher — see the mote on the dropdown markup below.
  let builderModeOpen = $state(false);

  function isBuilderLens(id: AppLens): boolean {
    return /builder/i.test(id);
  }

  function toggleBuilderMode(event: Event): void {
    event.preventDefault();
    event.stopPropagation();
    builderModeOpen = !builderModeOpen;
  }

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
      Settings shell takes the window over completely: topbar collapses to a
      single label and a × CLOSE affordance, nav becomes a parked bench-head
      plus a vertical page list, body is single-column.
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
              title="Builder mode menu (workup view 05)"
            >▾</button>
          {/if}
        </div>
      {/each}

      <!--
        TODO type=task priority=normal tags=ui,lens,topbar
        Wire the `+` affordance to the lens-creation flow. The button is
        disabled today because no end-to-end path exists from the topbar to
        a "create lens" action — lens creation lives inside the Builder
        lens form. Workup view 01 keeps this affordance so the user can
        always add a lens without diving into the Builder.
        Why: the workup treats the `+` as ambient access to lens creation;
        burying it inside Builder breaks the canonical state.
        How to apply: when the Builder lens has a "create blank lens" RPC,
        bind it here.
        ---
      -->
      <button
        class="lens-add"
        disabled
        title="Add lens — not yet wired (placeholder per Riso workup view 01)"
      >＋</button>

      {#if builderModeOpen && isBuilderLens(activeLens)}
        <!--
          Builder mode menu, view 05 of the Riso workup. Listed entries
          mirror the workup exactly: Team / Agent / Context / Lens / Skill /
          Tool. None of them dispatch — see the mote below.

          TODO type=issue priority=normal tags=ui,builder,mode-picker
          type: issue
          The builder mode menu is non-functional. The gateway has no mode
          dispatcher: today the Builder lens always renders the same
          BuilderDeck regardless of intent. Each entry should swap the
          stage modules (orchestration flow, agent inspector, tool
          palette, etc.) for the active mode.
          Why: workup view 05 places the mode picker as the central
          affordance for Builder; without it the lens doesn't deliver the
          authoring vocabulary the workup describes.
          How to apply: design a `builder_mode` runtime field; when an
          entry is selected, push a "select_mode" event over the wire and
          let the lens runtime swap modules. Promote to a task once the
          dispatcher is scoped.
          ---
        -->
        <div class="builder-pop" role="menu" aria-label="Builder mode">
          <div class="builder-pop-banner">Non-functional · workup view 05</div>
          {#each [
            { num: "01", name: "Team", hint: "topology + flow" },
            { num: "02", name: "Agent", hint: "role + tools" },
            { num: "03", name: "Context", hint: "assembly" },
            { num: "04", name: "Lens", hint: "layout + modules" },
            { num: "05", name: "Skill", hint: "trigger + content" },
            { num: "06", name: "Tool", hint: "schema + handler" },
          ] as item (item.num)}
            <button
              type="button"
              class="builder-pop-item"
              disabled
              title="Builder mode menu is not yet wired"
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
            {stampState.label}
          </span>
          <!--
            TODO type=task priority=normal tags=ui,topbar,stamp
            The workup stamp shows time-on-state (e.g. "TEAM LIVE 04:18",
            "RETRY 3 / 5"). Today we have no aggregator that tracks how
            long a turn has been busy or which retry attempt the provider
            chain is on, so the stamp shows only a label.
            Why: workup view 02/07 use the timer as the operator's primary
            cue that work is in flight; without it the stamp reads the
            same whether the agent is one second or one hour into a turn.
            How to apply: emit per-attempt retry events from the provider
            chain and a periodic "turn tick" event from the gateway, then
            render alongside the state label here.
            ---
          -->
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
            class:shell={!page.functional}
            onclick={() => onNavigate?.(`/settings/${page.id}`)}
            title={page.functional ? page.label : `${page.label} (not yet wired)`}
          >
            <span>{page.label}</span>
            <span class="ix">
              {#if !page.functional}
                <span class="shell-mark" aria-label="Non-functional placeholder">SHELL</span>
              {:else}
                {page.ix}
              {/if}
            </span>
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
      <!-- No bench selected: nav shows a "Recent" list per workup view 04 plus
           a quick legend. Selection still happens in the body via BenchPicker;
           the nav's recent list is a fast lane for re-opening recent work. -->
      <div class="bench-head picker-mode">
        <div class="crumb">
          <span class="bench-label">Choose a bench</span>
        </div>
        <h2 class="name">Swage</h2>
      </div>
      <div class="section">
        <div class="key">
          <span>Recent</span>
          <!--
            TODO type=task priority=normal tags=ui,bench,recent
            The Recent list orders by `last_opened` per workup view 04. The
            Bench API today exposes `created_at` but not `last_opened`, so
            this section is a non-functional shell — the list isn't even
            fetched until a bench is selected.
            Why: workup view 04 puts a Recent list in the nav so the user
            can re-enter their last bench in one click; without
            last_opened, ordering would be arbitrary and misleading.
            How to apply: persist `last_opened_at` on bench rows and
            expose it on /api/benches; sort here by descending value.
            ---
          -->
          <span class="shell-mark">SHELL</span>
        </div>
        <p class="recent-shell-body">
          Recent benches list (by last opened) lands here once the bench
          API exposes `last_opened_at`.
        </p>
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
        <header class="pane-head">
          <span class="pane-label">Conversation</span>
          <span class="pane-index">
            {#if streamingStore.isStreaming}
              Live
            {:else if streamingStore.turnError}
              Retrying
            {:else}
              Idle
            {/if}
          </span>
        </header>
        {#if activeConversation}
          <ConnectionIndicator store={connectionStore} {onRetry} />
        {/if}
        <MessageDisplay store={conversationStore} {streamingStore} />
        {#if streamingStore.pendingApproval}
          <!-- Approval replaces the input box (workup view 03). -->
          <ApprovalCard approval={streamingStore.pendingApproval} />
        {:else}
          <MessageInput store={streamingStore} />
        {/if}
        <!--
          Conversation meta strip (workup view 01/02/05). The token count is
          read from the streaming store once the gateway emits turn_complete;
          before that there is no live count. The 200,000 budget is currently
          a placeholder copied from the workup — see the mote below.

          TODO type=task priority=normal tags=ui,conversation,tokens
          Bind the token budget to the active model's context window. The
          gateway already has the model record (settings.models[*].context_window);
          surface that on the conversation API and read it here.
          Why: workup view 01 shows `tokens · 4,820 / 200,000` — the budget
          must reflect the active model, not a hard-coded number.
          How to apply: extend the conversation detail response to include
          `context_window`, then render `tokens / context_window`.
          ---
        -->
        <footer class="conv-meta" aria-hidden="false">
          <span class="conv-meta-tokens">
            tokens · <span class="conv-meta-shell" title="Live token totals are not wired yet">—</span>
          </span>
          <span class="conv-meta-state" data-tone={stampState.tone}>
            ● {stampState.label}
          </span>
        </footer>
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
      {:else}
        <!--
          No module placed by this lens. The Riso workup (view 01/02) puts
          stage modules on this side: shared reference (assistant default),
          active team + log tail + release notes (when a team is running).
          None of these are wired today, so we render the catalogue as a
          column of non-functional shells so the chrome matches the workup.

          TODO type=issue priority=normal tags=ui,stage,modules
          type: issue
          The module catalogue is not yet wired to the lens runtime. The
          gateway exposes lens descriptors with `ui_modules` and the
          assistant-lens default, but no module placement happens on the
          stage. Each shell below names the workup module it stands in
          for and the backend gap blocking it.
          Why: workup views 01/02 hinge on stage module presence; without
          it the pane reads as empty even though modules belong there.
          How to apply: design a "module placement" runtime field on the
          lens, expose the placed module's data on the conversation API,
          render real components when the data arrives.
          ---
        -->
        <section class="pane stage-pane stage-shell" aria-label="Module stage">
          <header class="pane-head">
            <span class="pane-label">Stage</span>
            <span class="pane-index">Workup shells</span>
          </header>
          <div class="stage-modules">
            <article class="stage-module" data-tone="auto">
              <header class="stage-module-head">
                <h3>Shared · workup placeholder</h3>
                <span class="stage-module-tag">SHELL</span>
              </header>
              <p class="stage-module-body">
                The assistant lens (workup view 01) places a shared reference
                module here — typically a file viewer auto-loaded into agent
                context. The file_read tool exists; module placement does not.
              </p>
            </article>

            <article class="stage-module" data-tone="streaming">
              <header class="stage-module-head">
                <h3>Active team · live</h3>
                <span class="stage-module-tag">SHELL</span>
              </header>
              <p class="stage-module-body">
                Workup view 02 places the active team topology here (per-agent
                step, model, status). The gateway emits `topology_*` events
                but the live module that consumes them isn't built.
              </p>
            </article>

            <article class="stage-module" data-tone="streaming">
              <header class="stage-module-head">
                <h3>Live log tail</h3>
                <span class="stage-module-tag">SHELL</span>
              </header>
              <p class="stage-module-body">
                Workup view 02 — streaming output across all running agents.
                No aggregator on the gateway today; per-agent token streams
                exist but there is no merged tail.
              </p>
            </article>

            <article class="stage-module" data-tone="static">
              <header class="stage-module-head">
                <h3>Release notes draft</h3>
                <span class="stage-module-tag">SHELL</span>
              </header>
              <p class="stage-module-body">
                Workup view 02 — collaboratively edited document module. No
                document concept exists yet; benches store files but there
                is no live "shared draft" abstraction.
              </p>
            </article>
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

  /* Non-functional settings entries advertise themselves with an explicit
     SHELL chip rather than fading into the background. The intent is that
     the user can still see what's coming without thinking they're broken. */
  .settings-page.shell .ix {
    opacity: 1;
  }

  .shell-mark {
    display: inline-block;
    padding: 2px 6px;
    background: var(--accent);
    color: var(--paper);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 9px;
    letter-spacing: 0.18em;
    text-transform: uppercase;
  }

  .settings-page.active .shell-mark {
    background: var(--paper);
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

  /* Caret as a separate button beside the Builder lens — workup view 05.
     The split lets clicking the lens body activate it without opening the
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

  /* Builder mode dropdown — non-functional shell per workup view 05. */
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
    cursor: not-allowed;
    opacity: 0.7;
  }

  .builder-pop-item:last-child {
    border-bottom: none;
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

  .recent-shell-body {
    margin: 0;
    color: var(--ink-soft);
    font-size: 12.5px;
    line-height: 1.5;
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

  /* Pane heads — workup view 01/02/05 place a small label + index above each
     pane. Index expresses live state ("LIVE", "Builder", "Workup shells"). */
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

  /* Conversation meta footer — workup view 01 places token / state below the
     input. Token totals are not wired (see mote in markup). */
  .conv-meta {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 8px 22px 12px;
    border-top: 1px dotted var(--rule-soft);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--ink-soft);
    flex-shrink: 0;
  }

  .conv-meta-shell {
    color: var(--ink-orange);
    border-bottom: 1px dashed var(--accent);
  }

  .conv-meta-state {
    padding: 2px 6px;
    border: 1px solid var(--rule-soft);
  }

  .conv-meta-state[data-tone="busy"] {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
  }

  .conv-meta-state[data-tone="alert"] {
    color: var(--ink-orange);
    border-color: var(--accent);
  }

  /* Stage shell — column of non-functional module placeholders. */
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

  /* Tonal hint on the left edge — preserves the workup vocabulary
     (auto = ink rule, streaming = orange rule, static = soft rule). */
  .stage-module[data-tone="auto"] {
    border-left-width: 4px;
  }

  .stage-module[data-tone="streaming"] {
    border-left: 4px solid var(--accent);
  }

  .stage-module[data-tone="static"] {
    border-left: 4px solid var(--rule-soft);
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
