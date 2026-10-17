<script lang="ts">
  import { fetchConversationSecuritySettings } from "../api/settings";
  import type { RelaxedCheck } from "../protocol/RelaxedCheck";
  import { CHECK_LABELS, relaxedBy, securitySettingsRevision } from "../security-settings";
  let { conversationId }: { conversationId: string } = $props();
  let relaxed = $state<RelaxedCheck[]>([]);
  let error = $state(false);
  $effect(() => {
    const id = conversationId;
    void $securitySettingsRevision;
    relaxed = []; error = false;
    let current = true;
    void fetchConversationSecuritySettings(id).then((reading) => {
      if (current) relaxed = reading.relaxed;
    }).catch(() => { if (current) error = true; });
    return () => { current = false; };
  });
</script>

{#if error}<span role="status">Security settings unavailable</span>
{:else if relaxed.length}
  <span class="security-badge" aria-label="Session security relaxations">
    {#each relaxed as entry (entry.check)}<span>{CHECK_LABELS[entry.check]} — {relaxedBy(entry)}</span>{/each}
  </span>
{/if}

<style>
  .security-badge { display: flex; flex-wrap: wrap; gap: 0.5rem; color: var(--warning); }
</style>
