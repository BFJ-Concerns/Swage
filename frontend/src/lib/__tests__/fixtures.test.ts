import { describe, expect, it } from "vitest";
import { makeAgentDefinition } from "./fixtures/agent-definitions";
import { makeConversationDetail, makeConversationSummary } from "./fixtures/conversations";

describe("shared frontend fixtures", () => {
  it("keeps conversation defaults when one field is overridden", () => {
    const summary = makeConversationSummary({ message_preview: "Latest" });
    const detail = makeConversationDetail({ messages: [] });
    expect(summary.message_preview).toBe("Latest");
    expect(summary.agent_id).toBe("gateway");
    expect(summary.has_interrupted_turn).toBe(false);
    expect(detail.topic_id).toBe("proj-default");
    expect(detail.agent_id).toBe("gateway");
  });

  it("keeps agent-definition defaults when one field is overridden", () => {
    const definition = makeAgentDefinition("reviewer", "topic", { display_name: "Reviewer" });
    expect(definition.id).toBe("reviewer");
    expect(definition.display_name).toBe("Reviewer");
    expect(definition.source_scope).toBe("topic");
    expect(definition.is_builtin).toBe(false);
    expect(definition.tool_permissions).toBeNull();
    expect(definition.skills).toEqual([]);
  });
});
