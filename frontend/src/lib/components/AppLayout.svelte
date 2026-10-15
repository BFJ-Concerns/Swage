<!--
  The application shell.

  The front door is the recent-sessions list with the session-start control
  beneath it. Once a session is in view — selected from the list, deep-linked,
  or about to start — the top bar carries the surfaces the session offers:
  its conversation, its work surfaces, the shared library, the operator view,
  and settings. The conversation is ambient: it renders beside the work
  surfaces rather than being a place to navigate to, so moving between them
  keeps the same channel and history.

  A session's topic (or the general scope) is shown in the bar as context,
  with a link to that topic's settings; it is never a gate the user passes
  through to reach their work.
-->
<script lang="ts">
  import ConversationChannel from "./ConversationChannel.svelte";
  import ConversationList from "./ConversationList.svelte";
  import OperatorDashboard from "./OperatorDashboard.svelte";
  import SurfacesPane from "./SurfacesPane.svelte";
  import SurfaceLibrary from "./SurfaceLibrary.svelte";
  import SettingsPage from "./SettingsPage.svelte";
  import WorkflowRunView from "./WorkflowRunView.svelte";
  import RecentSessions from "./RecentSessions.svelte";
  import SessionStart from "./SessionStart.svelte";
  import TopicSettingsPage from "./TopicSettingsPage.svelte";
  import ThemeToggle from "./ThemeToggle.svelte";
  import WorkflowRunDetail from "./WorkflowRunDetail.svelte";
  import { fetchWorkflowRun } from "../api/workflow-runs";
  import { conversationPath } from "../router";
  import type { DisplayPreferences } from "../display-preferences";
  import type { Route } from "../router";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type { ThemeMode } from "../theme";
  import type { ConnectionStore } from "../stores/connection";
  import type { ConversationStore } from "../stores/conversations";
  import type { TopicStore } from "../stores/topics";
  import type { StreamingStore } from "../stores/streaming";
  import type { SurfacesStore } from "../stores/surfaces";
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";
  import type { WorkflowRunDetailResponse } from "../protocol/WorkflowRunDetailResponse";

  type Surface = "conversation" | "operator" | "surfaces";

  interface Props {
    conversationStore: ConversationStore;
    connectionStore: ConnectionStore;
    streamingStore: StreamingStore;
    topicStore?: TopicStore;
    // A session is about to start in this scope: the composer is open and
    // the first message creates it. Null when no start is pending.
    pendingStart?: { topicId: string | null; agentId: string | null } | null;
    onStartSession?: (choice: { topicId: string | null; agentId: string | null }) => void;
    onCreateTopic?: (name: string) => Promise<void>;
    // The definitions offered for the scope chosen on the front door; the
    // parent reloads them when that choice changes.
    startAgentDefinitions?: EffectiveAgentDefinitionResponse[];
    onStartScopeChange?: (topicId: string | null) => void;
    // Leave the open session and return to the recent-sessions list.
    onOpenSessions?: () => void;
    onRetry: () => void;
    activeSurface: Surface;
    surfacesStore?: SurfacesStore;
    onSelectSurface: (next: Surface) => void;
    desktopRuntime?: DesktopRuntimeInfo | null;
    themeMode?: ThemeMode;
    displayPreferences?: DisplayPreferences;
    onSelectTheme?: (mode: ThemeMode) => void;
    modelOptions?: string[];
    modelCatalogueReady?: boolean;
    defaultModelId?: string | null;
    route?: Route;
    onNavigate?: (path: string) => void;
    onSettingsSaved?: () => void | Promise<void>;
    agentDefinitions?: EffectiveAgentDefinitionResponse[];
    onNewConversation?: (agentId?: string) => void;
  }

  let {
    conversationStore,
    connectionStore,
    streamingStore,
    topicStore,
    pendingStart = null,
    onStartSession,
    onCreateTopic,
    startAgentDefinitions = [],
    onStartScopeChange,
    onOpenSessions,
    onRetry,
    activeSurface,
    surfacesStore,
    onSelectSurface,
    desktopRuntime,
    themeMode = "system",
    displayPreferences,
    onSelectTheme,
    modelOptions = [],
    modelCatalogueReady = false,
    defaultModelId = null,
    route,
    onNavigate,
    onSettingsSaved,
    agentDefinitions = [],
    onNewConversation,
  }: Props = $props();

  const currentTopic = $derived(topicStore?.currentTopic ?? null);
  const activeConversation = $derived(conversationStore.activeConversation);
  // A session is in view when one is selected or about to start; only with
  // neither does the front door own the body.
  const inScope = $derived(activeConversation !== null || pendingStart !== null);
  const scopeName = $derived(currentTopic?.name ?? "General");

  // The agent the composer starts the next session under. Seeded from the
  // front door's choice, and reset when the scope changes so an agent from
  // another scope never lingers.
  let selectedAgentId = $state(pendingStart?.agentId ?? "");
  let selectedAgentTopicId = $state<string | null | undefined>(undefined);
  $effect(() => {
    const currentTopicId = currentTopic?.id ?? null;
    if (selectedAgentTopicId === undefined) {
      selectedAgentTopicId = currentTopicId;
    } else if (currentTopicId !== selectedAgentTopicId) {
      selectedAgentId = "";
      selectedAgentTopicId = currentTopicId;
    } else if (
      selectedAgentId &&
      !agentDefinitions.some((definition) => definition.id === selectedAgentId)
    ) {
      selectedAgentId = "";
    }
  });
  // A new pending start (not the one seeded above) brings its own agent
  // choice; the user's later change in the composer is left alone.
  let seededStart = pendingStart;
  $effect(() => {
    if (pendingStart === seededStart) return;
    seededStart = pendingStart;
    if (pendingStart) selectedAgentId = pendingStart.agentId ?? "";
  });

  // The scope chosen on the front door, bound into the start control so its
  // agent list follows the choice.
  let startTopicId = $state("");
  $effect(() => {
    onStartScopeChange?.(startTopicId || null);
  });

  const workflowRunId = $derived(route?.workflowRunId ?? null);
  let workflowRunDetail = $state<WorkflowRunDetailResponse | null>(null);
  let workflowRunLoadState = $state<"loading" | "loaded" | "absent" | "error">(
    "loading",
  );
  let workflowRunError = $state("");
  $effect(() => {
    const requestedRunId = workflowRunId;
    workflowRunDetail = null;
    workflowRunError = "";
    if (!requestedRunId) return;
    workflowRunLoadState = "loading";
    fetchWorkflowRun(requestedRunId)
      .then((loaded) => {
        if (requestedRunId !== workflowRunId) return;
        workflowRunDetail = loaded;
        workflowRunLoadState = loaded === null ? "absent" : "loaded";
      })
      .catch((error: unknown) => {
        if (requestedRunId !== workflowRunId) return;
        workflowRunError = error instanceof Error ? error.message : String(error);
        workflowRunLoadState = "error";
      });
  });
  const settingsTab = $derived(route?.settingsTab ?? null);
  const settingsTopicSlug = $derived(route?.settingsTopicSlug ?? null);
  const settingsTopicTab = $derived(route?.settingsTopicTab ?? null);
  const settingsMode = $derived(settingsTab !== null || settingsTopicSlug !== null);
  const libraryMode = $derived(route?.library ?? false);
  const settingsTopic = $derived(
    settingsTopicSlug
      ? (topicStore?.topics ?? []).find((topic) => topic.slug === settingsTopicSlug) ?? null
      : null,
  );

  // The surface the URL names, if any: the pane opens it and keeps the
  // address in step when the user opens another.
  const routedSurfaceId = $derived(route?.surfaceId ?? null);

  function closeSettings(): void {
    onNavigate?.(activeConversation?.id ? conversationPath(activeConversation.id) : "/");
  }

  function closeLibrary(): void {
    onNavigate?.(activeConversation?.id ? conversationPath(activeConversation.id) : "/");
  }

  function selectSurface(next: Surface): void {
    if (workflowRunId || libraryMode || settingsMode) {
      onNavigate?.(activeConversation?.id ? conversationPath(activeConversation.id) : "/");
    }
    onSelectSurface(next);
  }

  function openSurfaceRoute(surfaceId: string | null): void {
    if (!activeConversation) return;
    onNavigate?.(conversationPath(activeConversation.id, surfaceId));
  }

  function openLibrary(): void {
    onNavigate?.("/library");
  }

  function openTopicSettings(): void {
    if (!currentTopic) return;
    onNavigate?.(`/settings/topic/${encodeURIComponent(currentTopic.slug)}/overview`);
  }

  function isSurfaceActive(surface: Surface): boolean {
    return !settingsMode && !libraryMode && !workflowRunId && activeSurface === surface;
  }
