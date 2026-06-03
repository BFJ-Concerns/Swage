import { describe, expect, it } from "vitest";

import type { MessageView } from "../types";

describe("MessageView tool_calls", () => {
  it("can represent legacy messages that omit tool_calls entirely", () => {
    const msg: MessageView = {
      role: "assistant",
      content: "Hello",
      timestamp: "2025-01-01T00:00:00Z",
      turn_state: "completed",
    };

    expect("tool_calls" in msg).toBe(false);
    expect(msg.tool_calls).toBeUndefined();
  });
});
