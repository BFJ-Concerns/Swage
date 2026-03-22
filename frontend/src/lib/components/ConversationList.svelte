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
            onclick={() => store!.selectConversation(conversation.id)}
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

            <div class="item-footer">
              <span class="timestamp">{relativeTime(conversation.last_activity)}</span>

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
                  onclick={(e) => requestDelete(conversation.id, e)}
                >×</button>
              {/if}
            </div>
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
  .conversation-list {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }

  /* -------------------------------------------------------------------------
     New conversation button
  ------------------------------------------------------------------------- */

  .new-btn {
    width: 100%;
    padding: 10px 16px;
    background: none;
    border: none;
    border-bottom: 1px solid #e0e0e0;
    text-align: left;
    font-size: 0.875rem;
    font-weight: 500;
    color: #333;
    cursor: pointer;
  }

  .new-btn:hover {
    background: #f5f5f5;
  }

  .new-btn:disabled {
    color: #aaa;
    cursor: not-allowed;
  }

  /* -------------------------------------------------------------------------
     Empty state
  ------------------------------------------------------------------------- */

  .empty-state {
    padding: 24px 16px;
    text-align: center;
    color: #888;
    font-size: 0.875rem;
  }

  .empty-state p {
    margin: 0 0 4px;
  }

  .hint {
    font-size: 0.8rem;
    color: #aaa;
  }

  /* -------------------------------------------------------------------------
     Conversation list
  ------------------------------------------------------------------------- */

  .conversations {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .conversation-item {
    padding: 10px 12px;
    border-bottom: 1px solid #f0f0f0;
    cursor: pointer;
    /* Prevent text selection when clicking rapidly. */
    user-select: none;
  }

  .conversation-item:hover {
    background: #f7f7f7;
  }

  .conversation-item.active {
    background: #eef2ff;
    border-left: 3px solid #6366f1;
    padding-left: 9px;
  }

  /* -------------------------------------------------------------------------
     Item layout
  ------------------------------------------------------------------------- */

  .item-header {
    display: flex;
    align-items: flex-start;
    gap: 4px;
    margin-bottom: 4px;
  }

  .preview {
    flex: 1;
    font-size: 0.875rem;
    color: #222;
    /* Clamp to one line with an ellipsis — previews can be long. */
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .interrupted {
    flex-shrink: 0;
    font-size: 0.75rem;
    color: #d97706;
    /* Align with the top of the preview text. */
    line-height: 1.4;
  }

  .item-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .timestamp {
    font-size: 0.75rem;
    color: #999;
  }

  /* -------------------------------------------------------------------------
     Delete controls
  ------------------------------------------------------------------------- */

  .delete-btn {
    background: none;
    border: none;
    color: #ccc;
    font-size: 1rem;
    line-height: 1;
    padding: 0 2px;
    cursor: pointer;
    /* Hidden by default — revealed on row hover. */
    visibility: hidden;
  }

  .conversation-item:hover .delete-btn {
    visibility: visible;
  }

  .delete-btn:hover {
    color: #e53e3e;
  }

  .delete-confirm {
    display: flex;
    gap: 4px;
  }

  .confirm-btn,
  .cancel-btn {
    font-size: 0.75rem;
    padding: 2px 6px;
    border-radius: 3px;
    cursor: pointer;
    border: 1px solid transparent;
  }

  .confirm-btn {
    background: #fee2e2;
    color: #c53030;
    border-color: #fca5a5;
  }

  .confirm-btn:hover {
    background: #fecaca;
  }

  .cancel-btn {
    background: #f5f5f5;
    color: #555;
  }

  .cancel-btn:hover {
    background: #e5e5e5;
  }
</style>
