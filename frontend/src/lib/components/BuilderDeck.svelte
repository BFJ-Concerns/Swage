<script lang="ts">
  import OperatorDashboard from "./OperatorDashboard.svelte";
  import type { DesktopRuntimeInfo } from "../runtime";
  import type { ProjectStore } from "../stores/projects";

  interface Props {
    projectStore?: ProjectStore;
    desktopRuntime?: DesktopRuntimeInfo | null;
  }

  let { projectStore, desktopRuntime }: Props = $props();
</script>

<section class="builder-deck">
  <header class="builder-hero">
    <div class="hero-copy">
      <p class="eyebrow">Builder Lens</p>
      <h1>Shape the workspace, not just the next reply.</h1>
      <p class="lede">
        This lens foregrounds the machinery around the agent: project footing,
        runtime posture, and the live operator surface underneath.
      </p>
    </div>

    <div class="status-board">
      <article class="status-card">
        <span class="label">Current project</span>
        <strong>{projectStore?.currentProject?.name ?? "No project selected"}</strong>
        <span class="meta mono">
          {projectStore?.currentProject?.workspace_path ?? "workspace pending"}
        </span>
      </article>

      <article class="status-card">
        <span class="label">Runtime</span>
        <strong>{desktopRuntime ? "Desktop shell" : "Direct web"}</strong>
        <span class="meta">
          {desktopRuntime?.managedGateway ? "Managed gateway" : "Gateway-served UI"}
        </span>
      </article>

      <article class="status-card">
        <span class="label">Working set</span>
        <strong>{projectStore?.projects.length ?? 0} project(s)</strong>
        <span class="meta">Builder lens keeps structure and operations in view.</span>
      </article>
    </div>
  </header>

  <div class="operator-shell">
    <OperatorDashboard />
  </div>
</section>

<style>
  .builder-deck {
    display: flex;
    flex-direction: column;
    gap: 18px;
    padding: 22px;
    min-height: 0;
    overflow: auto;
  }

  .builder-hero {
    display: grid;
    grid-template-columns: minmax(0, 1.3fr) minmax(320px, 0.9fr);
    gap: 18px;
    align-items: stretch;
  }

  .hero-copy,
  .status-board {
    border: 1px solid rgba(120, 53, 15, 0.12);
    border-radius: 24px;
    background:
      linear-gradient(180deg, rgba(255, 252, 246, 0.96), rgba(248, 244, 235, 0.94));
    box-shadow:
      0 24px 50px rgba(120, 53, 15, 0.06),
      inset 0 1px 0 rgba(255, 255, 255, 0.78);
  }

  .hero-copy {
    padding: 28px;
  }

  .eyebrow {
    margin: 0 0 8px;
    font-size: 0.78rem;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: #9a3412;
    font-weight: 700;
  }

  h1 {
    margin: 0;
    font-size: clamp(2rem, 3vw, 3rem);
    line-height: 1.02;
    color: #1f2937;
  }

  .lede {
    margin: 14px 0 0;
    max-width: 42rem;
    font-size: 1rem;
    line-height: 1.6;
    color: #4b5563;
  }

  .status-board {
    display: grid;
    gap: 12px;
    padding: 18px;
  }

  .status-card {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 14px 16px;
    border-radius: 18px;
    background: rgba(255, 255, 255, 0.74);
    border: 1px solid rgba(148, 163, 184, 0.18);
  }

  .label {
    font-size: 0.78rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: #9a3412;
  }

  strong {
    font-size: 1.05rem;
    color: #111827;
  }

  .meta {
    color: #6b7280;
    font-size: 0.86rem;
  }

  .mono {
    font-family: "SFMono-Regular", "Menlo", "Monaco", monospace;
  }

  .operator-shell {
    min-height: 0;
  }

  @media (max-width: 980px) {
    .builder-hero {
      grid-template-columns: 1fr;
    }

    .builder-deck {
      padding: 16px;
    }
  }
</style>
