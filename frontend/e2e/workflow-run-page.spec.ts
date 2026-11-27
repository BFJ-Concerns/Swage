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

test("shows a newly opened decision while another worker is still running", async ({ page, request }) => {
  const runId = await startWorkflow(request, "Decision alongside a worker", `
    const worker = agentStart("Keep this workflow worker pending");
    await awaitEvent({ kinds: ["review.ready"] });
    const decision = hold({ content: "Publish the preview?", verdicts: ["publish", "keep"] });
    return await Promise.all([worker.done, decision]);
  `);

  await page.goto(`/workflow-runs/${runId}`);
  const worker = page.getByText("Still working", { exact: true });
  await expect(worker).toBeVisible();

  const event = await request.post(`/api/workflow-runs/${runId}/events`, {
    data: { kind: "review.ready", payload: null },
  });
  expect(event.ok()).toBe(true);

  const awaited = page.getByRole("region", { name: "Awaited work" });
  await expect(awaited.getByText("Decision hold: Publish the preview?")).toBeVisible();
  await expect(awaited.getByText("Permitted verdicts: publish, keep")).toBeVisible();
  await expect(worker).toBeVisible();
  await expect(page.getByText("running", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Ending", exact: true })).toBeVisible();
  await expect(awaited).toHaveCount(0);
});

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
