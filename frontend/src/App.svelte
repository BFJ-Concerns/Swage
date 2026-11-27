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
  import PairingDialog from "./lib/components/PairingDialog.svelte";
  import { getConversation, getThread } from "./lib/api/conversations";
  import { fetchSettings } from "./lib/api/settings";
  import { openSharedWorkSurfaceChannel } from "./lib/api/work-surface";
  import { listEffectiveAgentDefinitions } from "./lib/api/agent-definitions";
  import type { EffectiveAgentDefinitionResponse } from "./lib/protocol/EffectiveAgentDefinitionResponse";
  import type { ConversationStore } from "./lib/stores/conversations";
  import type { ConversationDetail } from "./lib/types";
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
  import { watchOpenConversation } from "./lib/open-conversation-updates";
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

  // A turn can outlive the socket it began on. While recovering it, follow
  // the gateway's session updates as well as the new socket.
  let streamingStore: StreamingStore;
  const recoveringTurns = new Map<string, { generation: number; reportMissingRequest: boolean }>();

  function recoveringTurn(conversationId: string) {
    const recovery = recoveringTurns.get(conversationId);
    const stream = streamingStore.streamFor?.(conversationId);
    if (recovery && stream?.isStreaming && stream.activeTurnGeneration === recovery.generation) {
      return recovery;
    }
    recoveringTurns.delete(conversationId);
    return undefined;
  }

  function streamIsInView(conversation: ConversationDetail, streamThreadId: string | null | undefined): boolean {
    const activeThreadId = conversationStore.activeThreadId ?? null;
    if (!streamThreadId || streamThreadId === conversation.root_thread_id) return activeThreadId === null;
    return streamThreadId === activeThreadId;
  }

  async function refreshOpenConversation(): Promise<void> {
    const conversationId = conversationStore.activeConversation?.id;
    if (!conversationId) return;
    const recovery = recoveringTurn(conversationId);
    await conversationStore.refreshActiveConversation();
    const refreshed = conversationStore.activeConversation;
    if (recovery && recoveringTurn(conversationId) === recovery && refreshed?.id === conversationId &&
        streamIsInView(refreshed, streamingStore.streamFor?.(conversationId).threadId)) {
      streamingStore.reconcileAfterReconnect(
        recovery.generation, conversationId, refreshed.messages, refreshed.queued, recovery.reportMissingRequest,
      );
      recoveringTurn(conversationId);
    }
  }

  async function refreshRecoveringConversation(conversationId: string): Promise<void> {
    const recovery = recoveringTurn(conversationId);
    try {
      const recovered = await getConversation(conversationId);
      const threadId = streamingStore.streamFor?.(conversationId).threadId;
      const root = !threadId || threadId === recovered.root_thread_id;
      const messages = root ? recovered.messages : (await getThread(threadId)).messages;
      if (recovery && recoveringTurn(conversationId) !== recovery) return;
      if (root && streamIsInView(recovered, threadId)) {
        conversationStore.replaceActiveConversationIfCurrent?.(recovered);
      }
      if (recovery) {
        streamingStore.reconcileAfterReconnect(
          recovery.generation, conversationId, messages, recovered.queued, recovery.reportMissingRequest,
        );
        recoveringTurn(conversationId);
      }
    } catch {
      // A failed read leaves the local stream visible for the next update.
    }
  }

  const connectionStore = createConnectionStore({
    onReconnect: async (conversationId, sentPendingMessages) => {
      streamingStore.clearConnectionError(conversationId);
      const stream = streamingStore.streamFor?.(conversationId);
      if (stream?.isStreaming) {
        recoveringTurns.set(conversationId, {
          generation: stream.activeTurnGeneration,
          // A frame sent on this socket may still be on its way when the
          // HTTP read completes. Wait for the gateway to account for it.
          reportMissingRequest: !sentPendingMessages,
        });
      }
      const active = conversationStore.activeConversation;
      if (active?.id === conversationId && streamIsInView(active, stream?.threadId)) {
        await refreshOpenConversation().catch(() => {});
      } else {
        await refreshRecoveringConversation(conversationId);
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

  // Surfaces and session lists share the gateway’s page-wide signal channel.
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

  async function openConversation(id: string, isCurrent: () => boolean): Promise<void> {
    actionError = null;
    scopeGeneration += 1;
    await conversationStore.selectConversation(id, isCurrent);
    if (!isCurrent()) return;
    if (currentRoute.surfaceId) activeSurface = "surfaces";
    else keepSurfaceForConversation();
    pendingStart = null;
    connectionStore.connectTo(id);

    const conversation = conversationStore.activeConversation;
    if (conversation) {
      await bringScopeIntoView(conversation.topic_id ?? null).catch((error) => {
        if (isCurrent()) actionError = describeActionError("Could not load session scope", error);
      });
    }
  }

  // The sidebar and agent list follow the session's scope: its topic, or
  // the general scope for a session without one. A scope already in view
  // is left alone so nothing reloads needlessly.
  async function bringScopeIntoView(
    topicId: string | null,
    knownDefinitions?: EffectiveAgentDefinitionResponse[],
  ): Promise<void> {
    const inView = topicStore.currentTopic?.id ?? null;
    if (inView === topicId && scopeLoadedFor === topicId) return;
    scopeLoadedFor = topicId;
    agentDefinitions = knownDefinitions ?? [];
    // Clear the old rows before publishing the new scope. Awaiting even a
    // local topic selection can yield a rendered frame beneath its header.
    const loading = conversationStore.loadConversations(topicId);
    if (topicId && inView !== topicId) {
      await Promise.all([loading, topicStore.selectTopic(topicId)]);
    } else {
      if (!topicId && inView) topicStore.closeTopic();
      await loading;
    }
    if (scopeLoadedFor === topicId) await loadScopeAgentDefinitions(topicId);
  }

  let scopeDefinitionsGeneration = 0;
  async function loadScopeAgentDefinitions(topicId: string | null): Promise<void> {
    const generation = ++scopeDefinitionsGeneration;
    try {
      const definitions = await listEffectiveAgentDefinitions(topicId);
      if (generation === scopeDefinitionsGeneration && scopeLoadedFor === topicId) {
        agentDefinitions = definitions;
      }
    } catch (error) {
      console.error("Failed to load agent definitions:", error);
    }
  }

  // The scope whose sidebar and agent list are loaded; null for the general
  // scope, undefined before any load.
  let scopeLoadedFor: string | null | undefined = undefined;

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

  function startNewConversation(): void {
    void startSession({ topicId: topicStore.currentTopic?.id ?? null, agentId: null }, activeSurface);
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
    refreshConversations: () => conversationStore.refreshConversations!(),
    selectConversation: (id) => router.navigate(conversationPath(id), { keepPreviousOnMissing: true }),
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
  // Links, session selectors and browser history share the same loading path.

  function handleRouteChange(route: Route): void {
    currentRoute = route;
    notFound = route.notFound;
    // A URL that names a surface means the work surface: the pane opens the
    // surface once the session is in view.
    if (route.conversationId && route.conversationId === conversationStore.activeConversation?.id) {
      if (route.surfaceId) activeSurface = "surfaces";
      else keepSurfaceForConversation();
    }
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
      selectConversation: openConversation,
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
  let globalModelOptions = $state<string[]>([]);
  let modelCatalogueReady = $state(false);
  let globalDefaultModelId = $state<string | null>(null);
  const defaultModelId = $derived(topicStore.currentTopic
    ? topicStore.currentTopic.effective_default_model ?? null : globalDefaultModelId);
  const modelOptions = $derived(topicStore.currentTopic && !defaultModelId ? []
    : defaultModelId && !globalModelOptions.includes(defaultModelId)
      ? [...globalModelOptions, defaultModelId] : globalModelOptions);
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
    globalModelOptions = Object.keys(settings.models);
    globalDefaultModelId = settings.default_model;
    modelCatalogueReady = true;
  }

  async function refreshModelCatalogueAfterSave(): Promise<void> {
    try {
      await refreshModelCatalogue();
      await topicStore.loadTopics();
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

  // A conversation open in this tab while another tab holds its socket follows
  // the gateway's session signal, so a turn stopped or settled elsewhere — or
  // by this tab's own Stop — shows here too.
  onMount(() =>
    watchOpenConversation({
      openConversationId: () => conversationStore.activeConversation?.id ?? null,
      holdsSocket: () => {
        const id = conversationStore.activeConversation?.id;
        return connectionStore.status === "connected" && (id === undefined || recoveringTurn(id) === undefined);
      },
      refresh: refreshOpenConversation,
    }),
  );

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
  async function startSession(
    choice: { topicId: string | null; agentId: string | null },
    surface: Surface = "conversation",
  ): Promise<void> {
    try {
      scopeGeneration += 1;
      connectionStore.deselect?.();
      conversationStore.clearActiveConversation();
      activeSurface = surface;
      router.navigate("/");
      const knownDefinitions = startScope === choice.topicId ? startAgentDefinitions : undefined;
      const loading = bringScopeIntoView(choice.topicId, knownDefinitions);
      pendingStart = choice;
      await loading;
    } catch (err) {
      console.error("Failed to start a session:", err);
    }
  }

  // The front door's start control changed its scope: offer that scope's
  // agents. The request is checked when it settles so a slow answer for an
  // earlier choice never replaces a later one.
  let startScopeGeneration = 0;
  let startScope: string | null | undefined;
  async function loadStartAgentDefinitions(topicId: string | null): Promise<void> {
    if (startScope !== topicId) startAgentDefinitions = [];
    startScope = topicId;
    const generation = ++startScopeGeneration;
    try {
      const definitions = await listEffectiveAgentDefinitions(topicId);
      if (generation === startScopeGeneration) startAgentDefinitions = definitions;
    } catch (error) {
      if (generation === startScopeGeneration) startAgentDefinitions = [];
      console.error("Failed to load agent definitions:", error);
    }
  }

  onMount(() => {
    const channel = openSharedWorkSurfaceChannel();
    function refreshPickers(): void {
      if (scopeLoadedFor !== undefined) void loadScopeAgentDefinitions(scopeLoadedFor);
      if (startScope !== undefined) void loadStartAgentDefinitions(startScope);
    }
    function refreshUnseenRecovery(id: string): void {
      const active = conversationStore.activeConversation;
      if (active?.id === id && streamIsInView(active, streamingStore.streamFor?.(id).threadId)) return;
      if (recoveringTurn(id) !== undefined) void refreshRecoveringConversation(id);
    }
    function refreshUnseenRecoveries(): void {
      for (const id of recoveringTurns.keys()) refreshUnseenRecovery(id);
    }
    channel.onSignal((event) => {
      if (event.type === "agent_definitions_changed" || event.type === "lagged") refreshPickers();
      if (event.type === "session_updated") refreshUnseenRecovery(event.session_id);
      if (event.type === "lagged") refreshUnseenRecoveries();
    });
    channel.onReconnect(() => {
      refreshPickers();
      refreshUnseenRecoveries();
    });
    return () => channel.close();
  });

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

      // 3. Open the session and its scope when the URL names one.
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

<PairingDialog />

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
