import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { fetchTurnContext, setBaseUrl } from "../api/turns";

function mockResponse<T>(body: T, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    text: async () => JSON.stringify(body),
  } as unknown as Response;
}

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn());
  setBaseUrl("");
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("fetchTurnContext", () => {
  it("sends GET /api/turns/{id}/context and returns the report", async () => {
    const report = {
      assembly_name: "default",
      elements: [
        {
          name: "orientation",
          source: "built-in identity",
          token_count: 120,
          priority: 110,
        },
      ],
      total_tokens: 120,
      compaction_applied: [],
      warnings: [],
      errors: [],
      duration_ms: 2,
    };
    vi.mocked(fetch).mockResolvedValueOnce(
      mockResponse({ turn_id: "t-1", report }),
    );

    const loaded = await fetchTurnContext("t-1");

    expect(fetch).toHaveBeenCalledWith("/api/turns/t-1/context");
    expect(loaded?.assembly_name).toBe("default");
    expect(loaded?.elements[0].name).toBe("orientation");
  });

  it("returns null for a turn with no recorded report", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(
      mockResponse({ code: "not_found" }, 404),
    );

    const loaded = await fetchTurnContext("t-2");
    expect(loaded).toBeNull();
  });

  it("throws a descriptive error on other failures", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));

    await expect(fetchTurnContext("t-3")).rejects.toThrow("HTTP 500");
  });
});
