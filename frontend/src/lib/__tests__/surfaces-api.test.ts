import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createSurface,
  createSurfaceAnnotation,
  writeSurfaceModule,
} from "../api/surfaces";

function response(body: unknown): Response {
  return {
    ok: true,
    status: 200,
    json: async () => body,
  } as Response;
}

beforeEach(() => vi.stubGlobal("fetch", vi.fn()));
afterEach(() => vi.unstubAllGlobals());

describe("surface API generated transport", () => {
  it("sends create-surface, module, and annotation bodies through generated routes", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock
      .mockResolvedValueOnce(response({ id: "surface", modules: [] }))
      .mockResolvedValueOnce(response({ id: "module", annotations: [] }))
      .mockResolvedValueOnce(response({ id: "annotation" }));

    await createSurface("session-id", "Research");
    await writeSurfaceModule("surface-id", { source: "# Notes" });
    await createSurfaceAnnotation("surface-id", {
      module_id: "module-id",
      body: "Check this",
      tags: ["review"],
    });

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/sessions/session-id/surfaces", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ title: "Research" }),
    });
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/surfaces/surface-id/modules", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ source: "# Notes" }),
    });
    expect(fetchMock).toHaveBeenNthCalledWith(3, "/api/surfaces/surface-id/annotations", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        module_id: "module-id",
        body: "Check this",
        tags: ["review"],
      }),
    });
  });
});
