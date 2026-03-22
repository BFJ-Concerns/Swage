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
  /* ---- Root container ----------------------------------------------------- */

  .tool-call {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px 10px;
    border-radius: 6px;
    font-size: 0.8125rem;
    border: 1px solid #e5e7eb;
    background: #fafafa;
    /* Compact: takes less vertical space than a message bubble. */
    max-width: 100%;
  }

  /* State-specific borders and backgrounds. */

  .tool-call.in-progress {
    border-color: #bfdbfe;
    background: #eff6ff;
  }

  .tool-call.completed {
    border-color: #bbf7d0;
    background: #f0fdf4;
  }

  /* Visually distinct error state — different border and background colour. */
  .tool-call.error {
    border-color: #fca5a5;
    background: #fff1f2;
  }

  /* ---- Header row --------------------------------------------------------- */

  .tool-header {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .tool-name {
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace;
    font-weight: 600;
    color: #374151;
  }

  .args-summary {
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace;
    color: #6b7280;
    /* Truncate long paths or commands rather than wrapping the header. */
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 40ch;
  }

  /* ---- Status icons and indicators --------------------------------------- */

  .status-icon {
    font-size: 0.75rem;
    flex-shrink: 0;
  }

  .status-icon.success {
    color: #16a34a;
  }

  .status-icon.error-icon {
    color: #dc2626;
  }

  /* Pulsing dot for in-progress calls — draws the eye without alarming. */
  .pulse {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #3b82f6;
    flex-shrink: 0;
    animation: pulse 1.4s ease-in-out infinite;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
      transform: scale(1);
    }
    50% {
      opacity: 0.4;
      transform: scale(0.85);
    }
  }

  /* ---- Duration and elapsed counter -------------------------------------- */

  .duration,
  .elapsed {
    margin-left: auto;
    color: #9ca3af;
    font-size: 0.75rem;
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
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace;
    font-size: 0.75rem;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    background: rgba(0, 0, 0, 0.04);
    border-radius: 4px;
    padding: 6px 8px;
    /* Cap height so a huge result doesn't swamp the conversation. */
    max-height: 300px;
    overflow-y: auto;
  }

  /* Collapsed: the preview pre has no height cap — it shows only a few lines. */
  .result-preview {
    max-height: none;
  }

  /* ---- Truncation notice -------------------------------------------------- */

  .truncated-notice {
    font-size: 0.7rem;
    color: #d97706;
    font-style: italic;
  }

  /* ---- Sub-calls (tools invoked within python_execute or similar) --------- */

  .sub-calls {
    display: flex;
    flex-direction: column;
    gap: 4px;
    /* Visual indentation: left border draws the eye to the nesting relationship. */
    padding-left: 12px;
    border-left: 2px solid #e5e7eb;
    margin-top: 4px;
  }

  /* ---- Expand / collapse toggle ------------------------------------------ */

  .toggle-btn {
    align-self: flex-start;
    padding: 1px 8px;
    font-size: 0.75rem;
    border-radius: 4px;
    cursor: pointer;
    background: transparent;
    border: 1px solid #d1d5db;
    color: #6b7280;
  }

  .toggle-btn:hover {
    background: #f3f4f6;
    color: #374151;
  }
</style>
