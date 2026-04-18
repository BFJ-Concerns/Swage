import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchLensRuntime, setLensBaseUrl, updateLensRuntime } from "../api/lens";

describe("lens api", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    setLensBaseUrl("");
  });

  it("fetches the active lens runtime", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          active_lens: "assistant",
          updated_at: "2026-04-18T10:00:00Z",
          available_lenses: [],
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );

    const runtime = await fetchLensRuntime();
    expect(runtime.active_lens).toBe("assistant");
    expect(fetchMock).toHaveBeenCalledWith("/api/lens-runtime", { method: "GET" });
  });

  it("updates the active lens runtime", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          active_lens: "builder",
          updated_at: "2026-04-18T10:00:00Z",
          available_lenses: [],
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );

    const runtime = await updateLensRuntime("builder");
    expect(runtime.active_lens).toBe("builder");
    expect(fetchMock).toHaveBeenCalledWith("/api/lens-runtime", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ lens: "builder" }),
    });
  });
});
