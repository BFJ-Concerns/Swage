import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createPage,
  createPageAnnotation,
  writePageModule,
} from "../api/pages";

function response(body: unknown): Response {
  return {
    ok: true,
    status: 200,
    json: async () => body,
  } as Response;
}

beforeEach(() => vi.stubGlobal("fetch", vi.fn()));
afterEach(() => vi.unstubAllGlobals());

describe("page API generated transport", () => {
  it("sends create-page, module, and annotation bodies through generated routes", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock
      .mockResolvedValueOnce(response({ id: "page", modules: [] }))
      .mockResolvedValueOnce(response({ id: "module", annotations: [] }))
      .mockResolvedValueOnce(response({ id: "annotation" }));

    await createPage("topic-id", "Research");
    await writePageModule("page-id", { source: "# Notes" });
    await createPageAnnotation("page-id", {
      module_id: "module-id",
      body: "Check this",
      tags: ["review"],
    });

    expect(fetchMock).toHaveBeenNthCalledWith(1, "/api/topics/topic-id/pages", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ title: "Research" }),
    });
    expect(fetchMock).toHaveBeenNthCalledWith(2, "/api/pages/page-id/modules", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ source: "# Notes" }),
    });
    expect(fetchMock).toHaveBeenNthCalledWith(3, "/api/pages/page-id/annotations", {
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
