<!--
  Pages — the space's agent-run work surfaces. A page is a stack of module
  instances; the document module renders markdown source. Agents mutate
  pages through their tools and the surface refreshes live off the
  page_updated signal (the store refetches; nothing renders from events).
-->
<script lang="ts">
  import DOMPurify from "dompurify";
  import { marked } from "marked";
  import type { PagesStore } from "../stores/pages";

  interface Props {
    store: PagesStore;
    spaceId: string;
  }

  let { store, spaceId }: Props = $props();

  let newTitle = $state("");
  // The module currently being edited by the user, if any.
  let editingModuleId = $state<string | null>(null);
  let editingSource = $state("");

  $effect(() => {
    void store.loadPages(spaceId);
  });

  function renderDocument(source: string): string {
    // Same discipline as conversation markdown: parse, then sanitise —
    // module source is shared work product and may quote anything.
    const raw = marked.parse(source) as string;
    return DOMPurify.sanitize(raw);
  }

  async function createPage(): Promise<void> {
    const title = newTitle.trim();
    if (!title) return;
    newTitle = "";
    await store.createPage(spaceId, title);
  }

  function startEditing(moduleId: string, source: string): void {
    editingModuleId = moduleId;
    editingSource = source;
  }

  async function saveEdit(): Promise<void> {
    const page = store.activePage;
    if (!page || editingModuleId === null) return;
    await store.writeModule(page.id, editingModuleId, editingSource);
    editingModuleId = null;
  }

  async function appendModule(): Promise<void> {
    const page = store.activePage;
    if (!page) return;
    await store.writeModule(page.id, null, "");
  }
</script>

<div class="pages">
  <aside aria-label="Pages">
    <form
      onsubmit={(event) => {
        event.preventDefault();
        void createPage();
      }}
    >
      <input placeholder="New page title" bind:value={newTitle} />
      <button type="submit">Create</button>
    </form>
    <ul>
      {#each store.pages as page (page.id)}
        <li>
          <button
            class:active={store.activePage?.id === page.id}
            onclick={() => void store.openPage(page.id)}
          >
            {page.title}
          </button>
        </li>
      {/each}
    </ul>
  </aside>

  <section aria-label="Page">
    {#if store.activePage}
      <h2>{store.activePage.title}</h2>
      {#each store.activePage.modules as module (module.id)}
        <article class="module" data-kind={module.kind}>
          {#if editingModuleId === module.id}
            <textarea bind:value={editingSource} rows="8"></textarea>
            <div class="module-actions">
              <button onclick={() => void saveEdit()}>Save</button>
              <button onclick={() => (editingModuleId = null)}>Cancel</button>
            </div>
          {:else}
            <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitised above -->
            <div class="module-content">{@html renderDocument(module.source)}</div>
            <div class="module-actions">
              <button onclick={() => startEditing(module.id, module.source)}>
                Edit
              </button>
            </div>
          {/if}
        </article>
      {/each}
      <button class="append" onclick={() => void appendModule()}>
        Add module
      </button>
    {:else}
      <p class="empty">Open a page, or create one — agents can write here too.</p>
    {/if}
  </section>
</div>

<style>
  .pages {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 16px;
    min-width: 0;
    height: 100%;
    overflow: auto;
  }

  aside ul {
    list-style: none;
    padding: 0;
    margin: 8px 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  aside button.active {
    font-weight: 600;
  }

  .module {
    border-left: 3px solid var(--ink);
    padding: 8px 0 8px 12px;
    margin: 12px 0;
  }

  .module textarea {
    width: 100%;
    font-family: var(--font-mono);
  }

  .module-actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }

  .empty {
    color: var(--ink-soft);
  }
</style>
