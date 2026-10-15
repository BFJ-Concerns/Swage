<!--
  Root application component.

  Startup sequence:
    1. Create stores (synchronous, no IO).
    2. Wire cross-store events: connection → streaming, reconnect → refresh.
    3. onMount: load the topic list, then initialise the router (reads the
       URL, selects any conversation referenced by the path — which brings
       that conversation's topic into scope).
    4. Render: loading splash → layout, or error screen if the gateway is unreachable.

  The front door is the recent-sessions list; a topic is the context a
  session carries, never a place the user must open first. Starting a session
  from the front door opens the composer in the chosen scope (a topic or
  none) and the first message creates the conversation there.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import AppLayout from "./lib/components/AppLayout.svelte";
  import { getConversation } from "./lib/api/conversations";
  import { fetchSettings } from "./lib/api/settings";
  import { listEffectiveAgentDefinitions } from "./lib/api/agent-definitions";
  import type { EffectiveAgentDefinitionResponse } from "./lib/protocol/EffectiveAgentDefinitionResponse";
  import type { ConversationStore } from "./lib/stores/conversations";
  import { ApiError } from "./lib/api/error";
  import { createConversationStore } from "./lib/stores/conversations";
  import { createConnectionStore } from "./lib/stores/connection";
  import { createSurfacesStore } from "./lib/stores/surfaces";
  import {
    createStreamingStore,
    withConversationAutoCreate,
    type StreamingStore,
  } from "./lib/stores/streaming";
  import { createTopicStore } from "./lib/stores/topics";
  import { conversationPath, createRouter } from "./lib/router";
  import type { Route } from "./lib/router";
  import { getDesktopRuntime } from "./lib/runtime";
  import {
    DISPLAY_PREFERENCES_CHANGED,
    readDisplayPreferences,
    writeDisplayPreferences,
    type DisplayPreferences,
  } from "./lib/display-preferences";
  type Surface = "conversation" | "operator" | "surfaces";

  // ---------------------------------------------------------------------------
  // Store creation
  // ---------------------------------------------------------------------------

  const topicStore = createTopicStore();
  const conversationStore = createConversationStore();

  // A reconnect reconciles the conversation that owns the socket, even while
  // another conversation is selected.
  let streamingStore: StreamingStore;
  const connectionStore = createConnectionStore({
    onReconnect: async (conversationId) => {
      streamingStore.clearConnectionError(conversationId);
      const recoveredTurnGeneration = streamingStore.streamFor?.(conversationId)
        .activeTurnGeneration ?? 0;
      if (conversationStore.activeConversation?.id === conversationId) {
        await conversationStore.refreshActiveConversation();
        streamingStore.reconcileAfterReconnect(recoveredTurnGeneration);
      } else {
        const recoveredConversation = await getConversation(conversationId);
        // The owner may have become active while its off-screen recovery was
        // pending. Install the same authoritative snapshot in that case.
        conversationStore.replaceActiveConversationIfCurrent?.(recoveredConversation);
        streamingStore.reconcileAfterReconnect(
          recoveredTurnGeneration,
          conversationId,
          recoveredConversation.messages,
        );
      }
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
  connectionStore.onServerEvent((event, conversationId) =>
    streamingStore.handleServerEvent(event, conversationId),
  );

  // The surfaces store opens its own work-surface channel: surface signals are not
  // turn events, and an open surface must stay live even with no conversation
  // selected.
  const surfacesStore = createSurfacesStore();

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
    actionError = null;
    scopeGeneration += 1;
    await conversationStore.selectConversation(id);
    keepSurfaceForConversation();

    // A deep-linked conversation may belong to a topic other than the one
    // currently in scope (or none, on first load). Bring the topic into
    // scope so the navigation and conversation list reflect it. A
    // conversation without a topic runs in the general scope and moves no
    // topic into scope.
    const conv = conversationStore.activeConversation;
    if (conv) await bringScopeIntoView(conv.topic_id ?? null);

    // Selecting a session settles any pending start: the composer now
    // belongs to this conversation.
    pendingStart = null;
    connectionStore.connectTo(id);
    // router is declared below but will be initialised before this is called.
    // The URL may already name one of this session's surfaces (a deep link
    // or a browser Back): keep it, so the surface opens.
    const surfaceId =
      currentRoute.conversationId === id ? currentRoute.surfaceId : null;
    router.navigate(conversationPath(id, surfaceId));
  }

  // The sidebar and agent list follow the session's scope: its topic, or
  // the general scope for a session without one. A scope already in view
  // is left alone so nothing reloads needlessly.
  async function bringScopeIntoView(topicId: string | null): Promise<void> {
    const inView = topicStore.currentTopic?.id ?? null;
    if (inView === topicId && scopeLoadedFor === topicId) return;
    scopeLoadedFor = topicId;
    // Clear the old rows before publishing the new scope. Awaiting even a
    // local topic selection can yield a rendered frame beneath its header.
    const loading = conversationStore.loadConversations(topicId);
    if (topicId && inView !== topicId) {
      await Promise.all([loading, topicStore.selectTopic(topicId)]);
    } else {
      if (!topicId && inView) topicStore.closeTopic();
      await loading;
    }
    try {
      agentDefinitions = await listEffectiveAgentDefinitions(topicId);
    } catch (error) {
      console.error("Failed to load agent definitions:", error);
    }
  }
  // The scope whose sidebar and agent list are loaded; null for the general
  // scope, undefined before any load.
  let scopeLoadedFor: string | null | undefined = undefined;

  async function selectAndNavigate(id: string): Promise<void> {
    try {
      await selectAndNavigateOrThrow(id);
    } catch (err) {
      actionError = describeActionError("Could not open conversation", err);
    }
  }

  // Creates a conversation in the scope in view, connects, and navigates to
  // it. Returns the new conversation's id, or null when creation failed —
  // the auto-create composer path uses the id to decide whether its pending
  // turn can be dispatched. The scope is captured when creation starts and
  // checked when it settles: a session created for one scope is never
  // activated after the user has moved to another.
  async function createAndNavigate(
    selectedModelId?: string | null,
    agentId?: string | null,
    effort?: import("./lib/protocol/Effort").Effort | null,
  ): Promise<string | null> {
    try {
      actionError = null;
      keepSurfaceForConversation();
      // The scope in view, or none: a conversation started without a topic
      // runs in the general scope.
      const topicId = topicStore.currentTopic?.id ?? null;
      const generation = ++scopeGeneration;
      await conversationStore.createConversation(
        topicId,
        selectedModelId,
        agentId,
        effort,
        () => generation === scopeGeneration,
      );
      if (generation !== scopeGeneration) {
        // The scope changed while the request was in flight. The session
        // exists in the scope it was asked for; it is not brought into the
        // one now in view.
        return null;
      }
      const id = conversationStore.activeConversation?.id;
      if (!id) return null;
      pendingStart = null;
      connectionStore.connectTo(id);
      router.navigate(conversationPath(id));
      return id;
    } catch (err) {
      actionError = describeActionError("Could not create conversation", err);
      return null;
    }
  }

  // Bumped whenever the scope in view changes, so an in-flight creation can
  // tell it has been overtaken.
  let scopeGeneration = 0;

  function startNewConversation(agentId?: string): void {
    void createAndNavigate(undefined, agentId);
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
    get activeThreadId() {
      return conversationStore.activeThreadId;
    },
    get branches() {
      return conversationStore.branches;
    },
    loadConversations: (topicId) =>
      conversationStore.loadConversations(topicId),
    selectConversation: selectAndNavigate,
    // The proxy captures topicId from topicStore at call time so child
    // components don't need to know about topics.
    createConversation: async (_topicId, selectedModelId, agentId, effort) => {
      await createAndNavigate(selectedModelId, agentId, effort);
    },
    deleteConversation: (id) => conversationStore.deleteConversation(id),
    clearActiveConversation: () => conversationStore.clearActiveConversation(),
    appendOptimisticUserMessage: (msg) =>
      conversationStore.appendOptimisticUserMessage(msg),
    appendAssistantMessage: (msg, conversationId) =>
      conversationStore.appendAssistantMessage(msg, conversationId),
    appendRecordedMessage: (msg, conversationId) =>
      conversationStore.appendRecordedMessage(msg, conversationId),
    appendQueuedMessage: (queued, conversationId) =>
      conversationStore.appendQueuedMessage(queued, conversationId),
    clearQueuedMessages: (conversationId, queuedIds) =>
      conversationStore.clearQueuedMessages(conversationId, queuedIds),
    refreshActiveConversation: () =>
      conversationStore.refreshActiveConversation(),
    selectThread: (threadId) => conversationStore.selectThread!(threadId),
    branchFrom: (messageId) => conversationStore.branchFrom!(messageId),
    abandonActiveBranch: () => conversationStore.abandonActiveBranch!(),
  };

  // The streaming store handed to the layout: identical to the base store,
  // except that sending with no conversation in scope creates one in the
  // current topic first (connect, then dispatch — the WebSocket client queues
  // the frame during the handshake). The composer that renders in an empty
  // topic therefore just works instead of dropping the message.
  const composerStreamingStore = withConversationAutoCreate(streamingStore, {
    hasActiveConversation: () => conversationStore.activeConversation !== null,
    createConversation: createAndNavigate,
  });

  // ---------------------------------------------------------------------------
  // Router
  // ---------------------------------------------------------------------------
  // selectAndNavigate is passed so router.init() can activate the right
  // conversation when the surface loads with a deep URL like /conversations/:id.

  function handleRouteChange(route: Route): void {
    currentRoute = route;
    notFound = route.notFound;
    // A URL that names a surface means the work surface: the pane opens the
    // surface once the session is in view.
    if (route.surfaceId) activeSurface = "surfaces";
    // A conversation the URL named is gone. Someone who was working in a
    // scope stays there with the composer ready, so the next message starts
    // afresh where they were; with no scope in view, the front door serves.
    if (route.notFound && scopeLoadedFor !== undefined && !pendingStart) {
      pendingStart = { topicId: scopeLoadedFor, agentId: null };
    }
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
        connectionStore.deselect?.();
        conversationStore.clearActiveConversation();
      },
      selectConversation: selectAndNavigateOrThrow,
    },
    onRouteChange: handleRouteChange,
    onNavigationError: (error) => {
      actionError = describeActionError("Could not open conversation", error);
    },
  });

  // ---------------------------------------------------------------------------
  // UI state
  // ---------------------------------------------------------------------------

  let isInitialising = $state(true);
  let loadError = $state<string | null>(null);
  let actionError = $state<string | null>(null);

  function describeActionError(action: string, error: unknown): string {
    const detail = error instanceof ApiError && error.code
      ? `${error.code}: ${error.message}`
      : error instanceof Error ? error.message : String(error);
    return `${action}: ${detail}`;
  }
  // Set to true when the router detects that the URL contained a stale ID.
  let notFound = $state(false);
  let activeSurface = $state<Surface>("conversation");
  let currentRoute = $state<Route>(router.currentRoute);
  let modelOptions = $state<string[]>([]);
  let modelCatalogueReady = $state(false);
  let defaultModelId = $state<string | null>(null);
  // The definitions offered by the scope in view: the composer's agent list.
  let agentDefinitions = $state<EffectiveAgentDefinitionResponse[]>([]);
  // The definitions offered by the scope chosen on the front door's start
  // control, which may differ from the scope in view.
  let startAgentDefinitions = $state<EffectiveAgentDefinitionResponse[]>([]);
  // A session about to start: the scope and agent chosen on the front door.
  // The composer is open; the first message creates the conversation.
  let pendingStart = $state<{ topicId: string | null; agentId: string | null } | null>(null);
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

    // The surfaces surface keeps the conversation socket: the conversation is
    // the control channel beside the work surface rather than a place to
    // navigate away to. Surface liveness does not depend on it — the surfaces
    // store listens on its own channel.
    if (next === "operator") return;

    // Conversations and Surfaces share the same ambient channel. Moving between
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

  // Starts a session from the front door: the chosen scope comes into view
  // with the composer open, and the first message creates the conversation
  // there. Nothing is created until then, so an abandoned start leaves no
  // empty session behind.
  async function startSession(choice: { topicId: string | null; agentId: string | null }): Promise<void> {
    try {
      scopeGeneration += 1;
      connectionStore.disconnect();
      conversationStore.clearActiveConversation();
      activeSurface = "conversation";
      pendingStart = choice;
      router.navigate("/");
      await bringScopeIntoView(choice.topicId);
    } catch (err) {
      console.error("Failed to start a session:", err);
    }
  }

  // The front door's start control changed its scope: offer that scope's
  // agents. The request is checked when it settles so a slow answer for an
  // earlier choice never replaces a later one.
  let startScopeGeneration = 0;
  async function loadStartAgentDefinitions(topicId: string | null): Promise<void> {
    const generation = ++startScopeGeneration;
    try {
      const definitions = await listEffectiveAgentDefinitions(topicId);
      if (generation === startScopeGeneration) startAgentDefinitions = definitions;
    } catch (error) {
      if (generation === startScopeGeneration) startAgentDefinitions = [];
      console.error("Failed to load agent definitions:", error);
    }
  }

  async function createTopic(name: string): Promise<void> {
    await topicStore.createTopic(name);
    // Creating a topic is not opening it: the front door stays as it was,
    // with the new topic offered in its selector.
    topicStore.closeTopic();
  }

  // Back to the front door: no session in view, so the recent-sessions list
  // owns the body again. The scope in view is left as it is — it is context
  // for the next start, not a place the user was standing.
  function showRecentSessions(): void {
    scopeGeneration += 1;
    connectionStore.disconnect();
    conversationStore.clearActiveConversation();
    pendingStart = null;
    router.navigate("/");
  }

  onMount(async () => {
    try {
      // 1. Load the topic list. No topic is restored as "open": the front
      //    door is the recent-sessions list, and a session brings its own
      //    scope into view when it is selected.
      await topicStore.loadTopics();
      topicStore.closeTopic();

      // 2. Fetch global model settings. Scoped data (conversations, agent
      //    definitions) loads when a session comes into view.
      try {
        await refreshModelCatalogue();
      } catch (error) {
        console.error("Failed to load model settings:", error);
      }

      // 3. Apply the URL. If it points at a conversation, selectAndNavigate
      //    will bring its scope into view along the way.
      await router.init();
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
    {topicStore}
    {pendingStart}
    onStartSession={(choice) => void startSession(choice)}
    onCreateTopic={createTopic}
    {startAgentDefinitions}
    onStartScopeChange={(topicId) => void loadStartAgentDefinitions(topicId)}
    onOpenSessions={showRecentSessions}
    onRetry={handleRetry}
    {activeSurface}
    {surfacesStore}
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

  {#if actionError}
    <div class="not-found-notice" role="alert">
      {actionError}
      <button class="dismiss" aria-label="Dismiss error" onclick={() => (actionError = null)}>✕</button>
    </div>
  {/if}

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
