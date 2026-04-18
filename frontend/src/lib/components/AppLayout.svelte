<!--
  Three-panel layout shell: sidebar (conversation list), main area (messages), input.
  Pure renderer — threads store props to child components.
-->
<script lang="ts">
  import ConversationList from "./ConversationList.svelte";
  import ProjectSelector from "./ProjectSelector.svelte";
  import BuilderDeck from "./BuilderDeck.svelte";
  import MessageDisplay from "./MessageDisplay.svelte";
  import MessageInput from "./MessageInput.svelte";
  import ConnectionIndicator from "./ConnectionIndicator.svelte";
  import OperatorDashboard from "./OperatorDashboard.svelte";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type { AppLens, LensSurface } from "../types";
  import type { ConversationStore } from "../stores/conversations";
  import type { ConnectionStore } from "../stores/connection";
  import type { StreamingStore } from "../stores/streaming";
  import type { ProjectStore } from "../stores/projects";

  interface Props {
    conversationStore: ConversationStore;
    connectionStore: ConnectionStore;
    streamingStore: StreamingStore;
    projectStore?: ProjectStore;
    onProjectChange?: () => void;
    onRetry: () => void;
    activeLens: AppLens;
    activeSurface: LensSurface;
    onSelectLens: (next: AppLens) => void | Promise<void>;
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
    activeSurface,
    onSelectLens,
    onSelectSurface,
    desktopRuntime,
  }: Props = $props();
</script>

<div class="layout">
  <aside class="sidebar">
    <!-- Project context at the top, conversation list fills the rest. -->
    <ProjectSelector {projectStore} {onProjectChange} />
    <div class="lens-switch" role="tablist" aria-label="Product lenses">
      <button
        class="toggle-btn"
        class:active={activeLens === "assistant"}
        onclick={() => onSelectLens("assistant")}
      >
        Assistant
      </button>
      <button
        class="toggle-btn"
        class:active={activeLens === "builder"}
        onclick={() => onSelectLens("builder")}
      >
        Builder
      </button>
    </div>
    <div class="mode-switch" role="tablist" aria-label="Primary views">
      {#if activeLens === "assistant"}
        <button
          class="toggle-btn"
          class:active={activeSurface === "conversation"}
          onclick={() => onSelectSurface("conversation")}
        >
          Conversations
        </button>
        <button
          class="toggle-btn"
          class:active={activeSurface === "operator"}
          onclick={() => onSelectSurface("operator")}
        >
          Operator
        </button>
      {:else}
        <button
          class="toggle-btn"
          class:active={activeSurface === "builder"}
          onclick={() => onSelectSurface("builder")}
        >
          Builder deck
        </button>
        <button
          class="toggle-btn"
          class:active={activeSurface === "conversation"}
          onclick={() => onSelectSurface("conversation")}
        >
          Conversations
        </button>
      {/if}
    </div>
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
      <OperatorDashboard />
    {:else if activeSurface === "builder"}
      <BuilderDeck {projectStore} {desktopRuntime} {activeLens} {onSelectLens} />
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
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px;
    padding: 10px 12px;
    background: rgba(255, 255, 255, 0.58);
  }

  .lens-switch {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px;
    padding: 10px 12px;
    border-bottom: 1px solid #e7ddd0;
    background: rgba(255, 255, 255, 0.68);
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
