<script lang="ts">
  import ConversationChannel from "./ConversationChannel.svelte";
  import ConversationList from "./ConversationList.svelte";
  import OperatorDashboard from "./OperatorDashboard.svelte";
  import PagesSurface from "./PagesSurface.svelte";
  import SettingsPage from "./SettingsPage.svelte";
  import WorkflowRunView from "./WorkflowRunView.svelte";
  import SpacePicker from "./SpacePicker.svelte";
  import SpaceSelector from "./SpaceSelector.svelte";
  import SpaceSettingsPage from "./SpaceSettingsPage.svelte";
  import ThemeToggle from "./ThemeToggle.svelte";
  import type { DisplayPreferences } from "../display-preferences";
  import type { Route } from "../router";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type { ThemeMode } from "../theme";
  import type { ConnectionStore } from "../stores/connection";
  import type { ConversationStore } from "../stores/conversations";
  import type { SpaceStore } from "../stores/spaces";
  import type { StreamingStore } from "../stores/streaming";
  import type { PagesStore } from "../stores/pages";
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";

  type Surface = "conversation" | "operator" | "pages";

  interface Props {
    conversationStore: ConversationStore;
    connectionStore: ConnectionStore;
    streamingStore: StreamingStore;
    spaceStore?: SpaceStore;
    onSpaceChange?: () => void;
    onRetry: () => void;
    activeSurface: Surface;
    pagesStore?: PagesStore;
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
    onNewConversation?: () => void;
  }

  let {
    conversationStore,
    connectionStore,
    streamingStore,
    spaceStore,
    onSpaceChange,
    onRetry,
    activeSurface,
    pagesStore,
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

  const currentSpace = $derived(spaceStore?.currentSpace ?? null);
  const activeConversation = $derived(conversationStore.activeConversation);
  let selectedAgentId = $state("");
  let selectedAgentSpaceId = $state<string | null | undefined>(undefined);
  $effect(() => {
    const currentSpaceId = currentSpace?.id ?? null;
    if (selectedAgentSpaceId === undefined) {
      selectedAgentSpaceId = currentSpaceId;
    } else if (currentSpaceId !== selectedAgentSpaceId) {
      selectedAgentId = "";
      selectedAgentSpaceId = currentSpaceId;
    } else if (
      selectedAgentId &&
      !agentDefinitions.some((definition) => definition.id === selectedAgentId)
    ) {
      selectedAgentId = "";
    }
  });
  const workflowRunId = $derived(route?.workflowRunId ?? null);
  const settingsTab = $derived(route?.settingsTab ?? null);
  const settingsSpaceSlug = $derived(route?.settingsSpaceSlug ?? null);
  const settingsSpaceTab = $derived(route?.settingsSpaceTab ?? null);
  const settingsMode = $derived(settingsTab !== null || settingsSpaceSlug !== null);
  const settingsSpace = $derived(
    settingsSpaceSlug
      ? (spaceStore?.spaces ?? []).find((space) => space.slug === settingsSpaceSlug) ?? null
      : null,
  );

  function closeSettings(): void {
    onNavigate?.(activeConversation?.id ? `/conversations/${activeConversation.id}` : "/");
  }

  function openCurrentSpaceWorkspaces(): void {
    if (!currentSpace) return;
    onNavigate?.(`/settings/space/${encodeURIComponent(currentSpace.slug)}/workspaces`);
  }
</script>

{#if currentSpace}
  <header class="topbar">
    <SpaceSelector {spaceStore} {onSpaceChange} />
    <nav aria-label="Surface">
      <button aria-pressed={!settingsMode && activeSurface === "conversation"} class:active={!settingsMode && activeSurface === "conversation"} onclick={() => onSelectSurface("conversation")}>Conversations</button>
      <button aria-pressed={!settingsMode && activeSurface === "pages"} class:active={!settingsMode && activeSurface === "pages"} onclick={() => onSelectSurface("pages")}>Pages</button>
      <button aria-pressed={!settingsMode && activeSurface === "operator"} class:active={!settingsMode && activeSurface === "operator"} onclick={() => onSelectSurface("operator")}>Operator</button>
      <button aria-pressed={settingsMode} class:active={settingsMode} onclick={() => onNavigate?.("/settings/providers")}>Settings</button>
    </nav>
    {#if desktopRuntime}<span>{desktopRuntime.managedGateway ? "MANAGED" : "REMOTE"}</span>{/if}
    {#if onSelectTheme}<ThemeToggle mode={themeMode} onSelect={onSelectTheme} />{/if}
  </header>
{/if}

{#if workflowRunId}
  <!-- A run addressed directly by URL owns the body: its account is the
       whole point of the visit. -->
  <WorkflowRunView runId={workflowRunId} />
{:else if settingsMode}
  <!-- The settings window chrome lives here; the pages own their content only. -->
  <div class="settings-window">
    <header class="settings-chrome">
      <h2>Settings</h2>
      <nav aria-label="Settings sections">
        <button class:active={settingsTab === "providers"} onclick={() => onNavigate?.("/settings/providers")}>Providers</button>
        <button class:active={settingsTab === "agents"} onclick={() => onNavigate?.("/settings/agents")}>Agents</button>
        <button class:active={settingsTab === "commands"} onclick={() => onNavigate?.("/settings/commands")}>Commands</button>
        <button class:active={settingsTab === "skills"} onclick={() => onNavigate?.("/settings/skills")}>Skills</button>
        <button class:active={settingsTab === "automation"} onclick={() => onNavigate?.("/settings/automation")}>Automation</button>
        {#if currentSpace}
          <button class:active={settingsSpaceTab === "workspaces"} onclick={openCurrentSpaceWorkspaces}>Workspaces</button>
        {/if}
      </nav>
      <button class="close" onclick={closeSettings} aria-label="Close settings">×</button>
    </header>
    {#if settingsSpace}
      <SpaceSettingsPage space={settingsSpace} tab={settingsSpaceTab ?? undefined} />
    {:else}
      <SettingsPage tab={settingsTab ?? undefined} spaceId={currentSpace?.id ?? null} spaces={spaceStore?.spaces ?? []} {onSettingsSaved} />
    {/if}
  </div>
{:else if !currentSpace}
  <button onclick={() => onNavigate?.("/settings/providers")}>Settings</button>
  <SpacePicker {spaceStore} {onSpaceChange} />
{:else if activeSurface === "operator"}
  <main class="operator"><OperatorDashboard /></main>
{:else if activeSurface === "pages"}
  <!--
    The work surface carries the control channel: the same channel markup and
    the same store instances as the conversation surface, so moving between
    pages keeps the conversation running rather than leaving it behind.
  -->
  <main class="pages-surface">
    <div class="work">
      {#if pagesStore && currentSpace}
        <PagesSurface store={pagesStore} spaceId={currentSpace.id} />
      {/if}
    </div>
    <ConversationChannel {conversationStore} {connectionStore} {streamingStore} {onRetry} {displayPreferences} {modelOptions} {modelCatalogueReady} {defaultModelId} {agentDefinitions} bind:selectedAgentId {onNavigate} {onNewConversation} />
  </main>
{:else}
  <main class="conversation">
    <aside><ConversationList store={conversationStore} {onNewConversation} /></aside>
    <ConversationChannel {conversationStore} {connectionStore} {streamingStore} {onRetry} {displayPreferences} {modelOptions} {modelCatalogueReady} {defaultModelId} {agentDefinitions} bind:selectedAgentId {onNavigate} {onNewConversation} />
  </main>
{/if}

<style>
  .topbar { display: flex; align-items: center; gap: 1rem; padding: .75rem; border-bottom: 1px solid var(--ink); }
  nav { display: flex; gap: .5rem; }
  .settings-window { height: calc(100vh - 4rem); overflow: auto; }
  .settings-chrome { display: flex; align-items: center; justify-content: space-between; padding: .75rem; border-bottom: 1px solid var(--ink); }
  .settings-chrome h2 { margin: 0; font-size: 1rem; }
  button.active { font-weight: bold; }
  .conversation { display: grid; grid-template-columns: minmax(16rem, 25%) 1fr; height: calc(100vh - 4rem); }
  .conversation aside { overflow: auto; border-right: 1px solid var(--ink); }
  /* The work surface and its docked channel share the conversation surface's
     two-column rule, mirrored: the work takes the wide column. */
  .pages-surface { display: grid; grid-template-columns: 1fr minmax(20rem, 33%); height: calc(100vh - 4rem); }
  .pages-surface .work { min-width: 0; overflow: auto; border-right: 1px solid var(--ink); }
  .operator { min-width: 0; overflow: auto; }
</style>
