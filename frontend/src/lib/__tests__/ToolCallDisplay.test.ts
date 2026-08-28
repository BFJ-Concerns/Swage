// @vitest-environment jsdom
/**
 * Tests for ToolCallDisplay component.
 *
 * Scenarios covered:
 *   - In-progress state: tool name, args summary, pulse indicator, no result
 *   - Completed state: tool name, args summary, result, duration, success class
 *   - Error state: error class applied, error message visible
 *   - Collapsible: short results expanded, long results collapsed with preview
 *   - Toggle: clicking toggle changes expanded/collapsed state
 *   - Arguments summary: platform-supplied summary with generic fallback
 *   - Truncation notice: shown when result was truncated
 */
import { describe, it, expect, afterEach } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import ToolCallDisplay from "../components/ToolCallDisplay.svelte";
import type { ToolCallView } from "../types";

type MountedComponent = ReturnType<typeof mount>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeInProgressCall(overrides: Partial<ToolCallView> = {}): ToolCallView {
  return {
    call_id: "c1",
    tool: "read_file",
    arguments: { path: "/tmp/foo.txt" },
    result: null,
    status: null,
    duration_ms: null,
    truncated: false,
    total_bytes: null,
    ...overrides,
  };
}

function makeCompletedCall(overrides: Partial<ToolCallView> = {}): ToolCallView {
  return {
    call_id: "c1",
    tool: "read_file",
    arguments: { path: "/tmp/foo.txt" },
    result: "file contents here",
    status: "success",
    duration_ms: 42,
    truncated: false,
    total_bytes: 18,
    ...overrides,
  };
}

function makeErrorCall(overrides: Partial<ToolCallView> = {}): ToolCallView {
  return {
    call_id: "c1",
    tool: "write_file",
    arguments: { path: "/tmp/no-access" },
    result: "Permission denied",
    status: "error",
    duration_ms: 3,
    truncated: false,
    total_bytes: null,
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// Setup / teardown
// ---------------------------------------------------------------------------

let component: MountedComponent | undefined;

afterEach(() => {
  if (component) {
    unmount(component);
    component = undefined;
  }
  while (document.body.firstChild) {
    document.body.removeChild(document.body.firstChild);
  }
});

// ---------------------------------------------------------------------------
// In-progress state
// ---------------------------------------------------------------------------

describe("in-progress state (isLive=true, no result)", () => {
  it("renders the tool name", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeInProgressCall({ tool: "read_file" }), isLive: true },
    });

    const root = document.querySelector(".tool-call");
    expect(root).not.toBeNull();
    expect(root!.textContent).toContain("read_file");
  });

  it("shows a pulsing/spinner indicator element", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeInProgressCall(), isLive: true },
    });

    // A dedicated element signals activity — class name "pulse", "spinner",
    // or "in-progress" on the root or a child.
    const hasPulse =
      document.querySelector(".pulse") !== null ||
      document.querySelector(".spinner") !== null ||
      document.querySelector(".in-progress") !== null;
    expect(hasPulse).toBe(true);
  });

  it("does not show result content", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeInProgressCall(), isLive: true },
    });

    // No result block when there's nothing to show.
    const resultEl = document.querySelector(".tool-result");
    expect(resultEl).toBeNull();
  });

  it("does not show duration", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeInProgressCall(), isLive: true },
    });

    const durationEl = document.querySelector(".duration");
    expect(durationEl).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Completed state
// ---------------------------------------------------------------------------

describe("completed state (status='success')", () => {
  it("renders the tool name", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeCompletedCall({ tool: "shell" }), isLive: false },
    });

    expect(document.querySelector(".tool-call")!.textContent).toContain("shell");
  });

  it("shows the result content", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ result: "file contents here" }),
        isLive: false,
      },
    });

    const resultEl = document.querySelector(".tool-result");
    expect(resultEl).not.toBeNull();
    expect(resultEl!.textContent).toContain("file contents here");
  });

  it("shows the duration", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ duration_ms: 42 }),
        isLive: false,
      },
    });

    const durationEl = document.querySelector(".duration");
    expect(durationEl).not.toBeNull();
    expect(durationEl!.textContent!.trim().length).toBeGreaterThan(0);
  });

  it("does not apply the error class for a successful call", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeCompletedCall(), isLive: false },
    });

    const root = document.querySelector(".tool-call");
    expect(root!.classList.contains("error")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Error state
// ---------------------------------------------------------------------------

describe("error state (status='error')", () => {
  it("applies a visually distinct error class to the root element", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeErrorCall(), isLive: false },
    });

    const root = document.querySelector(".tool-call");
    expect(root).not.toBeNull();
    expect(root!.classList.contains("error")).toBe(true);
  });

  it("shows the error message in the result", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeErrorCall({ result: "Permission denied" }),
        isLive: false,
      },
    });

    const resultEl = document.querySelector(".tool-result");
    expect(resultEl).not.toBeNull();
    expect(resultEl!.textContent).toContain("Permission denied");
  });

  it("shows the tool name", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeErrorCall({ tool: "write_file" }), isLive: false },
    });

    expect(document.querySelector(".tool-call")!.textContent).toContain("write_file");
  });
});

