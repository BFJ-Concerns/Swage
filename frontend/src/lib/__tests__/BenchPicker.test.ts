// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { mount, unmount } from "svelte";

import BenchPicker from "../components/BenchPicker.svelte";
import type { BenchStore } from "../stores/benches";
import type { Bench } from "../types";

type MountedComponent = ReturnType<typeof mount>;

const bench: Bench = {
  id: "bench-1",
  name: "Research",
  slug: "research",
  description: "",
  is_default: false,
  created_at: "2026-05-26T10:00:00Z",
};

function benchStore(): BenchStore {
  return {
    get benches() { return [bench]; },
    get currentBench() { return null; },
    get recentBenches() { return []; },
    get isLoading() { return false; },
    loadBenches: async () => {},
    selectBench: async () => {},
    createBench: async () => {},
    deleteBench: async () => {},
  };
}

describe("BenchPicker", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.innerHTML = "";
  });

  it("routes the bench card secondary menu to bench settings", () => {
    const onOpenBenchSettings = vi.fn();
    component = mount(BenchPicker, {
      target: document.body,
      props: { benchStore: benchStore(), onOpenBenchSettings },
    });

    (document.querySelector(".bench-menu") as HTMLElement).click();

    expect(onOpenBenchSettings).toHaveBeenCalledWith(bench);
  });
});
