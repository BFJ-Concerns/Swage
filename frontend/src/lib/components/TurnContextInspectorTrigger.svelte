<script lang="ts">
  interface Props {
    turnId: string;
    label?: string;
    presentation?: "message" | "history";
    onInspect: (turnId: string) => void;
  }

  let { turnId, label = "context", presentation = "message", onInspect }: Props = $props();
</script>

<button
  type="button"
  class:message={presentation === "message"}
  class:history={presentation === "history"}
  class="context-link"
  onclick={() => onInspect(turnId)}
>
  {label}
</button>

<style>
  .context-link { border: none; background: none; color: var(--ink-soft); cursor: pointer; text-decoration: underline dotted; }
  .context-link.message { padding: 0; font-size: 0.75rem; }
  .context-link.history { padding: 8px 0 0; }
  .context-link:hover { color: var(--ink); }
</style>
