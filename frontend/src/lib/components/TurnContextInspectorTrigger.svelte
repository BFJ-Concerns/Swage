<script lang="ts">
  import TurnContextInspector from "./TurnContextInspector.svelte";

  interface Props {
    turnId: string;
    label?: string;
    presentation?: "message" | "history";
  }

  let { turnId, label = "context", presentation = "message" }: Props = $props();
  let inspectedTurnId = $state<string | null>(null);
</script>

<button
  type="button"
  class:message={presentation === "message"}
  class:history={presentation === "history"}
  class="context-link"
  onclick={() => (inspectedTurnId = turnId)}
>
  {label}
</button>

{#if inspectedTurnId}
  <TurnContextInspector
    turnId={inspectedTurnId}
    onClose={() => (inspectedTurnId = null)}
  />
{/if}

<style>
  .context-link { border: none; background: none; color: var(--ink-soft); cursor: pointer; text-decoration: underline dotted; }
  .context-link.message { padding: 0; font-size: 0.75rem; }
  .context-link.history { padding: 8px 0 0; }
  .context-link:hover { color: var(--ink); }
</style>
