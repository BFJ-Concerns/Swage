<!--
  Message history display — renders the active conversation's messages.
  Handles markdown rendering, syntax highlighting, auto-scroll, and streaming.
-->
<script lang="ts">
  import { marked } from "marked";
  import { markedHighlight } from "marked-highlight";
  import hljs from "highlight.js";

  import type { ConversationStore } from "../stores/conversations";
  import type { StreamingStore } from "../stores/streaming";
  import type { ToolCallView } from "../types";
  import ToolCallDisplay from "./ToolCallDisplay.svelte";

  // ---- Markdown configuration -----------------------------------------------
  // Module-level setup: runs once when this module is first imported.
  // highlight.js provides syntax colouring for fenced code blocks.

  marked.use(
    markedHighlight({
      langPrefix: "hljs language-",
      highlight(code, lang) {
        // Fall back to plain text if the language isn't registered in hljs.
        const language = hljs.getLanguage(lang) ? lang : "plaintext";
        return hljs.highlight(code, { language }).value;
      },
    }),
  );

  /**
   * Build a display tree from a flat tool_calls list returned by the API.
   *
   * The API returns all tool calls in insertion order with parent_call_id set on
   * sub-tool calls. This function groups sub-calls under their parent so
   * ToolCallDisplay can render them recursively.
   */
  function buildToolCallTree(calls: ToolCallView[]): ToolCallView[] {
    const roots: ToolCallView[] = [];
    for (const call of calls) {
      if (!call.parent_call_id) {
        // Top-level call — attach any matching sub-calls.
        const subs = calls.filter((c) => c.parent_call_id === call.call_id);
        roots.push(subs.length > 0 ? { ...call, sub_calls: subs } : call);
      }
    }
    return roots;
  }

  function renderMarkdown(content: string): string {
    // marked.parse is synchronous when no async extensions are loaded.
    return marked.parse(content) as string;
  }

  // ---- Props ----------------------------------------------------------------

  interface Props {
    /** Injected by App.svelte once all stores are wired. */
    store?: ConversationStore;
    /** Injected by App.svelte — provides streaming state. */
    streamingStore?: StreamingStore;
  }

  let { store, streamingStore }: Props = $props();

  // ---- Auto-scroll ----------------------------------------------------------

  let scrollEl: HTMLDivElement | undefined = $state();

  // Paused when the user has scrolled up to read earlier messages.
  // Resumes automatically when they scroll back within the 50 px threshold.
  let userScrolledUp = $state(false);

  function handleScroll() {
    if (!scrollEl) return;
    const gap =
      scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight;
    userScrolledUp = gap > 50;
  }

  // Scroll to the bottom whenever the message list or streamed content grows.
  // requestAnimationFrame lets the DOM paint the new content first.
  $effect(() => {
    // These accesses establish reactive tracking once the stores use $state.
    const _msgCount = store?.activeConversation?.messages?.length ?? 0;
    const _streamLen = streamingStore?.streamedContent?.length ?? 0;

    if (!userScrolledUp && scrollEl) {
      requestAnimationFrame(() => {
        if (scrollEl) scrollEl.scrollTop = scrollEl.scrollHeight;
      });
    }
  });

  // ---- Derived display state ------------------------------------------------
  // These will become fully reactive once the stores are converted to $state.

  const messages = $derived(store?.activeConversation?.messages ?? []);
  const hasConversation = $derived(store?.activeConversation != null);
  const isStreaming = $derived(streamingStore?.isStreaming ?? false);
  const streamedContent = $derived(streamingStore?.streamedContent ?? "");
  const turnError = $derived(streamingStore?.turnError ?? null);
  // Live tool calls for the current in-progress turn.
  const activeToolCalls = $derived(streamingStore?.activeToolCalls ?? []);
</script>

<div
  class="message-display"
  bind:this={scrollEl}
  onscroll={handleScroll}
  role="log"
  aria-label="Conversation messages"
  aria-live="polite"
