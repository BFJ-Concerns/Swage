<script lang="ts">
  import { onMount } from "svelte";
  import { fetchInstallSecuritySettings, saveInstallSecuritySettings, fetchTopicSecuritySettings, saveTopicSecuritySettings } from "../../api/settings";
  import type { SecuritySettingsResponse } from "../../protocol/SecuritySettingsResponse";
  import type { RequireApprovalEntry } from "../../protocol/RequireApprovalEntry";
  import type { AccessGateMode } from "../../protocol/AccessGateMode";
  import ApprovalToolPicker from "./ApprovalToolPicker.svelte";
  import { CHECK_LABELS, relaxedBy, securitySettingsChanged } from "../../security-settings";

  let { topicId = null, onSaved }: { topicId?: string | null; onSaved?: () => void | Promise<void> } = $props();
  let security = $state<SecuritySettingsResponse | null>(null);
  let sandbox = $state("inherit");
  let confinement = $state("inherit");
  let approvalMode = $state("inherit");
  let approvals = $state<RequireApprovalEntry[]>([]);
  let access = $state("inherit");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state("");
  const scopeLabel = $derived(topicId ? "this topic" : "the install");
  function loaded(value: SecuritySettingsResponse): void {
    security = value;
    const own = value.overrides;
    sandbox = own.sandbox_enabled == null ? "inherit" : own.sandbox_enabled ? "on" : "off";
    confinement = own.allow_degraded_execution == null ? "inherit" : own.allow_degraded_execution ? "allow_degraded" : "strict";
    approvalMode = own.require_approval == null ? "inherit" : "own";
    approvals = own.require_approval ?? value.effective.require_approval;
    access = own.access_gate ?? "inherit";
  }
  onMount(async () => {
    try { loaded(await (topicId ? fetchTopicSecuritySettings(topicId) : fetchInstallSecuritySettings())); }
    catch (err) { error = err instanceof Error ? err.message : "Failed to load security settings."; }
  });
  async function save(): Promise<void> {
    busy = true; error = null; notice = "";
    const overrides = {
      sandbox_enabled: sandbox === "inherit" ? undefined : sandbox === "on",
      allow_degraded_execution: confinement === "inherit" ? undefined : confinement === "allow_degraded",
      require_approval: approvalMode === "inherit" ? undefined : approvals,
      ...(!topicId ? { access_gate: access === "inherit" ? undefined : access as AccessGateMode } : {}),
    };
    try {
      loaded(await (topicId ? saveTopicSecuritySettings(topicId, overrides) : saveInstallSecuritySettings(overrides)));
      securitySettingsChanged();
      await onSaved?.();
      notice = security!.relaxed.length ? "Saved. New turns use these settings." : `Saved. Every shipped check is in force for ${scopeLabel}.`;
    } catch (err) { error = err instanceof Error ? err.message : "Failed to save security settings."; }
    finally { busy = false; }
  }
</script>

<section aria-label="Security settings">
  <h2>Security checks</h2>
  <p>Every shipped check is on by default. Saved changes apply to the next turn without a restart. Relaxations are recorded in the audit trail.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if security}
    <div aria-label="Effective security checks">
      <p>Sandbox: {security.effective.sandbox_enabled ? "On" : "Off"}</p>
      <p>Sandbox confinement: {security.effective.allow_degraded_execution ? "Allow unconfined fallback" : "Strict"}</p>
      <p>Approval prompts: {security.effective.require_approval.map((entry) => typeof entry === "string" ? entry : entry.tool).join(", ") || "none"}</p>
      <p>Access gate: {security.effective.access_gate}{topicId ? " (install setting)" : ""}</p>
    </div>
    <div aria-label="Relaxed checks">
      {#each security.relaxed as entry (entry.check)}
        <p><strong>{CHECK_LABELS[entry.check]} — {relaxedBy(entry)}</strong><br />{entry.detail}</p>
      {:else}<p>Every shipped check is in force for {scopeLabel}.</p>{/each}
    </div>
    <form onsubmit={(event) => { event.preventDefault(); void save(); }}>
      <fieldset disabled={busy}>
        <label>Sandbox
          <select aria-label={`Sandbox for ${scopeLabel}`} bind:value={sandbox}>
            <option value="inherit">{topicId ? "Follow the install" : "Use shipped default"}</option>
            <option value="on">On</option><option value="off">Off (relaxed)</option>
          </select>
        </label>
        <label>Sandbox confinement
          <select aria-label={`Sandbox confinement for ${scopeLabel}`} bind:value={confinement}>
            <option value="inherit">{topicId ? "Follow the install" : "Use shipped default"}</option>
            <option value="strict">Strict</option><option value="allow_degraded">Allow unconfined fallback (relaxed)</option>
          </select>
        </label>
        <label>Approval prompts
          <select aria-label={`Approval prompts for ${scopeLabel}`} bind:value={approvalMode}>
            <option value="inherit">{topicId ? "Follow the install" : "Use shipped default"}</option>
            <option value="own">{topicId ? "This topic's own list" : "The install's own list"}</option>
          </select>
        </label>
        {#if approvalMode === "own"}
          <ApprovalToolPicker tools={security.available_tools} entries={approvals} onChange={(entries) => approvals = entries} />
        {/if}
        {#if !topicId}
          <label>Access gate
            <select aria-label="Access gate for the install" bind:value={access}>
              <option value="inherit">Use shipped default</option><option value="token">Require token</option>
              <option value="warn">Warn (relaxed)</option><option value="off">Off (relaxed)</option>
            </select>
          </label>
        {/if}
        <button type="submit">{busy ? "Saving…" : "Save security settings"}</button>
      </fieldset>
    </form>
    {#if notice}<p role="status">{notice}</p>{/if}
  {:else if !error}<p>Loading…</p>{/if}
</section>

<style>
  section, fieldset { display: grid; gap: 1rem; }
  fieldset { border: 0; padding: 0; }
  label { display: grid; gap: 0.25rem; }
  select, button { color: var(--ink); background: var(--paper); border: 1px solid var(--rule); padding: 0.5rem; }
</style>
