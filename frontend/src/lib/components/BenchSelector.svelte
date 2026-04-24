<!-- Bench selector — interactive dropdown with bench management. -->
<script lang="ts">
  import type { BenchStore } from "../stores/benches";
  import { onMount, onDestroy } from "svelte";

  interface Props {
    // Injected by App.svelte once all stores are wired. Optional so AppLayout
    // can render without crashing before the wiring is complete (same pattern
    // as ConversationList).
    benchStore?: BenchStore;
    // Callback invoked after the active bench changes, so App.svelte can
    // reload conversations for the newly selected bench.
    onBenchChange?: () => void;
  }

  let { benchStore, onBenchChange }: Props = $props();

  // ---------------------------------------------------------------------------
  // Local state
  // ---------------------------------------------------------------------------

  let isOpen = $state(false);
  let showCreateForm = $state(false);

  // Create-form input state.
  let newName = $state("");
  let createError = $state<string | null>(null);
  let isCreating = $state(false);

  // Track which bench is awaiting inline delete confirmation.
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
  // Bench selection
  // ---------------------------------------------------------------------------

  async function selectBench(id: string) {
    if (!benchStore) return;
    closeDropdown();
    await benchStore.selectBench(id);
    onBenchChange?.();
  }

  // ---------------------------------------------------------------------------
  // Bench creation
  // ---------------------------------------------------------------------------

  async function submitCreate(event: Event) {
    event.preventDefault();
    if (!benchStore || !newName.trim()) return;

    isCreating = true;
    createError = null;

    try {
      await benchStore.createBench(newName.trim());
      onBenchChange?.();
      closeDropdown();
    } catch (err) {
      // Surface API error messages, such as duplicate names.
      createError = err instanceof Error ? err.message : "Failed to create bench";
    } finally {
      isCreating = false;
    }
  }

  // ---------------------------------------------------------------------------
  // Bench deletion
  // ---------------------------------------------------------------------------

  function requestDelete(id: string, event: MouseEvent) {
    // Prevent the click from bubbling up to the bench-item and selecting it.
    event.stopPropagation();
    pendingDelete = id;
  }

  async function confirmDelete(id: string, event: MouseEvent) {
    event.stopPropagation();
    if (!benchStore) return;
    await benchStore.deleteBench(id);
    onBenchChange?.();
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
  const currentName = $derived(benchStore?.currentBench?.name ?? "default");
</script>

<div class="bench-selector">
  <span class="label">Bench</span>
  <button class="toggle-btn" onclick={toggleDropdown}>
    <span class="value">{currentName}</span>
    <span class="caret" class:open={isOpen}>▾</span>
  </button>

  {#if isOpen && benchStore}
    <div class="bench-dropdown">
      <!-- Bench list -->
      <ul class="bench-list">
        {#each benchStore.benches as bench (bench.id)}
          {@const isActive = benchStore.currentBench?.id === bench.id}
          {@const isDefault = bench.name === "default"}
          {@const isPendingDelete = pendingDelete === bench.id}

          <li
            class="bench-item"
            class:active={isActive}
          >
            <button
              class="bench-select-btn"
              onclick={() => selectBench(bench.id)}
            >
              <span class="bench-name">{bench.name}</span>
            </button>

            {#if isPendingDelete}
              <!-- Inline delete confirmation — no browser dialogs. -->
              <span class="delete-confirm">
                <button
                  class="confirm-delete-btn"
                  onclick={(e) => confirmDelete(bench.id, e)}
                >Delete</button>
                <button class="cancel-delete-btn" onclick={cancelDelete}>
                  Cancel
                </button>
              </span>
            {:else if !isDefault}
              <!-- Default bench cannot be deleted. -->
              <button
                class="delete-bench-btn"
                title="Delete bench"
                onclick={(e) => requestDelete(bench.id, e)}
              >×</button>
            {/if}
          </li>
        {/each}
      </ul>

      <!-- Divider before create action -->
      <div class="dropdown-divider"></div>

      {#if showCreateForm}
        <!-- Inline create form -->
        <form
          class="create-bench-form"
          onsubmit={submitCreate}
        >
          <input
            type="text"
            name="name"
            placeholder="Bench name"
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
          class="create-bench-btn"
          onclick={() => { showCreateForm = true; }}
        >
          + New bench
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* -------------------------------------------------------------------------
     Selector bar
  ------------------------------------------------------------------------- */

  .bench-selector {
    padding: 12px 16px;
    border-bottom: 1px solid #e0e0e0;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.875rem;
    position: relative;
  }

  .label {
    color: #888;
    flex-shrink: 0;
  }

  .toggle-btn {
    background: none;
    border: none;
    padding: 0;
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
    font-size: 0.875rem;
    color: inherit;
    /* Stretch to fill remaining sidebar width for a larger click target. */
    flex: 1;
  }

  .toggle-btn:hover .value {
    text-decoration: underline;
  }

  .value {
    font-weight: 500;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .caret {
    font-size: 0.75rem;
    color: #aaa;
    transition: transform 150ms ease;
    flex-shrink: 0;
  }

  .caret.open {
    transform: rotate(180deg);
  }

  /* -------------------------------------------------------------------------
     Dropdown
  ------------------------------------------------------------------------- */

  .bench-dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    right: 0;
    background: #fff;
    border: 1px solid #e0e0e0;
    border-top: none;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.08);
    z-index: 100;
  }

  /* -------------------------------------------------------------------------
     Bench list
  ------------------------------------------------------------------------- */

  .bench-list {
    list-style: none;
    margin: 0;
    padding: 4px 0;
  }

  .bench-item {
    display: flex;
    align-items: center;
    padding: 8px 12px;
    user-select: none;
    gap: 4px;
  }

  .bench-item:hover {
    background: #f5f5f5;
  }

  .bench-item.active {
    background: #eef2ff;
    font-weight: 500;
  }

  .bench-select-btn {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    padding: 0;
    text-align: left;
    cursor: pointer;
    font: inherit;
    color: inherit;
  }

  .bench-name {
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  /* -------------------------------------------------------------------------
     Delete controls
  ------------------------------------------------------------------------- */

  .delete-bench-btn {
    background: none;
    border: none;
    color: #ccc;
    font-size: 1rem;
    line-height: 1;
    padding: 0 2px;
    cursor: pointer;
    visibility: hidden;
    flex-shrink: 0;
  }

  .bench-item:hover .delete-bench-btn {
    visibility: visible;
  }

  .delete-bench-btn:hover {
    color: #e53e3e;
  }

  .delete-confirm {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
  }

  .confirm-delete-btn,
  .cancel-delete-btn {
    font-size: 0.75rem;
    padding: 2px 6px;
    border-radius: 3px;
    cursor: pointer;
    border: 1px solid transparent;
  }

  .confirm-delete-btn {
    background: #fee2e2;
    color: #c53030;
    border-color: #fca5a5;
  }

  .confirm-delete-btn:hover {
    background: #fecaca;
  }

  .cancel-delete-btn {
    background: #f5f5f5;
    color: #555;
  }

  .cancel-delete-btn:hover {
    background: #e5e5e5;
  }

  /* -------------------------------------------------------------------------
     Divider and create section
  ------------------------------------------------------------------------- */

  .dropdown-divider {
    border-top: 1px solid #f0f0f0;
    margin: 4px 0;
  }

  .create-bench-btn {
    width: 100%;
    padding: 8px 12px;
    background: none;
    border: none;
    text-align: left;
    font-size: 0.875rem;
    color: #555;
    cursor: pointer;
  }

  .create-bench-btn:hover {
    background: #f5f5f5;
    color: #333;
  }

  /* -------------------------------------------------------------------------
     Create bench form
  ------------------------------------------------------------------------- */

  .create-bench-form {
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .create-bench-form input {
    width: 100%;
    padding: 6px 8px;
    border: 1px solid #d0d0d0;
    border-radius: 4px;
    font-size: 0.8125rem;
    box-sizing: border-box;
  }

  .create-bench-form input:focus {
    outline: none;
    border-color: #6366f1;
    box-shadow: 0 0 0 2px rgba(99, 102, 241, 0.15);
  }

  .create-error {
    margin: 0;
    font-size: 0.75rem;
    color: #c53030;
  }

  .form-actions {
    display: flex;
    gap: 6px;
  }

  .submit-btn {
    padding: 5px 12px;
    background: #6366f1;
    color: #fff;
    border: none;
    border-radius: 4px;
    font-size: 0.8125rem;
    cursor: pointer;
  }

  .submit-btn:hover:not(:disabled) {
    background: #4f46e5;
  }

  .submit-btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .cancel-form-btn {
    padding: 5px 10px;
    background: #f5f5f5;
    color: #555;
    border: 1px solid #d0d0d0;
    border-radius: 4px;
    font-size: 0.8125rem;
    cursor: pointer;
  }

  .cancel-form-btn:hover {
    background: #e5e5e5;
  }
</style>
