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
  import { authorLabel, isPersonsMessage } from "../message-authorship";
  import {
    DEFAULT_DISPLAY_PREFERENCES,
    type DisplayPreferences,
  } from "../display-preferences";
  import type { ToolCallView } from "../types";
  import ToolCallDisplay from "./ToolCallDisplay.svelte";
  import TurnContextInspectorTrigger from "./TurnContextInspectorTrigger.svelte";

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
  let inspectorTarget: HTMLDivElement | undefined = $state();

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
    void (streamingStore?.streamFor?.(store?.activeConversation?.id ?? null).streamedContent.length ?? 0);

    if (!userScrolledUp && scrollEl) {
      requestAnimationFrame(() => {
        if (scrollEl) scrollEl.scrollTop = scrollEl.scrollHeight;
      });
    }
  });

  // ---- Derived display state ------------------------------------------------
  // These will become fully reactive once the stores are converted to $state.

  const messages = $derived(store?.activeConversation?.messages ?? []);
  const queued = $derived(store?.activeConversation?.queued ?? []);
  const hasConversation = $derived(store?.activeConversation != null);
  const stream = $derived(
    streamingStore?.streamFor?.(store?.activeConversation?.id ?? null),
  );
  const isStreaming = $derived(stream?.isStreaming ?? streamingStore?.isStreaming ?? false);
  const streamedContent = $derived(stream?.streamedContent ?? streamingStore?.streamedContent ?? "");
  const turnError = $derived(stream?.turnError ?? streamingStore?.turnError ?? null);
  const providerRetry = $derived(stream?.providerRetry ?? streamingStore?.providerRetry ?? null);
  const retryWaitSeconds = $derived(
    providerRetry ? Math.max(1, Math.ceil(providerRetry.wait_ms / 1000)) : 0,
  );
  const shouldRenderMarkdown = $derived(displayPreferences.conversation_rendering === "markdown");
  // Live tool calls for the current in-progress turn.
  const activeToolCalls = $derived(stream?.activeToolCalls ?? streamingStore?.activeToolCalls ?? []);

  function shouldShowModel(role: "user" | "assistant"): boolean {
    return (
      displayPreferences.message_model_display === "all" ||
      (displayPreferences.message_model_display === "assistant" && role === "assistant")
    );
  }

  // ---- Assistive status announcement ----------------------------------------
  // The message history itself is NOT a live region: the streaming renderer
  // rewrites the in-progress assistant message's innerHTML on every token, and
  // a live region on that container would queue each token for announcement —
  // an unusable wall of noise for screen-reader users (UI audit f-1).
  // Instead we derive a single coarse status string from the streaming store's
  // coarse flags. These flags only change on turn boundaries, so the dedicated
  // live region below updates a handful of times per turn, not per token.
  const streamingStatus = $derived(
    turnError
      ? `Error: ${turnError.message}`
      : providerRetry
        ? `Retrying provider: attempt ${providerRetry.attempt} of ${providerRetry.max_retries}`
        : isStreaming
          ? "Generating response"
          : streamedContent
            ? "Response complete"
            : "",
  );
</script>

<!--
  The history container uses role="region", NOT role="log". role="log" carries
  an implicit aria-live="polite", which would re-announce every streamed token
  (UI audit f-1). A neutral region lets sighted users read the stream while a
  dedicated sr-only live region below announces only coarse status.
-->
<div
  class="message-display"
  bind:this={scrollEl}
  onscroll={handleScroll}
  role="region"
  aria-label="Conversation messages"