</script>

{#if inScope}
  <header class="topbar">
    <button type="button" class="sessions-btn" onclick={() => onOpenSessions?.()}>All sessions</button>
    <span class="scope" aria-label="Session scope">
      {#if currentTopic}<span class="scope-label">Topic</span>{/if}
      {#if currentTopic}
        <button type="button" class="scope-name" onclick={openTopicSettings} title="Topic settings">{scopeName}</button>
      {:else}
        <span class="scope-name">{scopeName}</span>
      {/if}
    </span>
    <nav aria-label="Surface">
      <button aria-pressed={isSurfaceActive("conversation")} class:active={isSurfaceActive("conversation")} onclick={() => selectSurface("conversation")}>Conversation</button>
      <button aria-pressed={isSurfaceActive("surfaces")} class:active={isSurfaceActive("surfaces")} onclick={() => selectSurface("surfaces")}>Surfaces</button>
      <button aria-pressed={libraryMode} class:active={libraryMode} onclick={openLibrary}>Library</button>
      <button aria-pressed={isSurfaceActive("operator")} class:active={isSurfaceActive("operator")} onclick={() => selectSurface("operator")}>Operator</button>
      <button aria-pressed={settingsMode} class:active={settingsMode} onclick={() => onNavigate?.("/settings/providers")}>Settings</button>
    </nav>
    <span class="topbar-end">
      {#if desktopRuntime}<span class="runtime">{desktopRuntime.managedGateway ? "MANAGED" : "REMOTE"}</span>{/if}
      {#if onSelectTheme}<ThemeToggle mode={themeMode} onSelect={onSelectTheme} />{/if}
    </span>
  </header>
{/if}

{#if workflowRunId}
  <!-- A run addressed directly by URL owns the body: its terminal account
       and detailed journal are the complete record of the visit. -->
  <main class="workflow-run">
    {#if !inScope}
      <button type="button" class="back-btn" onclick={() => onNavigate?.("/")}>All sessions</button>
    {/if}
    <WorkflowRunView
      detail={workflowRunDetail}
      loadState={workflowRunLoadState}
      errorMessage={workflowRunError}
    />
    <WorkflowRunDetail
      detail={workflowRunDetail}
      loadState={workflowRunLoadState}
      errorMessage={workflowRunError}
    />
  </main>
{:else if settingsMode}
  <!-- The settings window chrome lives here; the surfaces own their content only. -->
  <div class="settings-window">
    <header class="settings-chrome">
      <h2>Settings</h2>
      <nav aria-label="Settings sections">
        <button class:active={settingsTab === "providers"} onclick={() => onNavigate?.("/settings/providers")}>Providers</button>
        <button class:active={settingsTab === "agents"} onclick={() => onNavigate?.("/settings/agents")}>Agents</button>
        <button class:active={settingsTab === "commands"} onclick={() => onNavigate?.("/settings/commands")}>Commands</button>
        <button class:active={settingsTab === "skills"} onclick={() => onNavigate?.("/settings/skills")}>Skills</button>
        <button class:active={settingsTab === "automation"} onclick={() => onNavigate?.("/settings/automation")}>Automation</button>
        <button class:active={settingsTab === "voice"} onclick={() => onNavigate?.("/settings/voice")}>Voice</button>
        <button class:active={settingsTab === "memory"} onclick={() => onNavigate?.("/settings/memory")}>Memory</button>
        {#each topicStore?.topics ?? [] as topic (topic.id)}
          <button
            class="topic-tab"
            class:active={settingsTopicSlug === topic.slug}
            onclick={() => onNavigate?.(`/settings/topic/${encodeURIComponent(topic.slug)}/overview`)}
          >
            {topic.name}
          </button>
        {/each}
      </nav>
      <button class="close" onclick={closeSettings} aria-label="Close settings">×</button>
    </header>
    {#if settingsTopicSlug}
      <TopicSettingsPage topic={settingsTopic} tab={settingsTopicTab ?? undefined} {onNavigate} onTopicsChanged={() => topicStore?.loadTopics()} />
    {:else}
      <SettingsPage tab={settingsTab ?? undefined} topicId={currentTopic?.id ?? null} topics={topicStore?.topics ?? []} memoryPage={route?.settingsMemoryPage ?? null} {onSettingsSaved} />
    {/if}
  </div>
{:else if libraryMode}
  <!-- The shared library's catalogue: what is saved, described and tagged,
       reachable from any session or from the front door. -->
  <main class="library">
    {#if !inScope}
      <button type="button" class="back-btn" onclick={() => onNavigate?.("/")}>All sessions</button>
    {/if}
    {#if surfacesStore}
      <SurfaceLibrary
        store={surfacesStore}
        sessionId={activeConversation?.id ?? null}
        topics={topicStore?.topics ?? []}
        onClose={closeLibrary}
        onSurfaceAdded={(surfaceId) => {
          onSelectSurface("surfaces");
          openSurfaceRoute(surfaceId);
        }}
      />
    {/if}
  </main>
{:else if !inScope}
  <!--
    The front door: the control that starts a new session — with a topic, or
    without — then every scope's recent sessions, so a growing list never
    pushes the start control down the page.
  -->
  <main class="landing">
    <div class="landing-bar">
      <button type="button" onclick={openLibrary}>Library</button>
      <button type="button" onclick={() => onNavigate?.("/settings/providers")}>Settings</button>
      {#if onSelectTheme}<ThemeToggle mode={themeMode} onSelect={onSelectTheme} />{/if}
    </div>
    <div class="landing-start">
      <SessionStart
        topics={topicStore?.topics ?? []}
        agentDefinitions={startAgentDefinitions}
        bind:selectedTopicId={startTopicId}
        onStart={(choice) => onStartSession?.(choice)}
        {onCreateTopic}
      />
    </div>
    <RecentSessions
      topics={topicStore?.topics ?? []}
      onSelectSession={(id) => void conversationStore.selectConversation(id)}
      onOpenWorkflowRun={(id) => onNavigate?.(`/workflow-runs/${encodeURIComponent(id)}`)}
    />
  </main>
{:else if activeSurface === "operator"}
  <main class="operator"><OperatorDashboard /></main>
{:else if activeSurface === "surfaces"}
  <!--
    The work surface carries the control channel: the same channel markup and
    the same store instances as the conversation surface, so moving between
    surfaces keeps the conversation running rather than leaving it behind.
  -->
  <main class="surfaces-surface">
    <div class="work">
      {#if surfacesStore && activeConversation}
        <SurfacesPane
          store={surfacesStore}
          sessionId={activeConversation.id}
          {routedSurfaceId}
          onOpenSurface={openSurfaceRoute}
          onOpenLibrary={openLibrary}
        />
      {:else}
        <p class="no-session">Surfaces belong to a session: send the first message to start one, then its surfaces open here.</p>
      {/if}
    </div>
    <ConversationChannel {conversationStore} {connectionStore} {streamingStore} {onRetry} {displayPreferences} {modelOptions} {modelCatalogueReady} {defaultModelId} {agentDefinitions} bind:selectedAgentId {onNavigate} {onNewConversation} />
  </main>
{:else}
  <main class="conversation">
    <aside><ConversationList store={conversationStore} onNewConversation={() => onNewConversation?.(selectedAgentId || undefined)} /></aside>
    <ConversationChannel {conversationStore} {connectionStore} {streamingStore} {onRetry} {displayPreferences} {modelOptions} {modelCatalogueReady} {defaultModelId} {agentDefinitions} bind:selectedAgentId {onNavigate} {onNewConversation} />
  </main>
{/if}

<style>
  .topbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem 1rem;
    padding: .75rem;
    border-bottom: 1px solid var(--ink);
  }
  .topbar-end { display: flex; align-items: center; gap: 1rem; margin-left: auto; }
  .runtime { font-family: var(--font-mono); font-size: var(--font-label-sm); letter-spacing: 0.12em; color: var(--ink-soft); }
  .scope {
    display: inline-flex;
    align-items: baseline;
    gap: 0.4rem;
    font-family: var(--font-mono);
    font-size: var(--font-label-sm);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-soft);
    min-width: 0;
  }
  .scope-name { color: var(--ink); font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 14rem; }
  button.scope-name { background: none; border: none; padding: 0; font: inherit; letter-spacing: inherit; text-transform: inherit; cursor: pointer; text-decoration: underline dotted; }
  button.scope-name:hover { color: var(--ink-orange); }
  .landing { height: 100vh; overflow: auto; display: flex; flex-direction: column; gap: 24px; padding-bottom: 40px; }
  .landing-bar { display: flex; justify-content: flex-end; align-items: center; gap: .75rem; margin: .75rem .75rem 0; }
  .landing-start { padding: 0 40px; }
  .library { height: 100vh; overflow: auto; }
  .back-btn { margin: .75rem; }
  nav { display: flex; gap: .5rem; flex-wrap: wrap; }
  .settings-window { height: calc(100vh - 4rem); overflow: auto; }
  .settings-chrome { display: flex; align-items: center; justify-content: space-between; gap: .75rem; flex-wrap: wrap; padding: .75rem; border-bottom: 1px solid var(--ink); }
  .settings-chrome h2 { margin: 0; font-size: 1rem; }
  .settings-chrome .topic-tab { border-left: 1px dotted var(--rule-soft); padding-left: .5rem; }
  .settings-chrome .topic-tab ~ .topic-tab { border-left: none; padding-left: 0; }
  button.active { font-weight: bold; }
  .conversation { display: grid; grid-template-columns: minmax(16rem, 25%) 1fr; height: calc(100vh - 4rem); }
  .conversation aside { overflow: auto; border-right: 1px solid var(--ink); }
  /* The work surface and its docked channel share the conversation surface's
     two-column rule, mirrored: the work takes the wide column. */
  .surfaces-surface { display: grid; grid-template-columns: 1fr minmax(20rem, 33%); height: calc(100vh - 4rem); }
  .surfaces-surface .work { min-width: 0; overflow: auto; border-right: 1px solid var(--ink); }
  .no-session { padding: 1rem; color: var(--ink-soft); }
  .operator { min-width: 0; overflow: auto; }

  /* Phone-sized viewports: the bar wraps onto lines rather than clipping at
     the right edge, and the two-column surfaces stack so every pane and
     every control stays reachable. */
  @media (max-width: 720px) {
    .topbar { padding: .5rem; }
    .topbar-end { margin-left: 0; width: 100%; justify-content: space-between; }
    .scope-name { max-width: 9rem; }
    .landing-start { padding: 0 16px; }
    .conversation,
    .surfaces-surface {
      grid-template-columns: 1fr;
      grid-template-rows: auto 1fr;
      height: auto;
      min-height: calc(100vh - 4rem);
    }
    .conversation aside,
    .surfaces-surface .work { border-right: none; border-bottom: 1px solid var(--ink); max-height: 40vh; }
  }
</style>
