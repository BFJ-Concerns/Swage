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
  }

  let {
    conversationStore,
    connectionStore,
    streamingStore,
    projectStore,
    onProjectChange,
    onRetry,
  }: Props = $props();
</script>

<div class="layout">
  <aside class="sidebar">
    <!-- Project context at the top, conversation list fills the rest. -->
    <ProjectSelector {projectStore} {onProjectChange} />
    <ConversationList store={conversationStore} />
  </aside>
  <main class="conversation-area">
    <!--
      ConnectionIndicator is non-intrusive — hidden when connected,
      visible as a banner when the WebSocket is down or reconnecting.
    -->
    <ConnectionIndicator store={connectionStore} {onRetry} />
    <!-- Messages fill the available space and scroll independently. -->
    <MessageDisplay store={conversationStore} {streamingStore} />
    <!-- Input anchors to the bottom of the conversation area. -->
    <MessageInput store={streamingStore} />
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
  }

  .conversation-area {
    flex: 1;
    display: flex;
    flex-direction: column;
    /* min-width: 0 prevents flex children from overflowing at narrow widths. */
    min-width: 0;
  }
</style>
