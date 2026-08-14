<script lang="ts">
  import ApprovalCard from "./ApprovalCard.svelte";
  import ConnectionIndicator from "./ConnectionIndicator.svelte";
  import ConversationList from "./ConversationList.svelte";
  import MessageDisplay from "./MessageDisplay.svelte";
  import MessageInput from "./MessageInput.svelte";
  import OperatorDashboard from "./OperatorDashboard.svelte";
  import SettingsPage from "./SettingsPage.svelte";
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

  type Surface = "conversation" | "operator";

  interface Props {
    conversationStore: ConversationStore;
    connectionStore: ConnectionStore;
    streamingStore: StreamingStore;
    spaceStore?: SpaceStore;
    onSpaceChange?: () => void;
    onRetry: () => void;
    activeSurface: Surface;
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
  }

  let {
    conversationStore,
    connectionStore,
    streamingStore,
    spaceStore,
    onSpaceChange,
    onRetry,
    activeSurface,
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
  }: Props = $props();

  const currentSpace = $derived(spaceStore?.currentSpace ?? null);
  const activeConversation = $derived(conversationStore.activeConversation);
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
</script>

{#if currentSpace}
  <header class="topbar">
    <SpaceSelector {spaceStore} {onSpaceChange} />
    <nav aria-label="Surface">
      <button aria-pressed={activeSurface === "conversation"} class:active={activeSurface === "conversation"} onclick={() => onSelectSurface("conversation")}>Conversation</button>
      <button aria-pressed={activeSurface === "operator"} class:active={activeSurface === "operator"} onclick={() => onSelectSurface("operator")}>Operator</button>
      <button onclick={() => onNavigate?.("/settings/providers")}>Settings</button>
    </nav>
    {#if desktopRuntime}<span>{desktopRuntime.managedGateway ? "MANAGED" : "REMOTE"}</span>{/if}
    {#if onSelectTheme}<ThemeToggle mode={themeMode} onSelect={onSelectTheme} />{/if}
  </header>
{/if}

{#if settingsMode}
  <!-- The settings window chrome lives here; the pages own their content only. -->
  <div class="settings-window">
    <header class="settings-chrome">
      <h2>Settings</h2>
      <button class="close" onclick={closeSettings} aria-label="Close settings">×</button>
    </header>
    {#if settingsSpace}
      <SpaceSettingsPage space={settingsSpace} tab={settingsSpaceTab ?? undefined} />
    {:else}
      <SettingsPage tab={settingsTab ?? undefined} {onSettingsSaved} />
    {/if}
  </div>
{:else if !currentSpace}
  <button onclick={() => onNavigate?.("/settings/providers")}>Settings</button>
  <SpacePicker {spaceStore} {onSpaceChange} />
{:else if activeSurface === "operator"}
  <main class="operator"><OperatorDashboard /></main>
{:else}
  <main class="conversation">
    <aside><ConversationList store={conversationStore} /></aside>
    <section aria-label="Conversation">
      {#if activeConversation}<ConnectionIndicator store={connectionStore} {onRetry} />{/if}
      <MessageDisplay store={conversationStore} {streamingStore} {displayPreferences} />
      {#if streamingStore.pendingApproval}
        <ApprovalCard approval={streamingStore.pendingApproval} />
      {:else}
        <MessageInput store={streamingStore} {modelOptions} {modelCatalogueReady} selectedModelId={activeConversation?.selected_model_id ?? defaultModelId} {onNavigate} />
      {/if}
    </section>
  </main>
{/if}

<style>
  .topbar { display: flex; align-items: center; gap: 1rem; padding: .75rem; border-bottom: 1px solid var(--ink, #222); }
  nav { display: flex; gap: .5rem; }
  .settings-window { height: calc(100vh - 4rem); overflow: auto; }
  .settings-chrome { display: flex; align-items: center; justify-content: space-between; padding: .75rem; border-bottom: 1px solid var(--ink, #222); }
  .settings-chrome h2 { margin: 0; font-size: 1rem; }
  button.active { font-weight: bold; }
  .conversation { display: grid; grid-template-columns: minmax(16rem, 25%) 1fr; height: calc(100vh - 4rem); }
  .conversation aside { overflow: auto; border-right: 1px solid var(--ink, #222); }
  .conversation section, .operator { min-width: 0; overflow: auto; }
</style>
