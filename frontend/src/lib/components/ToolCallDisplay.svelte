<!--
  Tool call activity display — renders a single tool invocation inline
  in the conversation. Shows tool name, arguments summary, result (collapsible),
  duration, and success/error status.
-->
<script lang="ts">
  import type { ToolCallView } from "../types";
  import { onDestroy } from "svelte";
  // Self-import for recursive sub-call rendering.
  import ToolCallDisplay from "./ToolCallDisplay.svelte";

  // ---- Props ----------------------------------------------------------------

  interface Props {
    toolCall: ToolCallView;
    // true for live calls in the current turn; false for persisted history.
    isLive: boolean;
  }

  let { toolCall, isLive }: Props = $props();

  // ---- State ----------------------------------------------------------------

  // Whether the result block is currently expanded. Default depends on length.
  let isExpanded = $state(false);

  // Elapsed seconds displayed while the call is in-progress.
  let elapsedSeconds = $state(0);

  // Timer handle — only active while isLive and no result yet.
  let timer: ReturnType<typeof setInterval> | null = null;

  // ---- Derived values -------------------------------------------------------

  // A call is in-progress when it is live and has no result yet.
  const isInProgress = $derived(isLive && toolCall.status === null);

  // True once the result is long enough to warrant collapsing.
  const resultText = $derived(
    typeof toolCall.result === "string"
      ? toolCall.result
      : toolCall.result != null
        ? JSON.stringify(toolCall.result, null, 2)
        : "",
  );
  const isLongResult = $derived(resultText.length >= 500);

  // Initialise expansion based on result length.
  $effect(() => {
    isExpanded = !isLongResult;
  });

  // ---- Elapsed timer --------------------------------------------------------

  $effect(() => {
    if (isInProgress) {
      // Reset and start counting when a new in-progress call arrives.
      elapsedSeconds = 0;
      timer = setInterval(() => {
        elapsedSeconds += 1;
      }, 1_000);
    } else {
      // Stop counting once the call completes or is no longer live.
      if (timer !== null) {
        clearInterval(timer);
        timer = null;
      }
    }
  });

  onDestroy(() => {
    if (timer !== null) clearInterval(timer);
  });

  // ---- Helpers --------------------------------------------------------------

  /** Extract a human-readable summary from the tool's arguments. */
  function argsSummary(tool: string, args: Record<string, unknown>): string {
    // Shell: show the command string.
    if (tool === "shell" && typeof args.command === "string") {
      return args.command;
    }
    // File tools: show the path.
    if (
      (tool === "read_file" ||
        tool === "write_file" ||
        tool === "search_file" ||
        tool === "list_files") &&
      typeof args.path === "string"
    ) {
      return args.path;
    }
    // Web search: show the query.
    if (tool === "web_search" && typeof args.query === "string") {
      return args.query;
    }
    // URL fetch: show the URL.
    if (tool === "fetch_url" && typeof args.url === "string") {
      return args.url;
    }
    // Generic fallback: first key=value pair.
    const firstKey = Object.keys(args)[0];
    if (firstKey !== undefined) {
      return `${firstKey}=${String(args[firstKey])}`;
    }
    return "";
  }

  /** Format a duration in milliseconds to a compact human-readable string. */
  function formatDuration(ms: number): string {
    if (ms < 1_000) return `${ms}ms`;
    const s = (ms / 1_000).toFixed(1);
    return `${s}s`;
  }
</script>

<div
  class="tool-call"
  class:in-progress={isInProgress}
  class:completed={toolCall.status === "success"}
  class:error={toolCall.status === "error"}
