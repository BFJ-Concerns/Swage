import { expect, test, type APIRequestContext } from "@playwright/test";

import type { TopicResponse } from "../src/lib/protocol/TopicResponse";
import type { WorkflowRun } from "../src/lib/protocol/WorkflowRun";

async function startWorkflow(request: APIRequestContext, name: string, source: string) {
  const topicResponse = await request.post("/api/topics", { data: { name } });
  expect(topicResponse.ok()).toBe(true);
  const topic: TopicResponse = await topicResponse.json();
  const runResponse = await request.post(`/api/topics/${topic.id}/workflow-runs`, {
    data: { source, args: null },
  });
  expect(runResponse.ok()).toBe(true);
  const run: WorkflowRun = await runResponse.json();
  return run.id;
}

test("gives the worker's attempts room under their label on wide and narrow screens", async ({ page, request }) => {
  const runId = await startWorkflow(
    request,
    "Worker attempts layout",
    'return await agent("Render a worker card");',
  );
  await page.goto(`/workflow-runs/${runId}`);
  const label = page.getByText("Attempts", { exact: true });
  const firstAttempt = page.getByText(/attempt 1 — the first dispatch/i);

  for (const width of [1280, 600]) {
    await page.setViewportSize({ width, height: 900 });
    await expect(firstAttempt).toBeVisible();
    const labelBounds = await label.boundingBox();
    const attemptBounds = await firstAttempt.boundingBox();
    expect(labelBounds).not.toBeNull();
    expect(attemptBounds).not.toBeNull();
    expect(attemptBounds!.y).toBeGreaterThanOrEqual(labelBounds!.y + labelBounds!.height);
  }
});
