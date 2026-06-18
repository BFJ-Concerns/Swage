<!--
  Sidebar conversation list — shows all conversations, supports create and delete.
  Pure renderer — reads from conversation store passed as a prop.
-->
<script lang="ts">
  import type { ConversationStore } from "../stores/conversations";
  import { relativeTime } from "../utils/relative-time";

  interface Props {
    // Injected by App.svelte once all stores are wired. Undefined during
    // scaffolding — the component renders a placeholder shell until connected.
    store?: ConversationStore;
  }

  let { store }: Props = $props();

  // Track which conversation is awaiting inline delete confirmation.
  // Only one can be pending at a time — clicking another collapses any open one.
  let pendingDelete: string | null = $state(null);

  function requestDelete(id: string, event: MouseEvent) {
    // Stop propagation so clicking the delete button doesn't also select the conversation.
    event.stopPropagation();
    pendingDelete = id;
  }

  function confirmDelete(id: string, event: MouseEvent) {
    event.stopPropagation();
    store?.deleteConversation(id);
    pendingDelete = null;
  }

  function cancelDelete(event: MouseEvent) {
    event.stopPropagation();
    pendingDelete = null;
  }

  function selectConversation(id: string) {
    store?.selectConversation(id);
  }
</script>

<div class="conversation-list">
  {#if store}
    <!-- New conversation button anchored at the top of the sidebar. -->
    <button
      class="new-btn"
      onclick={() => store!.createConversation()}
      disabled={store.isLoading}
    >
      + New conversation
    </button>

    {#if store.conversations.length === 0}
      <!-- Empty state — first-time user or all conversations deleted. -->
      <div class="empty-state">
        <p>No conversations yet.</p>
        <p class="hint">Start one with the button above.</p>
      </div>
    {:else}
      <ul class="conversations">
        {#each store.conversations as conversation (conversation.id)}
          {@const isActive = store.activeConversation?.id === conversation.id}
          {@const isPendingDelete = pendingDelete === conversation.id}

          <li
            class="conversation-item"
            class:active={isActive}
          >
            <button
              class="conversation-select-btn"
              onclick={() => selectConversation(conversation.id)}
            >
              <div class="item-header">
                <!-- Preview: the last message, or a label for new conversations. -->
                <span class="preview">
                  {conversation.message_preview ?? "New conversation"}
                </span>

                <!-- Warn when a turn was interrupted (e.g. by disconnection). -->
                {#if conversation.has_interrupted_turn}
                  <span class="interrupted" title="A previous response was interrupted">
                    ⚠
                  </span>
                {/if}
              </div>
              <span class="timestamp">{relativeTime(conversation.last_activity)}</span>
            </button>

            {#if isPendingDelete}
              <!-- Inline confirmation — no browser dialogs. -->
              <span class="delete-confirm">
                <button
                  class="confirm-btn"
                  onclick={(e) => confirmDelete(conversation.id, e)}
                >Delete</button>
                <button
                  class="cancel-btn"
                  onclick={cancelDelete}
                >Cancel</button>
              </span>
            {:else}
                <button
                  class="delete-btn"
                  title="Delete conversation"
                  aria-label={`Delete conversation: ${conversation.message_preview ?? "New conversation"}`}
                  onclick={(e) => requestDelete(conversation.id, e)}
                >
                  <span aria-hidden="true">×</span>
                </button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  {:else}
    <!-- Store not yet connected — rendered during scaffolding. -->
    <div class="placeholder"></div>
  {/if}
</div>

<style>
  /*
    Riso conversation list: each row is a single line with a dotted hairline
    rule below; the active row inverts to ink-on-paper with the orange offset
    "shadow" pseudo-element. New-conversation is a small mono-styled button
    above the list.
  */

  .conversation-list {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }

  /* New conversation trigger — sits above the rows. */
  .new-btn {
    width: 100%;
    padding: 8px 0 10px;
    margin-bottom: 6px;
    text-align: left;
    background: transparent;
    border: none;
    border-bottom: 1px dotted var(--rule-soft);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink);
    cursor: pointer;
  }

  .new-btn:hover:not(:disabled) {
    color: var(--ink-orange);
  }

  .new-btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  /* Empty state — short mono caption. */
  .empty-state {
    padding: 16px 0;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1.4;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .empty-state p {
    margin: 0 0 4px;
  }

  .hint {
    font-size: 10px;
    color: var(--ink-soft);
    letter-spacing: 0.08em;
  }

  /* Row list. */
  .conversations {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }

  .conversation-item {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: 6px;
    align-items: baseline;
    padding: 9px 0;
    border-bottom: 1px dotted var(--rule-soft);
    user-select: none;
    position: relative;
    isolation: isolate;
  }

  .conversation-item:last-child {
    border-bottom: none;
  }

  .conversation-item.active {
    /* Pull the row to the section edges so the ink fill reads as a band, then
       overlay the translucent orange offset. The 22px matches the nav padding
       in AppLayout.svelte. */
    background: var(--ink);
    color: var(--paper);
    margin: 0 -22px;
    padding: 9px 22px;
  }

  .conversation-item.active::before {
    content: "";
    position: absolute;
    inset: 0;
    background: var(--accent);
    transform: translate(var(--offset), var(--offset));
    z-index: -1;
    mix-blend-mode: var(--shadow-blend);
    opacity: var(--shadow-opacity);
    pointer-events: none;
  }

  .conversation-select-btn {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    padding: 0;
    text-align: left;
    cursor: pointer;
    font: inherit;
    color: inherit;
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 6px;
    align-items: baseline;
  }

  .item-header {
    display: flex;
    align-items: baseline;
    gap: 4px;
    min-width: 0;
  }

  .preview {
    font-family: var(--font-body);
    font-size: 13px;
    line-height: 1.3;
    color: inherit;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .conversation-item.active .preview {
    font-weight: 600;
  }

  .interrupted {
    flex-shrink: 0;
    color: var(--ink-orange);
    font-size: 11px;
    line-height: 1;
  }

  .conversation-item.active .interrupted {
    color: var(--accent);
  }

  .timestamp {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.06em;
    color: var(--ink-soft);
  }

  .conversation-item.active .timestamp {
    color: rgba(255, 248, 238, 0.7);
  }

  /* Delete affordance — always rendered so touch and keyboard can reach it,
     resting in a low-contrast state that brightens on hover/focus. UI audit
     f-4: visibility:hidden until row hover made it absent on touch and from
     the tab order while not hovered. */
  .delete-btn {
    background: none;
    border: none;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: var(--font-label-md);
    line-height: 1;
    padding: 0 4px;
    cursor: pointer;
    opacity: 0.4;
    transition: opacity 120ms ease, color 120ms ease;
  }

  .delete-btn:hover,
  .delete-btn:focus-visible {
    opacity: 1;
    color: var(--ink-orange);
  }

  .delete-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .conversation-item.active .delete-btn {
    color: rgba(255, 248, 238, 0.7);
    opacity: 0.6;
  }

  .conversation-item.active .delete-btn:hover,
  .conversation-item.active .delete-btn:focus-visible {
    color: var(--accent);
    opacity: 1;
  }

  .delete-confirm {
    display: flex;
    gap: 4px;
  }

  .confirm-btn,
  .cancel-btn {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding: 3px 8px;
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    cursor: pointer;
  }

  .confirm-btn {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
  }

  .confirm-btn:hover {
    filter: brightness(0.92);
  }

  .cancel-btn:hover {
    background: var(--ink);
    color: var(--paper);
  }
</style>
