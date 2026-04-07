<!-- Project selector — interactive dropdown with project management. -->
<script lang="ts">
  import type { ProjectStore } from "../stores/projects";
  import { onMount, onDestroy } from "svelte";

  interface Props {
    // Injected by App.svelte once all stores are wired. Optional so AppLayout
    // can render without crashing before the wiring is complete (same pattern
    // as ConversationList).
    projectStore?: ProjectStore;
    // Callback invoked after the active project changes, so App.svelte can
    // reload conversations for the newly selected project.
    onProjectChange?: () => void;
  }

  let { projectStore, onProjectChange }: Props = $props();

  // ---------------------------------------------------------------------------
  // Local state
  // ---------------------------------------------------------------------------

  let isOpen = $state(false);
  let showCreateForm = $state(false);

  // Create-form input state.
  let newName = $state("");
  let newPath = $state("");
  let createError = $state<string | null>(null);
  let isCreating = $state(false);

  // Track which project is awaiting inline delete confirmation.
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
    newPath = "";
    createError = null;
  }

  // ---------------------------------------------------------------------------
  // Project selection
  // ---------------------------------------------------------------------------

  async function selectProject(id: string) {
    if (!projectStore) return;
    // Close the dropdown and notify the parent immediately for responsiveness.
    // The store update (a local lookup) is run after so the UI doesn't lag.
    closeDropdown();
    onProjectChange?.();
    await projectStore.selectProject(id);
  }

  // ---------------------------------------------------------------------------
  // Project creation
  // ---------------------------------------------------------------------------

  async function submitCreate(event: Event) {
    event.preventDefault();
    if (!projectStore || !newName.trim() || !newPath.trim()) return;

    isCreating = true;
    createError = null;

    try {
      await projectStore.createProject(newName.trim(), newPath.trim());
      onProjectChange?.();
      closeDropdown();
    } catch (err) {
      // Surface API error messages (e.g. duplicate name, invalid path).
      createError = err instanceof Error ? err.message : "Failed to create project";
    } finally {
      isCreating = false;
    }
  }

  // ---------------------------------------------------------------------------
  // Project deletion
  // ---------------------------------------------------------------------------

  function requestDelete(id: string, event: MouseEvent) {
    // Prevent the click from bubbling up to the project-item and selecting it.
    event.stopPropagation();
    pendingDelete = id;
  }

  async function confirmDelete(id: string, event: MouseEvent) {
    event.stopPropagation();
    if (!projectStore) return;
    await projectStore.deleteProject(id);
    onProjectChange?.();
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
  const currentName = $derived(projectStore?.currentProject?.name ?? "default");
</script>

<div class="project-selector">
  <span class="label">Project</span>
  <button class="toggle-btn" onclick={toggleDropdown}>
    <span class="value">{currentName}</span>
    <span class="caret" class:open={isOpen}>▾</span>
  </button>

  {#if isOpen && projectStore}
    <div class="project-dropdown">
      <!-- Project list -->
      <ul class="project-list">
        {#each projectStore.projects as project (project.id)}
          {@const isActive = projectStore.currentProject?.id === project.id}
          {@const isDefault = project.name === "default"}
          {@const isPendingDelete = pendingDelete === project.id}

          <li
            class="project-item"
            class:active={isActive}
          >
            <button
              class="project-select-btn"
              onclick={() => selectProject(project.id)}
            >
              <span class="project-name">{project.name}</span>
            </button>

            {#if isPendingDelete}
              <!-- Inline delete confirmation — no browser dialogs. -->
              <span class="delete-confirm">
                <button
                  class="confirm-delete-btn"
                  onclick={(e) => confirmDelete(project.id, e)}
                >Delete</button>
                <button class="cancel-delete-btn" onclick={cancelDelete}>
                  Cancel
                </button>
              </span>
            {:else if !isDefault}
              <!-- Default project cannot be deleted. -->
              <button
                class="delete-project-btn"
                title="Delete project"
                onclick={(e) => requestDelete(project.id, e)}
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
          class="create-project-form"
          onsubmit={submitCreate}
        >
          <input
            type="text"
            name="name"
            placeholder="Project name"
            bind:value={newName}
            required
          />
          <input
            type="text"
            name="workspace_path"
            placeholder="Workspace path"
            bind:value={newPath}
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
          class="create-project-btn"
          onclick={() => { showCreateForm = true; }}
        >
          + New project
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* -------------------------------------------------------------------------
     Selector bar
  ------------------------------------------------------------------------- */

  .project-selector {
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

  .project-dropdown {
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
     Project list
  ------------------------------------------------------------------------- */

  .project-list {
    list-style: none;
    margin: 0;
    padding: 4px 0;
  }

  .project-item {
    display: flex;
    align-items: center;
    padding: 8px 12px;
    user-select: none;
    gap: 4px;
  }

  .project-item:hover {
    background: #f5f5f5;
  }

  .project-item.active {
    background: #eef2ff;
    font-weight: 500;
  }

  .project-select-btn {
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

  .project-name {
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  /* -------------------------------------------------------------------------
     Delete controls
  ------------------------------------------------------------------------- */

  .delete-project-btn {
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

  .project-item:hover .delete-project-btn {
    visibility: visible;
  }

  .delete-project-btn:hover {
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

  .create-project-btn {
    width: 100%;
    padding: 8px 12px;
    background: none;
    border: none;
    text-align: left;
    font-size: 0.875rem;
    color: #555;
    cursor: pointer;
  }

  .create-project-btn:hover {
    background: #f5f5f5;
    color: #333;
  }

  /* -------------------------------------------------------------------------
     Create project form
  ------------------------------------------------------------------------- */

  .create-project-form {
    padding: 8px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .create-project-form input {
    width: 100%;
    padding: 6px 8px;
    border: 1px solid #d0d0d0;
    border-radius: 4px;
    font-size: 0.8125rem;
    box-sizing: border-box;
  }

  .create-project-form input:focus {
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
