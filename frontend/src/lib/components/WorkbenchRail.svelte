<script lang="ts">
  import { listBenchMaterials, listSessionMaterials } from "../api/materials";
  import { listSharedReferences } from "../api/builder";
  import { fetchOperatorOverview } from "../api/operator";
  import WorkbenchOperations from "./WorkbenchOperations.svelte";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type {
    LensSurface,
    MaterialRecord,
    OperatorOverview,
    SharedReferenceSummary,
  } from "../types";
  import { relativeTime } from "../utils/relative-time";

  interface Props {
    benchId: string | null;
    benchName?: string | null;
    sessionId: string | null;
    activeSurface: LensSurface;
    desktopRuntime?: DesktopRuntimeInfo | null;
  }

  let {
    benchId,
    benchName = null,
    sessionId,
    activeSurface,
    desktopRuntime = null,
  }: Props = $props();

  let benchMaterials = $state<MaterialRecord[]>([]);
  let sessionMaterials = $state<MaterialRecord[]>([]);
  let sharedReferences = $state<SharedReferenceSummary[]>([]);
  let operatorOverview = $state<OperatorOverview | null>(null);
  let isLoading = $state(false);
  let loadError = $state<string | null>(null);

  let lastLoadKey = "";

  const materialCount = $derived(benchMaterials.length + sessionMaterials.length);
  const surfaceLabel = $derived(
    activeSurface === "builder"
      ? "Builder workbench"
      : activeSurface === "operator"
        ? "Operator workbench"
        : "Assistant workbench",
  );

  $effect(() => {
    const nextKey = `${benchId ?? ""}:${sessionId ?? ""}:${activeSurface}`;
    if (nextKey === lastLoadKey) {
      return;
    }
    lastLoadKey = nextKey;
    void refreshRail();
  });

  async function refreshRail(): Promise<void> {
    if (!benchId) {
      benchMaterials = [];
      sessionMaterials = [];
      sharedReferences = [];
      operatorOverview = null;
      loadError = null;
      return;
    }

    isLoading = true;
    loadError = null;

    try {
      const [nextBenchMaterials, nextSessionMaterials, nextSharedReferences, nextOverview] =
        await Promise.all([
          listBenchMaterials(benchId),
          sessionId ? listSessionMaterials(sessionId) : Promise.resolve([]),
          listSharedReferences(benchId),
          fetchOperatorOverview(),
        ]);

      benchMaterials = nextBenchMaterials;
      sessionMaterials = nextSessionMaterials;
      sharedReferences = nextSharedReferences;
      operatorOverview = nextOverview;
    } catch (error) {
      loadError =
        error instanceof Error ? error.message : "Failed to load the workbench rail.";
    } finally {
      isLoading = false;
    }
  }

</script>

