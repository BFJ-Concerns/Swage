<!--
  Starting a session — the front door's second half.

  A session starts from here with one choice made or none: the agent it runs
  under (the operator's default preselected), and optionally a topic to work
  in. With no topic the session is a general assistant in the general scope,
  inheriting nothing from any topic. Topics are offered as a quiet selector,
  not a gate — the primary action starts the session as it is configured.

  A topic that does not exist yet is created from the same control; creating
  one does not start a session.
-->
<script lang="ts">
  import AgentPicker from "./AgentPicker.svelte";
  import type { EffectiveAgentDefinitionResponse } from "../protocol/EffectiveAgentDefinitionResponse";
  import type { Topic } from "../types";

  interface Props {
    topics?: Topic[];
    // The definitions the chosen scope offers; reloaded by the parent when
    // the topic choice changes.
    agentDefinitions?: EffectiveAgentDefinitionResponse[];
    // The topic the start will use, "" for none. Bound so the parent can
    // reload the agent definitions the chosen scope offers.
    selectedTopicId?: string;
    onStart?: (choice: { topicId: string | null; agentId: string | null }) => void;
    onCreateTopic?: (name: string) => Promise<void>;
  }

  let {
    topics = [],
    agentDefinitions = [],
    selectedTopicId = $bindable(""),
    onStart,
    onCreateTopic,
  }: Props = $props();

  let selectedAgentId = $state("");
  let showCreateForm = $state(false);
  let newTopicName = $state("");
  let createError = $state<string | null>(null);
  let isCreating = $state(false);

  // The scope's definitions change with the topic; an agent that no longer
  // exists in the new scope must not linger as the selection.
  $effect(() => {
    if (
      selectedAgentId &&
      !agentDefinitions.some((definition) => definition.id === selectedAgentId)
    ) {
      selectedAgentId = "";
    }
  });

  function start(): void {
    onStart?.({
      topicId: selectedTopicId || null,
      agentId: selectedAgentId || null,
    });
  }

  async function submitCreate(event: Event): Promise<void> {
    event.preventDefault();
    const name = newTopicName.trim();
    if (!name || !onCreateTopic) return;
    isCreating = true;
    createError = null;
    try {
      await onCreateTopic(name);
      showCreateForm = false;
      newTopicName = "";
    } catch (error) {
      createError = error instanceof Error ? error.message : "Failed to create topic.";
    } finally {
      isCreating = false;
    }
  }
</script>

<section class="session-start" aria-label="Start a session">
  <p class="eyebrow">New session</p>
  <div class="controls">
    <label class="topic-choice">
      <span>Topic</span>
      <select aria-label="Topic" bind:value={selectedTopicId}>
        <option value="">No topic — general assistant</option>
        {#each topics as topic (topic.id)}
          <option value={topic.id}>{topic.name}</option>
        {/each}
      </select>
    </label>
    <AgentPicker definitions={agentDefinitions} bind:selectedAgentId />
    <button type="button" class="start-btn" onclick={start}>Start</button>
    {#if onCreateTopic}
      {#if showCreateForm}
        <form class="create-topic" onsubmit={submitCreate}>
          <input
            type="text"
            aria-label="New topic name"
            placeholder="New topic name"
            bind:value={newTopicName}
            autocomplete="off"
            required
          />
          <button type="submit" disabled={isCreating}>{isCreating ? "Creating…" : "Create"}</button>
          <button
            type="button"
            onclick={() => {
              showCreateForm = false;
              newTopicName = "";
              createError = null;
            }}
          >
            Cancel
          </button>
        </form>
      {:else}
        <button type="button" class="new-topic" onclick={() => (showCreateForm = true)}>+ New topic</button>
      {/if}
    {/if}
  </div>
  {#if createError}
    <p class="create-error" role="alert">{createError}</p>
  {/if}
  <p class="hint">
    {#if selectedTopicId}
      The session works in the topic: its memory, attached folders and configuration apply.
    {:else}
      Without a topic the session is a general assistant: general memory and knowledge, and nothing carried over from any topic.
    {/if}
  </p>
</section>

<style>
  .session-start {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 18px 20px;
    border: var(--border-strong);
    background: var(--paper);
    color: var(--ink);
  }

  .eyebrow {
    margin: 0;
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: var(--font-label-sm);
    line-height: 1;
    letter-spacing: 0.18em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 14px;
  }

  .topic-choice {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-size: var(--font-label-sm);
  }

  .topic-choice select,
  .create-topic input {
    border: 1px solid var(--rule-soft);
    background: var(--paper);
    color: var(--ink);
    font: inherit;
    padding: 3px 6px;
  }

  .topic-choice select {
    max-width: min(40vw, 20rem);
    cursor: pointer;
  }

  .start-btn {
    padding: 6px 16px;
    border: 2px solid var(--ink);
    background: var(--accent);
    color: var(--primary-fill-ink);
    font-family: var(--font-display);
    font-size: var(--font-body-lg);
    line-height: 1;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .start-btn:hover {
    background: var(--ink);
    color: var(--paper);
  }

  .new-topic,
  .create-topic button {
    background: transparent;
    border: 1px solid var(--ink);
    padding: 4px 10px;
    font-family: var(--font-mono);
    font-size: var(--font-label-sm);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink);
    cursor: pointer;
  }

  .new-topic:hover,
  .create-topic button:hover:not(:disabled) {
    color: var(--ink-orange);
  }

  .create-topic {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .create-error {
    margin: 0;
    color: var(--error);
    font-size: var(--font-body-sm);
  }

  .hint {
    margin: 0;
    color: var(--ink-soft);
    font-size: var(--font-body-sm);
    line-height: 1.5;
    max-width: 60ch;
  }
</style>
