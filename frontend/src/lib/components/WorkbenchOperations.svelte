<script lang="ts">
  import { createMaterial } from "../api/materials";
  import {
    bytesToBase64,
    createAttachment,
    createHeartbeat,
    createScheduledJob,
    exportRecoveryBundle,
    fetchVoiceCapabilities,
  } from "../api/workflows";
  import type {
    MaterialScopeKind,
    VoiceCapabilitiesResponse,
  } from "../types";

  interface Props {
    benchId: string | null;
    sessionId: string | null;
    onChanged?: () => void | Promise<void>;
  }

  let { benchId, sessionId, onChanged }: Props = $props();

  let noteTitle = $state("");
  let noteContent = $state("");
  let noteScope = $state<MaterialScopeKind>("session");
  let attachmentScope = $state<MaterialScopeKind>("session");
  let selectedFile = $state<File | null>(null);
  let scheduleMessage = $state("");
  let scheduleEvery = $state("1h");
  let heartbeatMessage = $state("");
  let heartbeatInterval = $state(900);
  let voiceCapabilities = $state<VoiceCapabilitiesResponse | null>(null);
  let recoverySummary = $state<string | null>(null);
  let errorMessage = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busyAction = $state<string | null>(null);

  const effectiveNoteScope = $derived(
    noteScope === "session" && sessionId ? "session" : "bench",
  );
  const effectiveAttachmentScope = $derived(
    attachmentScope === "session" && sessionId ? "session" : "bench",
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
      errorMessage = error instanceof Error ? error.message : "Workbench operation failed.";
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
      if (!benchId) throw new Error("Select a bench before saving material.");
      const title = noteTitle.trim();
      const content = noteContent.trim();
      if (!title || !content) {
        throw new Error("Material title and content are both required.");
      }

      await createMaterial({
        scope_kind: effectiveNoteScope,
        session_id: effectiveNoteScope === "session" && sessionId ? sessionId : undefined,
        bench_id: effectiveNoteScope === "bench" ? benchId : undefined,
        title,
        relative_path: buildRelativePath(title, effectiveNoteScope),
        mime_type: "text/markdown",
        content_text: content,
      });

      noteTitle = "";
      noteContent = "";
      return effectiveNoteScope === "session"
        ? "Saved material beside the active conversation."
        : "Saved material for the bench workspace.";
    });
  }

  async function queueAttachment(): Promise<void> {
    await runAction("attachment", async () => {
      if (!benchId) throw new Error("Select a bench before attaching material.");
      if (!selectedFile) throw new Error("Choose a file before queueing ingestion.");

      const bytes = new Uint8Array(await selectedFile.arrayBuffer());
      const response = await createAttachment({
        scope_kind: effectiveAttachmentScope,
        session_id:
          effectiveAttachmentScope === "session" && sessionId ? sessionId : undefined,
        bench_id: effectiveAttachmentScope === "bench" ? benchId : undefined,
        filename: selectedFile.name,
        mime_type: selectedFile.type || "application/octet-stream",
        content_base64: bytesToBase64(bytes),
      });

      selectedFile = null;
      return `Queued ${response.attachment.filename} for ingestion.`;
    });
  }

  async function schedulePrompt(): Promise<void> {
    await runAction("schedule", async () => {
      if (!sessionId) throw new Error("Open a conversation before scheduling work.");
      const message = scheduleMessage.trim();
      if (!message) throw new Error("Scheduled work needs a prompt.");

      const job = await createScheduledJob({
        session_id: sessionId,
        message,
        schedule: { kind: "interval", every: scheduleEvery.trim() || "1h" },
      });

      scheduleMessage = "";
      return `Scheduled recurring work: ${job.status}.`;
    });
  }

  async function createSessionHeartbeat(): Promise<void> {
    await runAction("heartbeat", async () => {
      if (!sessionId) throw new Error("Open a conversation before adding a heartbeat.");
      const message = heartbeatMessage.trim();
      if (!message) throw new Error("Heartbeat needs a check-in prompt.");

      const heartbeat = await createHeartbeat({
        session_id: sessionId,
        message,
        interval_seconds: heartbeatInterval,
      });

      heartbeatMessage = "";
      return `Heartbeat enabled every ${heartbeat.interval_seconds}s.`;
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
    <h3>Workbench operations</h3>
    <span>{canUseSessionWorkflow ? "live" : "bench only"}</span>
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
        <p>Write directly into session or bench-managed material.</p>
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
          class:active={effectiveNoteScope === "bench"}
          onclick={() => (noteScope = "bench")}
        >
          Bench
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
          placeholder="Capture a constraint, checklist, or bench note."
        ></textarea>
      </label>
      <button class="primary" onclick={saveMaterialNote} disabled={busyAction === "material" || !benchId}>
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
          class:active={effectiveAttachmentScope === "bench"}
          onclick={() => (attachmentScope = "bench")}
        >
          Bench
        </button>
      </div>
      <label>
        <span>File</span>
        <input type="file" onchange={handleFileChange} />
      </label>
      <button class="primary" onclick={queueAttachment} disabled={busyAction === "attachment" || !benchId}>
        {busyAction === "attachment" ? "Queueing..." : "Queue ingestion"}
      </button>
    </section>

    <section class="operation-card">
      <div>
        <strong>Schedule prompt</strong>
        <p>Create recurring work against the active conversation.</p>
      </div>
      <label>
        <span>Prompt</span>
        <textarea bind:value={scheduleMessage} rows="3" placeholder="Run the review checklist."></textarea>
      </label>
      <label>
        <span>Every</span>
        <input bind:value={scheduleEvery} placeholder="1h" />
      </label>
      <button class="primary" onclick={schedulePrompt} disabled={busyAction === "schedule" || !sessionId}>
        {busyAction === "schedule" ? "Scheduling..." : "Schedule"}
      </button>
    </section>

    <section class="operation-card">
      <div>
        <strong>Heartbeat</strong>
        <p>Add a bounded check-in loop to this conversation.</p>
      </div>
      <label>
        <span>Prompt</span>
        <textarea bind:value={heartbeatMessage} rows="3" placeholder="Check whether anything needs operator attention."></textarea>
      </label>
      <label>
        <span>Interval seconds</span>
        <input type="number" min="1" bind:value={heartbeatInterval} />
      </label>
      <button class="primary" onclick={createSessionHeartbeat} disabled={busyAction === "heartbeat" || !sessionId}>
        {busyAction === "heartbeat" ? "Starting..." : "Start heartbeat"}
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
  textarea {
    border-radius: 16px;
    border: 1px solid rgba(120, 53, 15, 0.14);
    background: rgba(255, 255, 255, 0.76);
    padding: 12px 14px;
    resize: vertical;
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
