import { expect, test, type Locator } from "@playwright/test";

test("keeps Settings headings, sections and save feedback clear of navigation", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto("/settings/providers");
  const heading = page.getByRole("heading", { name: "Providers", exact: true });
  const sections = page.getByRole("navigation", { name: "Settings sections" });
  const navigation = page.locator("header.topbar");
  await expect(heading).toBeVisible();

  async function clearOfNavigation(element: Locator): Promise<void> {
    const nav = await navigation.boundingBox();
    const box = await element.boundingBox();
    expect(nav).not.toBeNull();
    expect(box).not.toBeNull();
    const intersects = box!.x < nav!.x + nav!.width && box!.x + box!.width > nav!.x
      && box!.y < nav!.y + nav!.height && box!.y + box!.height > nav!.y;
    expect(intersects, `${await element.innerText()} overlaps application navigation`).toBe(false);
    await expect(element).toBeInViewport({ ratio: 1 });
  }

  await clearOfNavigation(heading);
  await clearOfNavigation(sections);
  const modelInput = page.getByRole("textbox", { name: /Add model by id/ });
  async function scrollFields(top: number): Promise<number> {
    return modelInput.evaluate((input, scrollTop) => {
      let parent = input.parentElement;
      while (parent && !(getComputedStyle(parent).overflowY === "auto" && parent.scrollHeight > parent.clientHeight)) {
        parent = parent.parentElement;
      }
      if (!parent) throw new Error("Settings fields have no scrolling container");
      parent.scrollTop = scrollTop;
      return parent.scrollTop;
    }, top);
  }
  // Check the transition as well as the endpoint: the heading must never
  // pass under application navigation while the actual fields scroll.
  for (const scrollTop of [20, 60, 120, 240]) {
    expect(await scrollFields(scrollTop)).toBeGreaterThan(0);
    await clearOfNavigation(heading);
    await clearOfNavigation(sections);
  }
  await scrollFields(0);
  await page.getByRole("textbox", { name: /Add model by id/ }).fill("settings-layout-model");
  await page.getByRole("button", { name: "Add model", exact: true }).click();
  const notice = page.locator(".notice").filter({ hasText: /Saved/ }).first();
  await expect(notice).toBeVisible();
  for (const top of [0, 60, 240]) {
    await scrollFields(top);
    await clearOfNavigation(heading);
    await clearOfNavigation(sections);
    await clearOfNavigation(notice);
  }
  await page.screenshot({ path: testInfo.outputPath("settings-saved.png") });
});
