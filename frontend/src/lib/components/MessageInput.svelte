<!--
  Message input with send/stop toggle — handles text entry and submission.
  Delegates sends to the streaming store; attaches images to the next message and registers
  files and folders as the session's reference material through the materials API.
-->
<script lang="ts">
  import { tick } from "svelte";
  import { archiveMaterial, createMaterial, listSessionMaterials } from "../api/materials";
  import type { MessageAttachment } from "../protocol/MessageAttachment";
  import type { StreamingStore } from "../stores/streaming";
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";
  import type { Effort } from "../protocol/Effort";
  import AgentPicker from "./AgentPicker.svelte";

  interface Props {
    // Injected by App.svelte once all stores are wired.
    store?: StreamingStore;
    selectedModelId?: string | null;
    modelOptions?: string[];
    modelCatalogueReady?: boolean;
    onNavigate?: (path: string) => void;
    agentDefinitions?: EffectiveAgentDefinitionResponse[];
    canSelectAgent?: boolean;
    selectedAgentId?: string;
    sessionId?: string | null;
  }

  let {
    store,
    selectedModelId = null,
    modelOptions = [],
    modelCatalogueReady = false,
    onNavigate,
    agentDefinitions = [],
    canSelectAgent = false,
    selectedAgentId = $bindable(""),
    sessionId = null,
  }: Props = $props();

  const hasNoConfiguredModels = $derived(modelCatalogueReady && modelOptions.length === 0);
  const isStreamingHere = $derived(
    store?.streamFor?.(sessionId).isStreaming ?? store?.isStreaming ?? false,
  );

  // Local draft content — cleared after every successful send.
  let content = $state("");
  let modelOverride = $state("");
  const preferredModel = $derived(canSelectAgent
    ? agentDefinitions.find((definition) => definition.id === selectedAgentId)?.model_id
    : undefined);
  const modelFallback = $derived(preferredModel && modelOptions.includes(preferredModel)
    ? preferredModel
    : selectedModelId ?? modelOptions[0] ?? "");
  const selectedModel = $derived(modelOverride || modelFallback);
  let modelSessionId = $state<string | null | undefined>(undefined);
  let modelDefaultId = $state<string | null | undefined>(undefined);

  // The composer's attachments. Images are a draft that travels with the
  // next message. Files, and the text files of a chosen folder, register at
  // once as the session's reference material, so the list below the field
  // is the session's active material, each removable.
  type DraftImage = { name: string; url: string };
  let images = $state<DraftImage[]>([]);
  let materials = $state<{ id: string; title: string }[]>([]);
  let attachmentError = $state<string | null>(null);
  let attachmentNotice = $state<string | null>(null);
  let pendingReads = $state(0);
  let sending = $state(false);
  let fileInput: HTMLInputElement | undefined = $state();
  let folderInput: HTMLInputElement | undefined = $state();
  let attachmentScope = $state<string | null | undefined>(undefined);
  // Bumped whenever the composer moves to another session; work begun for
  // the previous one checks it and stops.
  let attachmentGeneration = 0;
  // Set while this composer is creating the session it stands on: the move
  // from no session to that one keeps the draft instead of clearing it.
  let adoptingCreatedSession = false;

  // A folder brings in its text files only, and never a whole tree of them.
  const FOLDER_FILE_LIMIT = 500;
  const MATERIAL_BYTE_LIMIT = 1024 * 1024;

  $effect(() => {
    if (attachmentScope === sessionId) return;
    const adopted = adoptingCreatedSession && !attachmentScope && !!sessionId;
    attachmentScope = sessionId;
    if (!adopted) {
      attachmentGeneration += 1;
      images = [];
      materials = [];
      attachmentError = null;
      attachmentNotice = null;
    }
    adoptingCreatedSession = false;
    if (sessionId) void loadMaterials(sessionId, attachmentGeneration);
  });

  async function loadMaterials(target: string, generation: number): Promise<void> {
    try {
      const listed = (await listSessionMaterials(target))
        .filter((material) => material.status === "active")
        .map(({ id, title }) => ({ id, title }));
      if (generation !== attachmentGeneration) return;
      // Material registered while the list was loading stays shown.
      materials = [...listed, ...materials.filter((own) => !listed.some((m) => m.id === own.id))];
    } catch (error) {
      // Not something the person did: said beside the list, not as an alert.
      if (generation === attachmentGeneration) attachmentNotice = `Could not list this session's files: ${describe(error, "the gateway did not answer")}`;
    }
  }

  function describe(error: unknown, fallback: string): string {
    return error instanceof Error ? error.message : fallback;
  }

  function readFile(file: File, image: boolean): Promise<string> {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.onerror = () => reject(new Error(`Could not read ${file.name}.`));
      if (image) reader.readAsDataURL(file);
      else reader.readAsText(file);
    });
  }

  function isImage(file: File): boolean {
    return ["image/png", "image/jpeg"].includes(file.type);
  }

  function materialMimeType(name: string, type: string): string {
    if (/\.(md|markdown)$/i.test(name)) return "text/markdown";
    return type.startsWith("text/") ? type : "text/plain";
  }

  // The session files register against: the one in view, or one created
  // for them when the composer stands on none.
  async function targetSession(): Promise<string | null> {
    if (sessionId) return sessionId;
    adoptingCreatedSession = true;
    const requestedModel = canSelectAgent && selectedAgentId ? modelOverride : selectedModel;
    const created = await store?.ensureConversation?.(requestedModel || null, selectedAgentId || null, selectedEffort || null) ?? null;
    await tick();
    if (!created) adoptingCreatedSession = false;
    return created;
  }

  // Registers one file against `target`. The list takes it only while the
  // composer still stands where the attach began.
  async function register(generation: number, target: string, title: string, path: string, file: File, body: string): Promise<void> {
    const segments = path.split("/").map((segment) => segment.replace(/[^a-zA-Z0-9._-]/g, "-"));
    const registered = await createMaterial({
      scope_kind: "session",
      session_id: target,
      title,
      relative_path: `composer/${crypto.randomUUID()}/${segments.join("/")}`,
      mime_type: materialMimeType(file.name, file.type),
      content_text: body,
    });
    if (generation !== attachmentGeneration) return;
    materials = [...materials, { id: registered.id, title: registered.title }];
  }

  async function attachFiles(files: File[]): Promise<void> {
    const generation = attachmentGeneration;
    attachmentError = null;
    attachmentNotice = null;
    pendingReads += 1;
    try {
      let target: string | null = null;
      for (const file of files) {
        const image = isImage(file);
        if (!image && file.type.startsWith("image/")) throw new Error(`${file.name || "That image"} is not a PNG or JPEG image.`);
        const body = await readFile(file, image);
        if (generation !== attachmentGeneration) return;
        if (image) {
          images = [...images, { name: file.name || "Pasted image", url: body }];
          continue;
        }
        if (body.includes("\u0000")) throw new Error(`${file.name} is not text: choose a PNG or JPEG image, or a text file.`);
        target ??= await targetSession();
        if (!target) throw new Error("Could not create a session for the file. Please try again.");
        if (generation !== attachmentGeneration) return;
        await register(generation, target, file.name, file.name, file, body);
      }
    } catch (error) {
      if (generation === attachmentGeneration) attachmentError = describe(error, "Could not attach the file.");
    } finally {
      pendingReads -= 1;
    }
  }

  async function attachFolder(files: File[]): Promise<void> {
    const generation = attachmentGeneration;
    attachmentError = null;
    attachmentNotice = null;
    if (!files.length) return;
    pendingReads += 1;
    let attached = 0;
    let skipped = 0;
    try {
      // Hidden directories (.git and the like) are not reference material.
      const candidates = files.filter((file) =>
        !(file.webkitRelativePath || file.name).split("/").some((segment) => segment.startsWith(".")));
      skipped += files.length - candidates.length;
      if (candidates.length > FOLDER_FILE_LIMIT) {
        throw new Error(`That folder holds ${candidates.length} files; attach a folder of at most ${FOLDER_FILE_LIMIT}.`);
      }
      const target = await targetSession();
      if (!target) throw new Error("Could not create a session for the folder. Please try again.");
      for (const file of candidates) {
        if (generation !== attachmentGeneration) return;
        if (file.size > MATERIAL_BYTE_LIMIT || file.size === 0) { skipped += 1; continue; }
        const body = await readFile(file, false);
        if (body.includes("\u0000")) { skipped += 1; continue; }
        if (generation !== attachmentGeneration) return;
        const path = file.webkitRelativePath || file.name;
        await register(generation, target, path, path, file, body);
        attached += 1;
      }
    } catch (error) {
      if (generation === attachmentGeneration) attachmentError = describe(error, "Could not attach the folder.");
    } finally {
      pendingReads -= 1;
      if (generation === attachmentGeneration && (attached || skipped)) {
        attachmentNotice = `${attached} file${attached === 1 ? "" : "s"} attached from the folder` +
          (skipped ? `; ${skipped} skipped (not text, empty, over 1 MB, or hidden)` : "") + ".";
      }
    }
  }

  async function removeMaterial(id: string): Promise<void> {
    const generation = attachmentGeneration;
    attachmentError = null;
    try {
      await archiveMaterial(id);
      if (generation === attachmentGeneration) materials = materials.filter((material) => material.id !== id);
    } catch (error) {
      if (generation === attachmentGeneration) attachmentError = describe(error, "Could not remove the file.");
    }
  }

  function handlePaste(event: ClipboardEvent): void {
    const files = Array.from(event.clipboardData?.items ?? [])
      .filter((item) => item.kind === "file" && item.type.startsWith("image/"))
      .map((item) => item.getAsFile())
      .filter((file): file is File => file !== null);
    if (files.length) {
      event.preventDefault();
      void attachFiles(files);
    }
  }
  let selectedEffort = $state<"" | Effort>("");
  let effortSessionId = $state<string | null | undefined>(undefined);

  $effect(() => {
    if (effortSessionId === undefined) {
      effortSessionId = sessionId;
    } else if (sessionId !== effortSessionId) {
      selectedEffort = "";
      effortSessionId = sessionId;
    }
  });

  $effect(() => {
    if (sessionId !== modelSessionId || (!canSelectAgent && selectedModelId !== modelDefaultId) ||
        (modelOverride && !modelOptions.includes(modelOverride))) {
      modelOverride = "";
      modelSessionId = sessionId;
      modelDefaultId = selectedModelId;
    }
  });

  // Why the last Stop did not reach the turn. The turn keeps running, so this
  // is a notice beside the control, never the turn's own failure; it clears
  // once nothing is running here.
  let stopError = $state<string | null>(null);
  $effect(() => {
    if (!isStreamingHere) stopError = null;
  });

  async function stop(): Promise<void> {
    stopError = null;
    try {
      await store!.cancelTurn();
    } catch (error) {
      stopError = error instanceof Error ? error.message : "Stop did not reach the turn.";
    }
  }

  // Direct reference to the textarea DOM node for height adjustment and refocus.
  let textarea: HTMLTextAreaElement | undefined = $state(undefined);

  // Auto-grow: recalculate textarea height whenever content changes.
  // We reset to "auto" first so the element can shrink when text is deleted.
  // The CSS min-height holds the comfortable multi-line default; this only
  // takes over once the content outgrows it.
  $effect(() => {
    // Reading `content` makes this reactive — the effect re-runs on every keystroke.
    content;
    if (textarea) {
      textarea.style.height = "auto";
      // Clamp to the CSS max-height (320px) — overflow-y handles scrolling beyond that.
      textarea.style.height = `${Math.min(textarea.scrollHeight, 320)}px`;
    }
  });

  function send() {
    const trimmed = content.trim();
    // Guard: no-op when there is nothing to send. A turn in progress is not
    // a reason to refuse: the gateway queues the message for the next turn.
    if (!store || (!trimmed && images.length === 0) || hasNoConfiguredModels || pendingReads > 0 || sending) return;

    // At session start the gateway resolves the agent's current preference
    // unless the person explicitly chose a model in the composer.
    const requestedModel = canSelectAgent && selectedAgentId ? modelOverride : selectedModel;
    const sentImages = images;
    const clearSentDraft = () => {
      images = images.filter((image) => !sentImages.includes(image));
      if (content.trim() === trimmed) content = "";
      textarea?.focus();
    };
    const attachments = images.map(({ url }): MessageAttachment => ({ kind: "image_url", url }));
    if (!sessionId) adoptingCreatedSession = true;
    const result = attachments.length
      ? store.startTurn(trimmed, requestedModel || null, selectedAgentId || null, selectedEffort || null, attachments)
      : store.startTurn(trimmed, requestedModel || null, selectedAgentId || null, selectedEffort || null);
    if (result) {
      sending = true;
      void result.then((sent) => {
        if (sent) clearSentDraft();
        else adoptingCreatedSession = false;
      }).finally(() => { sending = false; });
    } else {
      clearSentDraft();
    }
  }

  function handleKeydown(event: KeyboardEvent) {
    // Enter sends; Shift+Enter falls through to the browser's default newline.
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      send();
    }
  }
