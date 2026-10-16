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
import capturedSnapshot from "./fixtures/browser-nested-json.json";

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

describe("rejected approval state (status='rejected')", () => {
  it("uses the same failed treatment as an error", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeErrorCall({
          result: "Tool call rejected: reviewer denied access",
          status: "rejected",
        }),
        isLive: false,
      },
    });

    const root = document.querySelector(".tool-call");
    expect(root?.classList.contains("error")).toBe(true);
    expect(root?.querySelector(".error-icon")?.textContent).toBe("✗");
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


describe("operation outcome indicators", () => {
  for (const isLive of [true, false]) {
    it(`shows failure and never success for an error record (${isLive ? "live" : "history"})`, () => {
      component = mount(ToolCallDisplay, {
        target: document.body,
        props: { toolCall: makeErrorCall(), isLive },
      });
      const root = document.querySelector(".tool-call");
      expect(root?.querySelector(".error-icon")?.textContent).toBe("✗");
      expect(root?.querySelector(".success")).toBeNull();
    });
    it(`shows success and never failure for a successful record (${isLive ? "live" : "history"})`, () => {
      component = mount(ToolCallDisplay, {
        target: document.body,
        props: { toolCall: makeCompletedCall(), isLive },
      });
      const root = document.querySelector(".tool-call");
      expect(root?.querySelector(".success")?.textContent).toBe("✓");
      expect(root?.querySelector(".error-icon")).toBeNull();
    });
  }
});


describe("expired approval result", () => {
  it("shows expiry as a failed call with a recovery action", () => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: {
        toolCall: makeErrorCall({
          status: "expired",
          result: "Tool call rejected: approval timed out",
        }),
        isLive: false,
      },
    });
    const root = document.querySelector(".tool-call");
    expect(root?.classList.contains("error")).toBe(true);
    expect(root?.textContent).toContain("Approval expired. Retry the operation.");
    expect(root?.querySelector(".result-content")?.textContent).toBe("Tool call rejected: approval timed out");
  });
});

describe("readable result payloads", () => {
  it.each([
    [{ output: '# Title\n\n## 1\n"quoted"' }, '# Title\n\n## 1\n"quoted"'],
    [{ output: JSON.stringify({ output: "first\nsecond", full_snapshot: true }) }, "first\nsecond"],
  ])("unwraps text output and keeps its envelope disclosed: %j", (result, expected) => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeCompletedCall({ result }), isLive: false },
    });
    expect(document.querySelector(".tool-result .result-content")!.textContent).toBe(expected);
    const disclosure = document.querySelector("details");
    expect(disclosure).not.toBeNull();
    expect(disclosure!.open).toBe(false);
    flushSync(() => disclosure!.querySelector("summary")!.click());
    expect(disclosure!.open).toBe(true);
    expect(disclosure!.querySelector("pre")!.textContent).toBe(JSON.stringify(result, null, 2));
  });

  it("formats a JSON-encoded structured result", () => {
    const value = { rows: [1, 2], complete: true };
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeCompletedCall({ result: JSON.stringify(value) }), isLive: false },
    });
    expect(document.querySelector(".tool-result .result-content")!.textContent).toBe(JSON.stringify(value, null, 2));
  });
});

describe("non-scalar argument headlines", () => {
  it.each([
    [{ options: { mode: "fast", count: 2 } }, "options={mode, count}"],
    [{ items: [{ path: "a" }, { path: "b" }] }, "items=[2 items]"],
  ])("summarises an unknown tool without a hint: %j", (args, expected) => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeCompletedCall({ tool: "unknown", arguments: args }), isLive: false },
    });
    expect(document.querySelector(".args-summary")!.textContent).toBe(expected);
  });
});

describe("JSON embedded in result text", () => {
  it("renders the captured nested browser JSON without snapshot quote escaping", () => {
    // Captured from the running browser tool; origin and trace id anonymised.
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeCompletedCall({ result: capturedSnapshot }), isLive: false },
    });
    flushSync(() => {});
    const toggle = document.querySelector(".toggle-btn") as HTMLButtonElement;
    flushSync(() => toggle.click());
    const text = document.querySelector(".tool-result .result-content")!.textContent!;
    expect(text).toContain('- StaticText {\n  "args": {');
    expect(text).toContain('"payload": {\n      "heading": "Booking"');
    expect(text).not.toContain('\\"args\\"');
    expect(text).not.toContain('"payload": "{');
    expect(text).toContain('checkbox "Pretty print" [checked=false, invalid=false] ref=s2e1');
    const disclosure = document.querySelector("details")!;
    flushSync(() => disclosure.querySelector("summary")!.click());
    expect(disclosure.open).toBe(true);
    expect(disclosure.querySelector("pre")!.textContent).toBe(JSON.stringify(capturedSnapshot, null, 2));
  });

  it("formats quoted JSON arrays in arbitrary text without changing surrounding text", () => {
    const output = `Before ${JSON.stringify('[{"path":"a"}]')} after.`;
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeCompletedCall({ tool: "unknown", result: { output } }), isLive: false },
    });
    expect(document.querySelector(".tool-result .result-content")!.textContent).toBe(
      'Before [\n  {\n    "path": "a"\n  }\n] after.',
    );
  });

  it.each([
    'Read "{not JSON}" and keep C:\\notes\\next.txt unchanged.',
    'The guest said "hello".\nSecond line.',
  ])("preserves ordinary and malformed quoted text: %s", (output) => {
    component = mount(ToolCallDisplay, {
      target: document.body,
      props: { toolCall: makeCompletedCall({ result: { output } }), isLive: false },
    });
    expect(document.querySelector(".tool-result .result-content")!.textContent).toBe(output);
  });
});
