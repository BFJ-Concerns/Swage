<script lang="ts">
  import { listSkills } from "../api/skills";
  import type { SkillItem } from "../protocol/SkillItem";

  interface Props {
    spaceId: string | null;
  }

  let { spaceId }: Props = $props();
  let skills = $state<SkillItem[]>([]);
  let selectedName = $state("");
  let loading = $state(false);
  let error = $state<string | null>(null);
  let loadGeneration = 0;

  const selected = $derived(skills.find((skill) => skill.name === selectedName) ?? null);

  $effect(() => {
    const currentSpaceId = spaceId;
    const generation = ++loadGeneration;
    skills = [];
    selectedName = "";
    error = null;
    if (!currentSpaceId) return;
    loading = true;
    void listSkills(currentSpaceId)
      .then((loaded) => {
        if (generation !== loadGeneration) return;
        skills = loaded;
        selectedName = loaded[0]?.name ?? "";
      })
      .catch((err: unknown) => {
        if (generation !== loadGeneration) return;
        error = err instanceof Error ? err.message : "Failed to load commands.";
      })
      .finally(() => {
        if (generation === loadGeneration) loading = false;
      });
  });

  function scopeLabel(skill: SkillItem): string {
    return skill.level === "space" ? "This space" : skill.level === "agent" ? "This agent" : "Global";
  }
</script>

<section class="commands" aria-label="Available commands">
  {#if !spaceId}
    <p class="empty">Select a space to inspect its available commands.</p>
  {:else if loading}
    <p class="loading">Loading commands…</p>
  {:else if error}
    <p class="error" role="alert">{error}</p>
  {:else if skills.length === 0}
    <p class="empty">No commands are available for this space.</p>
  {:else}
    <div class="command-list" role="list" aria-label="Available commands">
      {#each skills as skill (skill.name)}
        <button
          type="button"
          class:selected={skill.name === selectedName}
          aria-pressed={skill.name === selectedName}
          onclick={() => (selectedName = skill.name)}
        >
          <span class="command-name">/{skill.name}</span>
          <span class="source">{scopeLabel(skill)}</span>
          {#if skill.shipped}<span class="shipped">Shipped</span>{/if}
          {#if skill.description}<span class="description">{skill.description}</span>{/if}
        </button>
      {/each}
    </div>

    {#if selected}
      <article class="command-detail" aria-label={`/${selected.name} details`}>
        <header>
          <div>
            <p class="source">{scopeLabel(selected)}</p>
            <h2>/{selected.name}</h2>
          </div>
          {#if selected.shipped}<span class="shipped">Shipped</span>{/if}
        </header>
        <dl>
          <div><dt>Command</dt><dd><code>/{selected.name}</code></dd></div>
          <div><dt>Description</dt><dd>{selected.description || "Not provided"}</dd></div>
          <div><dt>Resolved scope</dt><dd>{scopeLabel(selected)}</dd></div>
          <div><dt>Tools</dt><dd>{selected.tools.length ? selected.tools.join(", ") : "None"}</dd></div>
          <div><dt>Activation</dt><dd>{selected.always_active ? "Always active" : "Available on invocation"}</dd></div>
          <div><dt>Source</dt><dd>{selected.shipped ? "Shipped with Swage" : "Operator-defined"}</dd></div>
        </dl>
      </article>
    {/if}
  {/if}
</section>

<style>
  .commands { display: grid; grid-template-columns: minmax(14rem, 1fr) minmax(20rem, 2fr); gap: 18px; align-items: start; }
  .command-list { display: flex; flex-direction: column; border: 2px solid var(--ink); }
  .command-list button { display: flex; flex-direction: column; align-items: flex-start; gap: 4px; padding: 14px; border: 0; border-bottom: 1px solid var(--ink); background: var(--paper); color: var(--ink); text-align: left; cursor: pointer; }
  .command-list button:last-child { border-bottom: 0; }
  .command-list button.selected { background: var(--ink); color: var(--paper); }
  .command-name { font-family: var(--font-display); font-size: 18px; letter-spacing: .04em; }
  .source, .shipped { font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; }
  .shipped { border: 1px solid currentColor; padding: 1px 4px; }
  .description { font-size: 12px; opacity: .75; }
  .command-detail { min-width: 0; border: 2px solid var(--ink); padding: 18px; }
  .command-detail header { display: flex; justify-content: space-between; gap: 16px; align-items: start; border-bottom: 1px solid var(--ink); padding-bottom: 12px; }
  .command-detail h2 { margin: 4px 0 0; font-family: var(--font-display); font-weight: 400; }
  dl { margin: 0; }
  dl div { display: grid; grid-template-columns: minmax(8rem, 1fr) minmax(0, 2fr); gap: 12px; padding: 12px 0; border-bottom: 1px solid color-mix(in srgb, var(--ink) 25%, transparent); }
  dl div:last-child { border-bottom: 0; padding-bottom: 0; }
  dt { font-family: var(--font-mono); font-size: 10px; letter-spacing: .12em; text-transform: uppercase; }
  dd, code { margin: 0; overflow-wrap: anywhere; }
  .loading, .empty, .error { grid-column: 1 / -1; margin: 0; padding: 16px; border: 2px solid var(--ink); }
  .error { background: var(--ink); color: var(--paper); border-color: var(--accent); }
  @media (max-width: 760px) { .commands { grid-template-columns: 1fr; } }
</style>