>
  {#if !hasConversation}
    <!-- No active conversation — first-time user or sidebar empty state. -->
    <div class="empty-state">
      <p>Select a conversation or start a new one.</p>
    </div>
  {:else if messages.length === 0 && !isStreaming}
    <!-- Conversation created but no messages exchanged yet. -->
    <div class="empty-state">
      <p>Send a message to start.</p>
    </div>
  {:else}
    <!-- ---- Committed messages from conversation history ---- -->
    {#each messages as message (message.timestamp)}
      <div
        class="message"
        class:user={message.role === "user"}
        class:assistant={message.role === "assistant"}
      >
        {#if message.role === "user"}
          <!-- User messages render as plain text (no markdown injection). -->
          <div class="message-content user-content">
            {message.content}
          </div>
        {:else}
          <!-- Assistant messages render as markdown. -->
          <div
            class="message-content assistant-content markdown-body"
            class:interrupted={message.turn_state === "interrupted" ||
              message.turn_state === "failed"}
          >
            <!-- marked output is trusted (server-generated content only). -->
            <!-- eslint-disable-next-line svelte/no-at-html-tags -->
            {@html renderMarkdown(message.content)}

            {#if message.turn_state === "interrupted" || message.turn_state === "failed"}
              <!-- Subtle notice below the partial response. -->
              <p class="interrupted-notice">↩ Response interrupted</p>
            {/if}
          </div>
        {/if}
      </div>

      <!-- ---- Persisted tool calls for this message (history) ---- -->
      <!-- The API returns a flat list; buildToolCallTree groups sub-calls under
           their parent so ToolCallDisplay can render nesting recursively. -->
      {#if message.role === "assistant" && message.tool_calls && message.tool_calls.length > 0}
        <div class="tool-calls-history">
          {#each buildToolCallTree(message.tool_calls) as toolCall (toolCall.call_id)}
            <ToolCallDisplay {toolCall} isLive={false} />
          {/each}
        </div>
      {/if}
    {/each}

    <!-- ---- Live streaming content: grows as tokens arrive ---- -->
    {#if isStreaming || streamedContent}
      <div class="message assistant streaming">
        <div class="message-content assistant-content markdown-body">
          <!-- Incremental markdown render — re-parses on each token. -->
          <!-- eslint-disable-next-line svelte/no-at-html-tags -->
          {@html renderMarkdown(streamedContent)}

          {#if isStreaming}
            <!-- Blinking cursor to signal active generation. -->
            <span class="cursor" aria-hidden="true">▌</span>
          {/if}
        </div>
      </div>
    {/if}

    <!-- ---- Live tool calls: updated in real-time as the agent works ---- -->
    <!-- isLive is true for calls still running (no result yet), false once done. -->
    {#if activeToolCalls.length > 0}
      <div class="tool-calls-live">
        {#each activeToolCalls as toolCall (toolCall.call_id)}
          <ToolCallDisplay {toolCall} isLive={toolCall.status === null} />
        {/each}
      </div>
    {/if}

    <!-- ---- Turn error: shown inline after any partial response ---- -->
    {#if turnError}
      <div class="turn-error" role="alert">
        <span class="error-icon" aria-hidden="true">⚠</span>
        <span class="error-message">{turnError.message}</span>
      </div>
    {/if}
  {/if}
</div>

<style>
  /* ---- Container ---------------------------------------------------------- */

  .message-display {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    /* Prevents flex item from overflowing its parent at constrained heights. */
    min-height: 0;
  }

  /* ---- Empty state -------------------------------------------------------- */

  .empty-state {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #999;
    font-size: 0.875rem;
  }

  .empty-state p {
    margin: 0;
  }

  /* ---- Message rows ------------------------------------------------------- */

  .message {
    display: flex;
    /* Limit bubble width so long messages stay readable. */
    max-width: 80%;
  }

  .message.user {
    /* Right-align the user's messages. */
    align-self: flex-end;
    flex-direction: row-reverse;
  }

  .message.assistant {
    align-self: flex-start;
  }

  /* ---- Message bubbles ---------------------------------------------------- */

  .message-content {
    padding: 10px 14px;
    border-radius: 12px;
    font-size: 0.9rem;
    line-height: 1.5;
    word-break: break-word;
  }

  .user-content {
    background: #1a1a1a;
    color: #fff;
    /* Rounded on all sides except the bottom-right, which points toward the user. */
    border-radius: 12px 12px 2px 12px;
    /* Preserve intentional newlines — the user controls their own line breaks. */
    white-space: pre-wrap;
  }

  .assistant-content {
    background: #f5f5f5;
    color: #222;
    border-radius: 2px 12px 12px 12px;
  }

  .assistant-content.interrupted {
    /* Amber left border flags that the response didn't complete normally. */
    border-left: 3px solid #d97706;
    padding-left: 11px;
  }

  /* ---- Streaming cursor --------------------------------------------------- */

  .cursor {
    display: inline-block;
    animation: blink 1s step-start infinite;
    color: #555;
    font-size: 0.8em;
  }

  @keyframes blink {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0;
    }
  }

  /* ---- Interrupted turn notice -------------------------------------------- */

  .interrupted-notice {
    margin: 8px 0 0;
    font-size: 0.75rem;
    color: #d97706;
    font-style: italic;
  }

  /* ---- Inline turn error -------------------------------------------------- */

  .turn-error {
    align-self: flex-start;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    background: #fff8f0;
    border: 1px solid #fcd9a0;
    border-radius: 8px;
    font-size: 0.8rem;
    color: #92400e;
    max-width: 80%;
  }

  .error-icon {
    flex-shrink: 0;
  }

  /* ---- Markdown body ------------------------------------------------------ */
  /* Scoped via :global() to reach into the {@html} output. */

  .markdown-body :global(p) {
    margin: 0 0 0.75em;
  }

  .markdown-body :global(p:last-child) {
    margin-bottom: 0;
  }

  .markdown-body :global(h1),
  .markdown-body :global(h2),
  .markdown-body :global(h3),
  .markdown-body :global(h4) {
    margin: 0.75em 0 0.25em;
    font-weight: 600;
    line-height: 1.25;
  }

  .markdown-body :global(ul),
  .markdown-body :global(ol) {
    margin: 0.5em 0;
    padding-left: 1.5em;
  }

  .markdown-body :global(li) {
    margin: 0.2em 0;
  }

  .markdown-body :global(code) {
    font-family:
      ui-monospace,
      SFMono-Regular,
      "SF Mono",
      Menlo,
      monospace;
    font-size: 0.85em;
    background: rgba(0, 0, 0, 0.06);
    padding: 0.1em 0.3em;
    border-radius: 3px;
  }

  .markdown-body :global(pre) {
    background: #1e1e1e;
    color: #d4d4d4;
    border-radius: 8px;
    padding: 12px 16px;
    overflow-x: auto;
    margin: 0.5em 0;
  }

  .markdown-body :global(pre code) {
    /* Code inside pre: inherit colour from hljs, remove inline-code background. */
    background: none;
    padding: 0;
    font-size: 0.85em;
    color: inherit;
  }

  .markdown-body :global(blockquote) {
    border-left: 3px solid #d0d0d0;
    margin: 0.5em 0;
    padding-left: 1em;
    color: #666;
  }

  .markdown-body :global(a) {
    color: #6366f1;
    text-decoration: underline;
  }

  .markdown-body :global(table) {
    border-collapse: collapse;
    width: 100%;
    margin: 0.5em 0;
    font-size: 0.875em;
  }

  .markdown-body :global(th),
  .markdown-body :global(td) {
    border: 1px solid #e0e0e0;
    padding: 6px 10px;
    text-align: left;
  }

  .markdown-body :global(th) {
    background: #f5f5f5;
    font-weight: 600;
  }
</style>
