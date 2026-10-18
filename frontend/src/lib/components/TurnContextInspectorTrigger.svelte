<script module lang="ts">
  let nextTriggerId = 0;
  // One inspector is open at a time, on the trigger that opened it; `round`
  // is the provider round it opens at, when an address named one.
  let inspectedTurn = $state<{ triggerId: number; turnId: string; round: number | null } | null>(null);
</script>

<script lang="ts">
  import { onDestroy } from "svelte";
  import TurnContextInspector from "./TurnContextInspector.svelte";

  interface Props {
    turnId: string;
    label?: string;
    presentation?: "message" | "history";
    inspectorTarget?: HTMLElement;
    /**
     * A provider round the address names on this turn: the inspector opens
     * as if the trigger were clicked, at that round's request. `null` when
     * the address names none; opening is then the reader's click.
     */
    openAtRound?: number | null;
  }

  let {
    turnId,
    label = "context",
    presentation = "message",
    inspectorTarget,
    openAtRound = null,
  }: Props = $props();

  const triggerId = nextTriggerId++;

  // An address that names a round opens the inspector once per round it
  // names; a later click or close is the reader's and is not overruled.
  $effect(() => {
    const round = openAtRound;
    if (round === null) return;
    inspectedTurn = { triggerId, turnId, round };
  });

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
  onclick={() => (inspectedTurn = { triggerId, turnId, round: null })}
>
  {label}
</button>

<div class="inspector-portal" use:portal={inspectorTarget}>
  {#if inspectedTurn?.triggerId === triggerId}
    <TurnContextInspector
      turnId={inspectedTurn.turnId}
      focusRound={inspectedTurn.round}
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
