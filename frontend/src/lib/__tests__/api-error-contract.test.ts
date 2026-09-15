import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { listEffectiveAgentDefinitions } from "../api/agent-definitions";
import { ApiError } from "../api/error";
import { listTopicMaterials, setMaterialsBaseUrl } from "../api/materials";
import { fetchOperatorOverview, setOperatorBaseUrl } from "../api/operator";
import { listPages, setBaseUrl as setPagesBaseUrl } from "../api/pages";
import { listSkills } from "../api/skills";
import { listTopicWorkspaces, setTopicsBaseUrl } from "../api/topics";
import { fetchTurnContext, setBaseUrl as setTurnsBaseUrl } from "../api/turns";
import { fetchVoiceCapabilities, setWorkflowBaseUrl } from "../api/workflows";
import { listConversations, setBaseUrl as setConversationsBaseUrl } from "../api/conversations";

function failedResponse(body: BodyInit, status: number): Response {
  return new Response(body, { status });
}

const wrappers = [
  { name: "conversations", request: () => listConversations("topic-1") },
  { name: "pages", request: () => listPages("topic-1") },
  { name: "operator", request: () => fetchOperatorOverview() },
  { name: "agent definitions", request: () => listEffectiveAgentDefinitions("topic-1") },
  { name: "materials", request: () => listTopicMaterials("topic-1") },
  { name: "skills", request: () => listSkills("topic-1") },
  { name: "topic workspaces", request: () => listTopicWorkspaces("topic-1") },
  { name: "turn context", request: () => fetchTurnContext("turn-1") },
  { name: "workflows", request: () => fetchVoiceCapabilities() },
] as const;

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn());
  setConversationsBaseUrl("");
  setPagesBaseUrl("");
  setOperatorBaseUrl("");
  setMaterialsBaseUrl("");
  setTopicsBaseUrl("");
  setTurnsBaseUrl("");
  setWorkflowBaseUrl("");
});

afterEach(() => vi.restoreAllMocks());

describe("frontend API error contract", () => {
  it.each(wrappers)("reads a contract error through $name", async ({ request }) => {
    vi.mocked(fetch).mockResolvedValueOnce(
      failedResponse(JSON.stringify({ code: "validation_error", message: "invalid input" }), 422),
    );

    const failure = await request().catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(ApiError);
    const error = failure as ApiError;
    expect(error.status).toBe(422);
    expect(error.is("validation_error")).toBe(true);
    expect(error.message).toBe("invalid input");
  });

  it.each(wrappers)("keeps an out-of-contract error diagnostic through $name", async ({ request }) => {
    vi.mocked(fetch).mockResolvedValueOnce(failedResponse("upstream unavailable", 502));

    const failure = await request().catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(ApiError);
    const error = failure as ApiError;
    expect(error.status).toBe(502);
    expect(error.code).toBeNull();
    expect(error.message).toContain("upstream unavailable");
  });
});
