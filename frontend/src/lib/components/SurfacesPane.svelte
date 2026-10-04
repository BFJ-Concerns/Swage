<!--
  Surfaces — the session's work surfaces. A surface is a stack of module
  instances; the document module renders markdown source. Each module carries
  the notes hung on it — the shared reference the user and the agent both
  point at instead of describing a location to each other. Agents mutate
  surfaces through their tools and the surface refreshes live off the
  surface_updated signal (the store refetches; nothing renders from events).
  A surface belongs to this session; the shared library is how one reaches
  another session — save it here, add it there. The open surface has an
  address: the pane opens the surface the URL names and reports the one the
  user opens so the URL follows.
-->
<script lang="ts">
  import DOMPurify from "dompurify";
  import { marked } from "marked";
  import type { SurfacesStore } from "../stores/surfaces";
  import type { SurfaceModuleView } from "../protocol/SurfaceModuleView";

  interface Props {
    store: SurfacesStore;
    sessionId: string;
    // The surface the URL names; opened once the session's list is loaded.
    routedSurfaceId?: string | null;
    // The user opened a surface (or closed one, with null): keep the URL in step.
    onOpenSurface?: (surfaceId: string | null) => void;
    onOpenLibrary?: () => void;
  }

  let { store, sessionId, routedSurfaceId = null, onOpenSurface, onOpenLibrary }: Props = $props();

  let newTitle = $state("");
  // The library entry a new surface is seeded from, "" for a blank surface.
  // The picker reads the catalogue; it never mutates it.
  let newEntryId = $state("");
  // The module currently being edited by the user, if any, and the revision
  // the editor opened at: a save names it, so a change made meanwhile is
  // never written over unseen.
  let editingModuleId = $state<string | null>(null);
  let editingSource = $state("");
  let editingRevision = $state<number | null>(null);
  // The module as it now stands, when a save was refused because it changed
  // while the editor was open. Shown beside the edit; replacing it is a
  // deliberate second act.
  let staleModule = $state<SurfaceModuleView | null>(null);
  // The module whose note the user is currently composing, if any.
  let annotatingModuleId = $state<string | null>(null);
  let annotationBody = $state("");
  // The note being reworded in place, if any.
  let editingAnnotationId = $state<string | null>(null);
  let editingAnnotationBody = $state("");
  // The save-to-library form for the open surface, when it is showing.
  let savingToLibrary = $state(false);
  let saveName = $state("");
  let saveDescription = $state("");
  let saveTags = $state("");
  let saveNotice = $state<string | null>(null);

  $effect(() => {
    // A picker selection belongs to one session's view: switching sessions
    // must not carry a selection into the next create.
    newEntryId = "";
    savingToLibrary = false;
    saveNotice = null;
    void store.loadSurfaces(sessionId);
  });

  // The URL names a surface: open it, unless it is already the open one. A
  // surface the session does not have is left unopened — the list shows
  // what there is, and the address is corrected when the user opens one.
  $effect(() => {
    const wanted = routedSurfaceId;
    if (!wanted || store.activeSurface?.id === wanted) return;
    if (!store.surfaces.some((surface) => surface.id === wanted)) return;
    void store.openSurface(wanted);
  });

  function open(surfaceId: string): void {
    void store.openSurface(surfaceId);
    onOpenSurface?.(surfaceId);
  }

  function renderDocument(source: string): string {
    // Same discipline as conversation markdown: parse, then sanitise —
    // module source is shared work product and may quote anything.
    const raw = marked.parse(source) as string;
    return DOMPurify.sanitize(raw);
  }

  async function createSurface(): Promise<void> {
    const title = newTitle.trim();
    if (!title) return;
    const entryId = newEntryId;
    newTitle = "";
    newEntryId = "";
    const created = await store.createSurface(sessionId, title, entryId || undefined);
    if (created) onOpenSurface?.(created.id);
  }

  function startSaving(): void {
    const surface = store.activeSurface;
    if (!surface) return;
    savingToLibrary = true;
    saveName = surface.title;
    saveDescription = "";
    saveTags = "";
    saveNotice = null;
  }

  async function saveToLibrary(): Promise<void> {
    const surface = store.activeSurface;
    const name = saveName.trim();
    const description = saveDescription.trim();
    if (!surface || !name || !description) return;
    const tags = saveTags.split(",").map((tag) => tag.trim()).filter(Boolean);
    await store.saveToLibrary(surface.id, name, description, tags);
    savingToLibrary = false;
    saveNotice = `Saved to the library as “${name}”.`;
  }

  function startEditing(module: SurfaceModuleView): void {
    editingModuleId = module.id;
    editingSource = module.source;
    editingRevision = module.revision;
    staleModule = null;
  }

  function stopEditing(): void {
    editingModuleId = null;
    editingRevision = null;
    staleModule = null;
  }

  async function saveEdit(): Promise<void> {
    const surface = store.activeSurface;
    if (!surface || editingModuleId === null) return;
    const newer = await store.writeModule(
      surface.id,
      editingModuleId,
      editingSource,
      editingRevision,
    );
    if (newer) {
      staleModule = newer;
      return;
    }
    stopEditing();
  }

  // The user has seen the newer version and still wants their edit: save
  // again naming the revision they have now seen.
  function replaceAnyway(): void {
    if (!staleModule) return;
    editingRevision = staleModule.revision;
    staleModule = null;
    void saveEdit();
  }

  async function appendModule(): Promise<void> {
    const surface = store.activeSurface;
    if (!surface) return;
    await store.writeModule(surface.id, null, "", null);
  }

  async function removeModule(moduleId: string): Promise<void> {
    const surface = store.activeSurface;
    if (!surface) return;
    if (editingModuleId === moduleId) stopEditing();
    await store.deleteModule(surface.id, moduleId);
  }

  async function moveModule(moduleId: string, position: number): Promise<void> {
    const surface = store.activeSurface;
    if (!surface) return;
    await store.moveModule(surface.id, moduleId, position);
  }

  function startAnnotating(moduleId: string): void {
    annotatingModuleId = moduleId;
    annotationBody = "";
  }

  function startEditingAnnotation(annotationId: string, body: string): void {
    editingAnnotationId = annotationId;
    editingAnnotationBody = body;
  }

  async function saveAnnotationEdit(tags: string[]): Promise<void> {
    const surface = store.activeSurface;
    const body = editingAnnotationBody.trim();
    if (!surface || editingAnnotationId === null || !body) return;
    const annotationId = editingAnnotationId;
    editingAnnotationId = null;
    await store.updateAnnotation(surface.id, annotationId, body, tags);
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
      {#if store.library.length > 0}
        <select aria-label="From the library" bind:value={newEntryId}>
          <option value="">Blank surface</option>
          {#each store.library as entry (entry.id)}
            <option value={entry.id} title={entry.description}>{entry.name}</option>
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
            aria-current={store.activeSurface?.id === surface.id ? "page" : undefined}
            onclick={() => open(surface.id)}
          >
            {surface.title}
          </button>
        </li>
      {:else}
        <li class="none">No surfaces yet.</li>
      {/each}
    </ul>
    {#if onOpenLibrary}
      <button type="button" class="library-link" onclick={onOpenLibrary}>Browse the library</button>
    {/if}
  </aside>

  <section aria-label="Surface">
    {#if store.activeSurface}
      <h2>{store.activeSurface.title}</h2>
      {#if store.activeSurface.seeded_from_name}
        <p class="lineage">
          Added from the library entry <strong
            >{store.activeSurface.seeded_from_name}</strong
          > — this surface's modules are its own; the entry is unaffected by
          edits here.
        </p>
      {/if}
      <div class="surface-actions">
        {#if savingToLibrary}
          <form
            class="save-form"
            aria-label="Save to the library"
            onsubmit={(event) => {
              event.preventDefault();
              void saveToLibrary();
            }}
          >
            <input aria-label="Library name" placeholder="Name" bind:value={saveName} />
            <input
              aria-label="Library description"
              placeholder="What it does and when to use it"
              bind:value={saveDescription}
            />
            <input aria-label="Library tags" placeholder="Tags, comma separated" bind:value={saveTags} />
            <button type="submit" disabled={!saveName.trim() || !saveDescription.trim()}>Save</button>
            <button type="button" onclick={() => (savingToLibrary = false)}>Cancel</button>
          </form>
        {:else}
          <button type="button" onclick={startSaving}>Save to library</button>
          {#if saveNotice}<span class="save-notice">{saveNotice}</span>{/if}
        {/if}
      </div>
      {#each store.activeSurface.modules as module, index (module.id)}
        <article class="module" data-kind={module.kind}>
          {#if editingModuleId === module.id}
            {#if staleModule}
              <div class="stale-notice" role="alert">
                <p>This module changed while you were editing it. It now reads:</p>
                <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitised above -->
                <div class="module-content">{@html renderDocument(staleModule.source)}</div>
                <div class="module-actions">
                  <button onclick={replaceAnyway}>Replace it with my edit</button>
                  <button onclick={stopEditing}>Keep the current version</button>
                </div>
              </div>
            {/if}
            <textarea bind:value={editingSource} rows="8"></textarea>
            <div class="module-actions">
              {#if !staleModule}
                <button onclick={() => void saveEdit()}>Save</button>
              {/if}
              <button onclick={stopEditing}>Cancel</button>
            </div>
          {:else if module.kind === "document"}
            <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitised above -->
            <div class="module-content">{@html renderDocument(module.source)}</div>
            <div class="module-actions">
              <button onclick={() => startEditing(module)}>
                Edit
              </button>
              <button onclick={() => startAnnotating(module.id)}>
                Add note
              </button>
              <button
                aria-label="Move module up"
                disabled={index === 0}
                onclick={() => void moveModule(module.id, index - 1)}
              >
                Move up
              </button>
              <button
                aria-label="Move module down"
                disabled={index === store.activeSurface.modules.length - 1}
                onclick={() => void moveModule(module.id, index + 1)}
              >
                Move down
              </button>
              <button aria-label="Delete module" onclick={() => void removeModule(module.id)}>
                Delete
              </button>
            </div>
          {/if}

          {#if module.annotations.length > 0}
            <ul class="annotations" aria-label="Notes on this module">
              {#each module.annotations as annotation (annotation.id)}
                <li data-annotation-id={annotation.id}>
                  <span class="annotation-author">{annotation.author}</span>
                  {#if editingAnnotationId === annotation.id}
                    <form
                      class="annotation-form"
                      aria-label="Edit note"
                      onsubmit={(event) => {
                        event.preventDefault();
                        void saveAnnotationEdit(annotation.tags);
                      }}
                    >
                      <input aria-label="Note text" bind:value={editingAnnotationBody} />
                      <button type="submit">Save</button>
                      <button type="button" onclick={() => (editingAnnotationId = null)}>Cancel</button>
                    </form>
                  {:else}
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
                      class="annotation-edit"
                      onclick={() => startEditingAnnotation(annotation.id, annotation.body)}
                    >
                      Edit
                    </button>
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
                  {/if}
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
      <p class="empty">Open a surface, or create one — agents can write here too. A surface belongs to this session; save it to the library to use it elsewhere.</p>
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

  /* Phone-sized: the surface list sits above the open surface. */
  @media (max-width: 720px) {
    .surfaces {
      grid-template-columns: 1fr;
      height: auto;
    }
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

  aside .none {
    color: var(--ink-soft);
    font-size: 0.9em;
  }

  .library-link {
    margin-top: 8px;
    background: none;
    border: none;
    padding: 0;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: var(--font-label-sm);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    text-decoration: underline dotted;
    cursor: pointer;
  }

  .library-link:hover {
    color: var(--ink-orange);
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

  .stale-notice {
    border-left: 3px solid var(--ink-orange);
    padding: 4px 0 4px 8px;
    margin-bottom: 8px;
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

  .surface-actions {
    display: flex;
    gap: 8px;
    align-items: baseline;
    margin: 0 0 12px;
  }

  .save-form {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .save-notice {
    color: var(--ink-soft);
    font-size: 0.9em;
  }

  .empty {
    color: var(--ink-soft);
  }
</style>
