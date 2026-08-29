<!--
  The conversation control channel — connection state, the message history,
  and the composer (or the approval it is waiting on). It is ambient: it
  rides whatever work surface the user is standing on rather than being a
  place they navigate to, so the same instance renders beside the pages
  surface and beside the conversation list.
-->
<script lang="ts">
  import ApprovalCard from "./ApprovalCard.svelte";
  import ConnectionIndicator from "./ConnectionIndicator.svelte";
  import MessageDisplay from "./MessageDisplay.svelte";
  import MessageInput from "./MessageInput.svelte";
  import type { DisplayPreferences } from "../display-preferences";
  import type { ConnectionStore } from "../stores/connection";
  import type { ConversationStore } from "../stores/conversations";
  import type { StreamingStore } from "../stores/streaming";
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";

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
  }: Props = $props();

  const activeConversation = $derived(conversationStore.activeConversation);
</script>

<section aria-label="Conversation">
  {#if activeConversation}<ConnectionIndicator store={connectionStore} {onRetry} />{/if}
  <MessageDisplay store={conversationStore} {streamingStore} {displayPreferences} />
  {#if streamingStore.pendingApproval}
    <ApprovalCard approval={streamingStore.pendingApproval} />
  {:else}
    <MessageInput store={streamingStore} {modelOptions} {modelCatalogueReady} selectedModelId={activeConversation?.selected_model_id ?? defaultModelId} sessionId={activeConversation?.id ?? null} {agentDefinitions} canSelectAgent={!activeConversation} bind:selectedAgentId {onNavigate} />
  {/if}
</section>

<style>
  section { min-width: 0; overflow: auto; }
</style>
