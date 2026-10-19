<!-- Topic-scoped settings pane. Global provider/runtime settings stay in
     SettingsPage; this component owns the topic's own data: its description,
     attached workspaces (including relinking a folder that has moved), its
     default surfaces and tags, and its sessions. -->
<script lang="ts">
  import { onMount } from "svelte";
  import WorkspaceAccessOptions from "./WorkspaceAccessOptions.svelte";
  import {
    attachTopicWorkspace,
    deleteTopic,
    detachTopicWorkspace,
    listTopicSessions,
    listTopicWorkspaces,
    relinkTopicWorkspace,
    updateTopicWorkspaceTrust,
    updateTopic,
  } from "../api/topics";
  import {
    addTopicDefaultSurface,
    listLibrary,
    listTopicDefaultSurfaces,
    removeTopicDefaultSurface,
  } from "../api/surfaces";
  import type { LibraryEntrySummary } from "../protocol/LibraryEntrySummary";
  import type { TopicSettingsTab } from "../router";
  import type { Topic, ConversationSummary, WorkspaceAttachment } from "../types";

  interface Props {
    topic: Topic | null;
    tab?: TopicSettingsTab;
    onNavigate?: (path: string) => void;
    // The topic's description changed or the topic was deleted: the parent
    // reloads its topic list.
    onTopicsChanged?: () => void | Promise<void>;
  }

  let { topic, tab = "overview", onNavigate, onTopicsChanged }: Props = $props();

  const tabs: { id: TopicSettingsTab; label: string }[] = [
    { id: "overview", label: "Overview" },
    { id: "workspaces", label: "Workspaces" },
    { id: "surfaces", label: "Surfaces" },
    { id: "sessions", label: "Sessions" },
  ];

  function openTab(next: TopicSettingsTab): void {
    if (!topic) return;
    onNavigate?.(`/settings/topic/${encodeURIComponent(topic.slug)}/${next}`);
  }

  let workspaces = $state<WorkspaceAttachment[]>([]);
  // Attaching decides whether the new folder becomes the topic's working
  // folder from this list, so the form waits for a real load rather than
  // reading an empty placeholder as "no attachments yet".
  let worktopicsLoaded = $state(false);
  let sessions = $state<ConversationSummary[]>([]);
  let isLoading = $state(false);
  let error = $state<string | null>(null);
  let workspacePath = $state("");
  let workspaceLabel = $state("");
  let workspaceCanWrite = $state(true);
  let workspaceIsTrusted = $state(false);
  let isMutating = $state(false);
  // The surface library and this topic's defaults from it, with the tags
  // that narrow the catalogue for the topic's sessions.
  let library = $state<LibraryEntrySummary[]>([]);
  let defaultSurfaces = $state<LibraryEntrySummary[]>([]);
  let tagsDraft = $state("");
  let tagsNotice = $state<string | null>(null);
  let surfacesLoaded = $state(false);
  // The description as edited here; saved explicitly.
  let aboutDraft = $state("");
  let aboutNotice = $state<string | null>(null);
  // Replacement paths typed for workspaces whose folder is missing.
  let relinkDrafts = $state<Record<string, string>>({});
  let relinkingWorkspaceId = $state<string | null>(null);
  let pendingDelete = $state(false);

  $effect(() => {
    aboutDraft = topic?.description ?? "";
  });

  onMount(loadTopicSettings);

  // The surfaces tab loads its library and defaults when it is shown,
  // whether the page mounted on it or the user moved to it.
  $effect(() => {
    if (tab === "surfaces" && !surfacesLoaded) void loadSurfaceSettings();
  });

  async function loadSurfaceSettings(): Promise<void> {
    if (!topic) return;
    try {
      const [catalogue, defaults] = await Promise.all([
        listLibrary(),
        listTopicDefaultSurfaces(topic.id),
      ]);
      library = catalogue.entries;
      defaultSurfaces = defaults.entries;
      tagsDraft = (topic.tags ?? []).join(", ");
      surfacesLoaded = true;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load the surface library.";
    }
  }

  async function designateDefault(entryId: string): Promise<void> {
    if (!topic) return;
    isMutating = true;
    try {
      defaultSurfaces = (await addTopicDefaultSurface(topic.id, entryId)).entries;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to designate the default surface.";
    } finally {
      isMutating = false;
    }
  }

  async function withdrawDefault(entryId: string): Promise<void> {
    if (!topic) return;
    isMutating = true;
    try {
      await removeTopicDefaultSurface(topic.id, entryId);
      defaultSurfaces = defaultSurfaces.filter((entry) => entry.id !== entryId);
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to withdraw the default surface.";
    } finally {
      isMutating = false;
    }
  }

  async function saveTags(): Promise<void> {
    if (!topic) return;
    isMutating = true;
    tagsNotice = null;
    try {
      const tags = tagsDraft.split(",").map((tag) => tag.trim()).filter(Boolean);
      const updated = await updateTopic(topic.id, { tags });
      tagsDraft = (updated.tags ?? []).join(", ");
      tagsNotice = "Saved.";
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to save the topic's tags.";
    } finally {
      isMutating = false;
    }
  }

  async function loadTopicSettings(): Promise<void> {
    if (!topic) return;
    isLoading = true;
    try {
      const [workspaceList, sessionList] = await Promise.all([
        listTopicWorkspaces(topic.id),
        listTopicSessions(topic.id),
      ]);
      workspaces = workspaceList;
      worktopicsLoaded = true;
      sessions = sessionList;
      relinkDrafts = Object.fromEntries(
        workspaceList.map((workspace) => [workspace.id, relinkDrafts[workspace.id] ?? ""]),
      );
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load topic settings.";
    } finally {
      isLoading = false;
    }
  }

  async function saveAbout(): Promise<void> {
    if (!topic) return;
    isMutating = true;
    aboutNotice = null;
    try {
      const updated = await updateTopic(topic.id, { description: aboutDraft.trim() });
      aboutDraft = updated.description ?? "";
      aboutNotice = "Saved.";
      await onTopicsChanged?.();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to save the topic's description.";
    } finally {
      isMutating = false;
    }
  }

  async function removeTopic(): Promise<void> {
    if (!topic) return;
    isMutating = true;
    try {
      await deleteTopic(topic.id);
      await onTopicsChanged?.();
      onNavigate?.("/settings/providers");
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to delete the topic.";
      isMutating = false;
    }
  }

  function setRelinkDraft(workspaceId: string, value: string): void {
    relinkDrafts = { ...relinkDrafts, [workspaceId]: value };
  }

  // Keep the attachment's identity and settings when its folder moves.
  async function relinkWorkspace(workspace: WorkspaceAttachment): Promise<void> {
    if (!topic) return;
    const replacementPath = relinkDrafts[workspace.id]?.trim();
    if (!replacementPath) return;
    relinkingWorkspaceId = workspace.id;
    isMutating = true;
    error = null;
    try {
      await relinkTopicWorkspace(topic.id, workspace.id, replacementPath);
      setRelinkDraft(workspace.id, "");
      await loadTopicSettings();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to relink workspace.";
    } finally {
      relinkingWorkspaceId = null;
      isMutating = false;
    }
  }

  async function attachWorkspace(): Promise<void> {
    if (!topic || !worktopicsLoaded || !workspacePath.trim()) return;
    isMutating = true;
    try {
      await attachTopicWorkspace(topic.id, {
        path: workspacePath.trim(),
        label: workspaceLabel.trim() || undefined,
        permissions: workspaceCanWrite ? ["read", "write"] : ["read"],
        is_trusted: workspaceIsTrusted,
        set_as_default: workspaces.length === 0,
      });
      workspacePath = "";
      workspaceLabel = "";
      workspaceCanWrite = true;
      workspaceIsTrusted = false;
      await loadTopicSettings();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to attach workspace.";
    } finally {
      isMutating = false;
    }
  }

  async function setWorkspaceTrust(workspace: WorkspaceAttachment, isTrusted: boolean): Promise<void> {
    if (!topic) return;
    isMutating = true;
    try {
      await updateTopicWorkspaceTrust(topic.id, workspace.id, { is_trusted: isTrusted });
      await loadTopicSettings();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to update workspace trust.";
    } finally {
      isMutating = false;
    }
  }

  async function detachWorkspace(workspace: WorkspaceAttachment): Promise<void> {
    if (!topic) return;
    isMutating = true;
    try {
      await detachTopicWorkspace(topic.id, workspace.id);
      await loadTopicSettings();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to detach workspace.";
    } finally {
      isMutating = false;
    }
  }
