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

<!--
  Restyled for the riso shell. In the new design this control sits in the
  navy ink-coloured bench-head (cream typography). The bench *name* is shown
  by the parent (AppLayout's <h2 class="name">), so this control becomes a
  small "switch / new" affordance underneath.

  TODO(debt): replace with a back-arrow that returns to a dedicated bench
  picker route, plus a ▾ menu for bench-scoped settings.
  Blocked by: bench picker route not yet implemented.
-->
<div class="bench-selector">
  <button class="toggle-btn" onclick={toggleDropdown} title="Switch bench">
    <span class="value">Switch — {currentName}</span>
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
  /*
    The selector inherits its colour context from its parent: when in the
    navy bench-head, "color: inherit" gives cream type; when in the picker
    nav (paper background), it gives navy ink. The dropdown opens below the
    bench-head and uses the riso-block treatment.
  */
  .bench-selector {
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
  .bench-dropdown {
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

  .bench-dropdown::after {
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

  /* Bench list rows. */
  .bench-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .bench-item {
    display: flex;
    align-items: center;
    padding: 9px 14px;
    gap: 4px;
    user-select: none;
    border-bottom: 1px dotted var(--rule-faint);
    color: var(--ink);
  }

  .bench-item:last-child {
    border-bottom: none;
  }

  .bench-item:hover {
    background: rgba(14, 59, 102, 0.08);
  }

  .bench-item.active {
    background: var(--ink);
    color: var(--paper);
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
    text-transform: inherit;
    letter-spacing: inherit;
  }

  .bench-name {
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  /* Delete controls. */
  .delete-bench-btn {
    background: none;
    border: none;
    color: inherit;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1;
    padding: 0 4px;
    cursor: pointer;
    visibility: hidden;
    flex-shrink: 0;
    opacity: 0.6;
  }

  .bench-item:hover .delete-bench-btn {
    visibility: visible;
  }

  .delete-bench-btn:hover {
    opacity: 1;
    color: var(--ink-orange);
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

  .create-bench-btn {
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

  .create-bench-btn:hover {
    background: rgba(240, 106, 58, 0.1);
  }

  /* Create-bench form. */
  .create-bench-form {
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    border-top: 1px dotted var(--rule-soft);
  }

  .create-bench-form input {
    width: 100%;
    padding: 8px 10px;
    border: 1px solid var(--ink);
    background: var(--paper);
    font-family: var(--font-body);
    font-size: 13px;
    color: var(--ink);
  }

  .create-bench-form input:focus {
    outline: none;
    background: rgba(240, 106, 58, 0.06);
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
