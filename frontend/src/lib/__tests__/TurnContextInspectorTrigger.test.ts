// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

vi.mock("../api/turns", () => ({
  fetchTurnContext: vi.fn(),
}));

import { fetchTurnContext } from "../api/turns";
import TurnContextInspectorTrigger from "../components/TurnContextInspectorTrigger.svelte";

const mounted: ReturnType<typeof mount>[] = [];

afterEach(() => {
  for (const component of mounted.splice(0)) unmount(component);
  document.body.replaceChildren();
  vi.mocked(fetchTurnContext).mockReset();
});

function trigger(turnId: string, openAtRound: number | null) {
  const component = mount(TurnContextInspectorTrigger, { target: document.body, props: { turnId, openAtRound } });
  mounted.push(component);
  flushSync();
  return component;
}

function inspectorOpen(): boolean {
  return document.querySelector(".context-inspector") !== null;
}

it("honours an address once: a later trigger for the same address leaves the reader's close alone", () => {
  vi.mocked(fetchTurnContext).mockImplementation(() => new Promise(() => {}));
  trigger("turn-1", 2);
  expect(inspectorOpen()).toBe(true);

  flushSync(() => document.querySelector<HTMLButtonElement>(".context-inspector .close")!.click());
  expect(inspectorOpen()).toBe(false);

  // Another message of the turn, or the transcript rebuilt beside a
  // surface, mounts a fresh trigger with the address unchanged.
  trigger("turn-1", 2);
  expect(inspectorOpen()).toBe(false);

  // A different round on the turn is a new address and opens.
  trigger("turn-1", 3);
  expect(inspectorOpen()).toBe(true);
});

it("opens again for an address once the previous one was withdrawn in place", () => {
  vi.mocked(fetchTurnContext).mockImplementation(() => new Promise(() => {}));
  trigger("turn-1", 2);
  flushSync(() => document.querySelector<HTMLButtonElement>(".context-inspector .close")!.click());
  expect(inspectorOpen()).toBe(false);

  // The focus moves off the turn: its trigger sees no address.
  trigger("turn-1", null);
  // Back on the same address, the inspector opens as a fresh arrival would.
  trigger("turn-1", 2);
  expect(inspectorOpen()).toBe(true);
});
