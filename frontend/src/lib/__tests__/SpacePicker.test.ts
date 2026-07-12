// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import SpacePicker from "../components/SpacePicker.svelte";
import type { SpaceStore } from "../stores/spaces";
import type { Space } from "../types";

type MountedComponent = ReturnType<typeof mount>;

const space: Space = {
  id: "space-1",
  name: "Research",
  slug: "research",
  description: "",
  is_default: false,
  created_at: "2026-05-26T10:00:00Z",
};

function spaceStore(): SpaceStore {
  return {
    get spaces() { return [space]; },
    get currentSpace() { return null; },
    get recentSpaces() { return []; },
    get isLoading() { return false; },
    loadSpaces: async () => {},
    selectSpace: async () => {},
    closeSpace: () => {},
    createSpace: async () => {},
    deleteSpace: async () => {},
  };
}

describe("SpacePicker", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.innerHTML = "";
  });

  it("routes the space card settings button to space settings", () => {
    const onOpenSpaceSettings = vi.fn();
    component = mount(SpacePicker, {
      target: document.body,
      props: { spaceStore: spaceStore(), onOpenSpaceSettings },
    });

    (document.querySelector(".space-settings") as HTMLElement).click();

    expect(onOpenSpaceSettings).toHaveBeenCalledWith(space);
  });

  it("renders Open as a real sibling button, not a nested span", () => {
    // UI audit f-2: Open and Settings must be first-class <button> siblings of
    // the card container, never nested interactive widgets inside a button.
    component = mount(SpacePicker, {
      target: document.body,
      props: { spaceStore: spaceStore() },
    });

    const openBtn = document.querySelector(".open-btn");
    expect(openBtn).toBeInstanceOf(HTMLButtonElement);
    // The card itself is no longer a button — it's a container.
    expect(document.querySelector(".space-card")).not.toBeInstanceOf(HTMLButtonElement);
    // No nested role="button" spans remain.
    expect(document.querySelectorAll('[role="button"]')).toHaveLength(0);
  });
});
