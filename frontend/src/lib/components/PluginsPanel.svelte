<!--
  Installed plugins: install a package, see what each plugin provides with
  its grants, secrets, variant and revisions, and enable it for the install
  or for the topic selected in Settings.
-->
<script lang="ts">
  import { installPlugin, listPlugins, setPluginEnablement } from "../api/plugins";
  import type { PluginSummary } from "../protocol/PluginSummary";

  interface Props {
    topicId: string | null;
  }

  let { topicId }: Props = $props();
  let plugins = $state<PluginSummary[]>([]);
  let selectedName = $state("");
  let loading = $state(false);
  let error = $state<string | null>(null);
  let sourcePath = $state("");
  let installing = $state(false);
  let changing = $state<string | null>(null);
  let loadGeneration = 0;

  const selected = $derived(plugins.find((plugin) => plugin.name === selectedName) ?? null);

  $effect(() => {
    const generation = ++loadGeneration;
    loading = true;
    error = null;
    void listPlugins()
      .then((loaded) => {
        if (generation !== loadGeneration) return;
        plugins = loaded;
        if (!loaded.some((plugin) => plugin.name === selectedName)) {
          selectedName = loaded[0]?.name ?? "";
        }
      })
      .catch((err: unknown) => {
        if (generation !== loadGeneration) return;
        error = err instanceof Error ? err.message : "Failed to load plugins.";
      })
      .finally(() => {
        if (generation === loadGeneration) loading = false;
      });
  });

  function replace(updated: PluginSummary) {
    const index = plugins.findIndex((plugin) => plugin.name === updated.name);
    plugins = index === -1 ? [...plugins, updated] : plugins.map((plugin, i) => (i === index ? updated : plugin));
  }

  async function install() {
    const path = sourcePath.trim();
    if (!path || installing) return;
    installing = true;
    error = null;
    try {
      const installed = await installPlugin(path);
      replace(installed);
      selectedName = installed.name;
      sourcePath = "";
    } catch (err: unknown) {
      error = err instanceof Error ? err.message : "Failed to install the plugin.";
    } finally {
      installing = false;
    }
  }

  /**
   * Ask the gateway to change the enablement; the box shows the gateway's
   * answer, so a refused change puts the box back where it was.
   */
  async function setEnabled(plugin: PluginSummary, scope: string | null, input: HTMLInputElement) {
    const enabled = input.checked;
    changing = plugin.name;
    error = null;
    try {
      replace(await setPluginEnablement(plugin.name, scope, enabled));
    } catch (err: unknown) {
      error = err instanceof Error ? err.message : "Failed to change the plugin's enablement.";
      input.checked = !enabled;
    } finally {
      changing = null;
    }
  }

  /** The topic's effective answer: its own where it gave one, the install's otherwise. */
  function enabledInTopic(plugin: PluginSummary, topic: string): boolean {
    if (plugin.enabled_topics.includes(topic)) return true;
    if (plugin.disabled_topics.includes(topic)) return false;
    return plugin.enabled_for_install;
  }

  function topicAnswer(plugin: PluginSummary, topic: string): string {
    if (plugin.enabled_topics.includes(topic)) return "enabled by this topic";
    if (plugin.disabled_topics.includes(topic)) return "disabled by this topic";
    return plugin.enabled_for_install ? "enabled by the install" : "not enabled";
  }

  function serviceLabel(plugin: PluginSummary): string {
    switch (plugin.service.state) {
      case "serving":
        return "Serving";
      case "terminated":
        return `Terminated: ${plugin.service.reason}; restarts on its next call`;
      case "failed":
        return `Failed to load: ${plugin.service.reason}`;
    }
  }
</script>

