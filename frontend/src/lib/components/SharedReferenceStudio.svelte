<script lang="ts">
  import {
    createSharedReferenceEvent,
    getSharedReference,
    isContextSnapshotPayload,
    isTeamTopologyPayload,
    listSharedReferences,
    updateTeamTopologyReference,
  } from "../api/builder";
  import type {
    SharedReferenceDetailResponse,
    SharedReferenceKind,
    SharedReferenceSummary,
  } from "../types";

  interface Props {
    spaceId: string | null;
    onTeamTopologyUpdated?: () => void | Promise<void>;
  }

  let { spaceId, onTeamTopologyUpdated }: Props = $props();

  let summaries = $state<SharedReferenceSummary[]>([]);
  let selectedKey = $state<string | null>(null);
  let detail = $state<SharedReferenceDetailResponse | null>(null);
  let isLoading = $state(false);
  let detailLoading = $state(false);
  let loadError = $state<string | null>(null);
  let detailError = $state<string | null>(null);
  let notice = $state<string | null>(null);

  $effect(() => {
    if (!spaceId) {
      summaries = [];
      selectedKey = null;
      detail = null;
      loadError = null;
      detailError = null;
      notice = null;
      return;
    }
    void refreshSummaries(spaceId);
  });

  async function refreshSummaries(nextSpaceId: string): Promise<void> {
    isLoading = true;
    loadError = null;

    try {
      const objects = await listSharedReferences(nextSpaceId);
      summaries = objects;

      const fallback = objects[0];
      const nextSelection =
        objects.find((summary) => `${summary.kind}:${summary.id}` === selectedKey) ?? fallback;

      if (nextSelection) {
        selectedKey = `${nextSelection.kind}:${nextSelection.id}`;
        await loadDetail(nextSelection.kind, nextSelection.id);
      } else {
        selectedKey = null;
        detail = null;
      }
    } catch (error) {
      loadError =
        error instanceof Error
          ? error.message
          : "Failed to load shared reference objects.";
    } finally {
      isLoading = false;
    }
  }

  async function loadDetail(kind: SharedReferenceKind, id: string): Promise<void> {
    detailLoading = true;
    detailError = null;

    try {
      detail = await getSharedReference(kind, id);
    } catch (error) {
      detailError =
        error instanceof Error ? error.message : "Failed to load shared object detail.";
    } finally {
      detailLoading = false;
    }
  }

  async function selectSummary(summary: SharedReferenceSummary): Promise<void> {
    selectedKey = `${summary.kind}:${summary.id}`;
    notice = null;
    await loadDetail(summary.kind, summary.id);
  }

  async function recordSelection(
    payload: Record<string, unknown>,
    successMessage: string,
  ): Promise<void> {
    if (!detail) return;

    detailError = null;
    notice = null;

    try {
      await createSharedReferenceEvent(detail.object.kind, detail.object.id, "selected", payload);
      detail = await getSharedReference(detail.object.kind, detail.object.id);
      notice = successMessage;
    } catch (error) {
      detailError =
        error instanceof Error ? error.message : "Failed to record shared-object selection.";
    }
  }

  async function promoteNode(agentId: string): Promise<void> {
    if (!detail || !isTeamTopologyPayload(detail.object.payload)) return;

    detailError = null;
    notice = null;

    try {
      detail = await updateTeamTopologyReference(detail.object.id, {
        entry_agent_id: agentId,
        members: detail.object.payload.members,
      });
      notice = `Promoted ${agentId} to the team lead.`;
      await onTeamTopologyUpdated?.();
      if (spaceId) {
        await refreshSummaries(spaceId);
      }
    } catch (error) {
      detailError =
        error instanceof Error ? error.message : "Failed to update team topology.";
    }
  }
</script>

