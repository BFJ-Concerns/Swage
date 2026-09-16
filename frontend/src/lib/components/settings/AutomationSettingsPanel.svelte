<script lang="ts">
  import ScheduleAutomationPanel from "../ScheduleAutomationPanel.svelte";
  import type { RequireApprovalEntry, SettingsResponse, Topic } from "../../types";
  interface Props { settings: SettingsResponse; topicId: string | null; sessionId: string | null; topics?: Topic[]; onValidation?: (bundlesInvalid: boolean, approvalsInvalid: boolean, message: string | null) => void; }
  let { settings = $bindable(), topicId, sessionId, topics = [], onValidation }: Props = $props();
  let bundlesInvalid = false;
  let approvalsInvalid = false;
  function changed(): void { settings = { ...settings }; }
  function lines(values: string[]): string { return values.join("\n"); }
  function parseLines(value: string): string[] { return value.split("\n").map((line) => line.trim()).filter(Boolean); }
  function updateBundles(value: string): void {
    try {
      const parsed = JSON.parse(value) as Record<string, unknown>; const bundles: Record<string, string[]> = {};
      for (const [name, tools] of Object.entries(parsed)) { if (!Array.isArray(tools) || tools.some((tool) => typeof tool !== "string")) throw new Error("Tool bundles must be a JSON object of string arrays."); bundles[name] = tools; }
      settings.tool_permissions.bundles = bundles; bundlesInvalid = false; changed(); onValidation?.(false, approvalsInvalid, null);
    } catch (error) { bundlesInvalid = true; onValidation?.(true, approvalsInvalid, error instanceof Error ? error.message : "Tool bundles must be valid JSON."); }
  }
  function updateApprovals(value: string): void {
    try {
      const parsed = JSON.parse(value) as unknown; if (!Array.isArray(parsed)) throw new Error("Approval rules must be a JSON array.");
      for (const entry of parsed) if (typeof entry !== "string" && (!entry || typeof entry !== "object" || typeof (entry as {tool?:unknown}).tool !== "string")) throw new Error("Approval rule objects need a tool string.");
      settings.tool_permissions.require_approval = parsed as RequireApprovalEntry[]; approvalsInvalid = false; changed(); onValidation?.(bundlesInvalid, false, null);
    } catch (error) { approvalsInvalid = true; onValidation?.(bundlesInvalid, true, error instanceof Error ? error.message : "Approval rules must be valid JSON."); }
  }
</script>

<div class="field"><div class="key">Schedules<small>Create and operate durable scheduled agent work.</small></div><div class="val"><ScheduleAutomationPanel {topicId} {sessionId} {topics} /></div></div>
<div class="field"><div class="key">Session archiving<small>Days without a user or assistant message before a session leaves the recent list. Only a session with a turn executing stays; the next turn on an archived session brings it back, and nothing is cancelled.</small></div><div class="val"><input type="number" class="text small" aria-label="Archive sessions after days" bind:value={settings.sessions.archive_after_days} oninput={changed} min="1" max="3650" placeholder="7"/></div></div>
<div class="field"><div class="key">Sandbox<small>Global sandbox policy written to config.toml. Saved changes require a gateway restart.</small></div><div class="val"><button type="button" class="opt" class:active={settings.sandbox.enabled} onclick={() => {settings.sandbox.enabled=true;changed();}}>Enabled</button><button type="button" class="opt" class:active={!settings.sandbox.enabled} onclick={() => {settings.sandbox.enabled=false;changed();}}>Disabled</button></div></div>
<div class="field"><div class="key">Approval defaults<small>Timeout and timeout policy for human approval requests without a per-tool override.</small></div><div class="val val-stack"><div class="search-row"><input type="number" class="text small" bind:value={settings.hitl.timeout} oninput={changed} min="1" placeholder="300"/><button type="button" class="opt" class:active={settings.hitl.timeout_policy === "reject"} onclick={() => {settings.hitl.timeout_policy="reject";changed();}}>Reject</button><button type="button" class="opt" class:active={settings.hitl.timeout_policy === "interrupt"} onclick={() => {settings.hitl.timeout_policy="interrupt";changed();}}>Interrupt</button></div></div></div>
<div class="field"><div class="key">Context assembly<small>Assembly strategy, assembly timeout, and optional token budget.</small></div><div class="val val-stack"><div class="search-row"><input type="text" class="text" bind:value={settings.context.assembly} oninput={changed} placeholder="default" autocomplete="off"/><input type="number" class="text small" bind:value={settings.context.timeout} oninput={changed} min="1" max="60" placeholder="5"/><input type="number" class="text small" value={settings.context.budget ?? ""} min="1" placeholder="model limit" oninput={(event)=>{settings.context.budget=event.currentTarget.value?Number(event.currentTarget.value):null;changed();}}/></div></div></div>
<div class="field"><div class="key">Tool allow rules<small>One rule per line. Use * for broad availability.</small></div><div class="val"><textarea class="text" rows="4" value={lines(settings.tool_permissions.allow)} oninput={(event)=>{settings.tool_permissions.allow=parseLines(event.currentTarget.value);changed();}}></textarea></div></div>
<div class="field"><div class="key">Tool deny rules<small>One denied tool name or pattern per line.</small></div><div class="val"><textarea class="text" rows="4" value={lines(settings.tool_permissions.deny)} oninput={(event)=>{settings.tool_permissions.deny=parseLines(event.currentTarget.value);changed();}}></textarea></div></div>
<div class="field"><div class="key">Tool bundles<small>JSON object mapping bundle names to tool arrays.</small></div><div class="val"><textarea class="text code" rows="7" value={JSON.stringify(settings.tool_permissions.bundles,null,2)} spellcheck="false" oninput={(event)=>updateBundles(event.currentTarget.value)}></textarea></div></div>
<div class="field"><div class="key">Approval-gated tools<small>JSON array of tool names or objects with timeout overrides.</small></div><div class="val"><textarea class="text code" rows="7" value={JSON.stringify(settings.tool_permissions.require_approval,null,2)} spellcheck="false" oninput={(event)=>updateApprovals(event.currentTarget.value)}></textarea></div></div>

<style>
  .field{display:grid;grid-template-columns:220px 1fr;gap:24px;border-top:1px solid var(--rule-soft);padding:14px 0;align-items:baseline}.key{font-family:var(--font-mono);font-weight:500;font-size:11px;line-height:1.5;letter-spacing:.18em;text-transform:uppercase}.key small{display:block;margin-top:4px;font-family:var(--font-body);font-weight:400;font-size:11px;line-height:1.4;letter-spacing:0;text-transform:none;color:var(--ink-soft)}.val{display:flex;gap:10px;flex-wrap:wrap}.val-stack{flex-direction:column;align-items:stretch;gap:12px}.search-row{display:flex;gap:8px;flex-wrap:wrap}.opt{display:inline-flex;align-items:center;border:1px solid var(--ink);background:var(--paper);color:var(--ink);padding:6px 12px;font-family:var(--font-display);font-size:14px;text-transform:uppercase;cursor:pointer}.opt.active{background:var(--ink);color:var(--paper)}.text{width:100%;max-width:480px;padding:9px 11px;border:1px solid var(--ink);background:var(--paper);color:var(--ink);font-family:var(--font-body);font-size:13px}.text.small{max-width:160px}textarea.text{resize:vertical;min-height:90px;font-family:var(--font-mono);font-size:12px}@media(max-width:760px){.field{grid-template-columns:1fr;gap:10px}}
</style>
