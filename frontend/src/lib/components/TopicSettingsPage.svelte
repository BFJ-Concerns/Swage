<!-- Topic-scoped settings pane. Global provider/runtime settings stay in
     SettingsPage; this component only reads topic-owned data. -->
<script lang="ts">
  import { onMount } from "svelte";
  import WorkspaceAccessOptions from "./WorkspaceAccessOptions.svelte";
  import {
    attachTopicWorkspace,
    detachTopicWorkspace,
    listTopicSessions,
    listTopicWorkspaces,
    updateTopicWorkspaceTrust,
  } from "../api/topics";
  import type { TopicSettingsTab } from "../router";
  import type { Topic, ConversationSummary, WorkspaceAttachment } from "../types";

  interface Props {
    topic: Topic | null;
    tab?: TopicSettingsTab;
  }

  let { topic, tab = "overview" }: Props = $props();

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

  onMount(loadTopicSettings);

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
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load topic settings.";
    } finally {
      isLoading = false;
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
            <span class="shell-label">Default topic</span>
            <span class="shell-value">{topic.is_default ? "Yes" : "No"}</span>
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
    {:else if tab === "sessions"}
      <section class="shell-section">
        <h2 class="shell-section-title">Sessions</h2>
        <div class="record-list">
          {#each sessions as session (session.id)}
            <article class="record-row">
              <strong>{session.message_preview || "Untitled session"}</strong>
              <span>{session.id}</span>
              <span>{new Date(session.last_activity).toLocaleString()}</span>
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

  .workspace-form { display: grid; grid-template-columns: minmax(14rem, 1fr) minmax(10rem, .6fr) auto; gap: 10px; align-items: end; padding: 12px 0; border-bottom: 1px dotted var(--rule-soft); }
  .workspace-form label { display: grid; gap: 5px; font-family: var(--font-mono); font-size: 10px; letter-spacing: .08em; text-transform: uppercase; }
  .workspace-form label span { color: var(--ink-soft); }
  input { min-width: 0; padding: 9px 10px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); font: inherit; letter-spacing: normal; text-transform: none; }
  button { padding: 9px 11px; border: 1px solid var(--ink); background: var(--ink); color: var(--paper); font: inherit; cursor: pointer; }
  button:disabled { cursor: wait; opacity: .6; }
  .workspace-actions { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 4px; }
  .workspace-actions button { background: var(--paper); color: var(--ink); }
  .workspace-actions .danger { border-color: var(--accent); color: var(--accent); }
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
