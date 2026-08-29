<!--
  Pages — the space's agent-run work surfaces. A page is a stack of module
  instances; the document module renders markdown source. Each module carries
  the notes hung on it — the shared reference the user and the agent both
  point at instead of describing a location to each other. Agents mutate
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
  // The template a new page is forked from, "" for a blank page. The picker
  // reads the space's templates; it never mutates them.
  let newTemplateId = $state("");
  // The module currently being edited by the user, if any.
  let editingModuleId = $state<string | null>(null);
  let editingSource = $state("");
  // The module whose note the user is currently composing, if any.
  let annotatingModuleId = $state<string | null>(null);
  let annotationBody = $state("");

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
    const templateId = newTemplateId;
    newTitle = "";
    newTemplateId = "";
    await store.createPage(spaceId, title, templateId || undefined);
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

  function startAnnotating(moduleId: string): void {
    annotatingModuleId = moduleId;
    annotationBody = "";
  }

  async function saveAnnotation(): Promise<void> {
    const page = store.activePage;
    const body = annotationBody.trim();
    if (!page || annotatingModuleId === null || !body) return;
    const moduleId = annotatingModuleId;
    annotatingModuleId = null;
    annotationBody = "";
    await store.annotate(page.id, moduleId, body);
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
      {#if store.templates.length > 0}
        <select aria-label="Template" bind:value={newTemplateId}>
          <option value="">Blank page</option>
          {#each store.templates as template (template.id)}
            <option value={template.id}>{template.name}</option>
          {/each}
        </select>
      {/if}
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
      {#if store.activePage.forked_from_template_id}
        {@const originId = store.activePage.forked_from_template_id}
        <p class="lineage">
          Forked from <strong
            >{store.templateName(originId) ?? originId}</strong
          > — this page's modules are its own; the template is unaffected by
          edits here.
        </p>
      {/if}
      {#each store.activePage.modules as module (module.id)}
        <article class="module" data-kind={module.kind}>
          {#if editingModuleId === module.id}
            <textarea bind:value={editingSource} rows="8"></textarea>
            <div class="module-actions">
              <button onclick={() => void saveEdit()}>Save</button>
              <button onclick={() => (editingModuleId = null)}>Cancel</button>
            </div>
          {:else if module.kind === "document"}
            <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitised above -->
            <div class="module-content">{@html renderDocument(module.source)}</div>
            <div class="module-actions">
              <button onclick={() => startEditing(module.id, module.source)}>
                Edit
              </button>
              <button onclick={() => startAnnotating(module.id)}>
                Add note
              </button>
            </div>
          {/if}

          {#if module.annotations.length > 0}
            <ul class="annotations" aria-label="Notes on this module">
              {#each module.annotations as annotation (annotation.id)}
                <li data-annotation-id={annotation.id}>
                  <span class="annotation-author">{annotation.author}</span>
                  <!-- Notes are other parties' words: sanitised like module
                       source, and rendered as what someone said, never as
                       anything the surface acts on. -->
                  <span class="annotation-body"
                    >{@html renderDocument(annotation.body)}</span
                  >
                  {#each annotation.tags as tag (tag)}
                    <span class="annotation-tag">{tag}</span>
                  {/each}
                  <button
                    class="annotation-remove"
                    onclick={() =>
                      void store.removeAnnotation(
                        store.activePage!.id,
                        annotation.id,
                      )}
                  >
                    Remove
                  </button>
                </li>
              {/each}
            </ul>
          {/if}

          {#if annotatingModuleId === module.id}
            <form
              class="annotation-form"
              onsubmit={(event) => {
                event.preventDefault();
                void saveAnnotation();
              }}
            >
              <input
                placeholder="Note on this module"
                bind:value={annotationBody}
              />
              <button type="submit">Save note</button>
              <button type="button" onclick={() => (annotatingModuleId = null)}>
                Cancel
              </button>
            </form>
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

  .annotations {
    list-style: none;
    padding: 0;
    margin: 8px 0 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 0.9em;
  }

  .annotations li {
    display: flex;
    align-items: baseline;
    gap: 8px;
    border-left: 2px solid var(--ink-soft);
    padding-left: 8px;
  }

  .annotation-author {
    color: var(--ink-soft);
    font-weight: 600;
  }

  .annotation-tag {
    color: var(--ink-soft);
    font-family: var(--font-mono);
  }

  .annotation-form {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }

  .lineage {
    color: var(--ink-soft);
    font-size: 0.9em;
    margin: 0 0 8px;
  }

  .empty {
    color: var(--ink-soft);
  }
</style>
