<!--
  Three-panel layout shell: sidebar (conversation list), main area (messages), input.
  Pure renderer — threads store props to child components.
-->
<script lang="ts">
  import ConversationList from "./ConversationList.svelte";
  import ProjectSelector from "./ProjectSelector.svelte";
  import MessageDisplay from "./MessageDisplay.svelte";
  import MessageInput from "./MessageInput.svelte";
  import ConnectionIndicator from "./ConnectionIndicator.svelte";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type { AppLens, LensDescriptor, LensSurface } from "../types";
  import type { ConversationStore } from "../stores/conversations";
  import type { ConnectionStore } from "../stores/connection";
  import type { StreamingStore } from "../stores/streaming";
  import type { ProjectStore } from "../stores/projects";

  type BuilderDeckModule = typeof import("./BuilderDeck.svelte");
  type OperatorDashboardModule = typeof import("./OperatorDashboard.svelte");

  interface Props {
    conversationStore: ConversationStore;
    connectionStore: ConnectionStore;
    streamingStore: StreamingStore;
    projectStore?: ProjectStore;
    onProjectChange?: () => void;
    onRetry: () => void;
    activeLens: AppLens;
    availableLenses: LensDescriptor[];
    activeSurface: LensSurface;
    onSelectLens: (next: AppLens) => void | Promise<void>;
    onRefreshLenses: (preferredSurface?: LensSurface) => void | Promise<void>;
    onSelectSurface: (next: LensSurface) => void;
    desktopRuntime?: DesktopRuntimeInfo | null;
  }

  let {
    conversationStore,
    connectionStore,
    streamingStore,
    projectStore,
    onProjectChange,
    onRetry,
    activeLens,
    availableLenses,
    activeSurface,
    onSelectLens,
    onRefreshLenses,
    onSelectSurface,
    desktopRuntime,
  }: Props = $props();

  let builderDeckModule = $state<Promise<BuilderDeckModule> | null>(null);
  let operatorDashboardModule = $state<Promise<OperatorDashboardModule> | null>(null);
  const activeLensDescriptor = $derived(
    availableLenses.find((lens) => lens.id === activeLens) ?? null,
  );
  const surfaceTabs = $derived(
    (activeLensDescriptor?.ui_modules ?? ["conversation"]) as LensSurface[],
  );

  $effect(() => {
    if (activeSurface === "builder" && !builderDeckModule) {
      builderDeckModule = import("./BuilderDeck.svelte");
    }

    if (activeSurface === "operator" && !operatorDashboardModule) {
      operatorDashboardModule = import("./OperatorDashboard.svelte");
    }
  });
</script>