<aside class="workbench-rail" aria-label="Workbench rail">
  <div class="rail-header">
    <div>
      <p class="rail-kicker">{surfaceLabel}</p>
      <h2>{benchName ?? "Workspace context"}</h2>
      <p class="rail-copy">
        Materials, shared objects, and runtime signal stay visible while the
        main stage changes.
      </p>
    </div>
    <button class="refresh-btn" onclick={refreshRail} disabled={isLoading}>
      {isLoading ? "Syncing…" : "Refresh"}
    </button>
  </div>

  {#if loadError}
    <p class="feedback error">{loadError}</p>
  {/if}

  <section class="rail-metrics">
    <article class="metric-card">
      <span class="metric-label">Materials</span>
      <strong>{materialCount}</strong>
      <span>{sessionMaterials.length} tied to this thread</span>
    </article>
    <article class="metric-card">
      <span class="metric-label">Shared refs</span>
      <strong>{sharedReferences.length}</strong>
      <span>durable visual objects</span>
    </article>
    <article class="metric-card">
      <span class="metric-label">Approvals</span>
      <strong>{operatorOverview?.approvals.length ?? "—"}</strong>
      <span>{operatorOverview?.gateway.health ?? "gateway state pending"}</span>
    </article>
  </section>

  {#if desktopRuntime}
    <section class="rail-panel runtime-panel">
      <div class="panel-heading">
        <h3>Desktop runtime</h3>
        <span>{desktopRuntime.managedGateway ? "managed" : "remote"}</span>
      </div>
      <p class="mono">{desktopRuntime.gatewayUrl}</p>
    </section>
  {/if}

  <WorkbenchOperations {benchId} {sessionId} onChanged={refreshRail} />

  <section class="rail-panel">
    <div class="panel-heading">
      <h3>Materials</h3>
      <span>{materialCount}</span>
    </div>
    <div class="stack">
      {#if sessionMaterials.length}
        <div class="subpanel">
          <strong>Active conversation</strong>
          <div class="item-list">
            {#each sessionMaterials as material (material.id)}
              <article class="item-card">
                <div class="item-title-row">
                  <strong>{material.title}</strong>
                  <span class="pill accent">session</span>
                </div>
                <p>{material.relative_path}</p>
                <span class="timestamp">{relativeTime(material.updated_at)}</span>
              </article>
            {/each}
          </div>
        </div>
      {/if}

      <div class="subpanel">
        <strong>Bench workspace</strong>
        {#if benchMaterials.length === 0}
          <p class="muted-note">No bench materials yet.</p>
        {:else}
          <div class="item-list">
            {#each benchMaterials as material (material.id)}
              <article class="item-card">
                <div class="item-title-row">
                  <strong>{material.title}</strong>
                  <span class="pill">{material.status}</span>
                </div>
                <p>{material.relative_path}</p>
                <span class="timestamp">{relativeTime(material.updated_at)}</span>
              </article>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  </section>

  <section class="rail-panel">
    <div class="panel-heading">
      <h3>Shared references</h3>
      <span>{sharedReferences.length}</span>
    </div>
    {#if sharedReferences.length === 0}
      <p class="muted-note">
        No shared objects yet. Team topology and context artefacts will appear here.
      </p>
    {:else}
      <div class="item-list">
        {#each sharedReferences.slice(0, 6) as reference (reference.id)}
          <article class="item-card">
            <div class="item-title-row">
              <strong>{reference.title}</strong>
              <span class="pill">{reference.kind.replaceAll("_", " ")}</span>
            </div>
            <p>{reference.subtitle}</p>
            <span class="timestamp">{relativeTime(reference.updated_at)}</span>
          </article>
        {/each}
      </div>
    {/if}
  </section>

  <section class="rail-panel">
    <div class="panel-heading">
      <h3>Operator pulse</h3>
      <span>{operatorOverview?.gateway.health ?? "…"}</span>
    </div>
    {#if !operatorOverview}
      <p class="muted-note">Operator signal is loading.</p>
    {:else}
      <div class="stack">
        <div class="signal-row">
          <strong>Gateway</strong>
          <span>{operatorOverview.gateway.active_turn_count} active turn(s)</span>
        </div>
        <div class="signal-row">
          <strong>Sessions</strong>
          <span>{operatorOverview.sessions.length} recent</span>
        </div>
        <div class="signal-row">
          <strong>Automation</strong>
          <span>
            {operatorOverview.scheduled_jobs.jobs.length + operatorOverview.heartbeats.heartbeats.length}
            live
          </span>
        </div>
        <div class="signal-row">
          <strong>Nodes</strong>
          <span>{operatorOverview.nodes.length} attached</span>
        </div>
        {#if operatorOverview.approvals.length}
          <div class="attention-callout">
            <strong>{operatorOverview.approvals.length} approval(s) waiting</strong>
            <span>{operatorOverview.approvals[0].prompt}</span>
          </div>
        {/if}
      </div>
    {/if}
  </section>
</aside>

<style>
  .workbench-rail {
    --rail-border: rgba(120, 53, 15, 0.14);
    --rail-panel: linear-gradient(180deg, rgba(255, 252, 247, 0.98), rgba(248, 243, 234, 0.94));
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 320px;
    max-width: 390px;
    padding: 18px;
    overflow: auto;
    border-left: 1px solid rgba(120, 53, 15, 0.1);
    background:
      radial-gradient(circle at top right, rgba(245, 158, 11, 0.12), transparent 30%),
      linear-gradient(180deg, rgba(255, 253, 249, 0.96), rgba(244, 239, 231, 0.98));
  }

  .rail-header,
  .rail-panel,
  .metric-card {
    border: 1px solid var(--rail-border);
    border-radius: 22px;
    background: var(--rail-panel);
    box-shadow:
      0 18px 35px rgba(120, 53, 15, 0.06),
      inset 0 1px 0 rgba(255, 255, 255, 0.8);
  }

  .rail-header,
  .rail-panel {
    padding: 18px;
  }

  .rail-header {
    display: flex;
    gap: 12px;
    justify-content: space-between;
    align-items: flex-start;
  }

  .rail-kicker {
    margin-bottom: 6px;
    font-size: 0.75rem;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: #9a3412;
    font-weight: 700;
  }

  h2,
  h3 {
    font-weight: 700;
    color: #1f2937;
  }

  h2 {
    font-size: 1.25rem;
  }

  h3 {
    font-size: 1rem;
  }

  .rail-copy,
  .muted-note,
  .item-card p,
  .signal-row span,
  .feedback {
    color: #5b4634;
    font-size: 0.92rem;
    line-height: 1.45;
  }

  .refresh-btn {
    border-radius: 999px;
    border: 1px solid rgba(120, 53, 15, 0.16);
    background: rgba(255, 255, 255, 0.72);
    padding: 9px 14px;
    transition:
      transform 140ms ease,
      background 140ms ease,
      border-color 140ms ease;
  }

  .refresh-btn:hover {
    transform: translateY(-1px);
  }

  .rail-metrics {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 10px;
  }

  .metric-card {
    padding: 14px;
    display: grid;
    gap: 4px;
  }

  .metric-label,
  .panel-heading span,
  .item-title-row .pill,
  .timestamp {
    font-size: 0.75rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .metric-card strong {
    font-size: 1.5rem;
    color: #7c2d12;
  }

  .panel-heading,
  .item-title-row,
  .signal-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
  }

  .stack,
  .item-list,
  .subpanel {
    display: grid;
    gap: 10px;
  }

  .subpanel strong {
    font-size: 0.86rem;
    color: #6b3f1d;
  }

  .item-card {
    display: grid;
    gap: 4px;
    padding: 12px 14px;
    border-radius: 16px;
    background: rgba(255, 255, 255, 0.64);
    border: 1px solid rgba(120, 53, 15, 0.08);
  }

  .pill {
    display: inline-flex;
    align-items: center;
    border-radius: 999px;
    padding: 3px 8px;
    background: rgba(31, 41, 55, 0.08);
    color: #374151;
  }

  .pill.accent {
    background: rgba(180, 83, 9, 0.12);
    color: #9a3412;
  }

  .attention-callout {
    display: grid;
    gap: 4px;
    border-radius: 16px;
    padding: 12px 14px;
    background: linear-gradient(135deg, rgba(254, 243, 199, 0.9), rgba(255, 247, 237, 0.94));
    border: 1px solid rgba(217, 119, 6, 0.18);
  }

  .feedback {
    border-radius: 16px;
    padding: 10px 12px;
  }

  .feedback.error {
    background: rgba(254, 226, 226, 0.76);
    color: #991b1b;
  }

  .mono {
    font-family: "SFMono-Regular", "Menlo", "Monaco", monospace;
    font-size: 0.82rem;
    color: #7c2d12;
    word-break: break-all;
  }

  @media (max-width: 1180px) {
    .workbench-rail {
      max-width: none;
      min-width: 0;
      border-left: none;
      border-top: 1px solid rgba(120, 53, 15, 0.1);
    }

    .rail-metrics {
      grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    }
  }
</style>
