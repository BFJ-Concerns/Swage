<!--
  Theme toggle — three states: light (riso), dark (cyanotype), system.

  System mode follows prefers-color-scheme and updates live if the OS theme
  changes. Selection persists in localStorage under "swage:theme".

  The component is presentational only: it renders three buttons and emits
  the chosen mode. Applying it to <body> is the parent's job (App.svelte),
  so that theme switching cascades to splash/error/not-found surfaces too.
-->
<script lang="ts">
  import type { ThemeMode } from "../theme";

  interface Props {
    mode: ThemeMode;
    onSelect: (mode: ThemeMode) => void;
  }

  let { mode, onSelect }: Props = $props();

  // Each option pairs a glyph with a word so the visible label names the
  // function, not the mechanism — and aria-label carries the same name into
  // the a11y tree so it is announced reliably (title alone is not a dependable
  // accessible name and never shows on touch). UI audit f-3.
  const options: Array<{
    value: ThemeMode;
    glyph: string;
    label: string;
    ariaLabel: string;
  }> = [
    { value: "light", glyph: "☀", label: "Light", ariaLabel: "Light theme" },
    { value: "dark", glyph: "☾", label: "Dark", ariaLabel: "Dark theme" },
    { value: "system", glyph: "⌂", label: "System", ariaLabel: "Follow system theme" },
  ];
</script>

<div class="theme-toggle" role="group" aria-label="Theme">
  {#each options as option (option.value)}
    <button
      type="button"
      class="opt"
      class:active={mode === option.value}
      onclick={() => onSelect(option.value)}
      aria-label={option.ariaLabel}
      aria-pressed={mode === option.value}
    >
      <span class="glyph" aria-hidden="true">{option.glyph}</span>
      <span class="word">{option.label}</span>
    </button>
  {/each}
</div>

<style>
  /*
    Three small mono segments matching the topbar's measurement vocabulary.
    Active state inverts to ink-on-paper without the orange offset (the
    toggle is a steady control — the offset is reserved for emphasis).
  */
  .theme-toggle {
    display: flex;
    align-items: center;
    border: 1px solid currentColor;
  }

  .opt {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 0 8px;
    height: 24px;
    background: transparent;
    color: inherit;
    border: none;
    border-right: 1px solid currentColor;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-label-sm);
    line-height: 1;
    letter-spacing: 0.04em;
    cursor: pointer;
    opacity: 0.65;
  }

  .opt:last-child {
    border-right: none;
  }

  .opt:hover {
    opacity: 1;
  }

  .opt.active {
    background: currentColor;
    opacity: 1;
  }

  /*
    Trick to invert text colour against the currentColor fill: the parent of
    this component decides what "currentColor" is. Active buttons paint that
    colour as background; the text needs to be the opposite. We use a CSS
    custom property the parent sets, falling back to paper.
  */
  .opt.active {
    color: var(--toggle-active-fg, var(--paper));
  }
</style>
