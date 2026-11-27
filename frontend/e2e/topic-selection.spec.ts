import { expect, test } from "@playwright/test";
import type { TopicResponse } from "../src/lib/protocol/TopicResponse";

test("keeps recent sessions and both topic pickers in view with a large catalogue", async ({ page, request }) => {
  const topics: TopicResponse[] = [];
  let sessionId: string | undefined;
  const message = "Which trail should we walk this weekend?";
  await page.setViewportSize({ width: 390, height: 844 });

  try {
    for (let i = 1; i <= 60; i++) {
      const response = await request.post("/api/topics", { data: { name: `Walking Club ${i}` } });
      await expect(response).toBeOK();
      topics.push(await response.json());
    }
    const topic = topics[58];
    await page.goto("/");
    const startTopic = page.getByRole("combobox", { name: "Topic", exact: true });
    await expect(startTopic).toBeInViewport({ ratio: 1 });
    await page.getByRole("searchbox", { name: "Search start topics" }).fill("club 59");
    await startTopic.selectOption({ label: topic.name });
    await page.getByRole("button", { name: "Start", exact: true }).click();
    await page.getByRole("textbox", { name: "Message" }).fill(message);
    await page.getByRole("button", { name: "Send", exact: true }).click();
    await expect(page).toHaveURL(/\/conversations\/[^/]+$/);
    sessionId = new URL(page.url()).pathname.split("/").at(-1);
    await expect(page.getByLabel("Assistant message").getByText("Streamed response reached the browser.")).toBeVisible();
    await page.getByRole("button", { name: "All sessions", exact: true }).click();

    const recent = page.getByRole("region", { name: "Recent sessions" });
    const row = recent.getByRole("list").getByRole("button").filter({ hasText: topic.name });
    await expect(row).toBeInViewport({ ratio: 1 });
    const filter = recent.getByRole("combobox", { name: "Topic filter" });
    await expect(filter).toBeInViewport({ ratio: 1 });
    await expect(filter).toHaveValue("");
    const search = recent.getByRole("searchbox", { name: "Search topic filters" });
    await search.fill("club 59");
    await filter.selectOption({ label: topic.name });
    await expect(row).toBeVisible();
    await search.fill("club 58");
    await filter.selectOption({ label: topics[57].name });
    await expect(recent.getByText("No sessions yet.", { exact: true })).toBeVisible();
    await filter.selectOption({ label: "All sessions" });
    await expect(row).toBeVisible();
  } finally {
    if (sessionId) await request.delete(`/api/conversations/${sessionId}`);
    for (const topic of topics.reverse()) await request.delete(`/api/topics/${topic.id}`);
  }
});
