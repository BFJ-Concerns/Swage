<!--
  Root application component.

  Startup sequence:
    1. Create stores (synchronous, no IO).
    2. Wire cross-store events: connection → streaming, reconnect → refresh.
    3. onMount: load spaces (restoring the last open space), load conversations
       for the selected space, then initialise router (reads URL, selects any
       conversation referenced by the path).
    4. Render: loading splash → layout, or error screen if the gateway is unreachable.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import AppLayout from "./lib/components/AppLayout.svelte";
  import { getConversation } from "./lib/api/conversations";
  import { fetchSettings } from "./lib/api/settings";
  import { listEffectiveAgentDefinitions } from "./lib/api/agent-definitions";
  import type { EffectiveAgentDefinitionResponse } from "./lib/protocol/EffectiveAgentDefinitionResponse";
  import type { ConversationStore } from "./lib/stores/conversations";
  import { createConversationStore } from "./lib/stores/conversations";
  import { createConnectionStore } from "./lib/stores/connection";
  import { createPagesStore } from "./lib/stores/pages";
  import {
    createStreamingStore,
    withConversationAutoCreate,
    type StreamingStore,
  } from "./lib/stores/streaming";
  import { createSpaceStore } from "./lib/stores/spaces";
  import { createRouter } from "./lib/router";
  import type { Route } from "./lib/router";
  import { getDesktopRuntime } from "./lib/runtime";
  import {
    DISPLAY_PREFERENCES_CHANGED,
    readDisplayPreferences,
    writeDisplayPreferences,
    type DisplayPreferences,
  } from "./lib/display-preferences";
  type Surface = "conversation" | "operator" | "pages";

  // ---------------------------------------------------------------------------
  // Store creation
  // ---------------------------------------------------------------------------

  const spaceStore = createSpaceStore();
  const conversationStore = createConversationStore();

  // onReconnect re-fetches the active conversation over HTTP after a dropped
  // WebSocket successfully reconnects, reconciling any state drift during downtime.
  let streamingStore: StreamingStore;
  const connectionStore = createConnectionStore({
    onReconnect: async () => {
      const recoveredTurnGeneration = streamingStore.activeTurnGeneration;
      await conversationStore.refreshActiveConversation();
      streamingStore.reconcileAfterReconnect(recoveredTurnGeneration);
    },
  });

  // The streaming store needs both dependencies so it can push committed messages
  // to the conversation history and dispatch outgoing messages via the connection.
  streamingStore = createStreamingStore({
    conversationStore,
    connectionStore,
  });

  // Route all server events from the WebSocket through the streaming store.
  // The connection store is a pure relay — interpretation belongs to streaming.
  connectionStore.onServerEvent((event) =>
    streamingStore.handleServerEvent(event),
  );

  // The pages store opens its own work-surface channel: page signals are not
  // turn events, and an open page must stay live even with no conversation
  // selected.
  const pagesStore = createPagesStore();

  // ---------------------------------------------------------------------------
  // Router-aware conversation actions
  // ---------------------------------------------------------------------------
  // These wrappers add URL and WebSocket side effects to the raw store methods.
  // Child components receive a proxy shaped like ConversationStore that calls
  // these instead of the originals, keeping the URL and connection in sync.

  // Selecting or starting a session is not navigation: the channel rides the
  // surface the user is standing on, so reaching a conversation must not move
  // them off their work. Only the operator surface, which carries no channel,
  // hands them back to the conversation surface.
  function keepSurfaceForConversation(): void {
    if (activeSurface === "operator") activeSurface = "conversation";
  }

  async function selectAndNavigateOrThrow(id: string): Promise<void> {
    keepSurfaceForConversation();
    await conversationStore.selectConversation(id);

    // A deep-linked conversation may belong to a space other than the one
    // currently in scope (or none, on first load). Bring the space into
    // scope so the navigation and conversation list reflect it.
    const conv = conversationStore.activeConversation;
    if (conv && spaceStore.currentSpace?.id !== conv.space_id) {
      await spaceStore.selectSpace(conv.space_id);
      await conversationStore.loadConversations(conv.space_id);
    }

    connectionStore.connectTo(id);
    // router is declared below but will be initialised before this is called.
    router.navigate(`/conversations/${id}`);
  }

  async function selectAndNavigate(id: string): Promise<void> {
    try {
      await selectAndNavigateOrThrow(id);
    } catch (err) {
      console.error("Failed to select conversation:", err);
    }
  }

  // Creates a conversation in the current space, connects, and navigates to
  // it. Returns the new conversation's id, or null when creation failed —
  // the auto-create composer path uses the id to decide whether its pending
  // turn can be dispatched.
  async function createAndNavigate(
    selectedModelId?: string | null,
    agentId?: string | null,
    effort?: import("./lib/protocol/Effort").Effort | null,
  ): Promise<string | null> {
    try {
      keepSurfaceForConversation();
      // Use the currently selected space — should always be set after startup.
      const spaceId = spaceStore.currentSpace?.id ?? "";
      await conversationStore.createConversation(spaceId, selectedModelId, agentId, effort);
      const id = conversationStore.activeConversation?.id;
      if (id) {
        connectionStore.connectTo(id);
        router.navigate(`/conversations/${id}`);
        return id;
      }
      return null;
    } catch (err) {
      console.error("Failed to create conversation:", err);
      return null;
    }
  }

  function startNewConversation(): void {
    keepSurfaceForConversation();
    connectionStore.disconnect();
    conversationStore.clearActiveConversation();
    router.navigate("/");
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
    loadConversations: (spaceId) =>
      conversationStore.loadConversations(spaceId),
    selectConversation: selectAndNavigate,
    // The proxy captures spaceId from spaceStore at call time so child
    // components don't need to know about spaces.
    createConversation: async (_spaceId, selectedModelId, agentId, effort) => {
      await createAndNavigate(selectedModelId, agentId, effort);
    },
    deleteConversation: (id) => conversationStore.deleteConversation(id),
    clearActiveConversation: () => conversationStore.clearActiveConversation(),
    appendOptimisticUserMessage: (msg) =>
      conversationStore.appendOptimisticUserMessage(msg),
    appendAssistantMessage: (msg) => conversationStore.appendAssistantMessage(msg),
    refreshActiveConversation: () =>
      conversationStore.refreshActiveConversation(),
  };

  // The streaming store handed to the layout: identical to the base store,
  // except that sending with no conversation in scope creates one in the
  // current space first (connect, then dispatch — the WebSocket client queues
  // the frame during the handshake). The composer that renders in an empty
  // space therefore just works instead of dropping the message.
  const composerStreamingStore = withConversationAutoCreate(streamingStore, {
    hasActiveConversation: () => conversationStore.activeConversation !== null,
    createConversation: createAndNavigate,
  });

  // ---------------------------------------------------------------------------
  // Router
  // ---------------------------------------------------------------------------
  // selectAndNavigate is passed so router.init() can activate the right
  // conversation when the page loads with a deep URL like /conversations/:id.

  function handleRouteChange(route: Route): void {
    currentRoute = route;
    notFound = route.notFound;
  }

  const router = createRouter({
    conversationStore: {
      get activeConversationId() {
        return conversationStore.activeConversation?.id ?? null;
      },
      checkConversationExists: async (id) => {
        await getConversation(id);
      },
      clearActiveConversation: () => {
        connectionStore.disconnect();
        conversationStore.clearActiveConversation();
      },
      selectConversation: selectAndNavigateOrThrow,
    },
    onRouteChange: handleRouteChange,
  });

  // ---------------------------------------------------------------------------
  // UI state
  // ---------------------------------------------------------------------------

  let isInitialising = $state(true);
  let loadError = $state<string | null>(null);
  // Set to true when the router detects that the URL contained a stale ID.
  let notFound = $state(false);
  let activeSurface = $state<Surface>("conversation");
  let currentRoute = $state<Route>(router.currentRoute);
  let modelOptions = $state<string[]>([]);
  let modelCatalogueReady = $state(false);
  let defaultModelId = $state<string | null>(null);
  let agentDefinitions = $state<EffectiveAgentDefinitionResponse[]>([]);
  const desktopRuntime = getDesktopRuntime();

  async function refreshModelCatalogue(): Promise<void> {
    const settings = await fetchSettings();
    modelOptions = Object.keys(settings.models);
    defaultModelId = settings.default_model;
    modelCatalogueReady = true;
  }

  async function refreshModelCatalogueAfterSave(): Promise<void> {
    try {
      await refreshModelCatalogue();
    } catch (error) {
      console.error("Failed to refresh model settings after saving:", error);
    }
  }

  // ---------------------------------------------------------------------------
  // Theme — light (riso) / dark (cyanotype) / system
  // ---------------------------------------------------------------------------
  // The chosen mode is what the user picked in the toggle; the resolved theme
  // is what's actually painted (system mode resolves through the OS preference).

  // Type lives in a shared module so child components type their props the
  // same way without round-tripping through a Svelte instance script.
  type ThemeMode = import("./lib/theme").ThemeMode;
  const initialDisplayPreferences = readDisplayPreferences();
  let displayPreferences = $state<DisplayPreferences>(initialDisplayPreferences);
  let themeMode = $state<ThemeMode>(initialDisplayPreferences.theme);

  function resolveTheme(mode: ThemeMode): "theme-riso" | "theme-cyanotype" {
    if (mode === "light") return "theme-riso";
    if (mode === "dark") return "theme-cyanotype";
    // System: follow the OS preference. matchMedia is unavailable in some
    // headless test environments, so fall back to light.
    if (typeof window === "undefined" || !window.matchMedia) return "theme-riso";
    return window.matchMedia("(prefers-color-scheme: dark)").matches
      ? "theme-cyanotype"
      : "theme-riso";
  }

  function applyTheme(mode: ThemeMode): void {
    const next = resolveTheme(mode);
    document.body.classList.remove("theme-riso", "theme-cyanotype");
    document.body.classList.add(next);
  }

  function applyDisplayPreferences(preferences: DisplayPreferences): void {
    document.body.classList.remove(
      "display-density-compact",
      "display-density-comfortable",
      "print-marks-full",
      "print-marks-minimal",
      "print-marks-off",
    );
    document.body.classList.add(`display-density-${preferences.density}`);
    document.body.classList.add(`print-marks-${preferences.print_marks}`);
    applyTheme(preferences.theme);
  }

  function setThemeMode(mode: ThemeMode): void {
    displayPreferences = { ...displayPreferences, theme: mode };
    themeMode = mode;
    writeDisplayPreferences(displayPreferences);
    applyDisplayPreferences(displayPreferences);
  }

  onMount(() => {
    applyDisplayPreferences(displayPreferences);

    const onChange = () => {
      if (displayPreferences.theme === "system") applyTheme("system");
    };
    const onPreferencesChange = (event: Event) => {
      const next = (event as CustomEvent<DisplayPreferences>).detail;
      displayPreferences = next;
      themeMode = next.theme;
      applyDisplayPreferences(next);
    };

    // Re-resolve when the OS theme changes, but only while in "system" mode —
    // an explicit light/dark choice should stick regardless of OS toggle.
    const media =
      typeof window !== "undefined" && window.matchMedia
        ? window.matchMedia("(prefers-color-scheme: dark)")
        : null;
    media?.addEventListener("change", onChange);
    window.addEventListener(DISPLAY_PREFERENCES_CHANGED, onPreferencesChange);
    return () => {
      media?.removeEventListener("change", onChange);
      window.removeEventListener(DISPLAY_PREFERENCES_CHANGED, onPreferencesChange);
    };
  });

  // Passed to ConnectionIndicator so the user can trigger a manual reconnect.
  function handleRetry(): void {
    const id = conversationStore.activeConversation?.id;
    if (id) connectionStore.connectTo(id);
  }

  function handleSelectSurface(next: Surface): void {
    const previous = activeSurface;
    activeSurface = next;

    // The pages surface keeps the conversation socket: the conversation is
    // the control channel beside the work surface rather than a place to
    // navigate away to. Page liveness does not depend on it — the pages
    // store listens on its own channel.
    if (next === "operator") {
      connectionStore.disconnect();
      return;
    }

    // Conversations and Pages share the same ambient channel. Moving between
    // them must retain its socket and in-memory history; only returning from
    // Operator needs to establish the connection again.
    if (previous === "operator") {
      const id = conversationStore.activeConversation?.id;
      if (id) connectionStore.connectTo(id);
    }
  }

  function navigate(path: string): void {
    router.navigate(path);
  }

  // ---------------------------------------------------------------------------
  // Startup
  // ---------------------------------------------------------------------------

  // Called by SpaceSelector when the user switches spaces.
  // Reloads the conversation list for the newly active space and clears
  // any cross-space UI state (active conversation, WebSocket connection).
  async function handleSpaceChange(): Promise<void> {
    try {
      const spaceId = spaceStore.currentSpace?.id;
      if (!spaceId) return;

      // Drop the WebSocket — it was bound to the previous space's session.
      connectionStore.disconnect();
      // Drop the active conversation too: left in place, the old space's
      // conversation would keep rendering and the composer would send into it.
      conversationStore.clearActiveConversation();
      // Reset the URL so the router doesn't try to restore a stale conversation.
      router.navigate("/");
      // Load the new space's conversations.
      await conversationStore.loadConversations(spaceId);
      agentDefinitions = await listEffectiveAgentDefinitions(spaceId);
    } catch (err) {
      console.error("Failed to switch space:", err);
    }
  }

  onMount(async () => {
    try {
      // 1. Load the space list and restore the last explicitly opened space.
      await spaceStore.loadSpaces();
      if (spaceStore.currentSpace) {
        await conversationStore.loadConversations(spaceStore.currentSpace.id);
      }

      // 2. Fetch global model settings. Space-scoped data (conversations,
      //    scoped runtime state loads when a space is opened — either via the
      //    picker or by a deep-linked conversation that selects its space.
      try {
        await refreshModelCatalogue();
      } catch (error) {
        console.error("Failed to load model settings:", error);
      }

      // 3. Apply the URL. If it points at a conversation, selectAndNavigate
      //    will bring its space into scope along the way.
      await router.init();
      if (spaceStore.currentSpace) {
        try {
          agentDefinitions = await listEffectiveAgentDefinitions(spaceStore.currentSpace.id);
        } catch (error) {
          console.error("Failed to load agent definitions:", error);
        }
      }
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
  <!--
    Loading splash — a low-fidelity skeleton of the window grid rather than a
    bare "Loading…" fragment. The shell is strongly branded, so a content-free
    text splash read as a broken render; this paints the layout first and lets
    content fill it. UI audit f-13.
  -->
  <div class="splash" role="status" aria-label="Loading Swage">
    <div class="splash-frame">
      <div class="splash-nav">
        <div class="sk sk-line sk-line-lg"></div>
        <div class="sk sk-line"></div>
        <div class="sk sk-line"></div>
        <div class="sk sk-line sk-line-sm"></div>
      </div>
      <div class="splash-body">
        <div class="sk sk-line sk-line-lg"></div>
        <div class="sk sk-line"></div>
        <div class="sk sk-line"></div>
        <div class="sk sk-line sk-line-sm"></div>
        <p class="splash-mark">Swage</p>
      </div>
    </div>
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
    streamingStore={composerStreamingStore}
    {spaceStore}
    onSpaceChange={handleSpaceChange}
    onRetry={handleRetry}
    {activeSurface}
    {pagesStore}
    onSelectSurface={handleSelectSurface}
    {desktopRuntime}
    {themeMode}
    {displayPreferences}
    onSelectTheme={setThemeMode}
    {modelOptions}
    {modelCatalogueReady}
    {defaultModelId}
    {agentDefinitions}
    onNewConversation={startNewConversation}
    route={currentRoute}
    onNavigate={navigate}
    onSettingsSaved={refreshModelCatalogueAfterSave}
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
    background: var(--paper);
    color: var(--ink);
    padding: 24px;
  }

  /* Skeleton of the window grid: a nav column and a body column, each carrying
     placeholder blocks where content will sit. */
  .splash-frame {
    width: 100%;
    max-width: 1100px;
    height: 100%;
    max-height: 640px;
    border: var(--border-strong);
    background: var(--paper);
    display: grid;
    grid-template-columns: clamp(220px, 18vw, 280px) 1fr;
    overflow: hidden;
  }

  .splash-nav,
  .splash-body {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px;
  }

  .splash-nav {
    background: var(--paper-deep);
    border-right: var(--border-strong);
  }

  .splash-body {
    position: relative;
    justify-content: center;
  }

  .splash-mark {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    font-family: var(--font-display);
    font-size: var(--font-display-1);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink-soft);
    opacity: 0.35;
    margin: 0;
  }

  .sk {
    height: 12px;
    background: var(--rule-faint);
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

  /* ---- Error screen --------------------------------------------------------- */
  /*
    Riso treatment: paper background, navy display heading, mono detail line,
    flat ink-bordered Retry button.
  */

  .error-screen {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 32px;
    background: var(--paper);
    color: var(--ink);
  }

  .error-heading {
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 32px;
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink);
    margin: 0;
  }

  .error-detail {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-soft);
    margin: 0;
  }

  .error-screen button {
    margin-top: 8px;
    padding: 8px 18px;
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-display);
    font-size: 16px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .error-screen button:hover {
    background: var(--ink);
    color: var(--paper);
  }

  /* ---- Not-found notice ----------------------------------------------------- */
  /* Fluoro-orange bar pinned to the bottom — keys with the alert banner. */

  .not-found-notice {
    position: fixed;
    bottom: 16px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    background: var(--accent);
    color: var(--paper);
    border: 2px solid var(--ink);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    z-index: 100;
  }

  .dismiss {
    background: none;
    border: none;
    cursor: pointer;
    color: var(--paper);
    font-family: var(--font-mono);
    font-size: 14px;
    line-height: 1;
    padding: 0;
  }

  .dismiss:hover {
    opacity: 0.7;
  }
</style>
