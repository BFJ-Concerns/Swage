<script lang="ts">
  type InformationalTab = "plugins" | "channels" | "nodes" | "voice" | "account";

  interface Props {
    tab: InformationalTab;
  }

  let { tab }: Props = $props();

  const CONTENT: Record<InformationalTab, { title: string; body: string }> = {
    plugins: {
      title: "Installed plugins",
      body: "No platform plugin registry yet. Bench-level extensions live in the Builder lens.",
    },
    channels: {
      title: "Delivery channels",
      body: "Channel management is read-only via the operator API; this page will configure them once a CRUD endpoint exists.",
    },
    nodes: {
      title: "Attached nodes",
      body: "Node listing will mirror /api/operator/overview.nodes once this page is wired.",
    },
    voice: {
      title: "Voice capabilities",
      body: "Read-only capability flags live on the voice endpoint; configuration arrives with a future settings backend.",
    },
    account: {
      title: "Account",
      body: "Swage runs locally — there is no account concept yet.",
    },
  };

  const content = $derived(CONTENT[tab]);
</script>

<section class="shell-section">
  <h2 class="shell-section-title">{content.title}</h2>
  <p class="shell-empty">{content.body}</p>
</section>

<style>
  .shell-section {
    padding: 16px 18px;
    border: 1px solid var(--rule-soft);
    background: var(--paper);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .shell-section-title {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 18px;
    line-height: 1.05;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink);
  }

  .shell-empty {
    margin: 0;
    color: var(--ink-soft);
    font-size: 13px;
    line-height: 1.55;
  }
</style>