</script>

<!--
  Riso input bar: navy "WRITE" label block on the left, paper-coloured field
  in the middle, fluoro orange "SEND" block on the right. Translucent orange
  offset shadow underneath when the bar is active.
-->
<div class="message-input" class:disabled={!store}>
  <span class="input-label" aria-hidden="true">Write</span>
  <!-- Field column: the textarea fills the height, with the model picker
       parked in a slim toolbar beneath it so the composer stays a clean
       three-block bar (WRITE · field · SEND) however tall it grows. -->
  <div class="field">
    <textarea
      bind:this={textarea}
      bind:value={content}
      placeholder="Send a message — Shift+Enter for a new line"
      rows="3"
      onkeydown={handleKeydown}
      onpaste={handlePaste}
      aria-label="Message"
    ></textarea>
    <div class="attachment-controls">
      <input
        bind:this={fileInput}
        type="file"
        multiple
        aria-label="Attach images or files"
        hidden
        onchange={(event) => {
          const files = Array.from(event.currentTarget.files ?? []);
          event.currentTarget.value = "";
          void attachFiles(files);
        }}
      />
      <input
        bind:this={folderInput}
        type="file"
        webkitdirectory
        aria-label="Attach a folder"
        hidden
        onchange={(event) => {
          const files = Array.from(event.currentTarget.files ?? []);
          event.currentTarget.value = "";
          void attachFolder(files);
        }}
      />
      <button type="button" disabled={!store || pendingReads > 0 || hasNoConfiguredModels} onclick={() => fileInput?.click()}>Attach files</button>
      <button type="button" disabled={!store || pendingReads > 0 || hasNoConfiguredModels} onclick={() => folderInput?.click()}>Attach folder</button>
      <span class="attachment-hint">Images go with your next message; files and folders stay with this session for the agent to read.</span>
    </div>
    {#if pendingReads > 0}<p class="attachment-feedback" role="status">Attaching…</p>{/if}
    {#if images.length || materials.length}
      <ul class="attachments" aria-label="Attachments">
        {#each images as image (image)}
          <li class="attachment attached-image">
            <img src={image.url} alt={image.name} />
            <span>{image.name} · with your next message</span>
            <button type="button" aria-label={`Remove ${image.name}`} onclick={() => { images = images.filter((other) => other !== image); }}>Remove</button>
          </li>
        {/each}
        {#each materials as material (material.id)}
          <li class="attachment">
            <span>{material.title} · reference for this session</span>
            <button type="button" aria-label={`Remove ${material.title}`} onclick={() => removeMaterial(material.id)}>Remove</button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if attachmentNotice}<p class="attachment-feedback" role="status">{attachmentNotice}</p>{/if}
    {#if attachmentError}<p class="attachment-feedback" role="alert">{attachmentError}</p>{/if}
    {#if modelOptions.length > 0 || canSelectAgent}
      <div class="field-toolbar">
        {#if canSelectAgent}
          <AgentPicker definitions={agentDefinitions} bind:selectedAgentId disabled={!store || isStreamingHere} />
          <select bind:value={selectedEffort} aria-label="Effort" disabled={!store || isStreamingHere}>
            <option value="">Model default</option>
            <option value="off">Reasoning off</option>
            <option value="minimal">Minimal effort</option>
            <option value="low">Low effort</option>
            <option value="medium">Medium effort</option>
            <option value="high">High effort</option>
            <option value="xhigh">Extra-high effort</option>
            <option value="max">Maximum effort</option>
          </select>
        {/if}
        {#if modelOptions.length > 0}
          <select
            class="model-select"
            value={selectedModel}
            onchange={(event) => { modelOverride = event.currentTarget.value; }}
            aria-label="Model"
            disabled={!store || isStreamingHere}
          >
            {#each modelOptions as modelId (modelId)}
              <option value={modelId}>{modelId}</option>
            {/each}
          </select>
        {/if}
      </div>
    {/if}
    {#if stopError}
      <p class="stop-notice" role="alert">Stop did not reach the turn: {stopError}</p>
    {/if}
    {#if hasNoConfiguredModels}
      <p class="model-guidance">
        No models are configured. Add a provider in
        <button type="button" onclick={() => onNavigate?.("/settings/providers")}>Settings</button>.
      </p>
    {/if}
  </div>

  {#if isStreamingHere}
    <button class="action stop" onclick={stop}>
      Stop
    </button>
  {:else}
    <button
      class="action send"
      onclick={send}
      disabled={(!content.trim() && images.length === 0) || !store || hasNoConfiguredModels || pendingReads > 0 || sending}
    >
      Send
    </button>
  {/if}
</div>

<style>
  .attachment-controls {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 6px 14px;
    font-size: 0.8rem;
    flex-wrap: wrap;
  }

  .attachment-hint {
    color: var(--ink-soft);
  }

  .attachments {
    list-style: none;
    margin: 0;
    padding: 0 14px 6px;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    max-height: 160px;
    overflow-y: auto;
    font-size: 0.8rem;
  }

  .attachment {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 2px 6px;
    border: 1px solid var(--rule-soft);
  }

  .attached-image img {
    width: 48px;
    height: 48px;
    object-fit: contain;
  }

  .attachment-feedback {
    margin: 0;
    padding: 6px 14px;
    font-size: 0.8rem;
  }

  .message-input {
    /* Padding lives on the conversation pane — the bar itself sits flush. */
    margin: 12px 22px 18px;
    border: 2px solid var(--ink);
    background: var(--paper);
    display: grid;
    grid-template-columns: 64px 1fr auto;
    align-items: stretch;
    position: relative;
    flex-shrink: 0;
    isolation: isolate;
  }

  /* The composer carries its presence through the 2px ink frame alone — no
     riso offset shadow. The translucent orange offset is reserved for the
     single active selection / primary action so it keeps meaning rather than
     reading as texture on every always-on surface (UI audit f-18). */

  .message-input.disabled {
    opacity: 0.5;
  }

  /* Left-hand block: navy fill with cream display label. */
  .input-label {
    background: var(--ink);
    color: var(--paper);
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--font-display);
    font-size: 18px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  /* Field column: textarea stacked over the optional model toolbar. */
  .field {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .field-toolbar {
    display: flex;
    align-items: center;
    padding: 6px 14px;
    border-top: 1px dotted var(--rule-soft);
    gap: 1rem;
  }

  .model-select {
    max-width: min(34vw, 18rem);
    border: 1px solid var(--rule-soft);
    background: var(--paper);
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: 0.72rem;
    letter-spacing: 0.04em;
    padding: 3px 6px;
    cursor: pointer;
  }

  .model-guidance,
  .stop-notice {
    margin: 0;
    padding: 7px 14px;
    border-top: 1px dotted var(--rule-soft);
    color: var(--ink-soft);
    font-family: var(--font-body);
    font-size: 0.8rem;
  }

  .model-guidance button {
    border: 0;
    padding: 0;
    background: none;
    color: var(--ink);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }

  textarea {
    flex: 1;
    border: none;
    background: transparent;
    padding: 14px 16px;
    font-family: var(--font-body);
    font-size: 14px;
    line-height: 1.55;
    color: var(--ink);
    /* Comfortable ~3-line default; grows to max before scrolling (kept in
       sync with the auto-grow clamp in the script). */
    min-height: 96px;
    max-height: 320px;
    resize: none;
    outline: none;
    overflow-y: auto;
  }

  textarea::placeholder {
    color: color-mix(in srgb, var(--ink) 50%, transparent);
    font-style: italic;
  }

  textarea:focus {
    background: color-mix(in srgb, var(--accent) 4%, transparent);
  }

  /* Right-hand block: fluoro orange fill with cream display label.
     Stop replaces Send during streaming — same geometry, redder fill. */
  .action {
    padding: 0 18px;
    background: var(--accent);
    color: var(--paper);
    font-family: var(--font-display);
    font-size: 16px;
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    border: none;
    cursor: pointer;
    display: flex;
    align-items: center;
    border-left: 1px solid color-mix(in srgb, var(--paper) 20%, transparent);
  }

  .action.stop {
    background: var(--ink);
  }

  .action.send:hover:not(:disabled),
  .action.stop:hover {
    filter: brightness(0.92);
  }

  .action:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
</style>
