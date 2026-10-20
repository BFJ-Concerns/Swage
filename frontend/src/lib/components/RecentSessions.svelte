<!--
  The recent-sessions list — the front door when nothing is open.

  Every scope's sessions, newest user-or-assistant message first, with
  automated sessions (schedule runs, workflow workers, delegated children)
  nested beneath the conversation or schedule that caused them. Each group
  collapses behind a disclosure showing its count and latest run's state;
  schedule groups start collapsed, and the choice is remembered per group.
  "All sessions"
  is the default; the topic filter narrows membership and nothing else — it
  never opens a topic or touches a session's working context. The archived
  toggle lists what the inactivity sweep has hidden, so an old session is
  found and resumed from here, never by an identifier.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { listRecentSessions } from "../api/sessions";
  import { sessionOutcomeLabel } from "../session-outcome";
  import { messagePreview } from "../message-preview";
  import { relativeTime } from "../utils/relative-time";
  import type { RecentSession } from "../protocol/RecentSession";
  import type { RecentSessionsEntry } from "../protocol/RecentSessionsEntry";
  import type { Topic } from "../types";

  const GROUP_EXPANDED_KEY = "swage:recent-session-groups";
  const LABEL_LIMIT = 60;

  interface Props {
    topics?: Topic[];
    // Resume a session: the parent brings its topic into scope and connects.
    onSelectSession?: (id: string) => void;
    // Open the record of a workflow run that was started outside any session.
    onOpenWorkflowRun?: (id: string) => void;
  }

  let { topics = [], onSelectSession, onOpenWorkflowRun }: Props = $props();

  let topicFilter = $state<string | null>(null);
  let topicSearch = $state("");
  const matchingTopics = $derived(topics.filter(topic =>
    topic.id === topicFilter || topic.name.toLocaleLowerCase().includes(topicSearch.trim().toLocaleLowerCase()),
  ));
  let showArchived = $state(false);
  let entries = $state<RecentSessionsEntry[]>([]);
  let loadState = $state<"loading" | "loaded" | "error">("loading");
  let errorMessage = $state("");
  // Per-group disclosure choices, keyed by entryKey; absent means the default.
  let groupExpanded = $state<Record<string, boolean>>(readGroupExpanded());
  // A slow response for an earlier filter must not overwrite a later one.
  let loadGeneration = 0;

  async function load(): Promise<void> {
    const generation = ++loadGeneration;
    loadState = "loading";
    try {
      const loaded = await listRecentSessions(topicFilter, showArchived);
      if (generation !== loadGeneration) return;
      entries = loaded;
      errorMessage = "";
      loadState = "loaded";
    } catch (error) {
      if (generation !== loadGeneration) return;
      errorMessage = error instanceof Error ? error.message : String(error);
      loadState = "error";
    }
  }

  onMount(() => {
    void load();
  });

  function setFilter(topicId: string | null): void {
    topicFilter = topicId;
    void load();
  }

  function toggleArchived(): void {
    showArchived = !showArchived;
    void load();
  }

  function sourceSession(entry: RecentSessionsEntry): RecentSession | null {
    return entry.source.kind === "conversation" ? entry.source.session : null;
  }

  function entryKey(entry: RecentSessionsEntry): string {
    return entry.source.kind === "conversation"
      ? `conversation:${entry.source.session.id}`
      : `${entry.source.kind}:${entry.source.id}`;
  }

  function readGroupExpanded(): Record<string, boolean> {
    try {
      const stored: unknown = JSON.parse(localStorage.getItem(GROUP_EXPANDED_KEY) ?? "{}");
      if (stored && typeof stored === "object" && !Array.isArray(stored)) {
        return Object.fromEntries(
          Object.entries(stored).filter((pair): pair is [string, boolean] => typeof pair[1] === "boolean"),
        );
      }
    } catch {
      // Unreadable storage falls back to the defaults.
    }
    return {};
  }

  function isExpanded(entry: RecentSessionsEntry): boolean {
    if (entry.automated.length === 0) return false;
    return groupExpanded[entryKey(entry)] ?? entry.source.kind !== "schedule";
  }

  function toggleGroup(entry: RecentSessionsEntry): void {
    groupExpanded = { ...groupExpanded, [entryKey(entry)]: !isExpanded(entry) };
    try {
      localStorage.setItem(GROUP_EXPANDED_KEY, JSON.stringify(groupExpanded));
    } catch {
      // The choice still holds for this visit when storage is unavailable.
    }
  }

  function groupCount(entry: RecentSessionsEntry): string {
    const count = entry.automated.length;
    const noun = entry.source.kind === "schedule" ? "run" : "automated session";
    return `${count} ${noun}${count === 1 ? "" : "s"}`;
  }

  function latestOutcome(entry: RecentSessionsEntry): string {
    const latest = entry.automated[0];
    if (latest.has_running_turn) return "Running";
    const outcome = sessionOutcomeLabel(latest.latest_turn_state);
    if (outcome) return outcome;
    if (latest.latest_turn_state === "completed") return "Completed";
    return `Last run ended ${relativeTime(latest.last_activity)}`;
  }

  function hasInterruptionHistory(entry: RecentSessionsEntry): boolean {
    return entry.automated.some((session, index) => session.has_interrupted_turn && (index > 0 || session.latest_turn_state !== "interrupted"));
  }

  // A schedule's brief can run to paragraphs and paths; the heading takes its
  // first line, cut at a word boundary.
  function conciseLabel(text: string): string {
    const firstLine = text.trim().split("\n", 1)[0].trim();
    if (firstLine.length <= LABEL_LIMIT) return firstLine;
    const cut = firstLine.slice(0, LABEL_LIMIT);
    const boundary = cut.lastIndexOf(" ");
    return `${(boundary > 0 ? cut.slice(0, boundary) : cut).trimEnd()}…`;
  }

  function sourceLabel(entry: RecentSessionsEntry): string {
    switch (entry.source.kind) {
      case "conversation":
        return messagePreview(entry.source.session.message_preview ?? "New conversation");
      case "schedule":
        return conciseLabel(entry.source.message);
      case "workflow_run":
        return entry.source.label ?? "Workflow run";
    }
  }

  function originLabel(session: RecentSession): string {
    switch (session.origin.kind) {
      case "schedule_run":
        return "scheduled run";
      case "workflow_worker":
        return "workflow worker";
      case "delegation":
        return "delegated";
      case "memory_curation":
        return "memory curation";
      case "direct":
        return "conversation";
    }
  }

  function openSource(entry: RecentSessionsEntry): void {
    if (entry.source.kind === "conversation") {
      onSelectSession?.(entry.source.session.id);
    } else if (entry.source.kind === "workflow_run") {
      onOpenWorkflowRun?.(entry.source.id);
    }
  }
