<!--
  Two-stage space picker:
    1. Card grid of space names. Single click on a space name selects it and reveals
       a details panel below the grid (about, last opened, conversation count,
       attached folders — though the data is still thin server-side).
    2. A deliberate second action — the space card's "Open" button —
       enters the space, which is what calls spaceStore.selectSpace().

  This component renders as the body content when no space is selected. The
  parent (AppLayout) collapses the top bar in that mode.

  Per the same doc, space cards show only the name — storage paths, attached
  folders, etc. are noise on the picker. Those reveal in the details panel.

-->
<script lang="ts">
  import WorkspaceAccessOptions from "./WorkspaceAccessOptions.svelte";
  import {
    attachSpaceWorkspace,
    detachSpaceWorkspace,
    listSpaceSessions,
    listSpaceWorkspaces,
    updateSpace,
  } from "../api/spaces";
  import type { SpaceStore } from "../stores/spaces";
  import type { Space, ConversationSummary, WorkspaceAttachment } from "../types";

  interface Props {
    spaceStore?: SpaceStore;
    onSpaceChange?: () => void;
    onOpenSpaceSettings?: (space: Space) => void;
  }

  let { spaceStore, onSpaceChange, onOpenSpaceSettings }: Props = $props();

  // The picker tracks its own selection separately from spaceStore.currentSpace
  // — picking a card is a preview, not a switch. Switching only happens when
  // the user explicitly opens the space.
  let selectedId = $state<string | null>(null);

  // Inline create form state.
  let showCreateForm = $state(false);
  let newName = $state("");
  let createError = $state<string | null>(null);
  let isCreating = $state(false);
  let selectedWorkspaces = $state<WorkspaceAttachment[]>([]);
  let selectedConversations = $state<ConversationSummary[]>([]);
  let detailError = $state<string | null>(null);
  let aboutDraft = $state("");
  let isSavingAbout = $state(false);
  let aboutNotice = $state<string | null>(null);
  let workspacePathDraft = $state("");
  let workspaceLabelDraft = $state("");
  let workspaceCanWrite = $state(true);
  let workspaceIsTrusted = $state(false);
  let isAttachingWorkspace = $state(false);
  let workspaceNotice = $state<string | null>(null);
  let relinkDrafts = $state<Record<string, string>>({});
  let relinkingWorkspaceId = $state<string | null>(null);

  const spaces = $derived(spaceStore?.spaces ?? []);
  const selected = $derived(
    selectedId ? spaces.find((b) => b.id === selectedId) ?? null : null,
  );
  const selectedLastOpened = $derived(selected?.last_opened_at ?? null);

  function selectCard(space: Space): void {
    selectedId = space.id;
    aboutDraft = space.description ?? "";
    aboutNotice = null;
    workspaceNotice = null;
  }

  $effect(() => {
    const spaceId = selected?.id;
    if (!spaceId) {
      selectedWorkspaces = [];
      selectedConversations = [];
      detailError = null;
      aboutDraft = "";
      aboutNotice = null;
      workspacePathDraft = "";
      workspaceLabelDraft = "";
      workspaceNotice = null;
      relinkDrafts = {};
      relinkingWorkspaceId = null;
      return;
    }
    aboutDraft = selected.description ?? "";
    void loadSpaceDetail(spaceId);
  });

  async function loadSpaceDetail(spaceId: string): Promise<void> {
    detailError = null;
    try {
      const [workspaces, conversations] = await Promise.all([
        listSpaceWorkspaces(spaceId),
        listSpaceSessions(spaceId),
      ]);
      if (selected?.id !== spaceId) return;
      selectedWorkspaces = workspaces;
      selectedConversations = conversations;
      relinkDrafts = Object.fromEntries(
        workspaces.map((workspace) => [workspace.id, relinkDrafts[workspace.id] ?? ""]),
      );
    } catch (error) {
      selectedWorkspaces = [];
      selectedConversations = [];
      detailError = error instanceof Error ? error.message : "Failed to load space detail.";
    }
  }

  async function openSpace(id: string): Promise<void> {
    if (!spaceStore) return;
    await spaceStore.selectSpace(id);
    onSpaceChange?.();
  }

  async function submitCreate(event: Event): Promise<void> {
    event.preventDefault();
    if (!spaceStore || !newName.trim()) return;

    isCreating = true;
    createError = null;
    try {
      await spaceStore.createSpace(newName.trim());
      showCreateForm = false;
      newName = "";
      onSpaceChange?.();
    } catch (err) {
      createError = err instanceof Error ? err.message : "Failed to create space";
    } finally {
      isCreating = false;
    }
  }

  async function saveSpaceAbout(): Promise<void> {
    if (!selected || !spaceStore) return;
    isSavingAbout = true;
    detailError = null;
    aboutNotice = null;
    try {
      const updated = await updateSpace(selected.id, { description: aboutDraft.trim() });
      await spaceStore.loadSpaces();
      selectedId = updated.id;
      aboutDraft = updated.description;
      aboutNotice = "Saved.";
    } catch (error) {
      detailError = error instanceof Error ? error.message : "Failed to save space about text.";
    } finally {
      isSavingAbout = false;
    }
  }

  function workspacePermissions(): string[] {
    return workspaceCanWrite ? ["read", "write"] : ["read"];
  }

  function workspacePermissionLabel(workspace: WorkspaceAttachment): string {
    if (workspace.permissions.includes("write")) return "read/write";
    if (workspace.permissions.includes("read")) return "read only";
    return "no file grants";
  }

  async function attachWorkspace(event: Event): Promise<void> {
    event.preventDefault();
    if (!selected || !workspacePathDraft.trim()) return;

    isAttachingWorkspace = true;
    detailError = null;
    workspaceNotice = null;
    try {
      await attachSpaceWorkspace(selected.id, {
        path: workspacePathDraft.trim(),
        label: workspaceLabelDraft.trim() || undefined,
        permissions: workspacePermissions(),
        is_trusted: workspaceIsTrusted,
        set_as_default: selectedWorkspaces.length === 0,
      });
      workspacePathDraft = "";
      workspaceLabelDraft = "";
      await loadSpaceDetail(selected.id);
      workspaceNotice = "Workspace attached.";
    } catch (error) {
      detailError = error instanceof Error ? error.message : "Failed to attach workspace.";
    } finally {
      isAttachingWorkspace = false;
    }
  }

  function setRelinkDraft(workspaceId: string, value: string): void {
    relinkDrafts = { ...relinkDrafts, [workspaceId]: value };
  }

  async function relinkWorkspace(workspace: WorkspaceAttachment): Promise<void> {
    if (!selected) return;
    const replacementPath = relinkDrafts[workspace.id]?.trim();
    if (!replacementPath) return;

    relinkingWorkspaceId = workspace.id;
    detailError = null;
    workspaceNotice = null;
    try {
      await detachSpaceWorkspace(selected.id, workspace.id);
      await attachSpaceWorkspace(selected.id, {
        path: replacementPath,
        label: workspace.label,
        permissions: workspace.permissions,
        is_trusted: workspace.is_trusted,
        set_as_default: workspace.is_default,
      });
      setRelinkDraft(workspace.id, "");
      await loadSpaceDetail(selected.id);
      workspaceNotice = "Workspace relinked.";
    } catch (error) {
      detailError = error instanceof Error ? error.message : "Failed to relink workspace.";
    } finally {
      relinkingWorkspaceId = null;
    }
  }
