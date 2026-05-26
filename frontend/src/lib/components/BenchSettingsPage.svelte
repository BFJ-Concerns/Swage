<!-- Bench-scoped settings pane. Global provider/runtime settings stay in
     SettingsPage; this component only reads bench-owned data. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { listBenchSessions, listBenchWorkspaces } from "../api/benches";
  import type { BenchSettingsTab } from "../router";
  import type { Bench, ConversationSummary, WorkspaceAttachment } from "../types";

  interface Props {
    bench: Bench | null;
    tab?: BenchSettingsTab;
  }

  let { bench, tab = "overview" }: Props = $props();

  let workspaces = $state<WorkspaceAttachment[]>([]);
  let sessions = $state<ConversationSummary[]>([]);
  let isLoading = $state(false);
  let error = $state<string | null>(null);

  onMount(loadBenchSettings);

  async function loadBenchSettings(): Promise<void> {
    if (!bench) return;
    isLoading = true;
    try {
      const [workspaceList, sessionList] = await Promise.all([
        listBenchWorkspaces(bench.id),
        listBenchSessions(bench.id),
      ]);
      workspaces = workspaceList;
      sessions = sessionList;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load bench settings.";
    } finally {
      isLoading = false;
    }
  }
</script>

<div class="bench-settings-pane" aria-label="Bench settings">
  {#if !bench}
    <section class="shell-section">
      <h1 class="title">Bench not found</h1>
      <p class="shell-empty">The requested bench settings route does not match a known bench.</p>
    </section>
  {:else}
    <div class="head">
      <div class="head-text">
        <p class="eyebrow">Bench settings</p>
        <h1 class="title">{bench.name}</h1>
        <p class="sub">Bench-scoped configuration and attached workspace state for {bench.slug}.</p>
      </div>
    </div>

    {#if error}
      <div class="notice error" role="alert">{error}</div>
    {:else if isLoading}
      <div class="notice">Loading bench settings…</div>
    {/if}

    {#if tab === "overview"}
      <section class="shell-section">
        <h2 class="shell-section-title">Overview</h2>
        <div class="shell-grid">
          <div class="shell-row">
            <span class="shell-label">Slug</span>
            <span class="shell-value">{bench.slug}</span>
          </div>
          <div class="shell-row">
            <span class="shell-label">Default bench</span>
            <span class="shell-value">{bench.is_default ? "Yes" : "No"}</span>
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
        <div class="record-list">
          {#each workspaces as workspace (workspace.id)}
            <article class="record-row">
              <strong>{workspace.label}</strong>
              <span>{workspace.path}</span>
              <span>{workspace.is_trusted ? "trusted" : "untrusted"} · {workspace.permissions.join(", ")}</span>
            </article>
          {:else}
            <p class="shell-empty">No workspace folders are attached to this bench.</p>
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
            <p class="shell-empty">No sessions have been created in this bench.</p>
          {/each}
        </div>
      </section>
    {/if}
  {/if}
</div>

<style>
  .bench-settings-pane {
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
  .record-row span {
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