<article class="builder-card studio-card">
  <div class="card-heading">
    <div>
      <p class="section-kicker">Reference Studio</p>
      <h2>Shared objects</h2>
    </div>
    {#if isLoading || detailLoading}
      <span class="chip">Syncing…</span>
    {:else if detail}
      <span class="chip">{detail.object.kind.replace("_", " ")}</span>
    {/if}
  </div>

  <p class="section-copy">
    Builder actions now target durable visual objects. Select a topology node
    or context element here and the gateway keeps a replayable interaction log
    beside the real underlying object.
  </p>

  {#if loadError}
    <p class="feedback error">{loadError}</p>
  {/if}

  <div class="studio-grid">
    <section class="reference-rail">
      <header>
        <h3>Objects</h3>
        <span>{summaries.length}</span>
      </header>

      {#if !spaceId}
        <p class="muted-note">Select a space to inspect shared objects.</p>
      {:else if summaries.length === 0 && !isLoading}
        <p class="muted-note">No shared objects yet. Create a team or generate context first.</p>
      {:else}
        <div class="summary-list">
          {#each summaries as summary}
            <button
              class="summary-item"
              class:selected={selectedKey === `${summary.kind}:${summary.id}`}
              onclick={() => selectSummary(summary)}
            >
              <span class="summary-kind">{summary.kind.replace("_", " ")}</span>
              <strong>{summary.title}</strong>
              <span>{summary.subtitle}</span>
            </button>
          {/each}
        </div>
      {/if}
    </section>

    <section class="reference-detail">
      {#if detailError}
        <p class="feedback error">{detailError}</p>
      {/if}
      {#if notice}
        <p class="feedback success">{notice}</p>
      {/if}

      {#if !detail}
        <div class="empty-detail">
          Choose a shared object to inspect its current shape and interaction history.
        </div>
      {:else}
        <header class="detail-header">
          <div>
            <h3>{detail.object.title}</h3>
            <p>{detail.object.subtitle}</p>
          </div>
        </header>

        {#if isTeamTopologyPayload(detail.object.payload)}
          <div class="detail-body">
            <section class="reference-panel">
              <div class="panel-heading">
                <h4>Team graph</h4>
                <span>{detail.object.payload.nodes.length} node(s)</span>
              </div>
              <div class="node-grid">
                {#each detail.object.payload.nodes as node}
                  <article class="node-card" class:entry={node.is_entry}>
                    <div>
                      <strong>{node.label}</strong>
                      <span>{node.is_entry ? "Current lead" : "Member"}</span>
                    </div>
                    <div class="inline-actions">
                      <button
                        class="secondary"
                        onclick={() =>
                          recordSelection(
                            { agent_id: node.agent_id, source: "builder-lens" },
                            `Selected ${node.agent_id}.`,
                          )}
                      >
                        Select node
                      </button>
                      {#if !node.is_entry}
                        <button class="primary" onclick={() => promoteNode(node.agent_id)}>
                          Promote to lead
                        </button>
                      {/if}
                    </div>
                  </article>
                {/each}
              </div>
            </section>

            <section class="reference-panel">
              <div class="panel-heading">
                <h4>Edges</h4>
                <span>{detail.object.payload.edges.length}</span>
              </div>
              <div class="edge-list">
                {#each detail.object.payload.edges as edge}
                  <div class="edge-row">
                    <strong>{edge.from}</strong>
                    <span>{edge.kind.replaceAll("_", " ")}</span>
                    <strong>{edge.to}</strong>
                  </div>
                {/each}
              </div>
            </section>
          </div>
        {:else if isContextSnapshotPayload(detail.object.payload)}
          <div class="detail-body">
            <section class="reference-panel">
              <div class="panel-heading">
                <h4>Assembly</h4>
                <span>{detail.object.payload.total_tokens} token(s)</span>
              </div>
              <div class="snapshot-meta">
                <span>Assembly: {detail.object.payload.assembly_name}</span>
                <span>Budget: {detail.object.payload.budget ?? "none"}</span>
                <span>{detail.object.payload.warnings.length} warning(s)</span>
              </div>
              <div class="element-list">
                {#each detail.object.payload.elements as element}
                  <article class="element-card">
                    <div class="element-copy">
                      <strong>{element.name}</strong>
                      <span>{element.source}</span>
                      <p>{element.preview ?? "No preview available."}</p>
                    </div>
                    <div class="element-meta">
                      <span>{element.token_count} token(s)</span>
                      <span>{element.child_count} child element(s)</span>
                      <button
                        class="secondary"
                        onclick={() =>
                          recordSelection(
                            {
                              element_id: element.element_id,
                              source: "builder-lens",
                            },
                            `Selected ${element.name}.`,
                          )}
                      >
                        Select element
                      </button>
                    </div>
                  </article>
                {/each}
              </div>
            </section>
            {#if detail.object.payload.provider_visible?.requests?.length || detail.object.payload.provider_visible?.reasoning?.length}
              <section class="reference-panel">
                <div class="panel-heading">
                  <h4>Provider Visible</h4>
                  <span>{detail.object.payload.provider_visible?.requests?.length ?? 0} request(s)</span>
                </div>
                <div class="snapshot-meta">
                  <span>{detail.object.payload.provider_visible?.reasoning?.length ?? 0} reasoning event(s)</span>
                </div>
                {#if detail.object.payload.provider_visible?.requests?.[0]}
                  <article class="provider-visible-card">
                    <strong>First request</strong>
                    <code>{JSON.stringify(detail.object.payload.provider_visible.requests[0], null, 2)}</code>
                  </article>
                {/if}
              </section>
            {/if}
          </div>
        {/if}

        <section class="reference-panel interaction-panel">
          <div class="panel-heading">
            <h4>Interaction trail</h4>
            <span>{detail.interactions.length}</span>
          </div>
          {#if detail.interactions.length === 0}
            <p class="muted-note">No interactions recorded for this object yet.</p>
          {:else}
            <div class="interaction-list">
              {#each detail.interactions as interaction}
                <article class="interaction-card">
                  <strong>{interaction.event_kind}</strong>
                  <span>{interaction.created_at}</span>
                  <code>{JSON.stringify(interaction.payload)}</code>
                </article>
              {/each}
            </div>
          {/if}
        </section>
      {/if}
    </section>
  </div>
</article>

<style>
  /*
    Riso treatment for the shared-reference studio. Flat ink-bordered panels,
    dotted hairlines between rows, mono kickers. Lives inside the builder
    deck, so it picks up button/input styles from the parent component when
    they render unscoped.
  */

  .studio-card {
    grid-column: 1 / -1;
    gap: 16px;
  }

  .studio-grid {
    display: grid;
    grid-template-columns: minmax(260px, 0.72fr) minmax(0, 1.4fr);
    gap: 14px;
    min-height: 30rem;
  }

  .reference-rail,
  .reference-detail,
  .reference-panel,
  .summary-item,
  .node-card,
  .element-card,
  .interaction-card,
  .empty-detail {
    border: 2px solid var(--ink);
    background: var(--paper);
  }

  .edge-row {
    border: 1px solid var(--rule-soft);
    background: transparent;
  }

  .reference-rail,
  .reference-detail {
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
  }

  .reference-rail header,
  .panel-heading,
  .detail-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: start;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--ink);
  }

  .summary-list,
  .interaction-list,
  .element-list,
  .edge-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
  }

  .summary-item {
    text-align: left;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    cursor: pointer;
  }

  .summary-item:hover {
    background: rgba(14, 59, 102, 0.06);
  }

  .summary-item.selected {
    background: var(--ink);
    color: var(--paper);
    border-color: var(--ink);
  }

  .summary-kind {
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 10px;
    line-height: 1;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--ink-soft);
  }

  .summary-item.selected .summary-kind {
    color: rgba(255, 248, 238, 0.7);
  }

  .reference-detail {
    overflow: auto;
  }

  .detail-header p,
  .snapshot-meta span,
  .element-copy p,
  .element-copy span,
  .interaction-card span,
  .edge-row span {
    margin: 0;
    color: var(--ink-soft);
    font-size: 12.5px;
    line-height: 1.5;
  }

  .detail-body {
    display: grid;
    gap: 12px;
  }

  .reference-panel {
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .node-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 12px;
  }

  .node-card,
  .element-card,
  .provider-visible-card,
  .interaction-card,
  .edge-row {
    padding: 12px 14px;
  }

  .node-card {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .node-card.entry {
    border-color: var(--accent);
    border-top-width: 4px;
  }

  .edge-row,
  .snapshot-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
    font-family: var(--font-mono);
    font-size: 11.5px;
  }

  .element-card {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 14px;
    align-items: start;
  }

  .element-copy,
  .element-meta,
  .provider-visible-card,
  .interaction-card {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .provider-visible-card code,
  .interaction-card code {
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--ink-orange);
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 1.4;
  }

  .empty-detail {
    padding: 22px;
    border-style: dashed;
    color: var(--ink-soft);
    font-family: var(--font-mono);
    font-weight: 500;
    font-size: 11px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  @media (max-width: 1100px) {
    .studio-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
