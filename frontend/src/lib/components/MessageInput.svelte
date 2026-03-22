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

<div class="message-input">
  <textarea
    bind:this={textarea}
    bind:value={content}
    placeholder="Send a message… (Shift+Enter for a new line)"
    rows="1"
    onkeydown={handleKeydown}
  ></textarea>

  {#if store?.isStreaming}
    <!--
      Stop replaces Send during streaming. The button is always enabled —
      stopping a running turn is always a valid action.
    -->
    <button class="stop-btn" onclick={() => store!.cancelTurn()}>
      ■ Stop
    </button>
  {:else}
    <button
      class="send-btn"
      onclick={send}
      disabled={!content.trim() || !store}
    >
      Send
    </button>
  {/if}
</div>

<style>
  .message-input {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid #e0e0e0;
    background: #fff;
    /* Anchored to the bottom of the conversation area — flex handles this
       from AppLayout.svelte's column layout. */
    flex-shrink: 0;
  }

  /* -------------------------------------------------------------------------
     Textarea
  ------------------------------------------------------------------------- */

  textarea {
    flex: 1;
    resize: none;
    border: 1px solid #d0d0d0;
    border-radius: 8px;
    padding: 8px 12px;
    font-family: inherit;
    font-size: 0.9375rem;
    line-height: 1.5;
    /* min/max control the auto-grow range. */
    min-height: 40px;
    max-height: 160px;
    /* Scrollbar only appears once content exceeds max-height. */
    overflow-y: auto;
    outline: none;
  }

  textarea:focus {
    border-color: #6366f1;
    box-shadow: 0 0 0 2px rgb(99 102 241 / 0.15);
  }

  /* -------------------------------------------------------------------------
     Action buttons
  ------------------------------------------------------------------------- */

  .send-btn,
  .stop-btn {
    padding: 8px 16px;
    border: none;
    border-radius: 8px;
    font-size: 0.875rem;
    font-weight: 500;
    cursor: pointer;
    /* Align with the bottom of the textarea, however tall it grows. */
    align-self: flex-end;
  }

  .send-btn {
    background: #1a1a1a;
    color: #fff;
  }

  .send-btn:hover:not(:disabled) {
    background: #333;
  }

  .send-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .stop-btn {
    background: #fee2e2;
    color: #991b1b;
    border: 1px solid #fca5a5;
  }

  .stop-btn:hover {
    background: #fecaca;
  }
</style>
