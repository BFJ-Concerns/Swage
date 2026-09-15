<script lang="ts">
  import { onMount } from "svelte";
  import { downloadRecoveryExport, exportRecoveryBundle, listRecoveryExports } from "../../api/workflows";
  import type { RecoveryExportSummary } from "../../types";

  let recoveryExports = $state<RecoveryExportSummary[]>([]);
  let isLoading = $state(false);
  let activeExportId = $state<string | null>(null);
  let status = $state("");
  let error = $state<string | null>(null);

  onMount(loadExports);

  async function loadExports(): Promise<void> {
    isLoading = true;
    try {
      recoveryExports = (await listRecoveryExports()).exports;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to load recovery exports.";
    } finally {
      isLoading = false;
    }
  }

  async function createExport(): Promise<void> {
    activeExportId = "new";
    try {
      await exportRecoveryBundle();
      await loadExports();
      status = "Recovery export captured.";
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to create recovery export.";
    } finally {
      activeExportId = null;
    }
  }

  async function downloadExport(exportId: string): Promise<void> {
    activeExportId = exportId;
    try {
      const json = JSON.stringify(await downloadRecoveryExport(exportId), null, 2);
      const filename = `swage-recovery-${exportId}.json`;
      const url = URL.createObjectURL(new Blob([json], { type: "application/json" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      link.click();
      URL.revokeObjectURL(url);
      status = `Downloaded ${filename}.`;
      error = null;
    } catch (err) {
      error = err instanceof Error ? err.message : "Failed to download recovery export.";
    } finally {
      activeExportId = null;
    }
  }

  function countSummary(item: RecoveryExportSummary): string {
    return [["topics", item.topics], ["workspaces", item.workspaces], ["sessions", item.sessions], ["secrets", item.secrets], ["channels", item.channel_links], ["devices", item.devices], ["schedules", item.schedules]].map(([label, value]) => `${label}: ${value}`).join(" · ");
  }

  function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    const kib = bytes / 1024;
    return kib < 1024 ? `${kib.toFixed(1)} KiB` : `${(kib / 1024).toFixed(1)} MiB`;
  }
</script>

{#if error}<div class="notice error" role="alert"><span>{error}</span><button type="button" class="notice-dismiss" onclick={() => (error = null)} aria-label="Dismiss">×</button></div>{/if}
{#if status}<div class="notice"><span>{status}</span></div>{/if}

<section class="shell-section">
  <div class="section-heading-row">
    <h2 class="shell-section-title">Recovery exports</h2>
    <div class="opt-row">
      <button type="button" class="btn" onclick={loadExports} disabled={isLoading || activeExportId !== null}>{isLoading ? "Refreshing" : "Refresh"}</button>
      <button type="button" class="btn primary" onclick={createExport} disabled={activeExportId !== null}>{activeExportId === "new" ? "Exporting" : "Export now"}</button>
    </div>
  </div>
  {#if recoveryExports.length === 0}
    <p class="shell-empty">No recovery exports have been produced by this gateway process.</p>
  {:else}
    <div class="recovery-export-list">
      {#each recoveryExports as recoveryExport (recoveryExport.id)}
        <article class="recovery-export-row">
          <div><strong>{new Date(recoveryExport.created_at).toLocaleString()}</strong><span>{countSummary(recoveryExport)}</span></div>
          <div class="recovery-export-actions"><span>{formatSize(recoveryExport.size_bytes)}</span><button type="button" class="btn" onclick={() => downloadExport(recoveryExport.id)} disabled={activeExportId !== null}>{activeExportId === recoveryExport.id ? "Preparing" : "Download"}</button></div>
        </article>
      {/each}
    </div>
  {/if}
</section>

<style>
  .notice { display: flex; justify-content: space-between; align-items: center; gap: 12px; padding: 10px 14px; background: var(--accent); color: var(--paper); border: 2px solid var(--ink); font-family: var(--font-mono); font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; }
  .notice.error { background: var(--ink); border-color: var(--accent); }
  .notice-dismiss { background: var(--paper); color: var(--ink); border: 1px solid var(--paper); padding: 0 8px; font-family: var(--font-mono); font-size: 14px; cursor: pointer; }
  .shell-section { padding: 16px 18px; border: 1px solid var(--rule-soft); background: var(--paper); display: flex; flex-direction: column; gap: 10px; }
  .section-heading-row, .recovery-export-actions, .opt-row { display: flex; align-items: center; gap: 10px; }
  .section-heading-row { justify-content: space-between; gap: 12px; flex-wrap: wrap; }
  .shell-section-title { margin: 0; font-family: var(--font-display); font-weight: 400; font-size: 18px; line-height: 1.05; letter-spacing: 0.06em; text-transform: uppercase; color: var(--ink); }
  .shell-empty { margin: 0; color: var(--ink-soft); font-size: 13px; line-height: 1.55; }
  .recovery-export-list { display: grid; gap: 8px; }
  .recovery-export-row { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 14px; align-items: center; padding: 12px 0; border-top: 1px dotted var(--rule-soft); }
  .recovery-export-row strong, .recovery-export-row span { display: block; }
  .recovery-export-row strong { color: var(--ink); font-size: 13px; line-height: 1.3; }
  .recovery-export-row span { color: var(--ink-soft); font-size: 12px; line-height: 1.45; }
  .recovery-export-actions { justify-content: flex-end; }
  .btn { background: var(--paper); color: var(--ink); border: 1px solid var(--ink); padding: 8px 16px; font-family: var(--font-display); font-size: 14px; line-height: 1; letter-spacing: 0.06em; text-transform: uppercase; cursor: pointer; }
  .btn:hover:not(:disabled) { background: var(--ink); color: var(--paper); }
  .btn.primary { background: var(--accent); color: var(--paper); border-color: var(--accent); }
  .btn:disabled { opacity: 0.4; cursor: not-allowed; }
  @media (max-width: 760px) { .recovery-export-row { grid-template-columns: 1fr; } .recovery-export-actions { justify-content: space-between; } }
</style>
