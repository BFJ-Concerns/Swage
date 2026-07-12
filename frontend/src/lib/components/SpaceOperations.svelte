<script lang="ts">
  import { createMaterial } from "../api/materials";
  import {
    bytesToBase64,
    createAttachment,
    createSchedule,
    exportRecoveryBundle,
    fetchVoiceCapabilities,
  } from "../api/workflows";
  import type {
    MaterialScopeKind,
    VoiceCapabilitiesResponse,
  } from "../types";
  import type { ScheduleSpec } from "../protocol/ScheduleSpec";
  import type { SessionPolicy } from "../protocol/SessionPolicy";

  interface Props {
    spaceId: string | null;
    sessionId: string | null;
    onChanged?: () => void | Promise<void>;
  }

  let { spaceId, sessionId, onChanged }: Props = $props();

  let noteTitle = $state("");
  let noteContent = $state("");
  let noteScope = $state<MaterialScopeKind>("session");
  let attachmentScope = $state<MaterialScopeKind>("session");
  let selectedFile = $state<File | null>(null);
  let scheduleMessage = $state("");
  let scheduleKind = $state<"interval" | "cron" | "one_shot">("interval");
  let scheduleEvery = $state("every 1 hours");
  let scheduleCron = $state("0 0 9 * * * *");
  let scheduleAt = $state("");
  let schedulePolicy = $state<"fresh" | "continuation">("fresh");
  let scheduleAgentId = $state("gateway");
  let gateEnabled = $state(false);
  let gateModel = $state("");
  let gateCriteria = $state("");
  let voiceCapabilities = $state<VoiceCapabilitiesResponse | null>(null);
  let recoverySummary = $state<string | null>(null);
  let errorMessage = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busyAction = $state<string | null>(null);

  const effectiveNoteScope = $derived(
    noteScope === "session" && sessionId ? "session" : "space",
  );
  const effectiveAttachmentScope = $derived(
    attachmentScope === "session" && sessionId ? "session" : "space",
  );
  const canUseSessionWorkflow = $derived(Boolean(sessionId));

  $effect(() => {
    void loadVoiceCapabilities();
  });

  function slugify(value: string): string {
    return value
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "");
  }

  function buildRelativePath(title: string, scope: MaterialScopeKind): string {
    const base = slugify(title) || "note";
    const stamp = new Date().toISOString().slice(0, 16).replace(/[:T]/g, "-");
    return scope === "session"
      ? `session-notes/${stamp}-${base}.md`
      : `shared/${stamp}-${base}.md`;
  }

  async function runAction(action: string, task: () => Promise<string>): Promise<void> {
    errorMessage = null;
    notice = null;
    busyAction = action;

    try {
      notice = await task();
      await onChanged?.();
    } catch (error) {
      errorMessage = error instanceof Error ? error.message : "Space operation failed.";
    } finally {
      busyAction = null;
    }
  }

  async function loadVoiceCapabilities(): Promise<void> {
    try {
      voiceCapabilities = await fetchVoiceCapabilities();
    } catch {
      voiceCapabilities = null;
    }
  }

  async function saveMaterialNote(): Promise<void> {
    await runAction("material", async () => {
      if (!spaceId) throw new Error("Select a space before saving material.");
      const title = noteTitle.trim();
      const content = noteContent.trim();
      if (!title || !content) {
        throw new Error("Material title and content are both required.");
      }

      await createMaterial({
        scope_kind: effectiveNoteScope,
        session_id: effectiveNoteScope === "session" && sessionId ? sessionId : undefined,
        space_id: effectiveNoteScope === "space" ? spaceId : undefined,
        title,
        relative_path: buildRelativePath(title, effectiveNoteScope),
        mime_type: "text/markdown",
        content_text: content,
      });

      noteTitle = "";
      noteContent = "";
      return effectiveNoteScope === "session"
        ? "Saved material beside the active conversation."
        : "Saved material for the space workspace.";
    });
  }

  async function queueAttachment(): Promise<void> {
    await runAction("attachment", async () => {
      if (!spaceId) throw new Error("Select a space before attaching material.");
      if (!selectedFile) throw new Error("Choose a file before queueing ingestion.");

      const bytes = new Uint8Array(await selectedFile.arrayBuffer());
      const response = await createAttachment({
        scope_kind: effectiveAttachmentScope,
        session_id:
          effectiveAttachmentScope === "session" && sessionId ? sessionId : undefined,
        space_id: effectiveAttachmentScope === "space" ? spaceId : undefined,
        filename: selectedFile.name,
        mime_type: selectedFile.type || "application/octet-stream",
        content_base64: bytesToBase64(bytes),
      });

      selectedFile = null;
      return `Queued ${response.attachment.filename} for ingestion.`;
    });
  }

  function buildScheduleSpec(): ScheduleSpec {
    if (scheduleKind === "interval") {
      const every = scheduleEvery.trim();
      if (!every) throw new Error("Interval schedules need an 'every N minutes' cadence.");
      return { kind: "interval", every };
    }
    if (scheduleKind === "cron") {
      const cron = scheduleCron.trim();
      if (!cron) throw new Error("Cron schedules need a cron expression.");
      return { kind: "cron", cron };
    }
    const at = scheduleAt.trim();
    if (!at) throw new Error("One-off schedules need a moment to fire at.");
    // datetime-local inputs omit the zone; treat the value as local time.
    return { kind: "one_shot", at: new Date(at).toISOString() };
  }

  function buildSessionPolicy(): SessionPolicy {
    if (schedulePolicy === "continuation") {
      if (!sessionId) {
        throw new Error("Open a conversation to schedule work that continues it.");
      }
      return { policy: "continuation", session_id: sessionId };
    }
    if (!spaceId) throw new Error("Select a space before scheduling work.");
    const agentId = scheduleAgentId.trim();
    if (!agentId) throw new Error("Fresh-session schedules need a target agent.");
    return { policy: "fresh", space_id: spaceId, agent_id: agentId };
  }

  async function scheduleWork(): Promise<void> {
    await runAction("schedule", async () => {
      const message = scheduleMessage.trim();
      if (!message) throw new Error("Scheduled work needs a prompt.");

      // A ticked gate with no model is an incomplete choice, not a silent
      // "no gate" — surface it before the request goes out.
      if (gateEnabled && !gateModel.trim()) {
        throw new Error("Enter a gate model, or turn the gate check off.");
      }

      const created = await createSchedule({
        message,
        spec: buildScheduleSpec(),
        session: buildSessionPolicy(),
        routing: null,
        gate: gateEnabled
          ? {
              model: gateModel.trim(),
              criteria: gateCriteria.trim() || null,
            }
          : null,
      });

      scheduleMessage = "";
      return created.next_run_at
        ? `Schedule created — next run ${new Date(created.next_run_at).toLocaleString()}.`
        : `Schedule created: ${created.status}.`;
    });
  }

  async function exportRecovery(): Promise<void> {
    await runAction("recovery", async () => {
      const bundle = await exportRecoveryBundle();
      const counts = Object.entries(bundle)
        .filter(([, value]) => Array.isArray(value))
        .map(([key, value]) => `${key}: ${(value as unknown[]).length}`);
      recoverySummary = counts.join(" · ");
      return "Recovery bundle exported from the gateway.";
    });
  }

  function handleFileChange(event: Event): void {
    selectedFile = (event.currentTarget as HTMLInputElement).files?.[0] ?? null;
  }
