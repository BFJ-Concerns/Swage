<script lang="ts">
  import { onMount } from "svelte";
  import { listChannelStatuses, listChannelLinks, listChannelLinkCodes } from "../api/channels";
  import type { ChannelStatusResponse } from "../protocol/ChannelStatusResponse";
  import type { ChannelLinkResponse } from "../protocol/ChannelLinkResponse";
  import type { ChannelLinkCodeResponse } from "../protocol/ChannelLinkCodeResponse";

  let channels = $state<ChannelStatusResponse[]>([]);
  let links = $state<ChannelLinkResponse[]>([]);
  let codes = $state<ChannelLinkCodeResponse[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  onMount(() => { void load(); });

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      [channels, links, codes] = await Promise.all([
        listChannelStatuses(), listChannelLinks(), listChannelLinkCodes(),
      ]);
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load channels.";
    } finally {
      loading = false;
    }
  }
</script>

<section class="channels" aria-label="Channel inventory">
  <button type="button" onclick={load} disabled={loading}>Refresh</button>
  {#if loading}
    <p>Loading channels…</p>
  {:else if error}
    <p role="alert">{error}</p>
  {:else}
    <section aria-label="Channel statuses">
      {#each channels as channel (channel.integration)}
        <article>
          <h2>{channel.integration}</h2>
          <dl>
            <div><dt>Status</dt><dd>{channel.status}</dd></div>
            <div><dt>Routes</dt><dd>{channel.route_count}</dd></div>
            <div><dt>Deliveries</dt><dd>{channel.delivery_count}</dd></div>
            <div><dt>Last delivery</dt><dd>{channel.last_delivery_at ? new Date(channel.last_delivery_at).toLocaleString() : "None"}</dd></div>
          </dl>
        </article>
      {:else}
        <p>No channels reported.</p>
      {/each}
    </section>
    <section aria-label="Channel links">
      <h2>Links</h2>
      {#each links as link (`${link.integration}:${link.account}:${link.conversation}`)}
        <article>
          <h3>{link.integration} · {link.conversation}</h3>
          <dl>
            <div><dt>Account</dt><dd>{link.account}</dd></div>
            <div><dt>Topic</dt><dd>{link.topic_id}</dd></div>
            <div><dt>Agent</dt><dd>{link.agent_id}</dd></div>
            <div><dt>Session</dt><dd>{link.session_id ?? "None"}</dd></div>
            <div><dt>Linked</dt><dd>{new Date(link.linked_at).toLocaleString()}</dd></div>
          </dl>
        </article>
      {:else}
        <p>No channel links.</p>
      {/each}
    </section>
    <section aria-label="Unredeemed link codes">
      <h2>Link codes</h2>
      <p>Type the code into the bot to link your chat. Expiry times are shown in your local time.</p>
      {#each codes as code (code.link_code)}
        <article>
          <code>{code.link_code}</code>
          <p>Expires <time datetime={code.expires_at}>{new Date(code.expires_at).toLocaleString()}</time></p>
        </article>
      {:else}
        <p>No unredeemed link codes.</p>
      {/each}
    </section>
  {/if}
</section>

<style>
  .channels, section { display: grid; gap: 14px; }
  article { border: 1px solid var(--rule-soft); padding: 16px; min-width: 0; }
  h2, h3, p { margin: 0; }
  h2, h3 { font-family: var(--font-display); font-weight: 400; }
  dl { margin-bottom: 0; }
  dl div { display: grid; grid-template-columns: minmax(6rem, 1fr) minmax(0, 3fr); gap: 12px; padding: 6px 0; }
  dt { color: var(--ink-soft); }
  dd { margin: 0; overflow-wrap: anywhere; }
  code { font-family: var(--font-mono); white-space: pre-wrap; overflow-wrap: anywhere; user-select: all; }
  button { justify-self: start; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); padding: 8px 16px; cursor: pointer; }
  [role="alert"] { color: var(--error); }
</style>
