<!--
  Root application component.

  Startup sequence:
    1. Create stores (synchronous, no IO).
    2. Wire cross-store events: connection → streaming, reconnect → refresh.
    3. onMount: load benches (auto-selects default), load conversations for
       the selected bench, then initialise router (reads URL, selects any
       conversation referenced by the path).
    4. Render: loading splash → layout, or error screen if the gateway is unreachable.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import AppLayout from "./lib/components/AppLayout.svelte";
  import { fetchLensRuntime, updateLensRuntime } from "./lib/api/lens";
  import { fetchSettings } from "./lib/api/settings";
  import type { ConversationStore } from "./lib/stores/conversations";
  import { createConversationStore } from "./lib/stores/conversations";
  import { createConnectionStore } from "./lib/stores/connection";
  import { createStreamingStore } from "./lib/stores/streaming";
  import { createBenchStore } from "./lib/stores/benches";
  import { createRouter } from "./lib/router";
  import type { Route } from "./lib/router";
  import { getDesktopRuntime } from "./lib/runtime";
  import type { AppLens, LensDescriptor, LensSurface } from "./lib/types";

  // ---------------------------------------------------------------------------
  // Store creation
  // ---------------------------------------------------------------------------

  const benchStore = createBenchStore();
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
      activeSurface = "conversation";
      await conversationStore.selectConversation(id);

      // A deep-linked conversation may belong to a bench other than the one
      // currently in scope (or none, on first load). Bring the bench into
      // scope so the nav, conversation list, and lens runtime reflect it.
      const conv = conversationStore.activeConversation;
      if (conv && benchStore.currentBench?.id !== conv.bench_id) {
        await benchStore.selectBench(conv.bench_id);
        await conversationStore.loadConversations(conv.bench_id);
        await refreshLensRuntime("conversation");
      }

      connectionStore.connectTo(id);
      // router is declared below but will be initialised before this is called.
      router.navigate(`/conversations/${id}`);
    } catch (err) {
      console.error("Failed to select conversation:", err);
    }
  }

  async function createAndNavigate(): Promise<void> {
    try {
      activeSurface = "conversation";
      // Use the currently selected bench — should always be set after startup.
      const benchId = benchStore.currentBench?.id ?? "";
      await conversationStore.createConversation(benchId);
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
    loadConversations: (benchId) =>
      conversationStore.loadConversations(benchId),
    selectConversation: selectAndNavigate,
    // No-arg wrapper: the proxy captures benchId from benchStore at
    // call time so ConversationList doesn't need to know about benches.
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
  let activeLens = $state<AppLens>("assistant");
  let activeSurface = $state<LensSurface>("conversation");
  let availableLenses = $state<LensDescriptor[]>([]);
  let currentRoute = $state<Route>(router.currentRoute);
  let activeModelContextWindow = $state<number | null>(null);
  const desktopRuntime = getDesktopRuntime();

  // ---------------------------------------------------------------------------
  // Theme — light (riso) / dark (cyanotype) / system
  // ---------------------------------------------------------------------------
  // The chosen mode is what the user picked in the toggle; the resolved theme
  // is what's actually painted (system mode resolves through the OS preference).

  // Type lives in a shared module so child components type their props the
  // same way without round-tripping through a Svelte instance script.
  type ThemeMode = import("./lib/theme").ThemeMode;
  const THEME_KEY = "swage:theme";

  function readStoredTheme(): ThemeMode {
    if (typeof localStorage === "undefined") return "system";
    const value = localStorage.getItem(THEME_KEY);
    return value === "light" || value === "dark" || value === "system" ? value : "system";
  }

  let themeMode = $state<ThemeMode>(readStoredTheme());

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

  function setThemeMode(mode: ThemeMode): void {
    themeMode = mode;
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(THEME_KEY, mode);
    }
    applyTheme(mode);
  }

  onMount(() => {
    applyTheme(themeMode);

    // Re-resolve when the OS theme changes, but only while in "system" mode —
    // an explicit light/dark choice should stick regardless of OS toggle.
    if (typeof window === "undefined" || !window.matchMedia) return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => {
      if (themeMode === "system") applyTheme("system");
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  });

  function getLensDescriptor(lensId: AppLens): LensDescriptor | undefined {
    return availableLenses.find((lens) => lens.id === lensId);
  }

  function fallbackSurfaceForModules(modules: LensSurface[]): LensSurface {
    if (modules.includes("conversation")) {
      return "conversation";
    }
    return modules[0] ?? "conversation";
  }

  function defaultSurfaceForLens(lensId: AppLens): LensSurface {
    const descriptor = getLensDescriptor(lensId);
    if (descriptor) {
      return descriptor.default_surface;
    }
    return "conversation";
  }

  // Passed to ConnectionIndicator so the user can trigger a manual reconnect.
  function handleRetry(): void {
    const id = conversationStore.activeConversation?.id;
    if (id) connectionStore.connectTo(id);
  }

  function handleSelectSurface(next: LensSurface): void {
    const descriptor = getLensDescriptor(activeLens);
    if (descriptor && !descriptor.ui_modules.includes(next)) {
      activeSurface = fallbackSurfaceForModules(descriptor.ui_modules);
      return;
    }

    activeSurface = next;

    if (next !== "conversation") {
      connectionStore.disconnect();
      return;
    }

    const id = conversationStore.activeConversation?.id;
    if (id) connectionStore.connectTo(id);
  }

  function navigate(path: string): void {
    router.navigate(path);
    currentRoute = router.currentRoute;
  }

  async function refreshLensRuntime(preferredSurface?: LensSurface): Promise<void> {
    const benchId = benchStore.currentBench?.id;
    if (!benchId) return;
    const runtime = await fetchLensRuntime(benchId);
    availableLenses = runtime.available_lenses;
    activeLens = runtime.active_lens;

    const descriptor = getLensDescriptor(runtime.active_lens);
    const nextSurface =
      preferredSurface && descriptor?.ui_modules.includes(preferredSurface)
        ? preferredSurface
        : defaultSurfaceForLens(runtime.active_lens);
    handleSelectSurface(nextSurface);
  }

  async function handleSelectLens(next: AppLens): Promise<void> {
    const benchId = benchStore.currentBench?.id;
    if (!benchId) return;
    const runtime = await updateLensRuntime(benchId, next);
    availableLenses = runtime.available_lenses;
    activeLens = runtime.active_lens;
    handleSelectSurface(defaultSurfaceForLens(runtime.active_lens));
  }

  // ---------------------------------------------------------------------------
  // Startup
  // ---------------------------------------------------------------------------

  // Called by BenchSelector when the user switches benches.
  // Reloads the conversation list for the newly active bench and clears
  // any cross-bench UI state (active conversation, WebSocket connection).
  async function handleBenchChange(): Promise<void> {
    try {
      const benchId = benchStore.currentBench?.id;
      if (!benchId) return;

      // Drop the WebSocket — it was bound to the previous bench's session.
      connectionStore.disconnect();
      // Reset the URL so the router doesn't try to restore a stale conversation.
      router.navigate("/");
      currentRoute = router.currentRoute;
      // Load the new bench's conversations.
      await conversationStore.loadConversations(benchId);
      await refreshLensRuntime("conversation");
    } catch (err) {
      console.error("Failed to switch bench:", err);
    }
  }

  onMount(async () => {
    try {
      // 1. Load the bench list. We deliberately do not auto-select — the user
      //    lands on the bench picker and chooses where to work.
      await benchStore.loadBenches();

      // 2. Fetch global model settings. Bench-scoped data (conversations,
      //    lens runtime) loads when a bench is opened — either via the
      //    picker or by a deep-linked conversation that selects its bench.
      const settings = await fetchSettings().catch((error) => {
        console.error("Failed to load model settings:", error);
        return null;
      });
      activeModelContextWindow = settings?.models[settings.default_model]?.context_window ?? null;

      // 3. Apply the URL. If it points at a conversation, selectAndNavigate
      //    will bring its bench into scope along the way.
      await router.init();
      currentRoute = router.currentRoute;

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
    {benchStore}
    onBenchChange={handleBenchChange}
    onRetry={handleRetry}
    {activeLens}
    {availableLenses}
    {activeSurface}
    onSelectLens={handleSelectLens}
    onRefreshLenses={refreshLensRuntime}
    onSelectSurface={handleSelectSurface}
    {desktopRuntime}
    {themeMode}
    onSelectTheme={setThemeMode}
    {activeModelContextWindow}
    route={currentRoute}
    onNavigate={navigate}
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
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
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
