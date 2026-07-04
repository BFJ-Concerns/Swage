// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import WorkbenchOperations from "../components/WorkbenchOperations.svelte";
import * as materialsApi from "../api/materials";
import * as workflowApi from "../api/workflows";

vi.mock("../api/materials", () => ({
  createMaterial: vi.fn(),
}));

vi.mock("../api/workflows", () => ({
  bytesToBase64: vi.fn(() => "encoded"),
  createAttachment: vi.fn(),
  createSchedule: vi.fn(),
  exportRecoveryBundle: vi.fn(),
  fetchVoiceCapabilities: vi.fn(),
}));

type MountedComponent = ReturnType<typeof mount>;

async function flushUi(): Promise<void> {
  for (let attempt = 0; attempt < 6; attempt += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

function inputByPlaceholder(placeholder: string): HTMLInputElement {
  const input = document.querySelector<HTMLInputElement>(`input[placeholder="${placeholder}"]`);
  if (!input) throw new Error(`missing input ${placeholder}`);
  return input;
}

function textareaByPlaceholder(placeholder: string): HTMLTextAreaElement {
  const textarea = document.querySelector<HTMLTextAreaElement>(
    `textarea[placeholder="${placeholder}"]`,
  );
  if (!textarea) throw new Error(`missing textarea ${placeholder}`);
  return textarea;
}

function setFieldValue(field: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  field.value = value;
  field.dispatchEvent(new Event("input", { bubbles: true }));
}

function clickButton(label: string): void {
  const button = [...document.querySelectorAll<HTMLButtonElement>("button")]
    .find((candidate) => candidate.textContent?.includes(label));
  if (!button) throw new Error(`missing button ${label}`);
  button.click();
}

describe("WorkbenchOperations", () => {
  let component: MountedComponent | undefined;
  let changed: () => void;

  beforeEach(() => {
    changed = vi.fn() as () => void;
    vi.mocked(workflowApi.fetchVoiceCapabilities).mockResolvedValue({
      transcription: true,
      synthesis: false,
      default_voice: null,
      supported_voices: [],
    });
    vi.mocked(materialsApi.createMaterial).mockResolvedValue({
      id: "material-1",
      scope_kind: "session",
      session_id: "session-1",
      bench_id: null,
      source_kind: "manual",
      source_attachment_id: null,
      status: "active",
      title: "Runbook",
      relative_path: "session-notes/runbook.md",
      absolute_path: "/tmp/runbook.md",
      mime_type: "text/markdown",
      content_text: "Ship it.",
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T00:00:00Z",
      archived_at: null,
    });
    vi.mocked(workflowApi.createSchedule).mockResolvedValue({
      id: "schedule-1",
      message: "Review the queue.",
      status: "active",
      spec: { kind: "interval", every: "every 1 hours" },
      session: { policy: "fresh", bench_id: "bench-1", agent_id: "gateway" },
      routing: {
        deliver_to_origin: false,
        home_target: null,
        explicit_targets: [],
        store_local_output: true,
      },
      gate: null,
      next_run_at: null,
      last_run_at: null,
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T00:00:00Z",
    });
    vi.mocked(workflowApi.exportRecoveryBundle).mockResolvedValue({
      version: 1,
      benches: [{}],
      sessions: [{}, {}],
      schedules: [],
    });
    vi.mocked(workflowApi.createAttachment).mockResolvedValue({
      attachment: {
        id: "attachment-1",
        scope_kind: "session",
        session_id: "session-1",
        bench_id: null,
        kind: "text",
        status: "queued",
        filename: "notes.txt",
        mime_type: "text/plain",
        byte_count: 5,
        ingested_markdown: null,
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      },
      job: {
        id: "job-attachment-1",
        kind: "attachment_ingestion",
        state: "queued",
        attachment_id: "attachment-1",
        result: null,
        error_message: null,
        attempt_count: 0,
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
        started_at: null,
        completed_at: null,
      },
    });
  });

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }
    vi.clearAllMocks();
  });

  it("saves a session material through the material API", async () => {
    component = mount(WorkbenchOperations, {
      target: document.body,
      props: {
        benchId: "bench-1",
        sessionId: "session-1",
        onChanged: changed,
      },
    });
    await flushUi();

    setFieldValue(inputByPlaceholder("Deployment checklist"), "Runbook");
    setFieldValue(
      textareaByPlaceholder("Capture a constraint, checklist, or bench note."),
      "Ship it.",
    );
    clickButton("Save material");
    await flushUi();

    expect(materialsApi.createMaterial).toHaveBeenCalledWith(
      expect.objectContaining({
        scope_kind: "session",
        session_id: "session-1",
        title: "Runbook",
      }),
    );
    expect(changed).toHaveBeenCalled();
  });

  it("queues an attachment for the active conversation", async () => {
    component = mount(WorkbenchOperations, {
      target: document.body,
      props: {
        benchId: "bench-1",
        sessionId: "session-1",
      },
    });
    await flushUi();

    const fileInput = document.querySelector<HTMLInputElement>('input[type="file"]');
    if (!fileInput) throw new Error("missing file input");
    Object.defineProperty(fileInput, "files", {
      configurable: true,
      value: [new File(["hello"], "notes.txt", { type: "text/plain" })],
    });
    fileInput.dispatchEvent(new Event("change", { bubbles: true }));
    clickButton("Queue ingestion");
    await flushUi();

    expect(workflowApi.bytesToBase64).toHaveBeenCalledWith(new Uint8Array([104, 101, 108, 108, 111]));
    expect(workflowApi.createAttachment).toHaveBeenCalledWith({
      scope_kind: "session",
      session_id: "session-1",
      filename: "notes.txt",
      mime_type: "text/plain",
      content_base64: "encoded",
    });
  });

  it("creates a fresh-session schedule with a gate", async () => {
    component = mount(WorkbenchOperations, {
      target: document.body,
      props: {
        benchId: "bench-1",
        sessionId: "session-1",
      },
    });
    await flushUi();

    setFieldValue(textareaByPlaceholder("Run the review checklist."), "Review the queue.");
    const gateToggle = document.querySelector<HTMLInputElement>('input[type="checkbox"]');
    if (!gateToggle) throw new Error("missing gate toggle");
    gateToggle.click();
    await flushUi();
    setFieldValue(inputByPlaceholder("model id or alias"), "cheap");
    setFieldValue(
      textareaByPlaceholder("Only when there is something new to act on."),
      "Only when the queue is non-empty.",
    );
    clickButton("Schedule");
    await flushUi();

    expect(workflowApi.createSchedule).toHaveBeenCalledWith({
      message: "Review the queue.",
      spec: { kind: "interval", every: "every 1 hours" },
      session: { policy: "fresh", bench_id: "bench-1", agent_id: "gateway" },
      routing: null,
      gate: { model: "cheap", criteria: "Only when the queue is non-empty." },
    });
  });

  it("creates a continuation schedule against the active conversation", async () => {
    component = mount(WorkbenchOperations, {
      target: document.body,
      props: {
        benchId: "bench-1",
        sessionId: "session-1",
      },
    });
    await flushUi();

    setFieldValue(textareaByPlaceholder("Run the review checklist."), "Check status.");
    clickButton("This conversation");
    await flushUi();
    clickButton("Schedule");
    await flushUi();

    expect(workflowApi.createSchedule).toHaveBeenCalledWith({
      message: "Check status.",
      spec: { kind: "interval", every: "every 1 hours" },
      session: { policy: "continuation", session_id: "session-1" },
      routing: null,
      gate: null,
    });
  });

  it("surfaces recovery export counts", async () => {
    component = mount(WorkbenchOperations, {
      target: document.body,
      props: {
        benchId: "bench-1",
        sessionId: "session-1",
      },
    });
    await flushUi();

    clickButton("Export recovery");
    await flushUi();

    expect(workflowApi.exportRecoveryBundle).toHaveBeenCalled();
    expect(document.body.textContent).toContain("benches: 1");
    expect(document.body.textContent).toContain("sessions: 2");
  });
});
