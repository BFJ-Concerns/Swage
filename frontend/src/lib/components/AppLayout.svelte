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
  import OperatorDashboard from "./OperatorDashboard.svelte";
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
    isOperatorMode: boolean;
    onToggleOperatorMode: (next: boolean) => void;
  }

  let {
    conversationStore,
    connectionStore,
    streamingStore,
    projectStore,
    onProjectChange,
    onRetry,
    isOperatorMode,
    onToggleOperatorMode,
  }: Props = $props();
</script>

<div class="layout">
  <aside class="sidebar">
    <!-- Project context at the top, conversation list fills the rest. -->
    <ProjectSelector {projectStore} {onProjectChange} />
    <div class="mode-switch" role="tablist" aria-label="Primary views">
      <button
        class="mode-btn"
        class:active={!isOperatorMode}
        onclick={() => onToggleOperatorMode(false)}
      >
        Conversations
      </button>
      <button
        class="mode-btn"
        class:active={isOperatorMode}
        onclick={() => onToggleOperatorMode(true)}
      >
        Operator
      </button>
    </div>
    <ConversationList store={conversationStore} />
  </aside>
  <main class="conversation-area">
    <!--
      ConnectionIndicator is non-intrusive — hidden when connected,
      visible as a banner when the WebSocket is down or reconnecting.
    -->
    {#if isOperatorMode}
      <OperatorDashboard />
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
    border-bottom: 1px solid #e7ddd0;
    background: rgba(255, 255, 255, 0.68);
  }

  .mode-btn {
    border: 1px solid rgba(154, 52, 18, 0.14);
    border-radius: 999px;
    padding: 8px 10px;
    background: rgba(255, 255, 255, 0.78);
    color: #6b7280;
    cursor: pointer;
    font-size: 0.82rem;
    font-weight: 600;
  }

  .mode-btn.active {
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