</script>

<section class="recent-sessions" aria-label={showArchived ? "Archived sessions" : "Recent sessions"}>
  <header class="recent-head">
    <p class="eyebrow">{showArchived ? "Archived sessions" : "Recent sessions"}</p>
    <input class="topic-search" type="search" aria-label="Search topic filters" placeholder="Search topics…" bind:value={topicSearch} />
    <div class="filters" role="group" aria-label="Topic filter">
      <button
        type="button"
        class="filter"
        class:active={topicFilter === null}
        aria-pressed={topicFilter === null}
        onclick={() => setFilter(null)}
      >
        All sessions
      </button>
      {#each matchingTopics as topic (topic.id)}
        <button
          type="button"
          class="filter"
          class:active={topicFilter === topic.id}
          aria-pressed={topicFilter === topic.id}
          onclick={() => setFilter(topic.id)}
        >
          {topic.name}
        </button>
      {/each}
    </div>
    <button type="button" class="archived-toggle" aria-pressed={showArchived} onclick={toggleArchived}>
      {showArchived ? "Back to recent" : "Archived"}
    </button>
  </header>

  {#if loadState === "loading"}
    <p class="state">Loading sessions…</p>
  {:else if loadState === "error"}
    <p class="state error" role="alert">Could not load sessions: {errorMessage}</p>
  {:else if entries.length === 0}
    <p class="state">{showArchived ? "Nothing is archived." : "No sessions yet."}</p>
  {:else}
    <ul class="entries">
      {#each entries as entry (entryKey(entry))}
        {@const session = sourceSession(entry)}
        {@const outcome = sessionOutcomeLabel(session?.latest_turn_state)}
        <li class="entry" class:archived={session?.archived_at != null}>
          <button
            type="button"
            class="row source"
            class:schedule={entry.source.kind === "schedule"}
            title={entry.source.kind === "schedule" ? entry.source.message : undefined}
            aria-expanded={entry.source.kind === "schedule" ? isExpanded(entry) : undefined}
            disabled={entry.source.kind === "schedule" && entry.automated.length === 0}
            onclick={() => (entry.source.kind === "schedule" ? toggleGroup(entry) : openSource(entry))}
          >
            <span class="session-text">
              <span class="preview">{sourceLabel(entry)}</span>
              <span class="meta">
                {#if session}
                  <span class="topic-label" title={session.topic_name}>{session.topic_name}</span>
                  {#if session.has_running_turn}<span class="running">running</span>{/if}
                  {#if outcome}<span class="interrupted" title={outcome} aria-label={outcome}>⚠</span>{/if}
                {:else if entry.source.kind === "schedule"}
                  schedule
                {:else}
                  workflow run
                {/if}
              </span>
              <span class="timestamp">{relativeTime(entry.last_activity)}</span>
            </span>
          </button>

          {#if entry.automated.length > 0}
            <button
              type="button"
              class="group-summary"
              aria-expanded={isExpanded(entry)}
              onclick={() => toggleGroup(entry)}
            >
              <span class="disclosure" aria-hidden="true">{isExpanded(entry) ? "▾" : "▸"}</span>
              {groupCount(entry)} · {latestOutcome(entry)}
              {#if hasInterruptionHistory(entry)}<span>· Previous response interrupted</span>{/if}
            </button>
          {/if}

          {#if entry.automated.length > 0 && isExpanded(entry)}
            <ul class="automated" aria-label="Automated sessions">
              {#each entry.automated as automated (automated.id)}
                {@const outcome = sessionOutcomeLabel(automated.latest_turn_state)}
                <li>
                  <button type="button" class="row nested" onclick={() => onSelectSession?.(automated.id)}>
                    <span class="session-text">
                      <span class="preview">{messagePreview(automated.message_preview ?? originLabel(automated))}</span>
                      <span class="meta">
                        {originLabel(automated)} · {automated.agent_id}
                        {#if outcome}<span class="interrupted" title={outcome} aria-label={outcome}>⚠</span>{/if}
                        {#if automated.has_running_turn}<span class="running">running</span>{/if}
                      </span>
                      <span class="timestamp">{relativeTime(automated.last_activity)}</span>
                    </span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .topic-search {
    min-width: 0;
    background: var(--paper);
    color: var(--ink);
    border: 1px solid var(--rule-soft);
    font: inherit;
    padding: 6px 8px;
  }

  .recent-sessions {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 32px 40px 0;
    color: var(--ink);
    font-family: var(--font-body);
  }

  .recent-head {
    display: flex;
    align-items: baseline;
    gap: 14px;
    flex-wrap: wrap;
  }

  .eyebrow {
    margin: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .filters {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  .filter,
  .archived-toggle {
    border: 1px solid var(--ink);
    background: var(--paper);
    color: var(--ink);
    padding: 4px 10px;
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .filter.active,
  .archived-toggle[aria-pressed="true"] {
    background: var(--ink);
    color: var(--paper);
  }

  .archived-toggle {
    margin-left: auto;
  }

  .state {
    margin: 0;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .state.error {
    color: var(--ink-orange);
  }

  .entries,
  .automated {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .entry {
    border-bottom: 1px dotted var(--rule-soft);
    padding: 4px 0;
  }

  .entry.archived .source .preview {
    color: var(--ink-soft);
  }

  .row {
    width: 100%;
    display: flex;
    gap: 12px;
    align-items: baseline;
    padding: 7px 0;
    background: none;
    border: none;
    text-align: left;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }

  .session-text {
    display: flex;
    align-items: baseline;
    gap: 12px;
    min-width: 0;
    max-width: 100%;
  }

  .meta {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }

  .topic-label {
    min-width: 0;
    max-width: 24ch;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .timestamp {
    flex-shrink: 0;
  }

  .row:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .group-summary {
    display: flex;
    gap: 6px;
    align-items: baseline;
    padding: 0 0 6px 22px;
    background: none;
    border: none;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.06em;
    color: var(--ink-soft);
    cursor: pointer;
  }

  .row.nested {
    padding-left: 22px;
    font-size: 12.5px;
  }

  .preview {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    min-width: 0;
    font-size: 13px;
  }

  .row.source .preview {
    font-weight: 600;
  }

  .meta,
  .timestamp {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.06em;
    color: var(--ink-soft);
    white-space: nowrap;
  }

  .running {
    margin-left: 6px;
    color: var(--ink-orange);
  }

  .interrupted {
    margin-left: 6px;
    color: var(--ink-orange);
  }
</style>