</script>

<div class="picker">
  <header class="picker-head">
    <p class="eyebrow">Pick a space</p>
    <h1 class="display">Choose where to work.</h1>
    <p class="lede">
      Each space keeps its conversations together, along with its attached workspace folders.
      Click a card to preview, then open it.
    </p>
  </header>

  <div class="space-grid">
    {#each spaces as space (space.id)}
      {@const isSelected = selectedId === space.id}
      <!--
        Card is a non-interactive container; every interactive target inside is
        a real sibling <button>. The previous markup nested <span role="button">
        for Open and Settings inside a card <button>, which is invalid HTML and
        produces unpredictable accessibility-tree behaviour (UI audit f-2).
        Preview (select) is the space-name button; Open and Settings are its
        siblings. Double-click as a skip-preview shortcut is gone — it doesn't
        translate to touch and was only discoverable after first selecting a
        card (UI audit f-11). Open is always visible, so preview is optional.
      -->
      <article class="space-card" class:selected={isSelected} aria-label={`Space ${space.name}`}>
        <div class="card-top">
          <button
            type="button"
            class="space-name-btn"
            onclick={() => selectCard(space)}
            aria-pressed={isSelected}
            aria-label={isSelected ? `Previewing space ${space.name}` : `Preview space ${space.name}`}
          >
            <h2 class="space-name">{space.name}</h2>
          </button>
          {#if onOpenSpaceSettings}
            <button
              type="button"
              class="space-settings"
              aria-label={`Space settings for ${space.name}`}
              title="Space settings"
              onclick={() => onOpenSpaceSettings?.(space)}
            >
              ⚙
            </button>
          {/if}
        </div>
        {#if space.is_default}
          <span class="default-mark">Default</span>
        {/if}
        <div class="card-actions">
          <button type="button" class="open-btn" onclick={() => openSpace(space.id)}>
            Open
          </button>
          <span class="hint">Enter to open</span>
        </div>
      </article>
    {/each}

    <!-- New space: dashed-border card that flips into an inline form. -->
    {#if showCreateForm}
      <form class="space-card new-form" onsubmit={submitCreate}>
        <label class="eyebrow" for="new-space-name">New space name</label>
        <input
          id="new-space-name"
          type="text"
          bind:value={newName}
          placeholder="e.g. research"
          autocomplete="off"
          required
        />
        {#if createError}
          <p class="create-error">{createError}</p>
        {/if}
        <div class="form-actions">
          <button type="submit" class="open-btn" disabled={isCreating}>
            {isCreating ? "Creating…" : "Create"}
          </button>
          <button
            type="button"
            class="cancel-btn"
            onclick={() => {
              showCreateForm = false;
              newName = "";
              createError = null;
            }}
          >
            Cancel
          </button>
        </div>
      </form>
    {:else}
      <button
        type="button"
        class="space-card new-card"
        onclick={() => {
          showCreateForm = true;
        }}
      >
        <span class="plus">＋</span>
        <span class="new-label">New space</span>
      </button>
    {/if}
  </div>

  {#if selected}
    <aside class="space-detail" aria-label="Space detail">
      <header class="detail-head">
        <p class="eyebrow">Space detail</p>
        <h2 class="detail-name">{selected.name}</h2>
      </header>
      <dl class="detail-grid">
        <dt>Slug</dt>
        <dd class="mono">{selected.slug}</dd>
        <dt>Default</dt>
        <dd>{selected.is_default ? "yes" : "no"}</dd>
        <dt>Created</dt>
        <dd class="mono">{selected.created_at || "—"}</dd>
        <dt>About</dt>
        <dd>
          <textarea
            class="about-input"
            bind:value={aboutDraft}
            rows="3"
            aria-label="Space about text"
            placeholder="What is this space for?"
          ></textarea>
          <div class="about-actions">
            <button
              type="button"
              class="save-about"
              onclick={saveSpaceAbout}
              disabled={isSavingAbout || aboutDraft.trim() === (selected.description ?? "")}
            >
              {isSavingAbout ? "Saving..." : "Save about"}
            </button>
            {#if aboutNotice}
              <span class="about-notice">{aboutNotice}</span>
            {/if}
          </div>
        </dd>
        <dt>Workspace</dt>
        <dd>
          <div class="workspace-panel">
            {#if selectedWorkspaces.length > 0}
              <div class="workspace-list">
                {#each selectedWorkspaces as workspace (workspace.id)}
                  <article class:missing={workspace.is_missing} class="workspace-row">
                    <div class="workspace-main">
                      <strong>{workspace.label}</strong>
                      <span class="workspace-path">{workspace.path}</span>
                      <span class="workspace-meta">
                        {workspacePermissionLabel(workspace)}
                        · {workspace.is_trusted ? "trusted" : "approval required"}
                        {workspace.is_default ? " · default" : ""}
                      </span>
                    </div>
                    {#if workspace.is_missing}
                      <div class="missing-workspace">
                        <span class="missing-label">Missing folder</span>
                        <div class="relink-row">
                          <input
                            type="text"
                            value={relinkDrafts[workspace.id] ?? ""}
                            placeholder="/new/path/to/folder"
                            aria-label={`Replacement path for ${workspace.label}`}
                            oninput={(event) =>
                              setRelinkDraft(workspace.id, event.currentTarget.value)}
                          />
                          <button
                            type="button"
                            class="save-about"
                            onclick={() => relinkWorkspace(workspace)}
                            disabled={
                              relinkingWorkspaceId === workspace.id ||
                              !(relinkDrafts[workspace.id] ?? "").trim()
                            }
                          >
                            {relinkingWorkspaceId === workspace.id ? "Relinking..." : "Relink"}
                          </button>
                        </div>
                      </div>
                    {/if}
                  </article>
                {/each}
              </div>
            {:else}
              <p class="empty-workspace">none attached</p>
            {/if}

            <form class="workspace-form" onsubmit={attachWorkspace}>
              <label>
                <span>Folder path</span>
                <input
                  type="text"
                  bind:value={workspacePathDraft}
                  placeholder="/home/user/Documents/Code/Repos/Tools/Swage"
                  autocomplete="off"
                />
              </label>
              <label>
                <span>Label</span>
                <input
                  type="text"
                  bind:value={workspaceLabelDraft}
                  placeholder="optional"
                  autocomplete="off"
                />
              </label>
              <WorkspaceAccessOptions bind:canWrite={workspaceCanWrite} bind:isTrusted={workspaceIsTrusted} />
              <button
                type="submit"
                class="save-about"
                disabled={isAttachingWorkspace || !workspacePathDraft.trim()}
              >
                {isAttachingWorkspace ? "Attaching..." : "Attach folder"}
              </button>
            </form>
            {#if workspaceNotice}
              <p class="about-notice">{workspaceNotice}</p>
            {/if}
          </div>
        </dd>
        <dt>Last opened</dt>
        <dd class="mono">{selectedLastOpened ?? "not opened this browser session"}</dd>
        <dt>Conversations</dt>
        <dd>{selectedConversations.length}</dd>
      </dl>
      {#if detailError}
        <p class="detail-error">{detailError}</p>
      {/if}
    </aside>
  {/if}
</div>

<style>
  /*
    Riso space picker. Card grid, two-stage selection, dashed-border "new" card.
    Inherits palette from <body> so it switches with the theme toggle.
  */

  .picker {
    height: 100%;
    padding: 32px 40px;
    display: flex;
    flex-direction: column;
    gap: 22px;
    overflow: auto;
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-body);
  }

  .picker-head {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-width: 60ch;
  }

  .eyebrow {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
    margin: 0;
  }

  h1.display {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: clamp(2.4rem, 4.5vw, 3.4rem);
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .lede {
    margin: 0;
    color: var(--ink-soft);
    font-size: 13.5px;
    line-height: 1.55;
    max-width: 50ch;
  }

  .space-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 14px;
  }

  /* Cards — flat ink frame. The offset orange shadow is reserved for the
     single active selection only, not hover, so it keeps meaning "selected"
     (UI audit f-18). Hovered cards signal state through border/fill alone. */
  .space-card {
    position: relative;
    background: var(--paper);
    color: var(--ink);
    border: var(--border-strong);
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    text-align: left;
    font: inherit;
    isolation: isolate;
  }

  .space-card::after {
    content: "";
    position: absolute;
    inset: 0;
    border: 2px solid var(--accent);
    transform: translate(var(--offset), var(--offset));
    z-index: -1;
    mix-blend-mode: var(--shadow-blend);
    opacity: 0;
    pointer-events: none;
    transition: opacity 120ms ease;
  }

  /* Shadow on selected only — the single active selection. Hover no longer
     paints the offset so it stops competing for emphasis (UI audit f-18). */
  .space-card.selected::after {
    opacity: var(--shadow-opacity);
  }

  .space-card.selected {
    background: var(--ink);
    color: var(--paper);
  }

  .card-top {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
  }

  /* The space name is the preview/select trigger — a real button so keyboard
     and SR are clean (UI audit f-2, f-12). It carries the display type; the
     <h2> inside keeps the heading semantics. */
  .space-name-btn {
    background: none;
    border: none;
    padding: 0;
    margin: 0;
    color: inherit;
    cursor: pointer;
    text-align: left;
    font: inherit;
    max-width: 100%;
  }

  .space-name-btn:hover {
    text-decoration: underline;
    text-decoration-thickness: 2px;
    text-underline-offset: 4px;
  }

  .space-name-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }

  .space-name {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: var(--font-display-3);
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: inherit;
  }

  .space-settings {
    flex-shrink: 0;
    background: none;
    border: 1px solid var(--rule-soft);
    color: inherit;
    opacity: 0.7;
    width: 26px;
    height: 26px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    padding: 0;
  }

  .space-settings:hover,
  .space-settings:focus-visible {
    opacity: 1;
    border-color: var(--ink);
  }

  .space-settings:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .default-mark {
    align-self: flex-start;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    /* Text label on paper — use the AA-safe ink-orange, not the decorative
       accent (UI audit f-8). On the selected (ink) card the accent reads
       fine as a fill-equivalent, so keep it there. */
    color: var(--ink-orange);
  }

  .space-card.selected .default-mark {
    color: var(--accent);
  }

  .card-actions {
    margin-top: auto;
    display: flex;
    align-items: center;
    gap: 12px;
    border-top: 1px dotted color-mix(in srgb, var(--paper) 40%, transparent);
    padding-top: 10px;
  }

  /* Always visible — preview is optional when Open is right here (UI audit
     f-11). Real <button>, sibling to the name/settings buttons (UI audit f-2). */
  .open-btn {
    display: inline-block;
    background: var(--accent);
    color: var(--paper);
    border: none;
    padding: 7px 14px;
    font-family: var(--font-display);
    font-size: var(--font-label-md);
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    cursor: pointer;
    user-select: none;
  }

  .open-btn:hover {
    filter: brightness(0.92);
  }

  .open-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .open-btn:focus-visible {
    outline: 2px solid var(--ink);
    outline-offset: 2px;
  }

  .hint {
    font-family: var(--font-body);
    font-style: italic;
    font-size: var(--font-label-sm);
    color: color-mix(in srgb, var(--paper) 70%, transparent);
  }

  /* New-space card and inline form. */
  .new-card,
  .new-form {
    border-style: dashed;
    align-items: center;
    justify-content: center;
    text-align: center;
    background: transparent;
  }

  .new-card::after,
  .new-form::after {
    display: none;
  }

  .new-card {
    color: var(--ink-soft);
  }

  .new-card:hover {
    color: var(--ink);
  }

  .plus {
    font-family: var(--font-display);
    font-size: 36px;
    line-height: 1;
  }

  .new-label {
    font-family: var(--font-display);
    font-size: 18px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .new-form {
    align-items: stretch;
    text-align: left;
    cursor: default;
  }

  .new-form input {
    padding: 9px 10px;
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    font-family: var(--font-body);
    font-size: 13px;
  }

  .new-form input:focus {
    outline: none;
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }

  .create-error {
    margin: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--error);
  }

  .form-actions {
    display: flex;
    gap: 8px;
  }

  .cancel-btn {
    background: var(--paper);
    color: var(--ink);
    border: 1px solid var(--ink);
    padding: 7px 14px;
    font-family: var(--font-display);
    font-size: 14px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .cancel-btn:hover {
    background: var(--ink);
    color: var(--paper);
  }

  /* Detail panel below the grid — same visual weight as a card. */
  .space-detail {
    border: 2px solid var(--ink);
    background: var(--paper);
    padding: 18px 22px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .detail-head {
    display: flex;
    flex-direction: column;
    gap: 4px;
    border-bottom: 1px solid var(--ink);
    padding-bottom: 8px;
  }

  .detail-name {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 24px;
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .detail-grid {
    display: grid;
    grid-template-columns: 100px 1fr;
    gap: 6px 18px;
    font-size: 13px;
    margin: 0;
  }

  .detail-grid dt {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-soft);
    align-self: center;
  }

  .detail-grid dd {
    margin: 0;
    color: var(--ink);
  }

  .workspace-panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .workspace-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .workspace-row {
    border: 1px solid var(--ink);
    padding: 10px;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 8px;
    background: color-mix(in srgb, var(--paper) 42%, transparent);
  }

  .workspace-row.missing {
    border-style: dashed;
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  .workspace-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .workspace-main strong {
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 18px;
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .workspace-path {
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.35;
    overflow-wrap: anywhere;
  }

  .workspace-meta,
  .missing-label {
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .missing-workspace,
  .workspace-form {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .relink-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 8px;
    align-items: center;
  }

  .workspace-form {
    border-top: 1px dotted var(--ink);
    padding-top: 10px;
  }

  .workspace-form label {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .workspace-form label span {
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .workspace-form input[type="text"],
  .relink-row input {
    width: 100%;
    min-width: 0;
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    padding: 7px 9px;
    font: inherit;
    font-size: 12.5px;
  }

  .workspace-form input:focus,
  .relink-row input:focus {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .empty-workspace {
    margin: 0;
  }

  .about-input {
    width: 100%;
    min-height: 72px;
    resize: vertical;
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    padding: 8px 10px;
    font: inherit;
    line-height: 1.45;
  }

  .about-input:focus {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .about-actions {
    margin-top: 8px;
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .save-about {
    border: 1px solid var(--ink);
    background: var(--ink);
    color: var(--paper);
    padding: 6px 10px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .save-about:disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }

  .about-notice {
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .detail-error {
    margin: 0;
    color: var(--error);
    font-size: var(--font-label-md);
  }

  .mono {
    font-family: var(--font-mono);
    font-size: 11.5px;
    letter-spacing: 0.04em;
  }

  @media (max-width: 720px) {
    .picker {
      padding: 24px 22px;
    }

    .space-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
