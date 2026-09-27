// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

import { listRecentSessions } from "../api/sessions";

describe("recent sessions API", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn());
  });

  it("sends both archived boolean values through the generated client", async () => {
    vi.mocked(fetch).mockImplementation(async () =>
      new Response(JSON.stringify({ entries: [] }), { status: 200 }),
    );

    await listRecentSessions(null, false);
    await listRecentSessions(null, true);

    expect(fetch).toHaveBeenNthCalledWith(1, "/api/sessions?archived=false", {
      method: "GET",
    });
    expect(fetch).toHaveBeenNthCalledWith(2, "/api/sessions?archived=true", {
      method: "GET",
    });
  });
});
