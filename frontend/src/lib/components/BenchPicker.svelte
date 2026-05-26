<!--
  Two-stage bench picker per Progress.md:
    1. Card grid of bench names. Single click on a card selects it and reveals
       a details panel below the grid (about, last opened, conversation count,
       attached folders — though the data is still thin server-side).
    2. A deliberate second action — the "Open bench" button or double-click —
       enters the bench, which is what calls benchStore.selectBench().

  This component renders as the body content when no bench is selected. The
  parent (AppLayout) collapses the top bar in that mode.

  Per the same doc, bench cards show only the name — storage paths, attached
  folders, etc. are noise on the picker. Those reveal in the details panel.

-->
<script lang="ts">
  import {
    attachBenchWorkspace,
    detachBenchWorkspace,
    listBenchSessions,
    listBenchWorkspaces,
    updateBench,
  } from "../api/benches";
  import type { BenchStore } from "../stores/benches";
  import type { Bench, ConversationSummary, WorkspaceAttachment } from "../types";

  interface Props {
    benchStore?: BenchStore;
    onBenchChange?: () => void;
    onOpenBenchSettings?: (bench: Bench) => void;
  }

  let { benchStore, onBenchChange, onOpenBenchSettings }: Props = $props();

  // The picker tracks its own selection separately from benchStore.currentBench
  // — picking a card is a preview, not a switch. Switching only happens when
  // the user explicitly opens the bench.
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

  const benches = $derived(benchStore?.benches ?? []);
  const selected = $derived(
    selectedId ? benches.find((b) => b.id === selectedId) ?? null : null,
  );
  const selectedLastOpened = $derived(selected?.last_opened_at ?? null);

  function selectCard(bench: Bench): void {
    selectedId = bench.id;
    aboutDraft = bench.description ?? "";
    aboutNotice = null;
    workspaceNotice = null;
  }

  $effect(() => {
    const benchId = selected?.id;
    if (!benchId) {
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
    void loadBenchDetail(benchId);
  });

  async function loadBenchDetail(benchId: string): Promise<void> {
    detailError = null;
    try {
      const [workspaces, conversations] = await Promise.all([
        listBenchWorkspaces(benchId),
        listBenchSessions(benchId),
      ]);
      if (selected?.id !== benchId) return;
      selectedWorkspaces = workspaces;
      selectedConversations = conversations;
      relinkDrafts = Object.fromEntries(
        workspaces.map((workspace) => [workspace.id, relinkDrafts[workspace.id] ?? ""]),
      );
    } catch (error) {
      selectedWorkspaces = [];
      selectedConversations = [];
      detailError = error instanceof Error ? error.message : "Failed to load bench detail.";
    }
  }

  async function openBench(id: string): Promise<void> {
    if (!benchStore) return;
    await benchStore.selectBench(id);
    onBenchChange?.();
  }

  async function submitCreate(event: Event): Promise<void> {
    event.preventDefault();
    if (!benchStore || !newName.trim()) return;

    isCreating = true;
    createError = null;
    try {
      await benchStore.createBench(newName.trim());
      showCreateForm = false;
      newName = "";
      onBenchChange?.();
    } catch (err) {
      createError = err instanceof Error ? err.message : "Failed to create bench";
    } finally {
      isCreating = false;
    }
  }

  async function saveBenchAbout(): Promise<void> {
    if (!selected || !benchStore) return;
    isSavingAbout = true;
    detailError = null;
    aboutNotice = null;
    try {
      const updated = await updateBench(selected.id, { description: aboutDraft.trim() });
      await benchStore.loadBenches();
      selectedId = updated.id;
      aboutDraft = updated.description;
      aboutNotice = "Saved.";
    } catch (error) {
      detailError = error instanceof Error ? error.message : "Failed to save bench about text.";
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
      await attachBenchWorkspace(selected.id, {
        path: workspacePathDraft.trim(),
        label: workspaceLabelDraft.trim() || undefined,
        permissions: workspacePermissions(),
        is_trusted: workspaceIsTrusted,
        set_as_default: selectedWorkspaces.length === 0,
      });
      workspacePathDraft = "";
      workspaceLabelDraft = "";
      await loadBenchDetail(selected.id);
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
      await detachBenchWorkspace(selected.id, workspace.id);
      await attachBenchWorkspace(selected.id, {
        path: replacementPath,
        label: workspace.label,
        permissions: workspace.permissions,
        is_trusted: workspace.is_trusted,
        set_as_default: workspace.is_default,
      });
      setRelinkDraft(workspace.id, "");
      await loadBenchDetail(selected.id);
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
    <p class="eyebrow">Pick a bench</p>
    <h1 class="display">Choose where to work.</h1>
    <p class="lede">
      Each bench is a separate workspace — its own conversations, memory, and
      attached files. Click a card to preview, then open it.
    </p>
  </header>

  <div class="bench-grid">
    {#each benches as bench (bench.id)}
      {@const isSelected = selectedId === bench.id}
      <!-- A native <button> avoids the a11y_no_noninteractive_element_to_interactive_role
           warning and gives Enter/Space activation for free. The visual still
           looks like a card; we just opt out of the default chrome via CSS. -->
      <button
        type="button"
        class="bench-card"
        class:selected={isSelected}
        onclick={() => selectCard(bench)}
        ondblclick={() => openBench(bench.id)}
      >
        <div class="card-top">
          <h2 class="bench-name">{bench.name}</h2>
          <!-- ▾ secondary trigger. Same nesting concern as the open button:
               render as a span with button semantics so the outer card-button
               stays valid HTML. -->
          <span
            role="button"
            tabindex="0"
            class="bench-menu"
            title="Open bench settings"
            onclick={(event) => {
              event.stopPropagation();
              onOpenBenchSettings?.(bench);
            }}
            onkeydown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                event.stopPropagation();
                onOpenBenchSettings?.(bench);
              }
            }}
          >
            ▾
          </span>
        </div>
        {#if bench.is_default}
          <span class="default-mark">Default</span>
        {/if}
        {#if isSelected}
          <div class="card-actions">
            <!-- Inner buttons render as spans-with-events to avoid nesting
                 <button> inside the card-button (invalid HTML). They keep
                 keyboard activation via tabindex + key handlers. -->
            <span
              role="button"
              tabindex="0"
              class="open-btn"
              onclick={(e) => {
                e.stopPropagation();
                openBench(bench.id);
              }}
              onkeydown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  e.stopPropagation();
                  openBench(bench.id);
                }
              }}
            >
              Open bench
            </span>
            <span class="hint">or double-click the card</span>
          </div>
        {/if}
      </button>
    {/each}

    <!-- New bench: dashed-border card that flips into an inline form. -->
    {#if showCreateForm}
      <form class="bench-card new-form" onsubmit={submitCreate}>
        <label class="eyebrow" for="new-bench-name">New bench name</label>
        <input
          id="new-bench-name"
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
        class="bench-card new-card"
        onclick={() => {
          showCreateForm = true;
        }}
      >
        <span class="plus">＋</span>
        <span class="new-label">New bench</span>
      </button>
    {/if}
  </div>

  {#if selected}
    <aside class="bench-detail" aria-label="Bench detail">
      <header class="detail-head">
        <p class="eyebrow">Bench detail</p>
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
            aria-label="Bench about text"
            placeholder="What is this bench for?"
          ></textarea>
          <div class="about-actions">
            <button
              type="button"
              class="save-about"
              onclick={saveBenchAbout}
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
              <div class="workspace-options">
                <label>
                  <input type="checkbox" bind:checked={workspaceCanWrite} />
                  <span>write access</span>
                </label>
                <label>
                  <input type="checkbox" bind:checked={workspaceIsTrusted} />
                  <span>trusted</span>
                </label>
              </div>
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
    Riso bench picker. Card grid, two-stage selection, dashed-border "new" card.
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

  .bench-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 14px;
  }

  /* Cards — flat ink frame, fluoro orange offset shadow on the active card. */
  .bench-card {
    position: relative;
    background: var(--paper);
    color: var(--ink);
    border: 2px solid var(--ink);
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    cursor: pointer;
    text-align: left;
    font: inherit;
    isolation: isolate;
  }

  .bench-card::after {
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

  .bench-card:hover::after,
  .bench-card.selected::after {
    opacity: var(--shadow-opacity);
  }

  .bench-card.selected {
    background: var(--ink);
    color: var(--paper);
  }

  .card-top {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
  }

  .bench-name {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 28px;
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: inherit;
  }

  .bench-menu {
    color: inherit;
    opacity: 0.55;
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 1;
    padding: 0 4px;
    user-select: none;
    cursor: pointer;
  }

  .bench-menu:hover,
  .bench-menu:focus-visible {
    opacity: 1;
  }

  .default-mark {
    align-self: flex-start;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--accent);
  }

  .bench-card.selected .default-mark {
    color: var(--accent);
  }

  .card-actions {
    margin-top: auto;
    display: flex;
    align-items: center;
    gap: 12px;
    border-top: 1px dotted rgba(255, 248, 238, 0.4);
    padding-top: 10px;
  }

  .open-btn {
    display: inline-block;
    background: var(--accent);
    color: var(--paper);
    border: none;
    padding: 7px 14px;
    font-family: var(--font-display);
    font-size: 14px;
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
    font-size: 11.5px;
    color: rgba(255, 248, 238, 0.7);
  }

  /* New-bench card and inline form. */
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
  .bench-detail {
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
    background: rgba(255, 248, 238, 0.42);
  }

  .workspace-row.missing {
    border-style: dashed;
    background: rgba(240, 106, 58, 0.08);
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

  .workspace-options {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
  }

  .workspace-options label {
    flex-direction: row;
    align-items: center;
    gap: 6px;
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
    color: var(--ink-orange);
    font-size: 12px;
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

    .bench-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