<section class="plugins" aria-label="Installed plugins">
  <form
    class="install"
    aria-label="Install a plugin"
    onsubmit={(event) => {
      event.preventDefault();
      void install();
    }}
  >
    <label class="field">
      <span class="key">Package directory on the gateway's host</span>
      <input
        type="text"
        bind:value={sourcePath}
        placeholder="/path/to/plugin"
        autocomplete="off"
        spellcheck="false"
      />
    </label>
    <button type="submit" class="btn" disabled={installing || !sourcePath.trim()}>
      {installing ? "Installing…" : "Install"}
    </button>
  </form>

  {#if error}
    <p class="error" role="alert">{error}</p>
  {/if}

  {#if loading}
    <p class="loading">Loading plugins…</p>
  {:else if plugins.length === 0}
    <p class="empty">No plugins are installed. Install a package directory holding a <code>plugin.toml</code>.</p>
  {:else}
    <div class="catalogue">
      <div class="catalogue-list" role="list" aria-label="Installed plugins">
        {#each plugins as plugin (plugin.name)}
          <button
            type="button"
            class:selected={plugin.name === selectedName}
            aria-pressed={plugin.name === selectedName}
            onclick={() => (selectedName = plugin.name)}
          >
            <span class="item-name">{plugin.name}</span>
            <span class="meta">v{plugin.version} · revision {plugin.revision}</span>
            <span class="meta" class:warn={plugin.service.state !== "serving"}>{serviceLabel(plugin)}</span>
            {#if plugin.description}<span class="description">{plugin.description}</span>{/if}
          </button>
        {/each}
      </div>

      {#if selected}
        <article class="catalogue-detail" aria-label={`${selected.name} details`}>
          <header>
            <div>
              <p class="meta">v{selected.version} · revision {selected.revision}</p>
              <h2>{selected.name}</h2>
            </div>
          </header>

          <div class="enablement" aria-label="Where the plugin is enabled">
            <label class="opt">
              <input
                type="checkbox"
                checked={selected.enabled_for_install}
                disabled={changing === selected.name}
                onchange={(event) => void setEnabled(selected, null, event.currentTarget as HTMLInputElement)}
              />
              <span>Enabled for the install</span>
            </label>
            {#if topicId}
              <label class="opt">
                <input
                  type="checkbox"
                  checked={enabledInTopic(selected, topicId)}
                  disabled={changing === selected.name}
                  onchange={(event) => void setEnabled(selected, topicId, event.currentTarget as HTMLInputElement)}
                />
                <span>Enabled in this topic <em class="meta">({topicAnswer(selected, topicId)})</em></span>
              </label>
            {/if}
          </div>

          <dl>
            <div><dt>Description</dt><dd>{selected.description || "Not provided"}</dd></div>
            <div><dt>Service</dt><dd>{serviceLabel(selected)}</dd></div>
            <div>
              <dt>Tools</dt>
              <dd>
                {#if selected.provides.tools.length === 0}
                  None
                {:else}
                  <ul class="plain">
                    {#each selected.provides.tools as tool (tool.name)}
                      <li><code>{tool.name}</code> — {tool.description}</li>
                    {/each}
                  </ul>
                {/if}
              </dd>
            </div>
            <div><dt>Grants</dt><dd>{selected.grants.length ? selected.grants.join(", ") : "None"}</dd></div>
            <div>
              <dt>Secrets</dt>
              <dd>
                {#if selected.secrets.length === 0}
                  None
                {:else}
                  <ul class="plain">
                    {#each selected.secrets as secret (secret.name)}
                      <li><code>{secret.name}</code> — {secret.description}</li>
                    {/each}
                  </ul>
                {/if}
              </dd>
            </div>
            <div><dt>Variant of</dt><dd>{selected.variant_of ?? "Not a variant"}</dd></div>
            <div>
              <dt>Revisions</dt>
              <dd>
                <ul class="plain">
                  {#each selected.revisions as revision (revision.revision)}
                    <li>
                      revision {revision.revision} · v{revision.version} · {revision.installed_at}
                      {#if revision.active}<span class="active">Active</span>{/if}
                      <span class="meta">from {revision.installed_from}</span>
                    </li>
                  {/each}
                </ul>
              </dd>
            </div>
          </dl>
        </article>
      {/if}
    </div>
  {/if}
</section>

<style>
  .plugins { display: flex; flex-direction: column; gap: 18px; }
  .install { display: flex; gap: 12px; align-items: flex-end; border: 2px solid var(--ink); padding: 14px; }
  .field { display: flex; flex-direction: column; gap: 6px; flex: 1; }
  .key, .meta, .active { font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; }
  .field input { font: inherit; padding: 8px 10px; border: 1px solid var(--ink); background: var(--paper); color: var(--ink); }
  .btn { font-family: var(--font-mono); font-size: 11px; letter-spacing: .12em; text-transform: uppercase; padding: 9px 14px; border: 2px solid var(--ink); background: var(--ink); color: var(--paper); cursor: pointer; }
  .btn:disabled { opacity: .5; cursor: default; }
  .catalogue { display: grid; grid-template-columns: minmax(14rem, 1fr) minmax(20rem, 2fr); gap: 18px; align-items: start; }
  .catalogue-list { display: flex; flex-direction: column; border: 2px solid var(--ink); }
  .catalogue-list button { display: flex; flex-direction: column; align-items: flex-start; gap: 4px; padding: 14px; border: 0; border-bottom: 1px solid var(--ink); background: var(--paper); color: var(--ink); text-align: left; cursor: pointer; }
  .catalogue-list button:last-child { border-bottom: 0; }
  .catalogue-list button.selected { background: var(--ink); color: var(--paper); }
  .item-name { font-family: var(--font-display); font-size: 18px; letter-spacing: .04em; text-transform: uppercase; }
  .meta.warn { color: var(--accent); }
  .description { font-size: 12px; opacity: .75; }
  .catalogue-detail { min-width: 0; border: 2px solid var(--ink); padding: 18px; }
  .catalogue-detail header { display: flex; justify-content: space-between; gap: 16px; align-items: start; border-bottom: 1px solid var(--ink); padding-bottom: 12px; }
  .catalogue-detail h2 { margin: 4px 0 0; font-family: var(--font-display); font-weight: 400; text-transform: uppercase; }
  .enablement { display: flex; flex-direction: column; gap: 8px; padding: 12px 0; border-bottom: 1px solid var(--ink); }
  .opt { display: flex; gap: 8px; align-items: center; font-size: 13px; }
  .opt em { font-style: normal; opacity: .7; }
  dl { margin: 0; }
  dl div { display: grid; grid-template-columns: minmax(8rem, 1fr) minmax(0, 2fr); gap: 12px; padding: 12px 0; border-bottom: 1px solid color-mix(in srgb, var(--ink) 25%, transparent); }
  dl div:last-child { border-bottom: 0; padding-bottom: 0; }
  dt { font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; }
  dd, code { margin: 0; overflow-wrap: anywhere; }
  .plain { margin: 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 6px; }
  .active { border: 1px solid currentColor; padding: 1px 4px; margin: 0 6px; }
  .loading, .empty, .error { margin: 0; padding: 16px; border: 2px solid var(--ink); }
  .error { background: var(--ink); color: var(--paper); border-color: var(--accent); }
  @media (max-width: 760px) { .catalogue { grid-template-columns: 1fr; } .install { flex-direction: column; align-items: stretch; } }
</style>
