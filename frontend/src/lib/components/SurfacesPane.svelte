<!--
  Surfaces — the topic's agent-run work surfaces. A surface is a stack of module
  instances; the document module renders markdown source. Each module carries
  the notes hung on it — the shared reference the user and the agent both
  point at instead of describing a location to each other. Agents mutate
  surfaces through their tools and the surface refreshes live off the
  surface_updated signal (the store refetches; nothing renders from events).
-->
<script lang="ts">
  import DOMPurify from "dompurify";
  import { marked } from "marked";
  import type { SurfacesStore } from "../stores/surfaces";

  interface Props {
    store: SurfacesStore;
    topicId: string;
  }

  let { store, topicId }: Props = $props();

  let newTitle = $state("");
  // The template a new surface is forked from, "" for a blank surface. The picker
  // reads the topic's templates; it never mutates them.
  let newTemplateId = $state("");
  // The module currently being edited by the user, if any.
  let editingModuleId = $state<string | null>(null);
  let editingSource = $state("");
  // The module whose note the user is currently composing, if any.
  let annotatingModuleId = $state<string | null>(null);
  let annotationBody = $state("");

  $effect(() => {
    // A template belongs to one topic: switching topics must not carry a
    // selection the new topic's picker never offered into the next create.
    newTemplateId = "";
    void store.loadSurfaces(topicId);
  });

  function renderDocument(source: string): string {
    // Same discipline as conversation markdown: parse, then sanitise —
    // module source is shared work product and may quote anything.
    const raw = marked.parse(source) as string;
    return DOMPurify.sanitize(raw);
  }

  async function createSurface(): Promise<void> {
    const title = newTitle.trim();
    if (!title) return;
    const templateId = newTemplateId;
    newTitle = "";
    newTemplateId = "";
    await store.createSurface(topicId, title, templateId || undefined);
  }

  function startEditing(moduleId: string, source: string): void {
    editingModuleId = moduleId;
    editingSource = source;
  }

  async function saveEdit(): Promise<void> {
    const surface = store.activeSurface;
    if (!surface || editingModuleId === null) return;
    await store.writeModule(surface.id, editingModuleId, editingSource);
    editingModuleId = null;
  }

  async function appendModule(): Promise<void> {
    const surface = store.activeSurface;
    if (!surface) return;
    await store.writeModule(surface.id, null, "");
  }

  function startAnnotating(moduleId: string): void {
    annotatingModuleId = moduleId;
    annotationBody = "";
  }

  async function saveAnnotation(): Promise<void> {
    const surface = store.activeSurface;
    const body = annotationBody.trim();
    if (!surface || annotatingModuleId === null || !body) return;
    const moduleId = annotatingModuleId;
    annotatingModuleId = null;
    annotationBody = "";
    await store.annotate(surface.id, moduleId, body);
  }
</script>

<div class="surfaces">
  <aside aria-label="Surfaces">
    <form
      onsubmit={(event) => {
        event.preventDefault();
        void createSurface();
      }}
    >
      <input placeholder="New surface title" bind:value={newTitle} />
      {#if store.templates.length > 0}
        <select aria-label="Template" bind:value={newTemplateId}>
          <option value="">Blank surface</option>
          {#each store.templates as template (template.id)}
            <option value={template.id}>{template.name}</option>
          {/each}
        </select>
      {/if}
      <button type="submit">Create</button>
    </form>
    <ul>
      {#each store.surfaces as surface (surface.id)}
        <li>
          <button
            class:active={store.activeSurface?.id === surface.id}
            onclick={() => void store.openSurface(surface.id)}
          >
            {surface.title}
          </button>
        </li>
      {/each}
    </ul>
  </aside>

  <section aria-label="Surface">
    {#if store.activeSurface}
      <h2>{store.activeSurface.title}</h2>
      {#if store.activeSurface.forked_from_template_id}
        {@const originId = store.activeSurface.forked_from_template_id}
        <p class="lineage">
          Forked from <strong
            >{store.templateName(originId) ?? originId}</strong
          > — this surface's modules are its own; the template is unaffected by
          edits here.
        </p>
      {/if}
      {#each store.activeSurface.modules as module (module.id)}
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
                        store.activeSurface!.id,
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
      <p class="empty">Open a surface, or create one — agents can write here too.</p>
    {/if}
  </section>
</div>

<style>
  .surfaces {
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
