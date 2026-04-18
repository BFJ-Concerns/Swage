<!--
  Root application component.

  Startup sequence:
    1. Create stores (synchronous, no IO).
    2. Wire cross-store events: connection → streaming, reconnect → refresh.
    3. onMount: load projects (auto-selects default), load conversations for
       the selected project, then initialise router (reads URL, selects any
       conversation referenced by the path).
    4. Render: loading splash → layout, or error screen if the gateway is unreachable.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import AppLayout from "./lib/components/AppLayout.svelte";
  import type { ConversationStore } from "./lib/stores/conversations";
  import { createConversationStore } from "./lib/stores/conversations";
  import { createConnectionStore } from "./lib/stores/connection";
  import { createStreamingStore } from "./lib/stores/streaming";
  import { createProjectStore } from "./lib/stores/projects";
  import { createRouter } from "./lib/router";
  import { getDesktopRuntime } from "./lib/runtime";

  // ---------------------------------------------------------------------------
  // Store creation
  // ---------------------------------------------------------------------------

  const projectStore = createProjectStore();
  const conversationStore = createConversationStore();

  // onReconnect re-fetches the active conversation over HTTP after a dropped
  // WebSocket successfully reconnects, reconciling any state drift during downtime.
  const connectionStore = createConnectionStore({
    onReconnect: () => conversationStore.refreshActiveConversation(),
  });

  // The streaming store needs both dependencies so it can push committed messages
  // to the conversation history and dispatch outgoing messages via the connection.
  const streamingStore = createStreamingStore({
    conversationStore,
    connectionStore,
  });

  // Route all server events from the WebSocket through the streaming store.
  // The connection store is a pure relay — interpretation belongs to streaming.
  connectionStore.onServerEvent((event) =>
    streamingStore.handleServerEvent(event),
  );

  // ---------------------------------------------------------------------------
  // Router-aware conversation actions
  // ---------------------------------------------------------------------------
  // These wrappers add URL and WebSocket side effects to the raw store methods.
  // Child components receive a proxy shaped like ConversationStore that calls
  // these instead of the originals, keeping the URL and connection in sync.

  async function selectAndNavigate(id: string): Promise<void> {
    try {
      isOperatorMode = false;
      await conversationStore.selectConversation(id);
      connectionStore.connectTo(id);
      // router is declared below but will be initialised before this is called.
      router.navigate(`/conversations/${id}`);
    } catch (err) {
      console.error("Failed to select conversation:", err);
    }
  }

  async function createAndNavigate(): Promise<void> {
    try {
      isOperatorMode = false;
      // Use the currently selected project — should always be set after startup.
      const projectId = projectStore.currentProject?.id ?? "";
      await conversationStore.createConversation(projectId);
      const id = conversationStore.activeConversation?.id;
      if (id) {
        connectionStore.connectTo(id);
        router.navigate(`/conversations/${id}`);
      }
    } catch (err) {
      console.error("Failed to create conversation:", err);
    }
  }

  // Proxy that implements ConversationStore but overrides the two navigation-
  // bearing actions. All other methods delegate straight to the base store.
  const routerAwareStore: ConversationStore = {
    get conversations() {
      return conversationStore.conversations;
    },
    get activeConversation() {
      return conversationStore.activeConversation;
    },
    get isLoading() {
      return conversationStore.isLoading;
    },
    loadConversations: (projectId) =>
      conversationStore.loadConversations(projectId),
    selectConversation: selectAndNavigate,
    // No-arg wrapper: the proxy captures projectId from projectStore at
    // call time so ConversationList doesn't need to know about projects.
    createConversation: createAndNavigate,
    deleteConversation: (id) => conversationStore.deleteConversation(id),
    appendOptimisticUserMessage: (msg) =>
      conversationStore.appendOptimisticUserMessage(msg),
    appendAssistantMessage: (msg) => conversationStore.appendAssistantMessage(msg),
    refreshActiveConversation: () =>
      conversationStore.refreshActiveConversation(),
  };

  // ---------------------------------------------------------------------------
  // Router
  // ---------------------------------------------------------------------------
  // selectAndNavigate is passed so router.init() can activate the right
  // conversation when the page loads with a deep URL like /conversations/:id.

  const router = createRouter({
    conversationStore: { selectConversation: selectAndNavigate },
  });

  // ---------------------------------------------------------------------------
  // UI state
  // ---------------------------------------------------------------------------

  let isInitialising = $state(true);
  let loadError = $state<string | null>(null);
  // Set to true when the router detects that the URL contained a stale ID.
  let notFound = $state(false);
  let isOperatorMode = $state(false);
  const desktopRuntime = getDesktopRuntime();

  // Passed to ConnectionIndicator so the user can trigger a manual reconnect.
  function handleRetry(): void {
    const id = conversationStore.activeConversation?.id;
    if (id) connectionStore.connectTo(id);
  }

  function handleToggleOperatorMode(next: boolean): void {
    isOperatorMode = next;

    if (next) {
      connectionStore.disconnect();
      return;
    }

    const id = conversationStore.activeConversation?.id;
    if (id) connectionStore.connectTo(id);
  }

  // ---------------------------------------------------------------------------
  // Startup
  // ---------------------------------------------------------------------------

  // Called by ProjectSelector when the user switches projects.
  // Reloads the conversation list for the newly active project and clears
  // any cross-project UI state (active conversation, WebSocket connection).
  async function handleProjectChange(): Promise<void> {
    try {
      const projectId = projectStore.currentProject?.id;
      if (!projectId) return;

      // Drop the WebSocket — it was bound to the previous project's session.
      connectionStore.disconnect();
      // Reset the URL so the router doesn't try to restore a stale conversation.
      router.navigate("/");
      // Load the new project's conversations.
      await conversationStore.loadConversations(projectId);
    } catch (err) {
      console.error("Failed to switch project:", err);
    }
  }

  onMount(async () => {
    try {
      // 1. Load projects — auto-selects the default project.
      await projectStore.loadProjects();

      // 2. Load conversations for the selected project. router.init() may call
      //    selectAndNavigate, which relies on the list being populated.
      const projectId = projectStore.currentProject?.id;
      if (projectId) {
        await conversationStore.loadConversations(projectId);
      }

      await router.init();

      // Snapshot the not-found flag — set by the router if the URL pointed
      // at a conversation that no longer exists on the server.
      notFound = router.currentRoute.notFound;
    } catch {
      // Any failure here means the gateway is unreachable. Show a clear error
      // rather than a blank screen with no indication of what went wrong.
      loadError = "Could not reach the gateway. Is the server running?";
    } finally {
      isInitialising = false;
    }
  });
