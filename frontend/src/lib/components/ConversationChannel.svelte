<!--
  The conversation control channel — the session controls, connection state,
  the message history, and the composer (or the approval it is waiting on).
  It is ambient: it rides whatever work surface the user is standing on
  rather than being a place they navigate to, so the same instance renders
  beside the surfaces surface and beside the conversation list. Sessions cycle
  within it — picking up an earlier conversation or starting a fresh one is
  done from here, without leaving the work in view.
-->
<script lang="ts">
  import ApprovalCard from "./ApprovalCard.svelte";
  import ConnectionIndicator from "./ConnectionIndicator.svelte";
  import MessageDisplay from "./MessageDisplay.svelte";
  import MessageInput from "./MessageInput.svelte";
  import AgentPicker from "./AgentPicker.svelte";
  import type { DisplayPreferences } from "../display-preferences";
  import type { ConnectionStore } from "../stores/connection";
  import type { ConversationStore } from "../stores/conversations";
  import type { StreamingStore } from "../stores/streaming";
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";
  import type { ConversationBranch } from "../types";
  import { relativeTime } from "../utils/relative-time";

  interface Props {
    conversationStore: ConversationStore;
    connectionStore: ConnectionStore;
    streamingStore: StreamingStore;
    onRetry: () => void;
    displayPreferences?: DisplayPreferences;
    modelOptions?: string[];
    modelCatalogueReady?: boolean;
    defaultModelId?: string | null;
    agentDefinitions?: EffectiveAgentDefinitionResponse[];
    selectedAgentId: string;
    onNavigate?: (path: string) => void;
    onNewConversation?: (agentId?: string) => void;
  }

  let {
    conversationStore,
    connectionStore,
    streamingStore,
    onRetry,
    displayPreferences,
    modelOptions = [],
    modelCatalogueReady = false,
    defaultModelId = null,
    agentDefinitions = [],
    selectedAgentId = $bindable(),
    onNavigate,
    onNewConversation,
  }: Props = $props();

  const activeConversation = $derived(conversationStore.activeConversation);
  const activeStream = $derived(
    streamingStore.streamFor?.(activeConversation?.id ?? null),
  );

  // The sessions the switcher offers. A conversation that is active but not
  // yet in the loaded list (just created, list not refreshed) still needs an
  // option, or the switcher would read as having nothing selected.
  const sessions = $derived.by(() => {
    const listed = conversationStore.conversations.map((conversation) => ({
      id: conversation.id,
      label: conversation.message_preview ?? "New conversation",
    }));
    const active = activeConversation;
    if (active && !listed.some((session) => session.id === active.id)) {
      listed.unshift({ id: active.id, label: "New conversation" });
    }
    return listed;
  });

  let selectedSessionId = $state("");
  $effect(() => {
    selectedSessionId = activeConversation?.id ?? "";
  });

  function cycleSession(id: string): void {
    if (id && id !== activeConversation?.id) conversationStore.selectConversation(id);
  }

  // ---- Threads ---------------------------------------------------------------
  // The switcher appears once the conversation has a branch: until then the
  // root thread is the only thread and a control would be noise.
  const branches = $derived(conversationStore.branches ?? []);
  const activeThreadId = $derived(conversationStore.activeThreadId ?? null);
  const activeBranch = $derived(
    branches.find((branch) => branch.child_thread_id === activeThreadId) ?? null,
  );
  const activeBranchAbandoned = $derived(activeBranch?.state === "abandoned");

  let selectedThreadId = $state("");
  $effect(() => {
    selectedThreadId = activeThreadId ?? "";
  });

  function branchLabel(branch: ConversationBranch): string {
    const from = branch.branched_from_preview
      ? `Branch from “${branch.branched_from_preview}”`
      : "Branch";
    const when = relativeTime(branch.created_at);
    return branch.state === "abandoned" ? `${from} · ${when} · abandoned` : `${from} · ${when}`;
  }

  // A thread action that fails (the conversation gone, the branch mid-turn,
  // the gateway away) is reported, and the switcher returns to the thread
  // the store still shows rather than naming one it could not open.
  function switchThread(id: string): void {
    const next = id === "" ? null : id;
    if (next === activeThreadId) return;
    conversationStore.selectThread?.(next).catch((error: unknown) => {
      console.error("Failed to switch thread:", error);
      selectedThreadId = activeThreadId ?? "";
    });
  }

  function abandonBranch(): void {
    conversationStore.abandonActiveBranch?.().catch((error: unknown) => {
      console.error("Failed to abandon branch:", error);
    });
  }
</script>

