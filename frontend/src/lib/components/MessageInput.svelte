<!--
  Message input with send/stop toggle — handles text entry and submission.
  Delegates sends to the streaming store and registers session files through the materials API.
-->
<script lang="ts">
  import { tick } from "svelte";
  import { createMaterial } from "../api/materials";
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
  let images = $state<{ name: string; url: string }[]>([]);
  let materials = $state<{ id: string; title: string }[]>([]);
  let attachmentError = $state<string | null>(null);
  let pendingReads = $state(0);
  let sending = $state(false);
  let fileInput: HTMLInputElement | undefined = $state();
  let attachmentScope = $state<string | null | undefined>(undefined);
  let attachmentGeneration = 0;

  $effect(() => {
    if (attachmentScope !== sessionId) {
      attachmentScope = sessionId;
      attachmentGeneration += 1;
      images = [];
      materials = [];
      attachmentError = null;
    }
  });

  function readFile(file: File, image: boolean): Promise<string> {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.onerror = () => reject(new Error(`Could not read ${file.name}.`));
      if (image) reader.readAsDataURL(file);
      else reader.readAsText(file);
    });
  }

  async function attachFiles(files: File[]): Promise<void> {
    let generation = attachmentGeneration;
    attachmentError = null;
    pendingReads += 1;
    try {
      for (const file of files) {
        const image = ["image/png", "image/jpeg"].includes(file.type);
        const text = file.type.startsWith("text/") || /\.(md|markdown|txt)$/i.test(file.name);
        if (!image && !text) throw new Error("Choose a PNG, JPEG, text or Markdown file.");
        const body = await readFile(file, image);
        if (generation !== attachmentGeneration) return;
        if (image) {
          images = [...images, { name: file.name || "Pasted image", url: body }];
        } else {
          const creatingSession = !sessionId;
          const pendingImages = images;
          const target = sessionId ?? await store?.ensureConversation?.(selectedModel || null, selectedAgentId || null, selectedEffort || null);
          await tick();
          if (!target) throw new Error("Could not create a session for the file. Please try again.");
          // Session creation can change the composer from an empty start to
          // its new session. A switch to a different session cancels intake.
          if (sessionId && sessionId !== target) return;
          // Only the session this creation returned can inherit its draft.
          // Any intervening switch beyond that one transition cancels it.
          if (creatingSession && sessionId === target && attachmentGeneration === generation + 1) {
            images = pendingImages;
            generation = attachmentGeneration;
          } else if (attachmentGeneration !== generation) return;
          const registered = await createMaterial({
            scope_kind: "session",
            session_id: target,
            title: file.name,
            relative_path: `composer/${Array.from(crypto.getRandomValues(new Uint32Array(4))).join("-")}-${file.name.replace(/[^a-zA-Z0-9._-]/g, "-")}`,
            mime_type: /\.(md|markdown)$/i.test(file.name) ? "text/markdown" : (file.type || "text/plain"),
            content_text: body,
          });
          if (generation !== attachmentGeneration || (sessionId && sessionId !== target)) return;
          materials = [...materials, { id: registered.id, title: registered.title }];
        }
      }
    } catch (error) {
      if (generation === attachmentGeneration) attachmentError = error instanceof Error ? error.message : "Could not attach the file.";
    } finally {
      pendingReads -= 1;
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

  let selectedModel = $state("");
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
    const fallback = selectedModelId ?? modelOptions[0] ?? "";
    if (!selectedModel || !modelOptions.includes(selectedModel)) selectedModel = fallback;
  });

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

    const generation = attachmentGeneration;
    const sentImages = images;
    const clearSentDraft = () => {
      if (attachmentGeneration === generation) images = images.filter((image) => !sentImages.includes(image));
      if (content.trim() === trimmed) content = "";
      textarea?.focus();
    };
    const result = images.length
      ? store.startTurn(trimmed, selectedModel || null, selectedAgentId || null, selectedEffort || null,
          images.map(({ url }): MessageAttachment => ({ kind: "image_url", url })))
      : store.startTurn(trimmed, selectedModel || null, selectedAgentId || null, selectedEffort || null);
    if (result) {
      sending = true;
      void result.then((sent) => { if (sent) clearSentDraft(); }).finally(() => { sending = false; });
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
        accept="image/png,image/jpeg,text/*,.md,.markdown,.txt"
        multiple
        aria-label="Attach images or files"
        hidden
        onchange={(event) => {
          const files = Array.from(event.currentTarget.files ?? []);
          event.currentTarget.value = "";
          void attachFiles(files);
        }}
      />
      <button type="button" disabled={!store || pendingReads > 0 || hasNoConfiguredModels} onclick={() => fileInput?.click()}>Attach</button>
      <span>Images with next message · Files for this session, reference only</span>
    </div>
    {#if pendingReads > 0}<p class="attachment-feedback" role="status">Attaching…</p>{/if}
    {#each images as image, index}
      <div class="attached-image">
        <img src={image.url} alt={image.name} />
        <span>{image.name} · This session · Will be sent with your message</span>
        <button type="button" aria-label={`Remove ${image.name}`} onclick={() => { images = images.filter((_, i) => i !== index); }}>Remove</button>
      </div>
    {/each}
    {#each materials as material (material.id)}
      <p class="attachment-feedback">{material.title} · Attached to this session · Reference only</p>
    {/each}
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
            bind:value={selectedModel}
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
    {#if hasNoConfiguredModels}
      <p class="model-guidance">
        No models are configured. Add a provider in
        <button type="button" onclick={() => onNavigate?.("/settings/providers")}>Settings</button>.
      </p>
    {/if}
  </div>

  {#if isStreamingHere}
    <button class="action stop" onclick={() => store!.cancelTurn()}>
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
  .attachment-controls, .attached-image {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 6px 14px;
    font-size: 0.8rem;
    flex-wrap: wrap;
  }

  .attached-image img {
    width: 64px;
    height: 64px;
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

  .model-guidance {
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