</script>

{#if isInitialising}
  <!-- Loading splash — shown only for the brief window before the HTTP response. -->
  <div class="splash">
    <p>Loading…</p>
  </div>
{:else if loadError}
  <!-- Gateway unreachable at startup — give the user something to act on. -->
  <div class="error-screen" role="alert">
    <p class="error-heading">Could not connect to the gateway</p>
    <p class="error-detail">{loadError}</p>
    <button onclick={() => window.location.reload()}>Retry</button>
  </div>
{:else}
  <AppLayout
    conversationStore={routerAwareStore}
    {connectionStore}
    {streamingStore}
    {projectStore}
    onProjectChange={handleProjectChange}
    onRetry={handleRetry}
    {isOperatorMode}
    onToggleOperatorMode={handleToggleOperatorMode}
    {desktopRuntime}
  />

  {#if notFound}
    <!--
      The URL referenced a conversation that no longer exists.
      The router has already redirected to "/" — this notice explains why.
      Rendered on top of the layout so the conversation list remains accessible.
    -->
    <div class="not-found-notice" role="alert">
      Conversation not found — it may have been deleted.
      <button class="dismiss" onclick={() => (notFound = false)}>✕</button>
    </div>
  {/if}
{/if}

<style>
  /* ---- Loading splash ------------------------------------------------------- */

  .splash {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #999;
    font-size: 0.9rem;
  }

  /* ---- Error screen --------------------------------------------------------- */

  .error-screen {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
  }

  .error-heading {
    font-weight: 600;
    color: #333;
    font-size: 1rem;
    margin: 0;
  }

  .error-detail {
    color: #666;
    font-size: 0.875rem;
    margin: 0;
  }

  .error-screen button {
    margin-top: 12px;
    padding: 8px 20px;
    border: 1px solid #d0d0d0;
    border-radius: 8px;
    background: #fff;
    font-size: 0.875rem;
    cursor: pointer;
  }

  .error-screen button:hover {
    background: #f5f5f5;
  }

  /* ---- Not-found notice ----------------------------------------------------- */
  /* Fixed banner at the bottom — visible over the layout without displacing it. */

  .not-found-notice {
    position: fixed;
    bottom: 16px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    background: #fff8e1;
    border: 1px solid #ffe082;
    border-radius: 8px;
    font-size: 0.875rem;
    color: #5d4037;
    box-shadow: 0 2px 8px rgb(0 0 0 / 0.1);
    /* Keep above the layout content. */
    z-index: 100;
  }

  .dismiss {
    background: none;
    border: none;
    cursor: pointer;
    font-size: 0.8rem;
    color: #8d6e63;
    padding: 0;
    line-height: 1;
  }

  .dismiss:hover {
    color: #5d4037;
  }
</style>
