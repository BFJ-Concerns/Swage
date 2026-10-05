// @vitest-environment jsdom
/**
 * The surfaces pane's module controls: the person's side of C3's joint work.
 * Every module offers edit, move and delete; a save names the revision the
 * editor opened at, and a save refused as stale shows the newer version and
 * leaves replacing it as a deliberate second act.
 */
import { describe, it, expect, afterEach, vi } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import { fromStore, writable } from "svelte/store";
import { compile } from "svelte/compiler";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import SurfacesPane from "../components/SurfacesPane.svelte";
import type { SurfacesStore } from "../stores/surfaces";
import type { SurfaceResponse } from "../protocol/SurfaceResponse";
import type { SurfaceModuleView } from "../protocol/SurfaceModuleView";

type MountedComponent = ReturnType<typeof mount>;

const SURFACE_ID = "11111111-1111-4111-8111-111111111111";
const SESSION_ID = "33333333-3333-4333-8333-333333333333";

function module(id: string, source: string, position: number): SurfaceModuleView {
  return {
    id,
    kind: "document",
    position,
    source,
    revision: 0,
    updated_at: "2026-10-04T10:00:00Z",
    annotations: [],
  };
}

function surface(modules: SurfaceModuleView[]): SurfaceResponse {
  return {
    id: SURFACE_ID,
    session_id: SESSION_ID,
    title: "Planning",
    created_at: "2026-10-04T10:00:00Z",
    updated_at: "2026-10-04T10:00:00Z",
    modules,
  };
}

function fakeStore(initial: SurfaceResponse) {
  // Reactive, as the real store's is: the pane re-reads it when it changes.
  const activeStore = writable<SurfaceResponse | null>(initial);
  const activeState = fromStore(activeStore);
  const store = {
    surfaces: [{ id: initial.id, title: initial.title, updated_at: initial.updated_at }],
    get activeSurface() {
      return activeState.current;
    },
    setActive(next: SurfaceResponse | null) {
      activeStore.set(next);
    },
    library: [],
    loadSurfaces: vi.fn(async () => {}),
    openSurface: vi.fn(async () => {}),
    closeSurface: vi.fn(),
    createSurface: vi.fn(async () => null),
    refreshLibrary: vi.fn(async () => {}),
    saveToLibrary: vi.fn(async () => {}),
    entryName: vi.fn(() => null),
    writeModule: vi.fn(async (): Promise<SurfaceModuleView | null> => null),
    deleteModule: vi.fn(async () => {}),
    moveModule: vi.fn(async () => {}),
    annotate: vi.fn(async () => {}),
    updateAnnotation: vi.fn(async () => {}),
    removeAnnotation: vi.fn(async () => {}),
    stopListening: vi.fn(),
  };
  return store as typeof store & SurfacesStore;
}

function button(label: string, within: ParentNode = document): HTMLButtonElement {
  const found = within.querySelector(`button[aria-label="${label}"]`) ??
    Array.from(within.querySelectorAll("button")).find(
      (candidate) => candidate.textContent?.trim() === label,
    );
  if (!(found instanceof HTMLButtonElement)) throw new Error(`no "${label}" button`);
  return found;
}

async function settled(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  flushSync();
}

