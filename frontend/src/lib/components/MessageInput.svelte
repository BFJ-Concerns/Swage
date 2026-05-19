<!--
  Message input with send/stop toggle — handles text entry and submission.
  Pure renderer — delegates to streaming store for send/cancel actions.
-->
<script lang="ts">
  import type { StreamingStore } from "../stores/streaming";

  interface Props {
    // Injected by App.svelte once all stores are wired.
    store?: StreamingStore;
  }

  let { store }: Props = $props();

  // Local draft content — cleared after every successful send.
  let content = $state("");

  // Direct reference to the textarea DOM node for height adjustment and refocus.
  let textarea: HTMLTextAreaElement | undefined = $state(undefined);

  // Auto-grow: recalculate textarea height whenever content changes.
  // We reset to "auto" first so the element can shrink when text is deleted.
  $effect(() => {
    // Reading `content` makes this reactive — the effect re-runs on every keystroke.
    content;
    if (textarea) {
      textarea.style.height = "auto";
      // Clamp to the CSS max-height (160px) — overflow-y handles scrolling beyond that.
      textarea.style.height = `${Math.min(textarea.scrollHeight, 160)}px`;
    }
  });

  function send() {
    const trimmed = content.trim();
    // Guard: no-op when there is nothing to send or a turn is in progress.
    if (!store || !trimmed || store.isStreaming) return;

    store.startTurn(trimmed);

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
  <textarea
    bind:this={textarea}
    bind:value={content}
    placeholder="Send a message — Shift+Enter for a new line"
    rows="1"
    onkeydown={handleKeydown}
    aria-label="Message"
  ></textarea>

  {#if store?.isStreaming}
    <button class="action stop" onclick={() => store!.cancelTurn()}>
      Stop
    </button>
  {:else}
    <button
      class="action send"
      onclick={send}
      disabled={!content.trim() || !store}
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

  /* Translucent orange offset — the riso "second pass". */
  .message-input::after {
    content: "";
    position: absolute;
    inset: 0;
    border: 2px solid var(--accent);
    transform: translate(var(--offset), var(--offset));
    z-index: -1;
    mix-blend-mode: var(--shadow-blend);
    opacity: var(--shadow-opacity);
    pointer-events: none;
  }

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

  textarea {
    border: none;
    background: transparent;
    padding: 12px 14px;
    font-family: var(--font-body);
    font-size: 13.5px;
    line-height: 1.55;
    color: var(--ink);
    min-height: 44px;
    max-height: 160px;
    resize: none;
    outline: none;
    overflow-y: auto;
  }

  textarea::placeholder {
    color: rgba(14, 59, 102, 0.5);
    font-style: italic;
  }

  textarea:focus {
    background: rgba(240, 106, 58, 0.04);
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
    border-left: 1px solid rgba(255, 248, 238, 0.2);
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