</script>

<div class="topic-settings-pane" aria-label="Topic settings">
  {#if !topic}
    <section class="shell-section">
      <h1 class="title">Topic not found</h1>
      <p class="shell-empty">The requested topic settings route does not match a known topic.</p>
    </section>
  {:else}
    <div class="head">
      <div class="head-text">
        <p class="eyebrow">Topic settings</p>
        <h1 class="title">{topic.name}</h1>
        <p class="sub">Topic-scoped configuration and attached workspace state for {topic.slug}.</p>
      </div>
      {#if onNavigate}
        <nav class="topic-tabs" aria-label="Topic settings sections">
          {#each tabs as entry (entry.id)}
            <button type="button" class:active={tab === entry.id} aria-pressed={tab === entry.id} onclick={() => openTab(entry.id)}>{entry.label}</button>
          {/each}
        </nav>
      {/if}
    </div>

    {#if error}
      <div class="notice error" role="alert">{error}</div>
    {:else if isLoading}
      <div class="notice">Loading topic settings…</div>
    {/if}

    {#if tab === "overview"}
      <section class="shell-section">
        <h2 class="shell-section-title">Overview</h2>
        <div class="shell-grid">
          <div class="shell-row">
            <span class="shell-label">Slug</span>
            <span class="shell-value">{topic.slug}</span>
          </div>
          <div class="shell-row">
            <span class="shell-label">Workspaces</span>
            <span class="shell-value">{workspaces.length}</span>
          </div>
          <div class="shell-row">
            <span class="shell-label">Sessions</span>
            <span class="shell-value">{sessions.length}</span>
          </div>
        </div>
      </section>
      <section class="shell-section">
        <h2 class="shell-section-title">About</h2>
        <p class="workspace-note">What this topic is for. Sessions in the topic see it as orientation.</p>
        <form
          class="about-form"
          onsubmit={(event) => {
            event.preventDefault();
            void saveAbout();
          }}
        >
          <textarea aria-label="Topic about text" rows="3" placeholder="What is this topic for?" bind:value={aboutDraft}></textarea>
          <div class="workspace-actions">
            <button type="submit" disabled={isMutating || aboutDraft.trim() === (topic.description ?? "")}>Save about</button>
            {#if aboutNotice}<span class="shell-value">{aboutNotice}</span>{/if}
          </div>
        </form>
      </section>
      <section class="shell-section">
        <h2 class="shell-section-title">Delete</h2>
        <p class="workspace-note">Deleting the topic removes its sessions and their surfaces. Library entries saved from it remain.</p>
        {#if pendingDelete}
          <div class="workspace-actions">
            <button type="button" class="danger" disabled={isMutating} onclick={() => void removeTopic()}>Delete {topic.name}</button>
            <button type="button" onclick={() => (pendingDelete = false)}>Keep</button>
          </div>
        {:else}
          <button type="button" class="danger" onclick={() => (pendingDelete = true)}>Delete topic…</button>
        {/if}
      </section>
    {:else if tab === "workspaces"}
      <section class="shell-section">
        <h2 class="shell-section-title">Workspaces</h2>
        <p class="workspace-note">Trusted workspace content can supply instructions to agents working in this topic.</p>
        <form class="workspace-form" onsubmit={(event) => { event.preventDefault(); void attachWorkspace(); }}>
          <label>
            Repository folder
            <input bind:value={workspacePath} required placeholder="/path/to/repository" disabled={isMutating} />
          </label>
          <label>
            Label <span>(optional)</span>
            <input bind:value={workspaceLabel} placeholder="Repository name" disabled={isMutating} />
          </label>
          <WorkspaceAccessOptions bind:canWrite={workspaceCanWrite} bind:isTrusted={workspaceIsTrusted} disabled={isMutating} />
          <button type="submit" disabled={isMutating || !worktopicsLoaded || !workspacePath.trim()}>
            {isMutating ? "Saving…" : "Attach workspace"}
          </button>
        </form>
        <div class="record-list">
          {#each workspaces as workspace (workspace.id)}
            <article class="record-row">
              <strong>{workspace.label}</strong>
              <span>{workspace.path}</span>
              <span>{workspace.is_trusted ? "trusted" : "untrusted"} · {workspace.permissions.join(", ")}{workspace.is_default ? " · working folder" : ""}</span>
              {#if workspace.is_missing}
                <div class="missing-workspace">
                  <span class="missing-label">Missing folder</span>
                  <div class="relink-row">
                    <input
                      type="text"
                      value={relinkDrafts[workspace.id] ?? ""}
                      placeholder="/new/path/to/folder"
                      aria-label={`Replacement path for ${workspace.label}`}
                      oninput={(event) => setRelinkDraft(workspace.id, event.currentTarget.value)}
                    />
                    <button
                      type="button"
                      onclick={() => void relinkWorkspace(workspace)}
                      disabled={relinkingWorkspaceId === workspace.id || !(relinkDrafts[workspace.id] ?? "").trim()}
                    >
                      {relinkingWorkspaceId === workspace.id ? "Relinking…" : "Relink"}
                    </button>
                  </div>
                </div>
              {/if}
              <div class="workspace-actions">
                {#if workspace.is_trusted}
                  <button type="button" onclick={() => void setWorkspaceTrust(workspace, false)} disabled={isMutating}>Revoke trust</button>
                {:else}
                  <button type="button" onclick={() => void setWorkspaceTrust(workspace, true)} disabled={isMutating}>Trust workspace</button>
                {/if}
                <button type="button" class="danger" onclick={() => void detachWorkspace(workspace)} disabled={isMutating}>Detach</button>
              </div>
            </article>
          {:else}
            <p class="shell-empty">No workspace folders are attached to this topic.</p>
          {/each}
        </div>
      </section>
    {:else if tab === "surfaces"}
      <section class="shell-section">
        <h2 class="shell-section-title">Default surfaces</h2>
        <p class="workspace-note">Library surfaces that open in each of this topic's new sessions. Any other library surface can still be added on demand.</p>
        <div class="record-list" aria-label="Default surfaces">
          {#each defaultSurfaces as entry (entry.id)}
            <article class="record-row">
              <strong>{entry.name}</strong>
              <time datetime={entry.updated_at}>Saved {entry.updated_at}</time>
              <span>From {entry.origin_topic_deleted ? "a deleted topic" : entry.origin_topic_id ? (entry.origin_topic_id === topic.id ? topic.name : entry.origin_topic_id) : "General"}</span>
              <span>{entry.description}</span>
              <button type="button" disabled={isMutating} onclick={() => void withdrawDefault(entry.id)}>Withdraw</button>
            </article>
          {:else}
            <p class="shell-empty">{surfacesLoaded ? "No default surfaces are designated." : "Loading…"}</p>
          {/each}
        </div>
      </section>
      <section class="shell-section">
        <h2 class="shell-section-title">Library</h2>
        <div class="record-list" aria-label="Surface library">
          {#each library.filter((entry) => !defaultSurfaces.some((d) => d.id === entry.id)) as entry (entry.id)}
            <article class="record-row">
              <strong>{entry.name}</strong>
              <time datetime={entry.updated_at}>Saved {entry.updated_at}</time>
              <span>From {entry.origin_topic_deleted ? "a deleted topic" : entry.origin_topic_id ? (entry.origin_topic_id === topic.id ? topic.name : entry.origin_topic_id) : "General"}</span>
              <span>{entry.description}</span>
              <span>{entry.tags.join(", ")}</span>
              <button type="button" disabled={isMutating} onclick={() => void designateDefault(entry.id)}>Make default</button>
            </article>
          {:else}
            <p class="shell-empty">{surfacesLoaded ? "Every library surface is already a default, or the library is empty." : "Loading…"}</p>
          {/each}
        </div>
      </section>
      <section class="shell-section">
        <h2 class="shell-section-title">Tags</h2>
        <p class="workspace-note">Labels that narrow the library catalogue an agent in this topic sees first. They grant nothing and add nothing on their own.</p>
        <form
          class="tags-form"
          onsubmit={(event) => {
            event.preventDefault();
            void saveTags();
          }}
        >
          <input aria-label="Topic tags" placeholder="Tags, comma separated" bind:value={tagsDraft} />
          <button type="submit" disabled={isMutating}>Save tags</button>
          {#if tagsNotice}<span class="shell-value">{tagsNotice}</span>{/if}
        </form>
      </section>
    {:else if tab === "sessions"}
      <section class="shell-section">
        <h2 class="shell-section-title">Sessions</h2>
        <div class="record-list">
          {#each sessions as session (session.id)}
            <article class="record-row">
              <strong>{session.message_preview || "Untitled session"}</strong>
              <span>{session.agent_id} · {new Date(session.last_activity).toLocaleString()}</span>
              {#if onNavigate}
                <button type="button" onclick={() => onNavigate?.(`/conversations/${encodeURIComponent(session.id)}`)}>Open</button>
              {/if}
            </article>
          {:else}
            <p class="shell-empty">No sessions have been created in this topic.</p>
          {/each}
        </div>
      </section>
    {/if}
  {/if}
</div>

<style>
  .topic-settings-pane {
    min-height: 100%;
    padding: 28px 32px;
    background: var(--paper);
    color: var(--ink);
    overflow: auto;
  }

  .head {
    display: flex;
    justify-content: space-between;
    gap: 20px;
    margin-bottom: 22px;
    padding-bottom: 18px;
    border-bottom: 2px solid var(--ink);
  }

  .eyebrow,
  .shell-label {
    margin: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .title {
    margin: 4px 0 6px;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: clamp(42px, 8vw, 76px);
    line-height: 0.9;
    letter-spacing: 0.02em;
    text-transform: uppercase;
  }

  .sub,
  .shell-empty,
  .shell-value,
  .record-row span,
  .workspace-note {
    margin: 0;
    color: var(--ink-soft);
    font-size: 13px;
    line-height: 1.55;
  }

  .shell-section {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 16px 18px;
    border: 1px solid var(--rule-soft);
    background: var(--paper);
  }

  .shell-section-title {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 18px;
    line-height: 1.05;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .shell-grid,
  .record-list {
    display: flex;
    flex-direction: column;
  }

  .shell-row {
    display: grid;
    grid-template-columns: minmax(180px, 0.4fr) 1fr;
    gap: 16px;
    padding: 10px 0;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .shell-row:last-child {
    border-bottom: none;
  }

  .tags-form {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }

  .shell-value {
    color: var(--ink);
  }

  .record-row {
    display: grid;
    gap: 4px;
    padding: 12px 0;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .record-row:last-child {
    border-bottom: none;
  }

  .record-row strong {
    color: var(--ink);
    font-size: 14px;
  }

  .workspace-note { max-width: 64ch; }
  .topic-tabs { display: flex; gap: 6px; flex-wrap: wrap; align-self: flex-start; }
  .topic-tabs button {
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    padding: 4px 10px;
    font-family: var(--font-mono);
    font-size: var(--font-label-sm);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    cursor: pointer;
  }
  .topic-tabs button.active { background: var(--ink); color: var(--paper); }
  .about-form { display: flex; flex-direction: column; gap: 8px; max-width: 64ch; }
  .about-form textarea { width: 100%; border: 1px solid var(--rule-soft); background: var(--paper); color: var(--ink); font: inherit; padding: 6px 8px; }
  .missing-workspace { display: flex; flex-direction: column; gap: 6px; margin-top: 4px; }
  .missing-label { color: var(--error); font-family: var(--font-mono); font-size: var(--font-label-sm); letter-spacing: 0.08em; text-transform: uppercase; }
  .relink-row { display: flex; gap: 8px; flex-wrap: wrap; }
  .relink-row input { flex: 1; min-width: 14rem; border: 1px solid var(--rule-soft); background: var(--paper); color: var(--ink); font: inherit; padding: 4px 8px; }

  .workspace-form { display: grid; grid-template-columns: minmax(14rem, 1fr) minmax(10rem, .6fr) auto; gap: 10px; align-items: end; padding: 12px 0; border-bottom: 1px dotted var(--rule-soft); }
  .workspace-form label { display: grid; gap: 5px; font-family: var(--font-mono); font-size: 10px; letter-spacing: .08em; text-transform: uppercase; }
  .workspace-form label span { color: var(--ink-soft); }
  input { min-width: 0; padding: 9px 10px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); font: inherit; letter-spacing: normal; text-transform: none; }
  button { padding: 9px 11px; border: 1px solid var(--ink); background: var(--ink); color: var(--paper); font: inherit; cursor: pointer; }
  button:disabled { cursor: wait; opacity: .6; }
  .workspace-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 4px; }
  .workspace-actions button { background: var(--paper); color: var(--ink); }
  button.danger { background: var(--paper); border-color: var(--error); color: var(--error); }
  button.danger:hover:not(:disabled) { background: var(--error); color: var(--paper); }
  @media (max-width: 760px) { .workspace-form { grid-template-columns: 1fr; } }

  .notice {
    margin-bottom: 14px;
    padding: 10px 12px;
    border: 1px solid var(--ink);
    color: var(--ink);
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .notice.error {
    border-color: var(--accent);
    color: var(--accent);
  }
</style>
