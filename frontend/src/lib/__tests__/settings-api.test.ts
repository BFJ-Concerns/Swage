// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";

import { discoverModels } from "../api/settings";

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
      protocol: "OpenAi" as const,
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
});
