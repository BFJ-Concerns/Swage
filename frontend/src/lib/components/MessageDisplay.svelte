<!--
  Message history display — renders the active conversation's messages.
  Handles markdown rendering, syntax highlighting, auto-scroll, and streaming.
-->
<script lang="ts">
  import DOMPurify from "dompurify";
  import { marked } from "marked";
  import { markedHighlight } from "marked-highlight";
  import hljs from "highlight.js/lib/core";
  import bash from "highlight.js/lib/languages/bash";
  import css from "highlight.js/lib/languages/css";
  import diff from "highlight.js/lib/languages/diff";
  import javascript from "highlight.js/lib/languages/javascript";
  import json from "highlight.js/lib/languages/json";
  import markdown from "highlight.js/lib/languages/markdown";
  import ini from "highlight.js/lib/languages/ini";
  import plaintext from "highlight.js/lib/languages/plaintext";
  import python from "highlight.js/lib/languages/python";
  import rust from "highlight.js/lib/languages/rust";
  import sql from "highlight.js/lib/languages/sql";
  import typescript from "highlight.js/lib/languages/typescript";
  import xml from "highlight.js/lib/languages/xml";
  import yaml from "highlight.js/lib/languages/yaml";

  import type { ConversationStore } from "../stores/conversations";
  import type { StreamingStore } from "../stores/streaming";
  import {
    DEFAULT_DISPLAY_PREFERENCES,
    type DisplayPreferences,
  } from "../display-preferences";
  import type { ToolCallView } from "../types";
  import ToolCallDisplay from "./ToolCallDisplay.svelte";

  // ---- Markdown configuration -----------------------------------------------
  // Module-level setup: runs once when this module is first imported.
  // highlight.js provides syntax colouring for fenced code blocks.
  // Register only the languages we actually expect to render in Swage output
  // so the conversation surface does not ship every lexer highlight.js offers.

  hljs.registerLanguage("bash", bash);
  hljs.registerLanguage("sh", bash);
  hljs.registerLanguage("shell", bash);
  hljs.registerLanguage("css", css);
  hljs.registerLanguage("diff", diff);
  hljs.registerLanguage("javascript", javascript);
  hljs.registerLanguage("js", javascript);
  hljs.registerLanguage("json", json);
  hljs.registerLanguage("ini", ini);
  hljs.registerLanguage("markdown", markdown);
  hljs.registerLanguage("md", markdown);
  hljs.registerLanguage("plaintext", plaintext);
  hljs.registerLanguage("text", plaintext);
  hljs.registerLanguage("txt", plaintext);
  hljs.registerLanguage("python", python);
  hljs.registerLanguage("py", python);
  hljs.registerLanguage("rust", rust);
  hljs.registerLanguage("rs", rust);
  hljs.registerLanguage("sql", sql);
  hljs.registerLanguage("toml", ini);
  hljs.registerLanguage("typescript", typescript);
  hljs.registerLanguage("ts", typescript);
  hljs.registerLanguage("html", xml);
  hljs.registerLanguage("xml", xml);
  hljs.registerLanguage("yaml", yaml);
  hljs.registerLanguage("yml", yaml);

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
    // DOMPurify sanitises the output to prevent XSS via crafted tool results
    // or prompt injection carrying malicious HTML/script payloads.
    const raw = marked.parse(content) as string;
    return DOMPurify.sanitize(raw);
  }

  // ---- Props ----------------------------------------------------------------

  interface Props {
    /** Injected by App.svelte once all stores are wired. */
    store?: ConversationStore;
    /** Injected by App.svelte — provides streaming state. */
    streamingStore?: StreamingStore;
    displayPreferences?: DisplayPreferences;
  }

  let { store, streamingStore, displayPreferences = DEFAULT_DISPLAY_PREFERENCES }: Props = $props();

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
    void (store?.activeConversation?.messages?.length ?? 0);
    void (streamingStore?.streamedContent?.length ?? 0);

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
  const shouldRenderMarkdown = $derived(displayPreferences.conversation_rendering === "markdown");
  // Live tool calls for the current in-progress turn.
  const activeToolCalls = $derived(streamingStore?.activeToolCalls ?? []);

  function shouldShowModel(role: "user" | "assistant"): boolean {
    return (
      displayPreferences.message_model_display === "all" ||
      (displayPreferences.message_model_display === "assistant" && role === "assistant")
    );
  }
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
            {#if shouldRenderMarkdown}
              <!-- marked output is trusted (server-generated content only). -->
              <!-- eslint-disable-next-line svelte/no-at-html-tags -->
              {@html renderMarkdown(message.content)}
            {:else}
              <div class="plain-body">{message.content}</div>
            {/if}

            {#if message.turn_state === "interrupted" || message.turn_state === "failed"}
              <!-- Subtle notice below the partial response. -->
              <p class="interrupted-notice">↩ Response interrupted</p>
            {/if}
          </div>
        {/if}
        {#if shouldShowModel(message.role)}
          <div class="message-model">Model not reported</div>
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
          {#if shouldRenderMarkdown}
            <!-- Incremental markdown render — re-parses on each token. -->
            <!-- eslint-disable-next-line svelte/no-at-html-tags -->
            {@html renderMarkdown(streamedContent)}
          {:else}
            <div class="plain-body">{streamedContent}</div>
          {/if}

          {#if isStreaming}
            <!-- Blinking cursor to signal active generation. -->
            <span class="cursor" aria-hidden="true">▌</span>
          {/if}
        </div>
      </div>
    {/if}

    <!-- TODO Emit provider retry events for inline retry chips -->
    <!-- Surface retry attempt, wait timer, and provider name so workup view 07 can render live. -->
    <!-- --- -->
    <!--
      Inline retry chip — workup view 07. Pinned to the assistant turn that's
      currently retrying so the failure stays local rather than splashing
      across the shell. Today the chip renders only as a placeholder when a
      turn ends in error: the provider chain has no per-attempt events, so
      we can't render the live "retry 3 of 5" state the workup specifies.

      Emit per-attempt provider events (`provider_retry_started`,
      `provider_retry_settled`) so the chip can render real attempt
      counts, the wait timer, and the active provider name. The retry
      decorator already tracks attempts internally; surface them to the
      WS observability channel.
      Why: workup view 07 specifies an inline retry chip with
      `WAIT 4s · provider · anthropic`; without per-attempt events the
      chip is stuck showing placeholder copy.
      How to apply: add a "retry" subscription to the WS layer, emit on
      each attempt, and bind here.
      ---
    -->
    {#if turnError}
      <div class="inline-retry shell" role="status">
        <span class="retry-mark">SHELL</span>
        <span class="retry-label">
          Inline retry chip — provider per-attempt events not yet wired.
        </span>
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
  /*
    Riso conversation surface. Messages are not bubbles in the design — they
    are short ink-coloured passages prefixed by an uppercased "WHO" eyebrow,
    bordered with a single coloured rule on the left:
      USER       → fluoro orange rule + faint orange wash
      ASSISTANT  → ink rule + faint ink wash
      TOOL       → dashed orange rule + transparent (mono type)
  */

  .message-display {
    flex: 1;
    overflow-y: auto;
    padding: 18px 22px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 0;
    background: var(--paper);
  }

  /* ---- Empty state -------------------------------------------------------- */

  .empty-state {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
  }

  .empty-state p {
    margin: 0;
  }

  /* ---- Message rows ------------------------------------------------------- */

  .message {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  /* The "WHO" eyebrow sits above the bubble. We don't have a separate slot in
     the markup, so the bubble's pseudo-element shows the role label.
     User messages get the orange ink colour; assistant messages stay navy. */

  .message::before {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.2em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .message.user::before {
    content: "You";
    color: var(--ink-orange);
  }

  .message.assistant::before {
    content: "Assistant";
  }

  /* ---- Message content ---------------------------------------------------- */

  .message-content {
    padding: 10px 14px;
    border-left: 3px solid var(--ink);
    background: rgba(14, 59, 102, 0.06);
    font-family: var(--font-body);
    font-size: 13.5px;
    line-height: 1.55;
    color: var(--ink);
    word-break: break-word;
    max-width: 80ch;
  }

  :global(body.display-density-comfortable) .message-display {
    gap: 18px;
  }

  :global(body.display-density-comfortable) .message-content {
    padding: 14px 18px;
    font-size: 14.5px;
    line-height: 1.7;
  }

  .user-content {
    border-left-color: var(--ink-orange);
    background: rgba(240, 106, 58, 0.08);
    /* User-written text preserves manual line breaks. */
    white-space: pre-wrap;
  }

  .plain-body {
    white-space: pre-wrap;
  }

  .message-model {
    width: fit-content;
    margin-top: 2px;
    padding: 3px 6px;
    border: 1px solid var(--rule-soft);
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .assistant-content.interrupted {
    /* Dashed orange rule replaces the solid ink rule when a turn was cut. */
    border-left-style: dashed;
    border-left-color: var(--accent);
  }

  /* ---- Streaming cursor --------------------------------------------------- */

  .cursor {
    display: inline-block;
    animation: blink 1s step-start infinite;
    color: var(--ink-orange);
    font-family: var(--font-mono);
  }

  @keyframes blink {
    0%, 100% { opacity: 1; }
    50% { opacity: 0; }
  }

  /* ---- Interrupted turn notice -------------------------------------------- */

  .interrupted-notice {
    margin: 8px 0 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-orange);
  }

  /* ---- Inline retry chip (workup view 07) -------------------------------- */

  .inline-retry {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    background: var(--paper);
    border: 1px solid var(--accent);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    line-height: 1;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink);
    max-width: 80ch;
  }

  .retry-mark {
    padding: 2px 6px;
    background: var(--accent);
    color: var(--paper);
    font-size: 9px;
    letter-spacing: 0.18em;
  }

  .retry-label {
    color: var(--ink-soft);
    text-transform: none;
    font-family: var(--font-body);
    font-weight: 400;
    font-size: 12px;
    letter-spacing: 0;
  }

  /* ---- Inline turn error -------------------------------------------------- */

  .turn-error {
    align-self: flex-start;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: rgba(240, 106, 58, 0.1);
    border-left: 3px solid var(--accent);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-orange);
    max-width: 80ch;
  }

  .error-icon {
    flex-shrink: 0;
  }

  /* ---- Markdown body ------------------------------------------------------ */
  /* Scoped via :global() to reach into {@html} output. */

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
    font-family: var(--font-display);
    font-weight: 400;
    line-height: 1.1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink-orange);
  }

  .markdown-body :global(h1) { font-size: 22px; }
  .markdown-body :global(h2) { font-size: 19px; }
  .markdown-body :global(h3) { font-size: 17px; }
  .markdown-body :global(h4) { font-size: 15px; }

  .markdown-body :global(ul),
  .markdown-body :global(ol) {
    margin: 0.5em 0;
    padding-left: 1.5em;
  }

  .markdown-body :global(li) {
    margin: 0.2em 0;
  }

  .markdown-body :global(code) {
    font-family: var(--font-mono);
    font-size: 0.85em;
    background: rgba(240, 106, 58, 0.12);
    border: 1px solid rgba(240, 106, 58, 0.45);
    padding: 0.05em 0.35em;
    color: var(--ink-orange);
  }

  .markdown-body :global(pre) {
    background: var(--paper-deep);
    color: var(--ink);
    border-left: 3px solid var(--ink);
    padding: 12px 16px;
    overflow-x: auto;
    margin: 0.6em 0;
    font-family: var(--font-mono);
  }

  .markdown-body :global(pre code) {
    background: none;
    border: none;
    padding: 0;
    color: inherit;
    font-size: 0.85em;
  }

  .markdown-body :global(blockquote) {
    border-left: 3px dashed var(--accent);
    margin: 0.5em 0;
    padding-left: 1em;
    color: var(--ink-soft);
  }

  .markdown-body :global(a) {
    color: var(--ink-orange);
    text-decoration: underline;
    text-decoration-thickness: 1px;
    text-underline-offset: 2px;
  }

  .markdown-body :global(table) {
    border-collapse: collapse;
    width: 100%;
    margin: 0.5em 0;
    font-size: 0.875em;
    border: 1px solid var(--ink);
  }

  .markdown-body :global(th),
  .markdown-body :global(td) {
    border: 1px solid var(--rule-soft);
    padding: 6px 10px;
    text-align: left;
  }

  .markdown-body :global(th) {
    background: var(--ink);
    color: var(--paper);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }
</style>