describe("SurfacesPane module controls", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.innerHTML = "";
  });

  it("offers move and delete on every module, and calls the store with the module's place", async () => {
    const store = fakeStore(surface([module("m-1", "# First", 0), module("m-2", "# Second", 1)]));
    component = mount(SurfacesPane, {
      target: document.body,
      props: { store, sessionId: SESSION_ID },
    });
    flushSync();

    const articles = document.querySelectorAll("article.module");
    expect(articles).toHaveLength(2);
    expect(button("Move module up", articles[0]).disabled).toBe(true);
    expect(button("Move module down", articles[1]).disabled).toBe(true);

    button("Move module down", articles[0]).click();
    await settled();
    expect(store.moveModule).toHaveBeenCalledWith(SURFACE_ID, "m-1", 1);

    button("Delete module", articles[1]).click();
    await settled();
    expect(store.deleteModule).toHaveBeenCalledWith(SURFACE_ID, "m-2");
  });

  it("keeps a draft whose module was removed meanwhile, as a new module or discarded", async () => {
    const opened = module("m-1", "# Mine", 0);
    const store = fakeStore(surface([opened]));
    // The save is refused because the module changed; by the time the store
    // refetches, the other party has removed the module altogether.
    store.writeModule.mockImplementationOnce(async () => {
      store.setActive(surface([]));
      return { ...opened, source: "# Theirs", revision: 1 };
    });
    store.writeModule.mockResolvedValueOnce(null);
    component = mount(SurfacesPane, {
      target: document.body,
      props: { store, sessionId: SESSION_ID },
    });
    flushSync();

    button("Edit").click();
    flushSync();
    const textarea = document.querySelector("article.module textarea") as HTMLTextAreaElement;
    textarea.value = "# Mine, edited";
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    button("Save").click();
    await settled();

    const orphan = document.querySelector('[aria-label="Draft of a removed module"]');
    expect(orphan).not.toBeNull();
    expect((orphan!.querySelector("textarea") as HTMLTextAreaElement).value).toBe("# Mine, edited");

    button("Add it as a new module").click();
    await settled();
    expect(store.writeModule).toHaveBeenLastCalledWith(SURFACE_ID, null, "# Mine, edited", null);
    expect(document.querySelector('[aria-label="Draft of a removed module"]')).toBeNull();
  });

  it("shows a mutation the gateway refused and refetches the surface instead of stranding the editor", async () => {
    const store = fakeStore(surface([module("m-1", "# Gone soon", 0)]));
    store.writeModule.mockRejectedValueOnce(new Error("no module m-1 on surface"));
    component = mount(SurfacesPane, {
      target: document.body,
      props: { store, sessionId: SESSION_ID },
    });
    flushSync();

    button("Edit").click();
    flushSync();
    button("Save").click();
    await settled();

    expect(document.querySelector('[role="alert"]')?.textContent).toContain("no module m-1 on surface");
    expect(document.querySelector("article.module textarea")).toBeNull();
    expect(store.openSurface).toHaveBeenCalledWith(SURFACE_ID);

    store.deleteModule.mockRejectedValueOnce(new Error("surface belongs to another session"));
    button("Delete module").click();
    await settled();
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("another session");
    expect(store.openSurface).toHaveBeenCalledTimes(2);
  });

  it("saves against the revision the editor opened at, shows the newer version when refused, and replaces only on a second deliberate act", async () => {
    const opened = { ...module("m-1", "# Mine, as opened", 0), revision: 3 };
    const store = fakeStore(surface([opened]));
    const newer = { ...opened, source: "# The agent's, meanwhile", revision: 4 };
    store.writeModule.mockResolvedValueOnce(newer).mockResolvedValueOnce(null);
    component = mount(SurfacesPane, {
      target: document.body,
      props: { store, sessionId: SESSION_ID },
    });
    flushSync();

    button("Edit").click();
    flushSync();
    const textarea = document.querySelector("article.module textarea") as HTMLTextAreaElement;
    textarea.value = "# Mine, edited";
    textarea.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    button("Save").click();
    await settled();

    expect(store.writeModule).toHaveBeenCalledWith(SURFACE_ID, "m-1", "# Mine, edited", 3);
    const notice = document.querySelector('[role="alert"]');
    expect(notice?.textContent).toContain("changed while you were editing");
    expect(notice?.textContent).toContain("The agent's, meanwhile");
    expect(document.querySelector("article.module textarea")).not.toBeNull();
    expect(
      Array.from(document.querySelectorAll("article.module button")).some(
        (candidate) => candidate.textContent?.trim() === "Save",
      ),
    ).toBe(false);

    button("Replace it with my edit").click();
    await settled();
    expect(store.writeModule).toHaveBeenLastCalledWith(SURFACE_ID, "m-1", "# Mine, edited", 4);
    expect(document.querySelector("article.module textarea")).toBeNull();
  });

  it("renders document tables as padded, separated cells", () => {
    const rota = "| Name | Shift | Cost | Status |\n| --- | --- | --- | --- |\n| Priya | Morning | 280 | Confirmed |";
    const store = fakeStore(surface([module("m-1", rota, 0)]));
    component = mount(SurfacesPane, {
      target: document.body,
      props: { store, sessionId: SESSION_ID },
    });
    flushSync();

    const content = document.querySelector("article.module .module-content");
    const table = content?.querySelector("table");
    expect(table).not.toBeNull();
    expect(Array.from(table!.querySelectorAll("th")).map((cell) => cell.textContent)).toEqual([
      "Name", "Shift", "Cost", "Status",
    ]);

    // Vitest does not load component styles, so apply the pane's own
    // stylesheet under the scope class the mounted pane carries.
    const scope = Array.from(content!.classList).find((name) => name.startsWith("svelte-"));
    expect(scope).toBeDefined();
    const source = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../components/SurfacesPane.svelte"),
      "utf8",
    );
    const { css } = compile(source, { filename: "SurfacesPane.svelte", css: "external", cssHash: () => scope! });
    const style = document.createElement("style");
    style.textContent = css?.code ?? "";
    document.head.append(style);
    try {
      for (const cell of table!.querySelectorAll("th, td")) {
        const computed = getComputedStyle(cell);
        expect(parseFloat(computed.paddingLeft)).toBeGreaterThan(0);
        expect(parseFloat(computed.paddingRight)).toBeGreaterThan(0);
      }
    } finally {
      style.remove();
    }
  });
});
