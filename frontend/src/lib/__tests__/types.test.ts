/**
 * Type-shape tests for tool-related additions to types.ts.
 *
 * TypeScript interfaces have no runtime representation, so we test them by
 * constructing objects that conform to the types and asserting their shape.
 * These tests act as living documentation of the contract and catch
 * accidental regressions during refactors.
 *
 * Scenarios covered:
 *   - ToolCallView: all required fields present with correct types
 *   - ToolCallView: nullable fields accept null
 *   - MessageView: tool_calls is optional (absent on plain messages)
 *   - MessageView: tool_calls accepts a ToolCallView array
 *   - ServerEvent: tool_start discriminant and fields
 *   - ServerEvent: tool_end discriminant and fields
 *   - ServerEvent: union still includes all original 4 event types
 */
import { describe, it, expect } from "vitest";

import type { ToolCallView, MessageView, ServerEvent } from "../types";

// ---------------------------------------------------------------------------
// ToolCallView
// ---------------------------------------------------------------------------

describe("ToolCallView", () => {
  it("accepts a fully populated call with a string result", () => {
    const call: ToolCallView = {
      call_id: "call-001",
      tool: "read_file",
      arguments: { path: "/tmp/foo.txt" },
      result: "file contents here",
      status: "success",
      duration_ms: 42,
      truncated: false,
      total_bytes: 18,
    };

    expect(call.call_id).toBe("call-001");
    expect(call.tool).toBe("read_file");
    expect(call.arguments).toEqual({ path: "/tmp/foo.txt" });
    expect(call.result).toBe("file contents here");
    expect(call.status).toBe("success");
    expect(call.duration_ms).toBe(42);
    expect(call.truncated).toBe(false);
    expect(call.total_bytes).toBe(18);
  });

  it("accepts a call with an object result", () => {
    const call: ToolCallView = {
      call_id: "call-002",
      tool: "search",
      arguments: { query: "rust async" },
      result: { hits: 3, items: ["a", "b", "c"] },
      status: "success",
      duration_ms: 150,
      truncated: false,
      total_bytes: null,
    };

    expect(call.result).toEqual({ hits: 3, items: ["a", "b", "c"] });
    expect(call.total_bytes).toBeNull();
  });

  it("accepts a call where result and status are null (in-progress / pending)", () => {
    const call: ToolCallView = {
      call_id: "call-003",
      tool: "shell",
      arguments: { command: "ls" },
      result: null,
      status: null,
      duration_ms: null,
      truncated: false,
      total_bytes: null,
    };

    expect(call.result).toBeNull();
    expect(call.status).toBeNull();
    expect(call.duration_ms).toBeNull();
  });

  it("accepts status: 'error'", () => {
    const call: ToolCallView = {
      call_id: "call-004",
      tool: "write_file",
      arguments: {},
      result: "Permission denied",
      status: "error",
      duration_ms: 5,
      truncated: false,
      total_bytes: null,
    };

    expect(call.status).toBe("error");
  });
});

// ---------------------------------------------------------------------------
// MessageView — tool_calls field
// ---------------------------------------------------------------------------

describe("MessageView tool_calls", () => {
  it("omitting tool_calls is valid (backward-compatible plain message)", () => {
    const msg: MessageView = {
      role: "assistant",
      content: "Hello!",
      timestamp: "2025-01-01T00:00:00Z",
      turn_state: "completed",
    };

    // tool_calls is absent — not even undefined in the object literal.
    expect("tool_calls" in msg).toBe(false);
    expect(msg.tool_calls).toBeUndefined();
  });

  it("accepts tool_calls as an empty array", () => {
    const msg: MessageView = {
      role: "assistant",
      content: "",
      timestamp: "2025-01-01T00:00:00Z",
      turn_state: "completed",
      tool_calls: [],
    };

    expect(msg.tool_calls).toEqual([]);
  });

  it("accepts tool_calls with one ToolCallView entry", () => {
    const toolCall: ToolCallView = {
      call_id: "c1",
      tool: "read_file",
      arguments: { path: "/etc/hosts" },
      result: "127.0.0.1 localhost",
      status: "success",
      duration_ms: 10,
      truncated: false,
      total_bytes: 20,
    };

    const msg: MessageView = {
      role: "assistant",
      content: "I read the hosts file.",
      timestamp: "2025-01-01T00:00:00Z",
      turn_state: "completed",
      tool_calls: [toolCall],
    };

    expect(msg.tool_calls).toHaveLength(1);
    expect(msg.tool_calls![0].tool).toBe("read_file");
  });
});

// ---------------------------------------------------------------------------
// ServerEvent — tool_start and tool_end
// ---------------------------------------------------------------------------

describe("ServerEvent tool variants", () => {
  it("tool_start has call_id, tool, and arguments", () => {
    const ev: ServerEvent = {
      type: "tool_start",
      call_id: "c1",
      tool: "shell",
      arguments: { command: "echo hi" },
    };

    expect(ev.type).toBe("tool_start");
    // Narrow via discriminant to access typed fields.
    if (ev.type === "tool_start") {
      expect(ev.call_id).toBe("c1");
      expect(ev.tool).toBe("shell");
      expect(ev.arguments).toEqual({ command: "echo hi" });
    }
  });

  it("tool_end has call_id, tool, result, status, duration_ms, truncated, total_bytes", () => {
    const ev: ServerEvent = {
      type: "tool_end",
      call_id: "c1",
      tool: "shell",
      result: "hi\n",
      status: "success",
      duration_ms: 30,
      truncated: false,
      total_bytes: null,
    };

    expect(ev.type).toBe("tool_end");
    if (ev.type === "tool_end") {
      expect(ev.call_id).toBe("c1");
      expect(ev.tool).toBe("shell");
      expect(ev.result).toBe("hi\n");
      expect(ev.status).toBe("success");
      expect(ev.duration_ms).toBe(30);
      expect(ev.truncated).toBe(false);
      expect(ev.total_bytes).toBeNull();
    }
  });

  it("tool_end accepts an object result", () => {
    const ev: ServerEvent = {
      type: "tool_end",
      call_id: "c2",
      tool: "search",
      result: { count: 5 },
      status: "success",
      duration_ms: 200,
      truncated: true,
      total_bytes: 4096,
    };

    if (ev.type === "tool_end") {
      expect(ev.result).toEqual({ count: 5 });
      expect(ev.truncated).toBe(true);
      expect(ev.total_bytes).toBe(4096);
    }
  });

  it("tool_end accepts status: 'error'", () => {
    const ev: ServerEvent = {
      type: "tool_end",
      call_id: "c3",
      tool: "write_file",
      result: "Permission denied",
      status: "error",
      duration_ms: 2,
      truncated: false,
      total_bytes: null,
    };

    if (ev.type === "tool_end") {
      expect(ev.status).toBe("error");
    }
  });

  // Verify the original four event types are still part of the union.
  it("still accepts the original token event", () => {
    const ev: ServerEvent = { type: "token", content: "hello" };
    expect(ev.type).toBe("token");
  });

  it("still accepts turn_complete", () => {
    const ev: ServerEvent = { type: "turn_complete", turn_id: "t-1" };
    expect(ev.type).toBe("turn_complete");
  });

  it("still accepts error", () => {
    const ev: ServerEvent = {
      type: "error",
      code: "provider_error",
      message: "oops",
    };
    expect(ev.type).toBe("error");
  });

  it("still accepts cancelled", () => {
    const ev: ServerEvent = { type: "cancelled" };
    expect(ev.type).toBe("cancelled");
  });
});