<section aria-label="Conversation">
  <!-- Session controls travel with the channel, so cycling to another
       conversation or starting one costs no surface. -->
  <header class="session-bar">
    <select
      class="session-select"
      aria-label="Active conversation"
      bind:value={selectedSessionId}
      onchange={() => cycleSession(selectedSessionId)}
    >
      {#if !activeConversation}
        <option value="">No conversation</option>
      {/if}
      {#each sessions as session (session.id)}
        <option value={session.id}>{session.label}</option>
      {/each}
    </select>
    {#if activeConversation}
      <AgentPicker definitions={agentDefinitions} bind:selectedAgentId />
    {/if}
    <button class="new-session" aria-label="New conversation" onclick={() => onNewConversation?.(selectedAgentId || undefined)}>+ New</button>
  </header>
  {#if activeConversation && branches.length > 0}
    <!-- Which thread the history and composer show. The root is always
         first; a branch is named by the message it was taken from, so two
         branches from different points read apart. -->
    <div class="thread-bar">
      <select
        class="thread-select"
        aria-label="Active thread"
        bind:value={selectedThreadId}
        onchange={(event) => switchThread(event.currentTarget.value)}
      >
        <option value="">Main thread</option>
        {#each branches as branch (branch.child_thread_id)}
          <option value={branch.child_thread_id}>{branchLabel(branch)}</option>
        {/each}
      </select>
      {#if activeBranch && !activeBranchAbandoned && !(activeStream?.isStreaming ?? false)}
        <!-- A branch mid-turn cannot be abandoned (the route refuses it);
             the control waits for the turn to settle or be cancelled. -->
        <button
          class="abandon-branch"
          aria-label="Abandon branch"
          onclick={abandonBranch}
        >Abandon</button>
      {/if}
    </div>
  {/if}
  {#if activeConversation}<ConnectionIndicator store={connectionStore} {onRetry} />{/if}
  <MessageDisplay store={conversationStore} {streamingStore} {displayPreferences} />
  <!-- Pinned to the foot of the channel's scroll, so a turn waiting on the
       user's decision shows its approval card in view however long the
       history above it is. -->
  <div class="composer-slot">
    {#if activeBranchAbandoned}
      <p class="thread-notice" role="status">This branch is abandoned — it takes no further messages. Switch to the main thread or another branch to continue.</p>
    {:else if activeStream?.pendingApproval ?? streamingStore.pendingApproval}
      <ApprovalCard approval={activeStream?.pendingApproval ?? streamingStore.pendingApproval!} />
    {:else}
      <MessageInput store={streamingStore} {modelOptions} {modelCatalogueReady} selectedModelId={activeConversation?.selected_model_id ?? defaultModelId} sessionId={activeConversation?.id ?? null} {agentDefinitions} canSelectAgent={!activeConversation} bind:selectedAgentId {onNavigate} />
    {/if}
  </div>
</section>

<style>
  section { min-width: 0; overflow: auto; }

  .composer-slot {
    position: sticky;
    bottom: 0;
    z-index: 1;
    background: var(--paper);
  }

  /* One mono line above the history: which session is in the channel, and the
     way to start another. Matches the conversation list's control register.
     Sticky within the channel's own scroll, so cycling stays reachable however
     far down the history the user has read. */
  .session-bar {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 22px;
    background: var(--paper);
    border-bottom: 1px dotted var(--rule-soft);
  }

  .session-select {
    flex: 1;
    min-width: 0;
    background: var(--paper);
    border: 1px solid var(--rule-soft);
    color: var(--ink);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1.4;
    letter-spacing: 0.08em;
    padding: 3px 6px;
  }

  .new-session {
    background: transparent;
    border: none;
    padding: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink);
    cursor: pointer;
    flex-shrink: 0;
  }

  .new-session:hover { color: var(--ink-orange); }

  /* The thread bar sits under the session bar in the same mono register:
     the switcher fills the line, the abandon control keeps to its end. */
  .thread-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 22px;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .thread-select {
    flex: 1;
    min-width: 0;
    background: var(--paper);
    border: 1px solid var(--rule-soft);
    color: var(--ink);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1.4;
    letter-spacing: 0.08em;
    padding: 3px 6px;
  }

  .abandon-branch {
    background: transparent;
    border: none;
    padding: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
    cursor: pointer;
    flex-shrink: 0;
  }

  .abandon-branch:hover { color: var(--ink-orange); }

  .thread-notice {
    margin: 0;
    padding: 12px 22px;
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.08em;
    color: var(--ink-soft);
    border-top: 1px dotted var(--rule-soft);
  }
</style>
