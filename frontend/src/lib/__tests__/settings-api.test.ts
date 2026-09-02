// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

import { discoverModels } from "../api/settings";
import { ApiError } from "../api/error";

function mockResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("settings api", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn());
  });

  it("uses GET model discovery for saved providers", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ provider_id: "openai", models: [] }));

    await discoverModels("openai", "gpt");

    expect(fetch).toHaveBeenCalledWith("/api/settings/providers/openai/models?q=gpt", {
      method: "GET",
    });
  });

  it("uses POST model discovery for draft providers", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ provider_id: "llama", models: [] }));
    const draft = {
      protocol: "openai" as const,
      base_url: "http://127.0.0.1:8080/v1",
      api_key_secret_ref: null,
      api_key_env: null,
    };

    await discoverModels("llama", "", draft);

    expect(fetch).toHaveBeenCalledWith("/api/settings/providers/llama/models", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(draft),
    });
  });

  it("reads a contract error body into a typed ApiError", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(
      mockResponse(
        { code: "validation_error", message: "validation error: unknown variant `OpenAi`" },
        422,
      ),
    );

    const failure = await discoverModels("llama").catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(ApiError);
    const apiError = failure as ApiError;
    expect(apiError.status).toBe(422);
    expect(apiError.code).toBe("validation_error");
    expect(apiError.is("validation_error")).toBe(true);
    expect(apiError.message).toBe("validation error: unknown variant `OpenAi`");
  });

  it("treats a code outside the contract as out of contract", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(
      mockResponse({ code: "teapot", message: "short and stout" }, 418),
    );

    const failure = await discoverModels("llama").catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(ApiError);
    const apiError = failure as ApiError;
    expect(apiError.code).toBeNull();
    expect(apiError.status).toBe(418);
  });

  it("keeps a response outside the contract as a diagnostic error", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(
      new Response("<html>bad gateway</html>", {
        status: 502,
        headers: { "Content-Type": "text/html" },
      }),
    );

    const failure = await discoverModels("llama").catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(ApiError);
    const apiError = failure as ApiError;
    expect(apiError.code).toBeNull();
    expect(apiError.message).toBe(
      "HTTP 502 from GET /api/settings/providers/llama/models: <html>bad gateway</html>",
    );
  });
});
