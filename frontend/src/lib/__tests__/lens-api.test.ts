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
          bench_id: "bench-1",
          active_lens: "assistant",
          updated_at: "2026-04-18T10:00:00Z",
          available_lenses: [],
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );

    const runtime = await fetchLensRuntime("bench-1");
    expect(runtime.active_lens).toBe("assistant");
    expect(fetchMock).toHaveBeenCalledWith("/api/benches/bench-1/lens-runtime", {
      method: "GET",
    });
  });

  it("updates the active lens runtime", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          bench_id: "bench-1",
          active_lens: "builder",
          updated_at: "2026-04-18T10:00:00Z",
          available_lenses: [],
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );

    const runtime = await updateLensRuntime("bench-1", "builder");
    expect(runtime.active_lens).toBe("builder");
    expect(fetchMock).toHaveBeenCalledWith("/api/benches/bench-1/lens-runtime", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ lens: "builder" }),
    });
  });
});
