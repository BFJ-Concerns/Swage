<!--
  Message input with send/stop toggle — handles text entry and submission.
  Pure renderer — delegates to streaming store for send/cancel actions.
-->
<script lang="ts">
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
    (store?.isStreaming ?? false) && (store?.streamBelongsTo?.(sessionId) ?? true),
  );

  // Local draft content — cleared after every successful send.
  let content = $state("");
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
    // Guard: no-op when there is nothing to send or a turn is in progress.
    if (!store || !trimmed || isStreamingHere || hasNoConfiguredModels) return;

    store.startTurn(trimmed, selectedModel || null, selectedAgentId || null, selectedEffort || null);

    // Clear draft and return focus to the textarea so the user can keep typing.
    content = "";
    textarea?.focus();
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
      aria-label="Message"
    ></textarea>
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
      disabled={!content.trim() || !store || hasNoConfiguredModels}
    >
      Send
    </button>
  {/if}
</div>

<style>
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
