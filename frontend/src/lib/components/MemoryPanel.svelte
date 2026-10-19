<script lang="ts">
  import { getMemoryPage, instructCurator, listMemoryPages } from "../api/memory";
  import type { MemoryInstructionResponse } from "../protocol/MemoryInstructionResponse";
  import type { MemoryPageResponse } from "../protocol/MemoryPageResponse";
  import type { MemoryPageSummary } from "../protocol/MemoryPageSummary";
  import type { MemoryPageAddress } from "../router";
  import type { Topic } from "../types";

  interface Props {
    // Every topic whose memory is listed beside the general scope.
    topics: Topic[];
    // The page to open on load — the one a curator notice links to.
    page: MemoryPageAddress | null;
  }

  interface ScopeGroup {
    topicId: string | null;
    label: string;
    pages: MemoryPageSummary[];
    error: string | null;
  }

  let { topics, page }: Props = $props();
  let groups = $state<ScopeGroup[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let opened = $state<{ topicId: string | null; page: MemoryPageResponse } | null>(null);
  let instructionScope = $state("");
  let instruction = $state("");
  let sending = $state(false);
  let result = $state<MemoryInstructionResponse | null>(null);
  let loadGeneration = 0;
  let readGeneration = 0;
  let selected: { topicId: string | null; reference: string } | null = null;
  const scopes = $derived([
    { topicId: null, label: "General" },
    ...topics.map((topic) => ({ topicId: topic.id, label: topic.name })),
  ]);

  function message(err: unknown, fallback: string): string {
    return err instanceof Error ? err.message : fallback;
  }

  async function loadGroups(
    requested: Array<{ topicId: string | null; label: string }>,
    previous: ScopeGroup[] = [],
  ): Promise<ScopeGroup[]> {
    return Promise.all(requested.map(async (scope) => {
      try {
        return { ...scope, pages: await listMemoryPages(scope.topicId), error: null };
      } catch (err) {
        return {
          ...scope,
          pages: previous.find((group) => group.topicId === scope.topicId)?.pages ?? [],
          error: `${scope.label}: ${message(err, "Failed to load memory pages.")}`,
        };
      }
    }));
  }

  async function openPage(topicId: string | null, pageReference: string): Promise<void> {
    const generation = ++readGeneration;
    selected = { topicId, reference: pageReference };
    instructionScope = topicId ?? "";
    opened = null;
    error = null;
    try {
      const page = await getMemoryPage(pageReference, topicId);
      if (generation === readGeneration) opened = { topicId, page };
    } catch (err) {
      if (generation === readGeneration) error = message(err, "Failed to read the memory page.");
    }
  }

  $effect(() => {
    const requestedScopes = scopes;
    const requested = page;
    const generation = ++loadGeneration;
    const selectionGeneration = ++readGeneration;
    selected = null;
    loading = true;
    error = null;
    opened = null;
    void loadGroups(requestedScopes)
      .then((loaded) => {
        if (generation !== loadGeneration) return;
        groups = loaded;
        // A reference spells its scope kind but not which topic, so a link
        // names the scope; one naming none opens the first scope listing it.
        const holder = requested
          ? loaded.find(
              (group) =>
                (requested.scope === null || (group.topicId ?? "general") === requested.scope) &&
                group.pages.some((listed) => listed.reference === requested.reference),
            )
          : undefined;
        if (holder && requested && selectionGeneration === readGeneration) {
          void openPage(holder.topicId, requested.reference);
        }
      })
      .finally(() => {
        if (generation === loadGeneration) loading = false;
      });
    return () => {
      ++loadGeneration;
      ++readGeneration;
    };
  });

  async function send(): Promise<void> {
    const text = instruction.trim();
    if (!text || sending) return;
    const topicId = instructionScope || null;
    sending = true;
    error = null;
    result = null;
    let applied: MemoryInstructionResponse;
    try {
      applied = await instructCurator(topicId, text);
    } catch (err) {
      error = message(err, "The curator could not apply the instruction.");
      sending = false;
      return;
    }
    result = applied;
    instruction = "";
    const changed = applied.pages_changed.find((page) => {
      const changedTopicId = page.reference.startsWith("general/") ? null : topicId;
      return selected?.topicId === changedTopicId && selected.reference === page.reference;
    });
    if (changed && selected) {
      ++readGeneration;
      opened = { topicId: selected.topicId, page: changed };
    }
    const generation = ++loadGeneration;
    try {
      const refreshed = await loadGroups(scopes, groups);
      if (generation === loadGeneration) {
        groups = refreshed;
        loading = false;
      }
    } finally {
      sending = false;
    }
  }

  const empty = $derived(groups.every((group) => group.pages.length === 0 && !group.error));
</script>

<section class="memory" aria-label="Memory">
  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  <div class="pages">
    {#if loading}
      <p class="note">Loading memory…</p>
    {:else if empty}
      <p class="note">No memory pages yet. The curator writes them from your conversations.</p>
    {:else}
      {#each groups as group (group.topicId ?? "general")}
        {#if group.pages.length > 0 || group.error}
          <div class="scope" role="list" aria-label={`${group.label} memory pages`}>
            <p class="scope-label">{group.label}</p>
            {#if group.error}<p class="note" role="alert">{group.error}</p>{/if}
            {#each group.pages as page (page.reference)}
              {@const selected = opened?.topicId === group.topicId && opened?.page.reference === page.reference}
              <button
                type="button"
                class:selected
                aria-pressed={selected}
                onclick={() => void openPage(group.topicId, page.reference)}
              >
                <span class="reference">{page.reference}</span>
                {#if page.summary}<span class="summary">{page.summary}</span>{/if}
              </button>
            {/each}
          </div>
        {/if}
      {/each}
    {/if}
  </div>

  <div class="detail">
    {#if opened}
      <article class="page" aria-label={`Memory page ${opened.page.reference}`}>
        <header>
          <code>{opened.page.reference}</code>
          {#if opened.page.summary}<p class="summary">{opened.page.summary}</p>{/if}
        </header>
        <pre>{opened.page.body}</pre>
      </article>
    {/if}

    <form class="curator" onsubmit={(event) => { event.preventDefault(); void send(); }}>
      <p class="scope-label">Tell the curator what to change</p>
      <select aria-label="Memory to instruct about" bind:value={instructionScope}>
        <option value="">General memory</option>
        {#each topics as topic (topic.id)}
          <option value={topic.id}>{topic.name}</option>
        {/each}
      </select>
      <textarea
        aria-label="Instruction to the curator"
        placeholder="Forget that I take Fridays off"
        rows="3"
        bind:value={instruction}
      ></textarea>
      <button type="submit" disabled={sending || !instruction.trim()}>
        {sending ? "Curator working…" : "Send to curator"}
      </button>
    </form>

    {#if result}
      <div class="result">
        <p class="reply">{result.reply}</p>
        {#if result.pages_changed.length === 0}
          <p class="note">No page changed.</p>
        {:else}
          <div aria-label="Pages the curator changed">
            {#each result.pages_changed as page (page.reference)}
              <article class="page">
                <header><code>{page.reference}</code></header>
                <pre>{page.body}</pre>
              </article>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  </div>
</section>

<style>
  .memory { display: grid; grid-template-columns: minmax(14rem, 1fr) minmax(20rem, 2fr); gap: 18px; align-items: start; }
  .pages { display: flex; flex-direction: column; gap: 12px; }
  .scope { display: flex; flex-direction: column; border: 2px solid var(--ink); }
  .scope-label { margin: 0; padding: 8px 14px; font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; border-bottom: 1px solid var(--ink); }
  .scope button { display: flex; flex-direction: column; align-items: flex-start; gap: 4px; padding: 12px 14px; border: 0; border-bottom: 1px solid var(--ink); background: var(--paper); color: var(--ink); text-align: left; cursor: pointer; }
  .scope button:last-child { border-bottom: 0; }
  .scope button.selected { background: var(--ink); color: var(--paper); }
  .reference, code { font-family: var(--font-mono); font-size: 12px; overflow-wrap: anywhere; }
  .summary { margin: 0; font-size: 12px; opacity: .75; }
  .detail { display: flex; flex-direction: column; gap: 18px; min-width: 0; }
  .page { border: 2px solid var(--ink); padding: 18px; }
  .page header { border-bottom: 1px solid var(--ink); padding-bottom: 10px; }
  pre { margin: 12px 0 0; font-family: var(--font-mono); font-size: 12px; white-space: pre-wrap; overflow-wrap: anywhere; }
  .curator { display: flex; flex-direction: column; gap: 10px; border: 2px solid var(--ink); padding: 0 0 14px; }
  .curator select, .curator textarea, .curator button { margin: 0 14px; }
  .curator textarea { font: inherit; resize: vertical; }
  .curator button { align-self: flex-start; padding: 8px 14px; border: 2px solid var(--ink); background: var(--ink); color: var(--paper); cursor: pointer; }
  .curator button:disabled { opacity: .5; cursor: default; }
  .result { display: flex; flex-direction: column; gap: 12px; }
  .reply { margin: 0; padding: 14px; border-left: 4px solid var(--accent); }
  .note { margin: 0; padding: 14px; border: 2px solid var(--ink); }
  .error { grid-column: 1 / -1; margin: 0; padding: 16px; border: 2px solid var(--accent); background: var(--ink); color: var(--paper); }
  @media (max-width: 760px) { .memory { grid-template-columns: 1fr; } }
</style>
