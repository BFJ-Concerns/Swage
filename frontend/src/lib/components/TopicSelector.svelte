<!-- Topic selector — interactive dropdown with topic management. -->
<script lang="ts">
  import type { TopicStore } from "../stores/topics";
  import { onMount, onDestroy } from "svelte";

  interface Props {
    // Injected by App.svelte once all stores are wired. Optional so AppLayout
    // can render without crashing before the wiring is complete (same pattern
    // as ConversationList).
    topicStore?: TopicStore;
    // Callback invoked after the active topic changes, so App.svelte can
    // reload conversations for the newly selected topic.
    onTopicChange?: () => void;
  }

  let { topicStore, onTopicChange }: Props = $props();

  // ---------------------------------------------------------------------------
  // Local state
  // ---------------------------------------------------------------------------

  let isOpen = $state(false);
  let showCreateForm = $state(false);

  // Create-form input state.
  let newName = $state("");
  let createError = $state<string | null>(null);
  let isCreating = $state(false);

  // Track which topic is awaiting inline delete confirmation.
  let pendingDelete = $state<string | null>(null);

  // ---------------------------------------------------------------------------
  // Keyboard: close on Escape
  // ---------------------------------------------------------------------------

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && isOpen) {
      closeDropdown();
    }
  }

  onMount(() => {
    document.addEventListener("keydown", handleKeydown);
  });

  onDestroy(() => {
    document.removeEventListener("keydown", handleKeydown);
  });

  // ---------------------------------------------------------------------------
  // Dropdown helpers
  // ---------------------------------------------------------------------------

  function toggleDropdown() {
    if (isOpen) {
      closeDropdown();
    } else {
      isOpen = true;
    }
  }

  function closeDropdown() {
    isOpen = false;
    showCreateForm = false;
    pendingDelete = null;
    newName = "";
    createError = null;
  }

  // ---------------------------------------------------------------------------
  // Topic selection
  // ---------------------------------------------------------------------------

  async function selectTopic(id: string) {
    if (!topicStore) return;
    closeDropdown();
    await topicStore.selectTopic(id);
    onTopicChange?.();
  }

  // ---------------------------------------------------------------------------
  // Topic creation
  // ---------------------------------------------------------------------------

  async function submitCreate(event: Event) {
    event.preventDefault();
    if (!topicStore || !newName.trim()) return;

    isCreating = true;
    createError = null;

    try {
      await topicStore.createTopic(newName.trim());
      onTopicChange?.();
      closeDropdown();
    } catch (err) {
      // Surface API error messages, such as duplicate names.
      createError = err instanceof Error ? err.message : "Failed to create topic";
    } finally {
      isCreating = false;
    }
  }

  // ---------------------------------------------------------------------------
  // Topic deletion
  // ---------------------------------------------------------------------------

  function requestDelete(id: string, event: MouseEvent) {
    // Prevent the click from bubbling up to the topic-item and selecting it.
    event.stopPropagation();
    pendingDelete = id;
  }

  async function confirmDelete(id: string, event: MouseEvent) {
    event.stopPropagation();
    if (!topicStore) return;
    await topicStore.deleteTopic(id);
    onTopicChange?.();
    pendingDelete = null;
  }

  function cancelDelete(event: MouseEvent) {
    event.stopPropagation();
    pendingDelete = null;
  }

  // ---------------------------------------------------------------------------
  // Derived
  // ---------------------------------------------------------------------------

  // The label to show on the selector button.
  const currentName = $derived(topicStore?.currentTopic?.name ?? "default");
</script>

