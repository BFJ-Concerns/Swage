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

  const options: Array<{ value: ThemeMode; label: string; title: string }> = [
    { value: "light", label: "L", title: "Light (riso)" },
    { value: "dark", label: "D", title: "Dark (cyanotype)" },
    { value: "system", label: "S", title: "Follow system" },
  ];
</script>

<div class="theme-toggle" role="group" aria-label="Theme">
  {#each options as option (option.value)}
    <button
      type="button"
      class="opt"
      class:active={mode === option.value}
      onclick={() => onSelect(option.value)}
      title={option.title}
      aria-pressed={mode === option.value}
    >
      {option.label}
    </button>
  {/each}
</div>

<style>
  /*
    Three small mono squares matching the topbar's measurement vocabulary.
    Active state inverts to ink-on-paper without the orange offset (the
    toggle is a steady control — the offset is reserved for emphasis).
  */
  .theme-toggle {
    display: flex;
    align-items: center;
    border: 1px solid currentColor;
  }

  .opt {
    width: 24px;
    height: 24px;
    background: transparent;
    color: inherit;
    border: none;
    border-right: 1px solid currentColor;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
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