// ---------------------------------------------------------------------------
// Collapsible result
// ---------------------------------------------------------------------------

describe("collapsible result", () => {
  it("expands short results (<500 chars) by default", () => {
    const shortResult = "short output";
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ result: shortResult }),
        isLive: false,
      },
    });

    // Full content visible — not hidden behind a collapsed state.
    const resultEl = document.querySelector(".tool-result");
    expect(resultEl!.textContent).toContain(shortResult);
    // No collapsed class on the result.
    expect(resultEl!.classList.contains("collapsed")).toBe(false);
  });

  it("collapses long results (>=500 chars) by default", () => {
    const longResult = "x".repeat(500);
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ result: longResult }),
        isLive: false,
      },
    });

    const resultEl = document.querySelector(".tool-result");
    expect(resultEl).not.toBeNull();
    // Root result block gets the collapsed class.
    expect(resultEl!.classList.contains("collapsed")).toBe(true);
  });

  it("shows a toggle button for long results", () => {
    const longResult = "y".repeat(500);
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ result: longResult }),
        isLive: false,
      },
    });

    const toggleBtn = document.querySelector(".toggle-btn");
    expect(toggleBtn).not.toBeNull();
  });

  it("expands a collapsed result when the toggle is clicked", () => {
    const longResult = "z".repeat(500);
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ result: longResult }),
        isLive: false,
      },
    });

    const toggleBtn = document.querySelector(".toggle-btn") as HTMLButtonElement;
    flushSync(() => toggleBtn.click());

    const resultEl = document.querySelector(".tool-result");
    expect(resultEl!.classList.contains("collapsed")).toBe(false);
  });

  it("collapses an expanded long result when the toggle is clicked again", () => {
    const longResult = "w".repeat(500);
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ result: longResult }),
        isLive: false,
      },
    });

    const toggleBtn = document.querySelector(".toggle-btn") as HTMLButtonElement;
    // Expand.
    flushSync(() => toggleBtn.click());
    // Collapse.
    flushSync(() => toggleBtn.click());

    const resultEl = document.querySelector(".tool-result");
    expect(resultEl!.classList.contains("collapsed")).toBe(true);
  });

  it("does not show a toggle button for short results", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ result: "short" }),
        isLive: false,
      },
    });

    const toggleBtn = document.querySelector(".toggle-btn");
    expect(toggleBtn).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Arguments summary
// ---------------------------------------------------------------------------

describe("arguments summary", () => {
  it("renders the platform-supplied summary when the call carries one", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({
          tool: "shell",
          arguments: { command: "echo hello", cwd: "/tmp" },
          summary: "echo hello",
        }),
        isLive: false,
      },
    });

    const summary = document.querySelector(".args-summary");
    expect(summary!.textContent).toBe("echo hello");
  });

  it("does not branch on the tool name — an unknown tool with a summary renders it", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({
          tool: "brand_new_tool",
          arguments: { anything: "else" },
          summary: "the hinted headline",
        }),
        isLive: false,
      },
    });

    const summary = document.querySelector(".args-summary");
    expect(summary!.textContent).toBe("the hinted headline");
  });

  it("falls back to the generic first key=value pair without a summary", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({
          tool: "custom_tool",
          arguments: { target: "some-value", other: "ignored" },
        }),
        isLive: false,
      },
    });

    const summary = document.querySelector(".args-summary");
    expect(summary!.textContent).toBe("target=some-value");
  });

  it("renders no summary span for empty arguments and no summary", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ tool: "custom_tool", arguments: {} }),
        isLive: false,
      },
    });

    expect(document.querySelector(".args-summary")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Truncation notice
// ---------------------------------------------------------------------------

describe("truncation notice", () => {
  it("shows a truncation notice when result was truncated", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ truncated: true, total_bytes: 4096 }),
        isLive: false,
      },
    });

    const notice = document.querySelector(".truncated-notice");
    expect(notice).not.toBeNull();
  });

  it("does not show a truncation notice when result was not truncated", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeCompletedCall({ truncated: false }),
        isLive: false,
      },
    });

    const notice = document.querySelector(".truncated-notice");
    expect(notice).toBeNull();
  });
});