>
  <!-- ---- Header: tool identity and inline status ---- -->
  <div class="tool-header">
    {#if isInProgress}
      <!-- Animated indicator for in-flight calls. -->
      <span class="pulse" aria-hidden="true"></span>
    {:else if toolCall.status === "success"}
      <span class="status-icon success" aria-hidden="true">✓</span>
    {:else if toolCall.status === "error"}
      <span class="status-icon error-icon" aria-hidden="true">✗</span>
    {/if}

    <span class="tool-name">{toolCall.tool}</span>

    {#if argsSummary(toolCall.tool, toolCall.arguments)}
      <span class="args-summary">{argsSummary(toolCall.tool, toolCall.arguments)}</span>
    {/if}

    {#if isInProgress}
      <!-- Live elapsed counter — updates every second. -->
      <span class="elapsed" aria-live="polite">{elapsedSeconds}s</span>
    {:else if toolCall.duration_ms !== null}
      <span class="duration">{formatDuration(toolCall.duration_ms)}</span>
    {/if}
  </div>

  <!-- ---- Result block: only shown once the call has completed ---- -->
  {#if toolCall.result !== null}
    <div class="tool-result" class:collapsed={isLongResult && !isExpanded}>
      {#if isLongResult && !isExpanded}
        <!-- Collapsed view: preview of first ~3 lines. -->
        <pre class="result-content result-preview">{resultText.split("\n").slice(0, 3).join("\n")}</pre>
      {:else}
        <pre class="result-content">{resultText}</pre>
      {/if}

      {#if toolCall.truncated}
        <!-- Notify the user that the full output was larger. -->
        <span class="truncated-notice">
          Output truncated
          {#if toolCall.total_bytes !== null}
            ({(toolCall.total_bytes / 1_024).toFixed(1)} KB total)
          {/if}
        </span>
      {/if}
    </div>

    {#if isLongResult}
      <!-- Toggle sits below the result block so it doesn't shift the header. -->
      <button
        class="toggle-btn"
        onclick={() => {
          isExpanded = !isExpanded;
        }}
        aria-expanded={isExpanded}
      >
        {isExpanded ? "Collapse" : "Expand"}
      </button>
    {/if}
  {/if}

  <!-- ---- Sub-calls: tools invoked within this call (e.g. from python_execute) ---- -->
  {#if toolCall.sub_calls && toolCall.sub_calls.length > 0}
    <div class="sub-calls">
      {#each toolCall.sub_calls as subCall (subCall.call_id)}
        <!-- Each sub-call has its own independent expand/collapse and elapsed timer. -->
        <ToolCallDisplay toolCall={subCall} {isLive} />
      {/each}
    </div>
  {/if}
</div>

<style>
  /*
    Tool calls render in the riso "tool" voice: dashed orange ink, mono type,
    no bubble background. The dashed left rule signals "the system speaks".
  */

  .tool-call {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px 0 6px 12px;
    border-left: 3px dashed var(--accent);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.4;
    color: var(--ink-orange);
    max-width: 100%;
  }

  .tool-call.completed {
    color: var(--ink);
    border-left-color: var(--ink);
    border-left-style: dashed;
  }

  .tool-call.error {
    /* Solid orange rule signals failure. */
    border-left-style: solid;
  }

  /* ---- Header row --------------------------------------------------------- */

  .tool-header {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    font-weight: 500;
    letter-spacing: 0.04em;
  }

  .tool-name {
    text-transform: uppercase;
    letter-spacing: 0.1em;
    font-weight: 500;
  }

  .args-summary {
    color: var(--ink-soft);
    font-weight: 400;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 48ch;
    /* Args summary inherits the mono family from .tool-call. */
  }

  /* ---- Status icons and indicators --------------------------------------- */

  .status-icon {
    font-size: 11px;
    flex-shrink: 0;
  }

  .status-icon.success {
    color: var(--ink);
  }

  .status-icon.error-icon {
    color: var(--accent);
  }

  /* Pulsing pip for in-progress calls — square not round, in keeping with
     the rest of the riso vocabulary. */
  .pulse {
    display: inline-block;
    width: 8px;
    height: 8px;
    background: var(--accent);
    flex-shrink: 0;
    animation: pulse 1.4s ease-in-out infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.35; }
  }

  /* ---- Duration and elapsed counter -------------------------------------- */

  .duration,
  .elapsed {
    margin-left: auto;
    color: var(--ink-soft);
    font-size: 10px;
    letter-spacing: 0.08em;
    flex-shrink: 0;
  }

  /* ---- Result block ------------------------------------------------------- */

  .tool-result {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .result-content {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 11.5px;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    background: var(--paper-deep);
    color: var(--ink);
    padding: 8px 10px;
    border-left: 2px solid var(--ink);
    max-height: 300px;
    overflow-y: auto;
  }

  .result-preview {
    max-height: none;
  }

  /* ---- Truncation notice -------------------------------------------------- */

  .truncated-notice {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-orange);
  }

  /* ---- Sub-calls ---------------------------------------------------------- */

  .sub-calls {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-left: 14px;
    border-left: 2px dotted var(--rule-soft);
    margin-top: 4px;
  }

  /* ---- Expand / collapse toggle ------------------------------------------ */

  .toggle-btn {
    align-self: flex-start;
    padding: 3px 10px;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    background: transparent;
    border: 1px solid var(--ink);
    color: var(--ink);
    cursor: pointer;
  }

  .toggle-btn:hover {
    background: var(--ink);
    color: var(--paper);
  }
</style>
