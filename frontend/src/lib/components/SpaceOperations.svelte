<script lang="ts">
  import { createMaterial } from "../api/materials";
  import {
    bytesToBase64,
    createAttachment,
    exportRecoveryBundle,
    fetchVoiceCapabilities,
  } from "../api/workflows";
  import type { MaterialScopeKind } from "../types";
  import type { VoiceCapabilitiesResponse } from "../protocol/VoiceCapabilitiesResponse";

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
    background: var(--paper);
    border: var(--rule);
  }

  .operation-card p,
  .capability-strip span,
  .recovery-summary {
    color: var(--ink-soft);
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
    border: var(--border);
    background: var(--paper);
    padding: 9px 14px;
  }

  .scope-toggle button.active {
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border-color: var(--accent);
    color: var(--ink-orange);
    font-weight: 700;
  }

  .primary {
    background: var(--ink-orange);
    border-color: var(--ink-orange);
    color: var(--paper);
    font-weight: 700;
  }

  .secondary {
    color: var(--ink-orange);
    font-weight: 700;
  }

  label {
    display: grid;
    gap: 6px;
  }

  label span {
    font-size: 0.82rem;
    font-weight: 700;
    color: var(--ink);
  }

  input,
  textarea {
    border-radius: 16px;
    border: var(--border);
    background: var(--paper);
    padding: 12px 14px;
    resize: vertical;
  }

  .feedback {
    border-radius: 16px;
    padding: 10px 12px;
    color: var(--ink-soft);
    font-size: 0.92rem;
    line-height: 1.45;
  }

  .feedback.error {
    background: color-mix(in srgb, var(--error) 12%, transparent);
    color: var(--error);
  }

  .feedback.success {
    background: color-mix(in srgb, var(--success) 12%, transparent);
    color: var(--success);
  }

  .capability-strip {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 14px;
    border-radius: 16px;
    background: var(--paper);
    border: var(--rule);
  }

  .capability-strip div {
    display: grid;
    gap: 2px;
  }
</style>
