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
    projectId: string | null;
    onTeamTopologyUpdated?: () => void | Promise<void>;
  }

  let { projectId, onTeamTopologyUpdated }: Props = $props();

  let summaries = $state<SharedReferenceSummary[]>([]);
  let selectedKey = $state<string | null>(null);
  let detail = $state<SharedReferenceDetailResponse | null>(null);
  let isLoading = $state(false);
  let detailLoading = $state(false);
  let loadError = $state<string | null>(null);
  let detailError = $state<string | null>(null);
  let notice = $state<string | null>(null);

  $effect(() => {
    if (!projectId) {
      summaries = [];
      selectedKey = null;
      detail = null;
      loadError = null;
      detailError = null;
      notice = null;
      return;
    }
    void refreshSummaries(projectId);
  });

  async function refreshSummaries(nextProjectId: string): Promise<void> {
    isLoading = true;
    loadError = null;

    try {
      const objects = await listSharedReferences(nextProjectId);
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
      if (projectId) {
        await refreshSummaries(projectId);
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

      {#if !projectId}
        <p class="muted-note">Select a project to inspect shared objects.</p>
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
  .studio-card {
    grid-column: 1 / -1;
    gap: 18px;
  }

  .studio-grid {
    display: grid;
    grid-template-columns: minmax(260px, 0.72fr) minmax(0, 1.4fr);
    gap: 16px;
    min-height: 32rem;
  }

  .reference-rail,
  .reference-detail,
  .reference-panel,
  .summary-item,
  .node-card,
  .element-card,
  .interaction-card,
  .edge-row,
  .empty-detail {
    border-radius: 20px;
    border: 1px solid rgba(120, 53, 15, 0.12);
    background: rgba(255, 255, 255, 0.76);
  }

  .reference-rail,
  .reference-detail {
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 0;
  }

  .reference-rail header,
  .panel-heading,
  .detail-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    align-items: start;
  }

  .summary-list,
  .interaction-list,
  .element-list,
  .edge-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
  }

  .summary-item {
    text-align: left;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    cursor: pointer;
  }

  .summary-item.selected {
    border-color: rgba(180, 83, 9, 0.48);
    box-shadow: inset 0 0 0 1px rgba(180, 83, 9, 0.12);
    background: linear-gradient(180deg, rgba(255, 247, 237, 0.98), rgba(255, 255, 255, 0.82));
  }

  .summary-kind {
    font-size: 0.75rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: #9a3412;
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
    color: #6b7280;
  }

  .detail-body {
    display: grid;
    gap: 14px;
  }

  .reference-panel {
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .node-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 12px;
  }

  .node-card,
  .element-card,
  .interaction-card,
  .edge-row {
    padding: 14px;
  }

  .node-card {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .node-card.entry {
    background: linear-gradient(180deg, rgba(255, 247, 237, 0.94), rgba(255, 255, 255, 0.82));
    border-color: rgba(180, 83, 9, 0.32);
  }

  .edge-row,
  .snapshot-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
  }

  .element-card {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 14px;
    align-items: start;
  }

  .element-copy,
  .element-meta,
  .interaction-card {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .interaction-card code {
    white-space: pre-wrap;
    word-break: break-word;
    color: #7c2d12;
  }

  .empty-detail {
    padding: 24px;
    color: #7c2d12;
  }

  @media (max-width: 1100px) {
    .studio-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