<!--
  Restyled for the riso shell. In the new design this control sits in the
  navy ink-coloured topic-head (cream typography). The topic *name* is shown
  by the parent (AppLayout's <h2 class="name">), so this control becomes a
  small "switch / new" affordance underneath.

  TODO(debt): replace with a back-arrow that returns to a dedicated topic
  picker route, plus a ▾ menu for topic-scoped settings.
  Blocked by: topic picker route not yet implemented.
-->
<div class="topic-selector">
  <button class="toggle-btn" onclick={toggleDropdown} title="Switch topic">
    <span class="value">Switch — {currentName}</span>
    <span class="caret" class:open={isOpen}>▾</span>
  </button>

  {#if isOpen && topicStore}
    <div class="topic-dropdown">
      <!-- Topic list -->
      <ul class="topic-list">
        {#each topicStore.topics as topic (topic.id)}
          {@const isActive = topicStore.currentTopic?.id === topic.id}
          {@const isDefault = topic.name === "default"}
          {@const isPendingDelete = pendingDelete === topic.id}

          <li
            class="topic-item"
            class:active={isActive}
          >
            <button
              class="topic-select-btn"
              onclick={() => selectTopic(topic.id)}
            >
              <span class="topic-name">{topic.name}</span>
            </button>

            {#if isPendingDelete}
              <!-- Inline delete confirmation — no browser dialogs. -->
              <span class="delete-confirm">
                <button
                  class="confirm-delete-btn"
                  onclick={(e) => confirmDelete(topic.id, e)}
                >Delete</button>
                <button class="cancel-delete-btn" onclick={cancelDelete}>
                  Cancel
                </button>
              </span>
            {:else if !isDefault}
              <!-- Default topic cannot be deleted. -->
              <button
                class="delete-topic-btn"
                title="Delete topic"
                aria-label={`Delete topic ${topic.name}`}
                onclick={(e) => requestDelete(topic.id, e)}
              >
                <span aria-hidden="true">×</span>
              </button>
            {/if}
          </li>
        {/each}
      </ul>

      <!-- Divider before create action -->
      <div class="dropdown-divider"></div>

      {#if showCreateForm}
        <!-- Inline create form -->
        <form
          class="create-topic-form"
          onsubmit={submitCreate}
        >
          <input
            type="text"
            name="name"
            placeholder="Topic name"
            bind:value={newName}
            required
          />
          {#if createError}
            <p class="create-error">{createError}</p>
          {/if}
          <div class="form-actions">
            <button type="submit" class="submit-btn" disabled={isCreating}>
              {isCreating ? "Creating…" : "Create"}
            </button>
            <button
              type="button"
              class="cancel-form-btn"
              onclick={() => { showCreateForm = false; createError = null; }}
            >
              Cancel
            </button>
          </div>
        </form>
      {:else}
        <button
          class="create-topic-btn"
          onclick={() => { showCreateForm = true; }}
        >
          + New topic
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  /*
    The selector inherits its colour context from its parent: when in the
    navy topic-head, "color: inherit" gives cream type; when in the picker
    nav (paper background), it gives navy ink. The dropdown opens below the
    topic-head and uses the riso-block treatment.
  */
  .topic-selector {
    margin-top: 12px;
    position: relative;
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .toggle-btn {
    width: 100%;
    background: transparent;
    border: 1px solid currentColor;
    padding: 7px 10px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    cursor: pointer;
    color: inherit;
    font: inherit;
    text-transform: inherit;
    letter-spacing: inherit;
    opacity: 0.85;
  }

  .toggle-btn:hover {
    opacity: 1;
  }

  .value {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .caret {
    flex-shrink: 0;
    transition: transform 150ms ease;
    font-family: var(--font-mono);
  }

  .caret.open {
    transform: rotate(180deg);
  }

  /* Dropdown: a riso-block panel that drops below the toggle. */
  .topic-dropdown {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    right: 0;
    background: var(--paper);
    color: var(--ink);
    border: 2px solid var(--ink);
    z-index: 100;
    isolation: isolate;
  }

  .topic-dropdown::after {
    content: "";
    position: absolute;
    inset: 0;
    border: 2px solid var(--accent);
    transform: translate(var(--offset), var(--offset));
    z-index: -1;
    mix-blend-mode: var(--shadow-blend);
    opacity: var(--shadow-opacity);
    pointer-events: none;
  }

  /* Topic list rows. */
  .topic-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .topic-item {
    display: flex;
    align-items: center;
    padding: 9px 14px;
    gap: 4px;
    user-select: none;
    border-bottom: 1px dotted var(--rule-faint);
    color: var(--ink);
  }

  .topic-item:last-child {
    border-bottom: none;
  }

  .topic-item:hover {
    background: color-mix(in srgb, var(--ink) 8%, transparent);
  }

  .topic-item.active {
    background: var(--ink);
    color: var(--paper);
  }

  .topic-select-btn {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    padding: 0;
    text-align: left;
    cursor: pointer;
    font: inherit;
    color: inherit;
    text-transform: inherit;
    letter-spacing: inherit;
  }

  .topic-name {
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  /* Delete controls — always rendered so touch and keyboard can reach them,
     low-contrast until hover/focus. UI audit f-4. */
  .delete-topic-btn {
    background: none;
    border: none;
    color: inherit;
    font-family: var(--font-mono);
    font-size: var(--font-label-md);
    line-height: 1;
    padding: 0 4px;
    cursor: pointer;
    flex-shrink: 0;
    opacity: 0.4;
    transition: opacity 120ms ease, color 120ms ease;
  }

  .delete-topic-btn:hover,
  .delete-topic-btn:focus-visible {
    opacity: 1;
    color: var(--ink-orange);
  }

  .delete-topic-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .delete-confirm {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
  }

  .confirm-delete-btn,
  .cancel-delete-btn {
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

  .confirm-delete-btn {
    background: var(--accent);
    color: var(--paper);
    border-color: var(--accent);
  }

  .confirm-delete-btn:hover {
    filter: brightness(0.92);
  }

  .cancel-delete-btn:hover {
    background: var(--ink);
    color: var(--paper);
  }

  /* Divider above the create form. */
  .dropdown-divider {
    border-top: 1px dotted var(--rule-soft);
    margin: 4px 0;
  }

  .create-topic-btn {
    width: 100%;
    padding: 9px 14px;
    background: none;
    border: none;
    text-align: left;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-orange);
    cursor: pointer;
  }

  .create-topic-btn:hover {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }

  /* Create-topic form. */
  .create-topic-form {
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    border-top: 1px dotted var(--rule-soft);
  }

  .create-topic-form input {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid var(--ink);
    background: var(--paper);
    font-family: var(--font-body);
    font-size: 13px;
    color: var(--ink);
  }

  .create-topic-form input:focus {
    outline: none;
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }

  .create-error {
    margin: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-orange);
  }

  .form-actions {
    display: flex;
    gap: 6px;
  }

  .submit-btn,
  .cancel-form-btn {
    font-family: var(--font-display);
    font-size: 14px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    padding: 6px 12px;
    border: 1px solid var(--ink);
    cursor: pointer;
  }

  .submit-btn {
    background: var(--ink);
    color: var(--paper);
  }

  .submit-btn:hover:not(:disabled) {
    background: var(--accent);
    border-color: var(--accent);
  }

  .submit-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .cancel-form-btn {
    background: var(--paper);
    color: var(--ink);
  }

  .cancel-form-btn:hover {
    background: var(--ink);
    color: var(--paper);
  }
</style>
