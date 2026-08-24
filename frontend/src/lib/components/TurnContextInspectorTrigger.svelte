<script module lang="ts">
  let nextTriggerId = 0;
  let inspectedTurn = $state<{ triggerId: number; turnId: string } | null>(null);
</script>

<script lang="ts">
  import { onDestroy } from "svelte";
  import TurnContextInspector from "./TurnContextInspector.svelte";

  interface Props {
    turnId: string;
    label?: string;
    presentation?: "message" | "history";
    inspectorTarget?: HTMLElement;
  }

  let {
    turnId,
    label = "context",
    presentation = "message",
    inspectorTarget,
  }: Props = $props();

  const triggerId = nextTriggerId++;

  onDestroy(() => {
    if (inspectedTurn?.triggerId === triggerId) {
      inspectedTurn = null;
    }
  });

  function portal(node: HTMLElement, target: HTMLElement | undefined) {
    const originalParent = node.parentNode;
    const originalNextSibling = node.nextSibling;

    function move(destination: HTMLElement | undefined) {
      if (destination && node.parentNode !== destination) {
        destination.appendChild(node);
      }
    }

    move(target);

    return {
      update: move,
      destroy() {
        if (originalParent && node.parentNode !== originalParent) {
          originalParent.insertBefore(node, originalNextSibling);
        }
      },
    };
  }
</script>

<button
  type="button"
  class:message={presentation === "message"}
  class:history={presentation === "history"}
  class="context-link"
  onclick={() => (inspectedTurn = { triggerId, turnId })}
>
  {label}
</button>

<div class="inspector-portal" use:portal={inspectorTarget}>
  {#if inspectedTurn?.triggerId === triggerId}
    <TurnContextInspector
      turnId={inspectedTurn.turnId}
      onClose={() => (inspectedTurn = null)}
    />
  {/if}
</div>

<style>
  .context-link { border: none; background: none; color: var(--ink-soft); cursor: pointer; text-decoration: underline dotted; }
  .context-link.message { padding: 0; font-size: 0.75rem; }
  .context-link.history { padding: 8px 0 0; }
  .context-link:hover { color: var(--ink); }
</style>
