import { afterEach, describe, expect, it, vi } from "vitest";
import { listEffectiveAgentDefinitions } from "../api/agent-definitions";

afterEach(() => vi.unstubAllGlobals());

describe("listEffectiveAgentDefinitions", () => {
  it("reads the effective registry for one topic", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ definitions: [{ id: "gateway", is_builtin: true }] }),
    });
    vi.stubGlobal("fetch", fetchMock);

    const definitions = await listEffectiveAgentDefinitions("topic-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/topics/topic-1/agent-definitions", {
      method: "GET",
    });
    expect(definitions[0].id).toBe("gateway");
  });
});