<div class="layout">
  <aside class="sidebar">
    <!-- Project context at the top, conversation list fills the rest. -->
    <ProjectSelector {projectStore} {onProjectChange} />
    <div class="lens-switch" role="tablist" aria-label="Product lenses">
      {#each availableLenses as lens (lens.id)}
        <button
          class="toggle-btn"
          class:active={activeLens === lens.id}
          onclick={() => onSelectLens(lens.id)}
        >
          {lens.title.replace(/ lens$/i, "")}
        </button>
      {/each}
    </div>
    <div class="mode-switch" role="tablist" aria-label="Primary views">
      {#each surfaceTabs as surface (surface)}
        <button
          class="toggle-btn"
          class:active={activeSurface === surface}
          onclick={() => onSelectSurface(surface)}
        >
          {surface === "builder"
            ? "Builder deck"
            : surface === "operator"
              ? "Operator"
              : "Conversations"}
        </button>
      {/each}
    </div>
    {#if activeLensDescriptor}
      <div class="lens-summary">
        <span class="lens-summary-title">{activeLensDescriptor.summary}</span>
        {#if activeLensDescriptor.prominent_team_ids.length}
          <div class="team-chip-row" aria-label="Prominent teams">
            {#each activeLensDescriptor.prominent_team_ids as teamId (teamId)}
              <span class="team-chip">{teamId}</span>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
    {#if desktopRuntime}
      <div class="runtime-chip" title={desktopRuntime.gatewayUrl}>
        <span class="runtime-label">Desktop shell</span>
        <span class="runtime-meta">
          {desktopRuntime.managedGateway ? "Managed gateway" : "Remote gateway"}
        </span>
      </div>
    {/if}
    <ConversationList store={conversationStore} />
  </aside>
  <main class="conversation-area">
    <!--
      ConnectionIndicator is non-intrusive — hidden when connected,
      visible as a banner when the WebSocket is down or reconnecting.
    -->
    {#if activeSurface === "operator"}
      {#if operatorDashboardModule}
        {#await operatorDashboardModule then module}
          {@const OperatorDashboard = module.default}
          <OperatorDashboard />
        {:catch error}
          <div class="surface-state" role="alert">
            {error instanceof Error
              ? error.message
              : "Failed to load the operator surface."}
          </div>
        {/await}
      {:else}
        <div class="surface-state">Loading operator surface…</div>
      {/if}
    {:else if activeSurface === "builder"}
      {#if builderDeckModule}
        {#await builderDeckModule then module}
          {@const BuilderDeck = module.default}
          <BuilderDeck
            {projectStore}
            {desktopRuntime}
            {activeLens}
            {availableLenses}
            {onSelectLens}
            {onRefreshLenses}
          />
        {:catch error}
          <div class="surface-state" role="alert">
            {error instanceof Error
              ? error.message
              : "Failed to load the builder surface."}
          </div>
        {/await}
      {:else}
        <div class="surface-state">Loading builder surface…</div>
      {/if}
    {:else}
      <ConnectionIndicator store={connectionStore} {onRetry} />
      <!-- Messages fill the available space and scroll independently. -->
      <MessageDisplay store={conversationStore} {streamingStore} />
      <!-- Input anchors to the bottom of the conversation area. -->
      <MessageInput store={streamingStore} />
    {/if}
  </main>
</div>

<style>
  .layout {
    display: flex;
    height: 100%;
    width: 100%;
  }

  .sidebar {
    /* Fixed 280px width — wide enough for conversation previews. */
    width: 280px;
    border-right: 1px solid #e0e0e0;
    display: flex;
    flex-direction: column;
    /* Sidebar content scrolls independently via ConversationList overflow. */
    overflow: hidden;
    background:
      linear-gradient(180deg, rgba(255, 251, 245, 0.98), rgba(248, 244, 236, 0.98));
  }

  .mode-switch {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(0, 1fr));
    gap: 6px;
    padding: 10px 12px;
    background: rgba(255, 255, 255, 0.58);
  }

  .lens-switch {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(0, 1fr));
    gap: 6px;
    padding: 10px 12px;
    border-bottom: 1px solid #e7ddd0;
    background: rgba(255, 255, 255, 0.68);
  }

  .lens-summary {
    display: grid;
    gap: 8px;
    padding: 10px 12px 2px;
    color: #4b5563;
  }

  .lens-summary-title {
    font-size: 0.82rem;
    line-height: 1.45;
  }

  .team-chip-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .team-chip {
    border-radius: 999px;
    padding: 4px 9px;
    background: rgba(17, 24, 39, 0.08);
    color: #1f2937;
    font-size: 0.76rem;
    font-weight: 600;
  }

  .runtime-chip {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 8px 12px 0;
    padding: 10px 12px;
    border: 1px solid rgba(146, 64, 14, 0.12);
    border-radius: 14px;
    background:
      linear-gradient(135deg, rgba(255, 247, 237, 0.94), rgba(255, 255, 255, 0.88));
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.72);
  }

  .runtime-label {
    font-size: 0.78rem;
    font-weight: 700;
    color: #7c2d12;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .runtime-meta {
    font-size: 0.82rem;
    color: #6b7280;
  }

  .toggle-btn {
    border: 1px solid rgba(154, 52, 18, 0.14);
    border-radius: 999px;
    padding: 8px 10px;
    background: rgba(255, 255, 255, 0.78);
    color: #6b7280;
    cursor: pointer;
    font-size: 0.82rem;
    font-weight: 600;
  }

  .toggle-btn.active {
    background: #1f2937;
    color: #fffaf0;
    border-color: #1f2937;
  }

  .conversation-area {
    flex: 1;
    display: flex;
    flex-direction: column;
    /* min-width: 0 prevents flex children from overflowing at narrow widths. */
    min-width: 0;
  }

  .surface-state {
    margin: auto;
    padding: 18px 22px;
    border-radius: 18px;
    border: 1px solid rgba(154, 52, 18, 0.18);
    background: rgba(255, 251, 235, 0.82);
    color: #7c2d12;
    font-weight: 600;
  }

  @media (max-width: 820px) {
    .layout {
      flex-direction: column;
    }

    .sidebar {
      width: 100%;
      border-right: none;
      border-bottom: 1px solid #e0e0e0;
    }

    .conversation-area {
      min-height: 0;
    }
  }
</style>
