// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import AutomationSettingsPanel from "../components/settings/AutomationSettingsPanel.svelte";
import { emptySettings } from "./fixtures/settings";

let component: ReturnType<typeof mount> | undefined;
afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  document.body.innerHTML = "";
});

it("names every Automation input and visibly labels context, approval and permission fields", () => {
  const settings = emptySettings();
  settings.tool_runtime.search = { provider_type: "searxng", base_url: "https://search.example.org" };
  component = mount(AutomationSettingsPanel, { target: document.body, props: { settings } });
  flushSync();
  for (const input of document.querySelectorAll("input, textarea, select")) {
    expect.soft(input).toHaveAccessibleName();
  }
  for (const name of [
    "Approval timeout (seconds)", "Context assembly", "Context assembly timeout (seconds)",
    "Context token budget", "Tool allow rules", "Tool deny rules", "Tool bundles", "Approval-gated tools",
  ]) {
    const label = [...document.querySelectorAll("label")].find((label) => label.textContent?.trim() === name);
    expect.soft(label, `${name}: visible associated label`).toBeDefined();
    const input = label?.control;
    expect.soft(input, `${name}: labelled control`).toBeTruthy();
    if (input) expect.soft(input).toHaveAccessibleName(name);
  }
});
