<script lang="ts">
  import {
    readDisplayPreferences,
    writeDisplayPreferences,
    type DisplayPreferences,
  } from "../../display-preferences";

  let displayPreferences = $state<DisplayPreferences>(readDisplayPreferences());
  let status = $state("");

  const THEME_OPTIONS = ["system", "light", "dark"] as const;
  const DENSITY_OPTIONS = ["compact", "comfortable"] as const;
  const MODEL_OPTIONS = ["off", "assistant", "all"] as const;
  const RENDERING_OPTIONS = ["markdown", "plain"] as const;
  const PRINT_OPTIONS = ["full", "minimal", "off"] as const;

  function setPreference<K extends keyof DisplayPreferences>(
    key: K,
    value: DisplayPreferences[K],
  ): void {
    displayPreferences = { ...displayPreferences, [key]: value };
    writeDisplayPreferences(displayPreferences);
    status = "Display preferences saved.";
  }
</script>

{#if status}
  <div class="notice"><span>{status}</span></div>
{/if}

<section class="shell-section">
  <h2 class="shell-section-title">Display preferences</h2>
  <div class="shell-grid">
    {#each [
      { label: "Theme", aria: "Theme", key: "theme", options: THEME_OPTIONS },
      { label: "Density", aria: "Density", key: "density", options: DENSITY_OPTIONS },
      { label: "Show model on each message", aria: "Message model display", key: "message_model_display", options: MODEL_OPTIONS },
      { label: "Conversation rendering", aria: "Conversation rendering", key: "conversation_rendering", options: RENDERING_OPTIONS },
      { label: "Print marks", aria: "Print marks", key: "print_marks", options: PRINT_OPTIONS },
    ] as const as readonly { label: string; aria: string; key: keyof DisplayPreferences; options: readonly string[] }[] as row}
      <div class="shell-row">
        <span class="shell-label">{row.label}</span>
        <div class="segmented-control" role="group" aria-label={row.aria}>
          {#each row.options as option}
            <button
              type="button"
              class="opt"
              class:active={displayPreferences[row.key] === option}
              onclick={() => setPreference(row.key, option as never)}
            >{option}</button>
          {/each}
        </div>
      </div>
    {/each}
  </div>
</section>

<style>
  .notice { display: flex; padding: 10px 14px; background: var(--accent); color: var(--paper); border: 2px solid var(--ink); font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; }
  .shell-section { padding: 16px 18px; border: 1px solid var(--rule-soft); background: var(--paper); display: flex; flex-direction: column; gap: 10px; }
  .shell-section-title { margin: 0; font-family: var(--font-display); font-weight: 400; font-size: 18px; line-height: 1.05; letter-spacing: 0.06em; text-transform: uppercase; color: var(--ink); }
  .shell-grid { display: flex; flex-direction: column; }
  .shell-row { display: grid; grid-template-columns: minmax(180px, 0.4fr) 1fr; gap: 16px; padding: 10px 0; border-bottom: 1px dotted var(--rule-soft); }
  .shell-row:last-child { border-bottom: none; }
  .shell-label { font-family: var(--font-mono); font-weight: 500; font-size: 11px; line-height: 1; letter-spacing: 0.16em; text-transform: uppercase; color: var(--ink-soft); }
  .segmented-control { display: flex; flex-wrap: wrap; gap: 8px; }
  .opt { position: relative; display: inline-flex; align-items: center; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); padding: 6px 12px; font-family: var(--font-display); font-size: 14px; line-height: 1; letter-spacing: 0.04em; text-transform: uppercase; cursor: pointer; isolation: isolate; }
  .opt:hover:not(.active) { background: rgba(14, 59, 102, 0.08); }
  .opt.active { background: var(--ink); color: var(--paper); }
  .opt.active::before { content: ""; position: absolute; inset: 0; background: var(--accent); transform: translate(2px, 2px); z-index: -1; mix-blend-mode: var(--shadow-blend); opacity: var(--shadow-opacity); pointer-events: none; }
  @media (max-width: 760px) { .shell-row { grid-template-columns: 1fr; } }
</style>