</script>

<section class="rail-panel operations-panel">
  <div class="panel-heading">
    <h3>Space operations</h3>
    <span>{canUseSessionWorkflow ? "live" : "space only"}</span>
  </div>

  {#if errorMessage}
    <p class="feedback error">{errorMessage}</p>
  {/if}
  {#if notice}
    <p class="feedback success">{notice}</p>
  {/if}

  <div class="operation-grid">
    <section class="operation-card">
      <div>
        <strong>Capture material</strong>
        <p>Write directly into session or space-managed material.</p>
      </div>
      <div class="scope-toggle" role="tablist" aria-label="Material scope">
        <button
          class:active={effectiveNoteScope === "session"}
          disabled={!sessionId}
          onclick={() => (noteScope = "session")}
        >
          Session
        </button>
        <button
          class:active={effectiveNoteScope === "space"}
          onclick={() => (noteScope = "space")}
        >
          Space
        </button>
      </div>
      <label>
        <span>Title</span>
        <input bind:value={noteTitle} placeholder="Deployment checklist" />
      </label>
      <label>
        <span>Content</span>
        <textarea
          bind:value={noteContent}
          rows="5"
          placeholder="Capture a constraint, checklist, or space note."
        ></textarea>
      </label>
      <button class="primary" onclick={saveMaterialNote} disabled={busyAction === "material" || !spaceId}>
        {busyAction === "material" ? "Saving..." : "Save material"}
      </button>
    </section>

    <section class="operation-card">
      <div>
        <strong>Ingest attachment</strong>
        <p>Queue a file through the background ingestion pipeline.</p>
      </div>
      <div class="scope-toggle" role="tablist" aria-label="Attachment scope">
        <button
          class:active={effectiveAttachmentScope === "session"}
          disabled={!sessionId}
          onclick={() => (attachmentScope = "session")}
        >
          Session
        </button>
        <button
          class:active={effectiveAttachmentScope === "space"}
          onclick={() => (attachmentScope = "space")}
        >
          Space
        </button>
      </div>
      <label>
        <span>File</span>
        <input type="file" onchange={handleFileChange} />
      </label>
      <button class="primary" onclick={queueAttachment} disabled={busyAction === "attachment" || !spaceId}>
        {busyAction === "attachment" ? "Queueing..." : "Queue ingestion"}
      </button>
    </section>

    <section class="operation-card">
      <div>
        <strong>Schedule work</strong>
        <p>Ask an agent to do something on a cadence, on the clock, or once.</p>
      </div>
      <label>
        <span>Prompt</span>
        <textarea bind:value={scheduleMessage} rows="3" placeholder="Run the review checklist."></textarea>
      </label>
      <label>
        <span>When</span>
        <select bind:value={scheduleKind}>
          <option value="interval">On a cadence</option>
          <option value="cron">On the clock (cron)</option>
          <option value="one_shot">Once, at a moment</option>
        </select>
      </label>
      {#if scheduleKind === "interval"}
        <label>
          <span>Cadence</span>
          <input bind:value={scheduleEvery} placeholder="every 1 hours" />
        </label>
      {:else if scheduleKind === "cron"}
        <label>
          <span>Cron expression</span>
          <input bind:value={scheduleCron} placeholder="0 0 9 * * * *" />
        </label>
      {:else}
        <label>
          <span>Moment</span>
          <input type="datetime-local" bind:value={scheduleAt} />
        </label>
      {/if}
      <div class="scope-toggle" role="tablist" aria-label="Schedule session policy">
        <button
          class:active={schedulePolicy === "fresh"}
          onclick={() => (schedulePolicy = "fresh")}
        >
          Fresh session
        </button>
        <button
          class:active={schedulePolicy === "continuation"}
          disabled={!sessionId}
          onclick={() => (schedulePolicy = "continuation")}
        >
          This conversation
        </button>
      </div>
      {#if schedulePolicy === "fresh"}
        <label>
          <span>Agent</span>
          <input bind:value={scheduleAgentId} placeholder="gateway" />
        </label>
      {/if}
      <label class="gate-toggle">
        <input type="checkbox" bind:checked={gateEnabled} />
        <span>Check with a small model first</span>
      </label>
      {#if gateEnabled}
        <label>
          <span>Gate model</span>
          <input bind:value={gateModel} placeholder="model id or alias" />
        </label>
        <label>
          <span>Run when…</span>
          <textarea
            bind:value={gateCriteria}
            rows="2"
            placeholder="Only when there is something new to act on."
          ></textarea>
        </label>
      {/if}
      <button class="primary" onclick={scheduleWork} disabled={busyAction === "schedule" || !spaceId}>
        {busyAction === "schedule" ? "Scheduling..." : "Schedule"}
      </button>
    </section>
  </div>

  <div class="capability-strip">
    <div>
      <strong>Voice</strong>
      <span>
        {voiceCapabilities
          ? `${voiceCapabilities.transcription ? "transcription" : "no transcription"} · ${voiceCapabilities.synthesis ? "speech" : "no speech"}`
          : "capability unknown"}
      </span>
    </div>
    <button class="secondary" onclick={exportRecovery} disabled={busyAction === "recovery"}>
      {busyAction === "recovery" ? "Exporting..." : "Export recovery"}
    </button>
  </div>

  {#if recoverySummary}
    <p class="recovery-summary">{recoverySummary}</p>
  {/if}
</section>

<style>
  .operations-panel {
    display: grid;
    gap: 14px;
  }

  .operation-grid {
    display: grid;
    gap: 12px;
  }

  .operation-card {
    display: grid;
    gap: 10px;
    padding: 14px;
    border-radius: 16px;
    background: rgba(255, 255, 255, 0.64);
    border: 1px solid rgba(120, 53, 15, 0.08);
  }

  .operation-card p,
  .capability-strip span,
  .recovery-summary {
    color: #5b4634;
    font-size: 0.88rem;
    line-height: 1.4;
  }

  .scope-toggle {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px;
  }

  .scope-toggle button,
  .primary,
  .secondary {
    border-radius: 999px;
    border: 1px solid rgba(120, 53, 15, 0.16);
    background: rgba(255, 255, 255, 0.72);
    padding: 9px 14px;
  }

  .scope-toggle button.active {
    background: rgba(180, 83, 9, 0.14);
    border-color: rgba(146, 64, 14, 0.28);
    color: #7c2d12;
    font-weight: 700;
  }

  .primary {
    background: linear-gradient(135deg, #8a3b12, #b45309);
    border-color: rgba(146, 64, 14, 0.32);
    color: #fff9f1;
    font-weight: 700;
  }

  .secondary {
    color: #7c2d12;
    font-weight: 700;
  }

  label {
    display: grid;
    gap: 6px;
  }

  label span {
    font-size: 0.82rem;
    font-weight: 700;
    color: #6b3f1d;
  }

  input,
  select,
  textarea {
    border-radius: 16px;
    border: 1px solid rgba(120, 53, 15, 0.14);
    background: rgba(255, 255, 255, 0.76);
    padding: 12px 14px;
    resize: vertical;
  }

  .gate-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .gate-toggle input[type="checkbox"] {
    width: auto;
    padding: 0;
  }

  .feedback {
    border-radius: 16px;
    padding: 10px 12px;
    color: #5b4634;
    font-size: 0.92rem;
    line-height: 1.45;
  }

  .feedback.error {
    background: rgba(254, 226, 226, 0.76);
    color: #991b1b;
  }

  .feedback.success {
    background: rgba(220, 252, 231, 0.8);
    color: #166534;
  }

  .capability-strip {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 14px;
    border-radius: 16px;
    background: rgba(255, 255, 255, 0.58);
    border: 1px solid rgba(120, 53, 15, 0.08);
  }

  .capability-strip div {
    display: grid;
    gap: 2px;
  }
</style>
