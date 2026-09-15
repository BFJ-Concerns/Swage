<script lang="ts">
  import ConversationChannel from "./ConversationChannel.svelte";
  import ConversationList from "./ConversationList.svelte";
  import OperatorDashboard from "./OperatorDashboard.svelte";
  import SurfacesPane from "./SurfacesPane.svelte";
  import SettingsPage from "./SettingsPage.svelte";
  import WorkflowRunView from "./WorkflowRunView.svelte";
  import TopicPicker from "./TopicPicker.svelte";
  import RecentSessions from "./RecentSessions.svelte";
  import TopicSelector from "./TopicSelector.svelte";
  import TopicSettingsPage from "./TopicSettingsPage.svelte";
  import ThemeToggle from "./ThemeToggle.svelte";
  import WorkflowRunDetail from "./WorkflowRunDetail.svelte";
  import { fetchWorkflowRun } from "../api/workflow-runs";
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
    onTopicChange?: () => void;
    onStartWithoutTopic?: () => void;
    // Leave the open topic or conversation and return to the recent-sessions
    // list.
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
    onTopicChange,
    onStartWithoutTopic,
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
  // The shell is in scope with a topic open or with a general-scope
  // conversation active; only with neither does the picker own the body.
  const inScope = $derived(currentTopic !== null || activeConversation !== null);
  let selectedAgentId = $state("");
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
  const settingsTopic = $derived(
    settingsTopicSlug
      ? (topicStore?.topics ?? []).find((topic) => topic.slug === settingsTopicSlug) ?? null
      : null,
  );

  function closeSettings(): void {
    onNavigate?.(activeConversation?.id ? `/conversations/${activeConversation.id}` : "/");
  }

  function selectSurface(next: Surface): void {
    if (workflowRunId) {
      onNavigate?.("/");
    }
    onSelectSurface(next);
  }

  function openCurrentTopicWorkspaces(): void {
    if (!currentTopic) return;
    onNavigate?.(`/settings/topic/${encodeURIComponent(currentTopic.slug)}/workspaces`);
  }
</script>

{#if inScope}
  <header class="topbar">
    <button type="button" class="sessions-btn" onclick={() => onOpenSessions?.()}>All sessions</button>
    <TopicSelector {topicStore} {onTopicChange} />
    <nav aria-label="Surface">
      <button aria-pressed={!settingsMode && activeSurface === "conversation"} class:active={!settingsMode && activeSurface === "conversation"} onclick={() => selectSurface("conversation")}>Conversations</button>
      <button aria-pressed={!settingsMode && activeSurface === "surfaces"} class:active={!settingsMode && activeSurface === "surfaces"} onclick={() => selectSurface("surfaces")}>Surfaces</button>
      <button aria-pressed={!settingsMode && activeSurface === "operator"} class:active={!settingsMode && activeSurface === "operator"} onclick={() => selectSurface("operator")}>Operator</button>
      <button aria-pressed={settingsMode} class:active={settingsMode} onclick={() => onNavigate?.("/settings/providers")}>Settings</button>
    </nav>
    {#if desktopRuntime}<span>{desktopRuntime.managedGateway ? "MANAGED" : "REMOTE"}</span>{/if}
    {#if onSelectTheme}<ThemeToggle mode={themeMode} onSelect={onSelectTheme} />{/if}
  </header>
{/if}

{#if workflowRunId}
  <!-- A run addressed directly by URL owns the body: its terminal account
       and detailed journal are the complete record of the visit. -->
  <main class="workflow-run">
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
        {#if currentTopic}
          <button class:active={settingsTopicTab === "workspaces"} onclick={openCurrentTopicWorkspaces}>Workspaces</button>
        {/if}
      </nav>
      <button class="close" onclick={closeSettings} aria-label="Close settings">×</button>
    </header>
    {#if settingsTopic}
      <TopicSettingsPage topic={settingsTopic} tab={settingsTopicTab ?? undefined} />
    {:else}
      <SettingsPage tab={settingsTab ?? undefined} topicId={currentTopic?.id ?? null} topics={topicStore?.topics ?? []} {onSettingsSaved} />
    {/if}
  </div>
{:else if !inScope}
  <!--
    The front door: the recent-sessions list across every scope first, the
    topic cards beneath it for starting somewhere new.
  -->
  <main class="landing">
    <button class="landing-settings" onclick={() => onNavigate?.("/settings/providers")}>Settings</button>
    <RecentSessions
      topics={topicStore?.topics ?? []}
      onSelectSession={(id) => void conversationStore.selectConversation(id)}
      onOpenWorkflowRun={(id) => onNavigate?.(`/workflow-runs/${encodeURIComponent(id)}`)}
    />
    <TopicPicker {topicStore} {onTopicChange} {onStartWithoutTopic} />
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
      {#if surfacesStore && currentTopic}
        <SurfacesPane store={surfacesStore} topicId={currentTopic.id} />
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
  .topbar { display: flex; align-items: center; gap: 1rem; padding: .75rem; border-bottom: 1px solid var(--ink); }
  .landing { height: 100vh; overflow: auto; display: flex; flex-direction: column; }
  .landing-settings { align-self: flex-end; margin: .75rem .75rem 0; }
  nav { display: flex; gap: .5rem; }
  .settings-window { height: calc(100vh - 4rem); overflow: auto; }
  .settings-chrome { display: flex; align-items: center; justify-content: space-between; padding: .75rem; border-bottom: 1px solid var(--ink); }
  .settings-chrome h2 { margin: 0; font-size: 1rem; }
  button.active { font-weight: bold; }
  .conversation { display: grid; grid-template-columns: minmax(16rem, 25%) 1fr; height: calc(100vh - 4rem); }
  .conversation aside { overflow: auto; border-right: 1px solid var(--ink); }
  /* The work surface and its docked channel share the conversation surface's
     two-column rule, mirrored: the work takes the wide column. */
  .surfaces-surface { display: grid; grid-template-columns: 1fr minmax(20rem, 33%); height: calc(100vh - 4rem); }
  .surfaces-surface .work { min-width: 0; overflow: auto; border-right: 1px solid var(--ink); }
  .operator { min-width: 0; overflow: auto; }
</style>
