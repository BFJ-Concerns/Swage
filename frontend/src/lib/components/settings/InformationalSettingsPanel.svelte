<script lang="ts">
  type InformationalTab = "plugins" | "channels" | "devices" | "account";

  interface Props {
    tab: InformationalTab;
  }

  let { tab }: Props = $props();

  const CONTENT: Record<InformationalTab, { title: string; body: string }> = {
    plugins: {
      title: "Installed plugins",
      body: "Swage has no plugin system yet. Tools are built in, and skills are added under Skills.",
    },
    channels: {
      title: "Delivery channels",
      body: "Channels are configured in config.toml — the Telegram channel under [channels.telegram] — and cannot be managed from here yet.",
    },
    devices: {
      title: "Attached devices",
      body: "Devices register and pair through the API, as the documentation describes. They are not listed here yet.",
    },
    account: {
      title: "Account",
      body: "Swage runs for a single operator on their own machine, so there is no account to manage.",
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