>
  <!-- Dedicated, throttled assistive status — see streamingStatus above. -->
  <div class="sr-only" aria-live="polite" aria-atomic="true">{streamingStatus}</div>
  {#if !hasConversation}
    <!-- No active conversation — first-time user, or every conversation
         deleted. The help copy points at the channel's own session controls,
         which travel with it onto every surface, rather than at a sidebar
         list that only one surface has.
         Three registers, not one: Bebas display for the headline (caps are
         fine in display type), sentence-case body for the help copy, and
         uppercase mono reserved for the small category label. UI audit f-10. -->
    <div class="empty-state">
      <p class="empty-headline">Nothing here yet</p>
      <p class="empty-help">Pick up a conversation from the selector above, or start a new one there.</p>
      <p class="empty-label">Empty · Conversation</p>
    </div>
  {:else if messages.length === 0 && !isStreaming}
    <!-- Conversation created but no messages exchanged yet. -->
    <div class="empty-state">
      <p class="empty-headline">No messages</p>
      <p class="empty-help">Send a message below to start the conversation.</p>
    </div>
  {:else}
    <!-- ---- Committed messages from conversation history ---- -->
    {#each messages as message, messageIndex (message.timestamp)}
      <div
        class="message"
        class:user={isPersonsMessage(message)}
        class:assistant={!isPersonsMessage(message)}
        class:platform={!isPersonsMessage(message) && message.role === "user"}
        aria-label={isPersonsMessage(message) ? "Your message" : `${authorLabel(message)} message`}
      >
        <!-- The author label is real DOM text, not a CSS ::before — so it
             reaches the accessibility tree reliably across screen-reader/browser
             combos (UI audit f-14). It names who wrote the message — the person,
             the assistant, or the platform path (a schedule, a delegation) that
             composed it — never the provider-facing role, so a brief the
             platform wrote is not shown as the person's words. The person's
             messages sit on the right with mirrored borders so the sides read
             at a glance even with colour off (UI audit f-23). -->
        <span class="role-label" class:user-label={isPersonsMessage(message)}>
          {authorLabel(message)}
        </span>
        <!-- The assistant invokes tools before producing its response. Keep
             persisted calls inside that turn and ahead of its content. -->
        {#if message.role === "assistant" && message.tool_calls && message.tool_calls.length > 0}
          <div class="tool-calls-history">
            {#each buildToolCallTree(message.tool_calls) as toolCall (toolCall.call_id)}
              <ToolCallDisplay {toolCall} isLive={false} />
            {/each}
          </div>
        {/if}
        {#if isPersonsMessage(message)}
          <!-- The person's own messages render as plain text (no markdown injection). -->
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

            {#if message.turn_state === "interrupted"}
              <!-- Subtle notice below the partial response. -->
              <p class="interrupted-notice">↩ Response interrupted</p>
            {:else if message.turn_state === "failed"}
              <p class="interrupted-notice">⚠ Response failed</p>
            {/if}
          </div>
        {/if}
        {#if message.origin === "platform" && message.turn_state === "refused" && streamingStore}
          <!-- Recorded requests carry the turn id; an optimistic request is
               still the person's preceding message until the history is read. -->
          {@const request = messages.find(candidate => isPersonsMessage(candidate) && candidate.turn_id === message.turn_id) ?? messages.slice(0, messageIndex).reverse().find(candidate => isPersonsMessage(candidate) && !candidate.turn_id)}
          {#if request}
            <button class="branch-link" disabled={isStreaming} onclick={() => streamingStore?.startTurn(request.content)}>Retry request</button>
          {/if}
        {/if}
        {#if shouldShowModel(message.role) && message.model}
          <div class="message-model">{message.model}</div>
        {/if}
        {#if message.role === "assistant" && message.turn_id}
          <!-- Inspectability affordance: every agent turn's assembled
               context is one click away, per the platform's
               reconstructable-afterwards claim. -->
          <TurnContextInspectorTrigger
            turnId={message.turn_id}
            presentation="message"
            {inspectorTarget}
          />
        {/if}
        {#if message.id !== undefined && store?.branchFrom}
          <!-- Any recorded message is a point to branch from: the branch
               carries the history up to here and diverges after it, so
               exploring an alternative never overwrites this thread. -->
          <button
            class="branch-link"
            aria-label="Branch from this message"
            onclick={() => store?.branchFrom?.(message.id!)}
          >Branch from here</button>
        {/if}
      </div>
    {/each}

    <!-- ---- Live tool calls: updated in real-time as the agent works ---- -->
    <!-- isLive is true for calls still running (no result yet), false once done. -->
    {#if activeToolCalls.length > 0}
      <div class="tool-calls-live">
        {#each activeToolCalls as toolCall (toolCall.call_id)}
          <ToolCallDisplay {toolCall} isLive={toolCall.status === null} />
        {/each}
      </div>
    {/if}

    <!-- ---- Messages held for the next turn ---- -->
    {#each queued as held (held.id)}
      <div class="message queued" class:user={held.origin === "user"} aria-label="Queued message">
        <span class="role-label" class:user-label={held.origin === "user"}>
          {authorLabel({ role: "user", origin: held.origin })} · queued
        </span>
        <div class="message-content user-content queued-content">{held.content}</div>
      </div>
    {/each}

    <!-- ---- Live streaming content: grows as tokens arrive ---- -->
    {#if isStreaming || streamedContent}
      <div class="message assistant streaming" aria-label="Assistant message">
        <span class="role-label">Assistant</span>
        <div class="message-content assistant-content markdown-body">
          {#if shouldRenderMarkdown}
            <!-- Incremental markdown render — re-parses on each token. -->
            <!-- eslint-disable-next-line svelte/no-at-html-tags -->
            {@html renderMarkdown(streamedContent)}
          {:else}
            <div class="plain-body">{streamedContent}</div>
          {/if}

          {#if isStreaming}
            <!-- Blinking cursor to signal active generation. Decoration only —
                 muted ink so it stops competing with status/error colour
                 (UI audit f-17). -->
            <span class="cursor" aria-hidden="true">▌</span>
          {/if}
        </div>
      </div>
    {/if}

    {#if providerRetry}
      <div class="inline-retry" role="status">
        <span class="retry-mark">
          {providerRetry.status === "waiting"
            ? `WAIT ${retryWaitSeconds}s`
            : providerRetry.status === "resuming"
              ? "RESUME"
              : "FAILED"}
        </span>
        <span class="retry-label">
          retry {providerRetry.attempt} of {providerRetry.max_retries} · provider · {providerRetry.provider}
        </span>
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

<div class="turn-context-inspector-outlet" bind:this={inspectorTarget}></div>


<style>
  /* The branch action reads like the context link beside it: a dotted
     underline in soft ink, no button chrome, so it never competes with the
     message. */
  .branch-link {
    border: none;
    background: none;
    padding: 0;
    margin-top: 4px;
    align-self: flex-start;
    font-size: 0.75rem;
    color: var(--ink-soft);
    cursor: pointer;
    text-decoration: underline dotted;
  }
  .branch-link:hover { color: var(--ink); }
  .message.user .branch-link { align-self: flex-end; }

  /*
    Riso conversation surface. Messages are not bubbles in the design — they
    are short ink-coloured passages prefixed by an uppercased "WHO" label,
    bordered with a single coloured rule:
      ASSISTANT  → left ink rule + faint ink wash, left-aligned
      USER       → right ink-orange rule + faint orange wash, right-aligned
    Roles sit on opposite sides with mirrored borders so they read at a glance
    even with colour off (UI audit f-23).
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
  /* Three registers (UI audit f-10): display headline carries the caps burden,
     sentence-case body delivers the help, uppercase mono is reserved for the
     small category label. The old all-caps-mono help copy hurt word-shape
     recognition and was indistinguishable from a metadata label. */

  .empty-state {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    text-align: center;
    color: var(--ink);
    padding: 24px;
  }

  .empty-headline {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: var(--font-display-2);
    line-height: 1;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .empty-help {
    margin: 0;
    font-family: var(--font-body);
    font-size: var(--font-body-sm);
    line-height: 1.55;
    color: var(--ink-soft);
    max-width: 34ch;
  }

  .empty-label {
    margin: 6px 0 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  /* ---- Message rows ------------------------------------------------------- */

  .message {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  /* User messages sit on the right; assistant messages on the left. The role
     label and content both follow the side, so the border side mirrors too. */
  .message.user {
    align-items: flex-end;
  }

  /* The "WHO" label is real DOM text (UI audit f-14) — not a CSS ::before,
     which screen readers expose inconsistently. */
  .role-label {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-meta);
    line-height: 1;
    letter-spacing: 0.2em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .role-label.user-label {
    color: var(--ink-orange);
  }

  /* A message the platform wrote — a schedule's brief, a delegation's
     notice — reads on the assistant's side with a softer rule, so it is
     neither the person's words nor the model's reply at a glance. */
  .message.platform .message-content {
    border-left-color: var(--ink-soft);
  }

  /* A message held for the next turn: the person's words, not yet taken up,
     so it reads muted until the running turn ends. */
  .message.queued .queued-content {
    opacity: 0.7;
    border-style: dashed;
  }

  /* ---- Message content ---------------------------------------------------- */

  .message-content {
    padding: 10px 14px;
    border-left: 3px solid var(--ink);
    background: color-mix(in srgb, var(--ink) 6%, transparent);
    font-family: var(--font-body);
    font-size: var(--font-body-sm);
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
    font-size: var(--font-body-md);
    line-height: 1.7;
  }

  /* Assistant: left rule, left-aligned (default above). */
  .user-content {
    /* Mirror to the right side so user and assistant read as distinct roles
       even with colour off (UI audit f-23). */
    border-left: none;
    border-right: 3px solid var(--ink-orange);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
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
    font-size: var(--font-meta);
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .assistant-content.interrupted {
    /* Dashed error-coloured rule replaces the solid ink rule when a turn was
       cut. Uses --error so an interrupted turn reads as trouble, not "another
       orange thing" (UI audit f-17). */
    border-left-style: dashed;
    border-left-color: var(--error);
  }

  /* ---- Streaming cursor --------------------------------------------------- */

  .cursor {
    display: inline-block;
    animation: blink 1s step-start infinite;
    /* Decoration drops to muted ink so it stops competing with status and
       error colour (UI audit f-17). */
    color: var(--ink-soft);
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
    font-size: var(--font-meta);
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--error);
  }

  /* ---- Inline retry chip -------------------------------------------------- */

  .inline-retry {
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    background: var(--paper);
    border: 1px solid var(--error);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-label-sm);
    line-height: 1;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--ink);
    max-width: 80ch;
  }

  .retry-mark {
    padding: 2px 6px;
    background: var(--error);
    color: var(--paper);
    font-size: var(--font-meta);
    letter-spacing: 0.18em;
  }

  .retry-label {
    color: var(--ink-soft);
    text-transform: none;
    font-family: var(--font-body);
    font-weight: 400;
    font-size: var(--font-label-md);
    letter-spacing: 0;
  }

  /* ---- Inline turn error -------------------------------------------------- */

  .turn-error {
    align-self: flex-start;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: color-mix(in srgb, var(--error) 10%, transparent);
    border-left: 3px solid var(--error);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-label-sm);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--error);
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
    /* Inline code is decoration, not status — drop off the orange family so it
       stops competing with primary action and error colour (UI audit f-17). */
    font-family: var(--font-mono);
    font-size: 0.85em;
    background: var(--paper-deep);
    border: 1px solid var(--rule-faint);
    padding: 0.05em 0.35em;
    color: var(--ink);
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
    border-left: 3px dashed var(--rule-soft);
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
