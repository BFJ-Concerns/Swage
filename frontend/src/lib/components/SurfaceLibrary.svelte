<!--
  The shared surface library — the catalogue of surfaces saved for reuse.

  Each entry carries the name, description and tags written when it was
  saved; the catalogue never carries content. From a session, an entry is
  added as a new surface of that session (with its saved content inside the
  entry's origin topic, the bare shape elsewhere). From the front door the
  catalogue is read-only: an entry needs a session to be added to. Entries
  can be removed here; removal leaves every surface seeded from the entry
  untouched.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { deleteLibraryEntry } from "../api/surfaces";
  import type { SurfacesStore } from "../stores/surfaces";
  import type { Topic } from "../types";

  interface Props {
    store: SurfacesStore;
    // The session an entry is added to; null on the front door.
    sessionId: string | null;
    // Names the scope each entry was saved from.
    topics?: Topic[];
    onClose?: () => void;
    // The entry was added as a surface of the session: show it.
    onSurfaceAdded?: (surfaceId: string) => void;
  }

  let { store, sessionId, topics = [], onClose, onSurfaceAdded }: Props = $props();

  function originName(topicId: string | undefined): string {
    if (!topicId) return "General";
    return topics.find((topic) => topic.id === topicId)?.name ?? "Another topic";
  }

  let filter = $state("");
  let busyEntryId = $state<string | null>(null);
  let pendingRemove = $state<string | null>(null);
  let error = $state<string | null>(null);

  onMount(() => {
    void store.refreshLibrary();
  });

  const entries = $derived.by(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return store.library;
    return store.library.filter(
      (entry) =>
        entry.name.toLowerCase().includes(needle) ||
        entry.description.toLowerCase().includes(needle) ||
        entry.tags.some((tag) => tag.includes(needle)),
    );
  });

  async function addToSession(entryId: string, name: string): Promise<void> {
    if (!sessionId) return;
    busyEntryId = entryId;
    error = null;
    try {
      const surface = await store.createSurface(sessionId, name, entryId);
      if (surface) onSurfaceAdded?.(surface.id);
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to add the surface.";
    } finally {
      busyEntryId = null;
    }
  }

  async function remove(entryId: string): Promise<void> {
    busyEntryId = entryId;
    error = null;
    try {
      await deleteLibraryEntry(entryId);
      await store.refreshLibrary();
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to remove the entry.";
    } finally {
      busyEntryId = null;
      pendingRemove = null;
    }
  }
</script>

<section class="surface-library" aria-label="Surface library">
  <header class="head">
    <div>
      <p class="eyebrow">Shared library</p>
      <h1 class="title">Surfaces to reuse</h1>
      <p class="lede">
        Saved surfaces, described by what they do and when to use them. Adding one gives this session its own copy; the entry stays as it was.
      </p>
    </div>
    {#if onClose}
      <button type="button" class="close" onclick={onClose} aria-label="Close library">×</button>
    {/if}
  </header>

  <div class="tools">
    <input
      type="search"
      aria-label="Filter the library"
      placeholder="Filter by name, description or tag"
      bind:value={filter}
    />
    {#if !sessionId}
      <p class="note">Open or start a session to add a surface from here.</p>
    {/if}
  </div>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  {#if store.library.length === 0}
    <p class="empty">Nothing is saved yet. Save a surface from a session to see it here.</p>
  {:else if entries.length === 0}
    <p class="empty">No entry matches the filter.</p>
  {:else}
    <ul class="entries">
      {#each entries as entry (entry.id)}
        <li class="entry">
          <div class="entry-text">
            <strong class="entry-name">{entry.name}</strong>
            <time datetime={entry.updated_at}>Saved {entry.updated_at}</time>
            <span class="entry-description">{entry.description}</span>
            {#if entry.tags.length > 0}
              <span class="entry-tags">
                {#each entry.tags as tag (tag)}
                  <span class="tag">{tag}</span>
                {/each}
              </span>
            {/if}
            <span class="entry-origin">Saved from {entry.origin_topic_deleted ? "a deleted topic" : originName(entry.origin_topic_id ?? undefined)}</span>
          </div>
          <div class="entry-actions">
            {#if sessionId}
              <button
                type="button"
                disabled={busyEntryId === entry.id}
                onclick={() => void addToSession(entry.id, entry.name)}
              >
                Add to session
              </button>
            {/if}
            {#if pendingRemove === entry.id}
              <button type="button" class="danger" disabled={busyEntryId === entry.id} onclick={() => void remove(entry.id)}>Remove</button>
              <button type="button" onclick={() => (pendingRemove = null)}>Keep</button>
            {:else}
              <button type="button" class="quiet" onclick={() => (pendingRemove = entry.id)}>Remove…</button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .surface-library {
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 32px 40px;
    color: var(--ink);
    font-family: var(--font-body);
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
  }

  .eyebrow {
    margin: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-label-sm);
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .title {
    margin: 6px 0 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: var(--font-display-2);
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .lede {
    margin: 8px 0 0;
    color: var(--ink-soft);
    font-size: var(--font-body-sm);
    line-height: 1.55;
    max-width: 60ch;
  }

  .close {
    background: none;
    border: 1px solid var(--ink);
    color: var(--ink);
    width: 28px;
    height: 28px;
    font-size: var(--font-body-lg);
    line-height: 1;
    cursor: pointer;
    flex-shrink: 0;
  }

  .tools {
    display: flex;
    align-items: center;
    gap: 14px;
    flex-wrap: wrap;
  }

  .tools input {
    min-width: 18rem;
    border: 1px solid var(--rule-soft);
    background: var(--paper);
    color: var(--ink);
    padding: 5px 8px;
    font: inherit;
  }

  .note,
  .empty {
    margin: 0;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: var(--font-label-sm);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .error {
    margin: 0;
    color: var(--error);
  }

  .entries {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }

  .entry {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 16px;
    align-items: start;
    padding: 12px 0;
    border-bottom: 1px dotted var(--rule-soft);
  }

  .entry-text {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .entry-name {
    font-size: var(--font-body-md);
  }

  .entry-description {
    font-size: var(--font-body-sm);
    line-height: 1.5;
  }

  .entry-tags {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  .tag,
  .entry-origin {
    font-family: var(--font-mono);
    font-size: var(--font-meta);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .tag {
    border: 1px solid var(--rule-soft);
    padding: 1px 6px;
  }

  .entry-actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    justify-content: flex-end;
  }

  .entry-actions button {
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

  .entry-actions button:hover:not(:disabled) {
    background: var(--ink);
    color: var(--paper);
  }

  .entry-actions button.quiet {
    border-color: var(--rule-soft);
    color: var(--ink-soft);
  }

  .entry-actions button.danger {
    border-color: var(--error);
    color: var(--error);
  }

  .entry-actions button.danger:hover:not(:disabled) {
    background: var(--error);
    color: var(--paper);
  }

  @media (max-width: 720px) {
    .surface-library { padding: 16px; }
    .entry { grid-template-columns: 1fr; }
    .entry-actions { justify-content: flex-start; }
    .tools input { min-width: 0; width: 100%; }
  }
</style>
