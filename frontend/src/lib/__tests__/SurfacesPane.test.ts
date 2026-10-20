// @vitest-environment jsdom
/**
 * The surfaces pane's module controls: the person's side of editing a
 * surface together with the agent.
 * Every module offers edit, move and delete; a save names the revision the
 * editor opened at, and a save refused as stale shows the newer version and
 * leaves replacing it as a deliberate second act.
 */
import { describe, it, expect, afterEach, onTestFinished, vi } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import { fromStore, writable } from "svelte/store";
import { compile } from "svelte/compiler";
import SurfaceLibrary from "../components/SurfaceLibrary.svelte";
import MessageDisplay from "../components/MessageDisplay.svelte";
import messageDisplaySource from "../components/MessageDisplay.svelte?raw";
import { renderedStyles } from "./rendered-styles";
import { createConversationStore } from "../stores/conversations";
import * as conversationApi from "../api/conversations";
import SurfacesPane from "../components/SurfacesPane.svelte";
import surfacesPaneSource from "../components/SurfacesPane.svelte?raw";
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
    delivery: null,
    annotations: [],
  };
}

function surface(modules: SurfaceModuleView[]): SurfaceResponse {
  return {
    id: SURFACE_ID,
    session_id: SESSION_ID,
    shape_only: false,
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
    renameSurface: vi.fn(async () => {}),
    resolveAnnotation: vi.fn(async () => {}),
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

  it("names the schedule and run that delivered a module, and says nothing on one written by hand", async () => {
    const delivered: SurfaceModuleView = {
      ...module("m-1", "DELIVERY-SURFACE-OK", 0),
      delivery: {
        by: "schedule_run",
        run_id: "44444444-4444-4444-8444-444444444444",
        schedule_id: "22222222-2222-4222-8222-222222222222",
        delivered_at: "2026-10-04T10:05:00Z",
      },
    };
    const store = fakeStore(surface([delivered, module("m-2", "# Mine", 1)]));
    component = mount(SurfacesPane, {
      target: document.body,
      props: { store, sessionId: SESSION_ID },
    });
    flushSync();

    const lines = Array.from(document.querySelectorAll(".module-delivery"));
    expect(lines).toHaveLength(1);
    const text = lines[0].textContent!.replace(/\s+/g, " ").trim();
    expect(text).toContain("Delivered by schedule 22222222, run 44444444 at");
    expect(lines[0].querySelector("time")?.getAttribute("datetime")).toBe("2026-10-04T10:05:00Z");
    expect(document.querySelectorAll(".module")[1].querySelector(".module-delivery")).toBeNull();
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

  it("links every module in order to its own article, including the seventh", () => {
    const modules = Array.from({ length: 7 }, (_, index) =>
      module(`m-${index + 1}`, `# Harbour ${index + 1}`, index),
    );
    const store = fakeStore(surface(modules));
    component = mount(SurfacesPane, {
      target: document.body,
      props: { store, sessionId: SESSION_ID },
    });
    flushSync();

    const links = Array.from(document.querySelectorAll<HTMLAnchorElement>(
      'nav[aria-label="Modules"] a',
    ));
    expect.soft(links).toHaveLength(7);
    for (const index of modules.keys()) {
      const article = document.querySelectorAll("article.module")[index];
      const link = links[index];
      expect.soft(link?.textContent ?? "").toContain(`Harbour ${index + 1}`);
      expect.soft(link?.getAttribute("href")).toBe(`#${article.id || "missing-target"}`);
      if (link) expect(document.getElementById(link.hash.slice(1))).toBe(article);
      expect(button("Edit", article)).toBeInstanceOf(HTMLButtonElement);
      expect(button("Add note", article)).toBeInstanceOf(HTMLButtonElement);
    }

    // The links follow the rendered order when the shared surface changes.
    store.setActive(surface([modules[6], modules[0]]));
    flushSync();
    const reordered = document.querySelectorAll('nav[aria-label="Modules"] a');
    expect.soft(reordered).toHaveLength(2);
    expect.soft(reordered[0]?.textContent ?? "").toContain("Harbour 7");
    expect.soft(reordered[1]?.textContent ?? "").toContain("Harbour 1");
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

  it("updates conversation and surface cells through one shared table rule", async () => {
    const source = "| Name | Shift |\n| --- | --- |\n| Priya | Morning |";
    const conversations = createConversationStore();
    vi.spyOn(conversationApi, "getConversation").mockResolvedValue({
      id: SESSION_ID, agent_id: "gateway", agent_display_name: "Swage default",
      topic_id: null, created_at: "2026-10-20T12:00:00Z", last_activity: "2026-10-20T12:00:00Z",
      messages: [{ role: "assistant", content: source, timestamp: "2026-10-20T12:00:00Z", turn_state: "completed" }],
    });
    vi.spyOn(conversationApi, "listBranches").mockResolvedValue([]);
    onTestFinished(() => { vi.restoreAllMocks(); });
    await conversations.selectConversation(SESSION_ID);
    const conversation = mount(MessageDisplay, { target: document.body, props: { store: conversations } });
    onTestFinished(() => unmount(conversation));
    component = mount(SurfacesPane, {
      target: document.body,
      props: { store: fakeStore(surface([module("m-1", source, 0)])), sessionId: SESSION_ID },
    });
    flushSync();
    const styles: HTMLStyleElement[] = [];
    onTestFinished(() => styles.forEach((style) => style.remove()));
    for (const [source, root, filename] of [
      [messageDisplaySource, ".message-display", "MessageDisplay.svelte"],
      [surfacesPaneSource, ".module-content", "SurfacesPane.svelte"],
    ]) {
      const scope = [...document.querySelector(root)!.classList].find((name) => name.startsWith("svelte-")) ?? "svelte-unstyled";
      const { css } = compile(source, { filename, css: "external", cssHash: () => scope });
      const style = renderedStyles(css!.code);
      styles.push(style);
    }
    const shared = renderedStyles();
    styles.push(shared);
    const cells = [".assistant-content", ".module-content"].flatMap((root) =>
      [...document.querySelectorAll(`${root} th, ${root} td`)],
    );
    expect(cells).toHaveLength(8);
    for (const cell of cells) {
      expect(getComputedStyle(cell).borderLeftStyle).toBe("solid");
      expect(getComputedStyle(cell).borderLeftWidth).toBe("1px");
    }
    // Change the production stylesheet's shared cell rule, never a rule
    // supplied by this test. Independently owned rules will not follow it.
    for (const rule of shared.sheet!.cssRules) {
      if (rule instanceof CSSStyleRule && rule.selectorText.includes(".riso-tables td")) {
        rule.style.setProperty("padding", "17px");
      }
    }
    shared.textContent = [...shared.sheet!.cssRules].map((rule) => rule.cssText).join("\n");
    for (const cell of cells) expect(getComputedStyle(cell).paddingLeft).toBe("17px");
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
    // stylesheet under the scope class the mounted pane carries (Svelte adds
    // none to an element no rule targets, leaving the cells unstyled).
    const scope = Array.from(content!.classList).find((name) => name.startsWith("svelte-")) ??
      "svelte-unstyled";
    const { css } = compile(surfacesPaneSource, { filename: "SurfacesPane.svelte", css: "external", cssHash: () => scope });
    const style = renderedStyles(css?.code ?? "");
    try {
      for (const cell of table!.querySelectorAll("th, td")) {
        const computed = getComputedStyle(cell);
        expect(computed.borderLeftStyle).toBe("solid");
        expect(computed.borderLeftWidth).toBe("1px");
        expect(computed.borderTopStyle).toBe("solid");
        expect(computed.borderTopWidth).toBe("1px");
        // Beyond the browser default's 1px, which lets cell texts touch.
        expect(parseFloat(computed.paddingLeft)).toBeGreaterThanOrEqual(4);
        expect(parseFloat(computed.paddingRight)).toBeGreaterThanOrEqual(4);
      }
    } finally {
      style.remove();
    }
  });
});


describe("surface workspace corrections", () => {
  let component: MountedComponent | undefined;
  afterEach(() => { if (component) unmount(component); component = undefined; document.body.innerHTML = ""; });
  function show(store: SurfacesStore) {
    component = mount(SurfacesPane, { target: document.body, props: { store, sessionId: SESSION_ID } });
    flushSync();
  }
  it("keeps visible save labels and reviewable values with a preservation statement", async () => {
    show(fakeStore(surface([])));
    button("Save to library").click(); await settled();
    for (const name of ["Library name", "Library description", "Library tags"]) {
      const field = document.querySelector(`[aria-label="${name}"]`) as HTMLInputElement;
      expect(field?.labels?.length, name).toBeGreaterThan(0);
      expect(field.labels![0].textContent).toContain(name);
    }
    expect(document.querySelector('[aria-label="Library description"]')?.tagName).toBe("TEXTAREA");
    expect(document.body.textContent).toContain("Notes stay with this session");
  });
  it("explains that a repeated name creates a separate library entry", async () => {
    const store = fakeStore(surface([]));
    Object.assign(store, { library: [{ id: "saved", name: "Planning", description: "Plan", tags: [], updated_at: "2026-10-01T10:00:00Z", origin_topic_deleted: false }] });
    show(store); button("Save to library").click(); await settled();
    const description = document.querySelector('[aria-label="Library description"]') as HTMLInputElement;
    description.value = "A plan"; description.dispatchEvent(new Event("input", { bubbles: true }));
    await settled(); button("Save").click(); await settled();
    expect(store.saveToLibrary).toHaveBeenCalled();
    expect(document.body.textContent).toContain("separate entry");
  });
  it("explains shape-only seeding but does not label a populated copy as empty", async () => {
    const store = fakeStore(Object.assign(surface([]), { shape_only: true, seeded_from_name: "Atlas" }));
    show(store);
    expect(document.body.textContent).toContain("start empty");
    store.setActive(Object.assign(surface([module("m", "# Saved content", 0)]), { shape_only: false, seeded_from_name: "Atlas" }));
    await settled(); expect(document.body.textContent).not.toContain("start empty");
    expect(document.body.textContent).toContain("Saved content");
  });
  it("renames from the header through the store", async () => {
    const renameSurface = vi.fn(async () => {});
    const store = Object.assign(fakeStore(surface([])), { renameSurface });
    show(store); expect(document.body.textContent).toContain("Rename surface"); button("Rename surface").click(); await settled();
    const title = document.querySelector('[aria-label="Surface title"]') as HTMLInputElement;
    title.value = "Boreal plan"; title.dispatchEvent(new Event("input", { bubbles: true }));
    await settled(); button("Save title").click(); await settled();
    expect(renameSurface).toHaveBeenCalledWith(SURFACE_ID, "Boreal plan");
    button("Rename surface").click(); await settled();
    store.setActive({ ...surface([]), id: "other" }); await settled();
    expect(document.querySelector('[aria-label="Surface title"]')).toBeNull();
    expect(renameSurface).toHaveBeenCalledTimes(1);
  });
  it("ticks rendered checklists through the revision-guarded save", async () => {
    const store = fakeStore(surface([module("m", "- [ ] Draft approved by Maya\n- [x] Retain this", 0)]));
    show(store);
    const checkbox = document.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(checkbox.disabled).toBe(false); checkbox.click(); await settled();
    expect(store.writeModule).toHaveBeenCalledWith(SURFACE_ID, "m", "- [x] Draft approved by Maya\n- [x] Retain this", 0);
  });
  it("shows the newer source when a checklist toggle conflicts", async () => {
    const original = module("m", "- [ ] Draft approved by Maya", 0);
    const store = fakeStore(surface([original]));
    store.writeModule.mockResolvedValueOnce({ ...original, source: "- [ ] Newer wording", revision: 1 });
    show(store); (document.querySelector('input[type="checkbox"]') as HTMLInputElement).click(); await settled();
    expect(document.querySelector('[role="alert"]')?.textContent ?? "").toContain("Newer wording");
    expect(store.writeModule).toHaveBeenCalledTimes(1);
  });
  it.each(["rename", "resolution"] as const)("preserves module and note drafts after a failed %s", async (operation) => {
    const plan = module("m", "# Plan", 0);
    plan.annotations = [
      { id: "note", module_id: "m", body: "Act on this", tags: [], author: "gateway", created_at: "2026-10-01T10:00:00Z", updated_at: "2026-10-01T10:00:00Z" },
    ];
    const initial = surface([plan]);
    const store = fakeStore(initial);
    const failure = new Error(`${operation} temporarily unavailable`);
    if (operation === "rename") store.renameSurface.mockRejectedValueOnce(failure);
    else store.resolveAnnotation.mockRejectedValueOnce(failure);
    store.openSurface.mockImplementationOnce(async () => {
      store.setActive({ ...initial, modules: [{ ...plan, source: "# Refreshed plan", revision: 1 }] });
    });
    show(store);
    button("Add note").click();
    await settled();
    const note = document.querySelector('input[placeholder="Note on this module"]') as HTMLInputElement;
    note.value = "Unsaved note";
    note.dispatchEvent(new Event("input", { bubbles: true }));
    button("Edit", document.querySelector(".module-actions")!).click();
    await settled();
    const editor = document.querySelector("article.module textarea") as HTMLTextAreaElement;
    editor.value = "# Unsaved module edit";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    await settled();

    if (operation === "rename") {
      button("Rename surface").click();
      await settled();
      const title = document.querySelector('[aria-label="Surface title"]') as HTMLInputElement;
      title.value = "Renamed plan";
      title.dispatchEvent(new Event("input", { bubbles: true }));
      await settled();
      button("Save title").click();
    } else button("Resolve").click();
    await settled();

    expect(document.querySelector('[role="alert"]')?.textContent).toContain(failure.message);
    expect(store.openSurface).toHaveBeenCalledWith(SURFACE_ID);
    expect.soft(document.querySelector<HTMLTextAreaElement>("article.module textarea")?.value, "module draft").toBe("# Unsaved module edit");
    expect.soft(document.querySelector<HTMLInputElement>('input[placeholder="Note on this module"]')?.value, "note draft").toBe("Unsaved note");
    expect(store.writeModule).not.toHaveBeenCalled();
    expect(store.annotate).not.toHaveBeenCalled();
  });

  it("preserves an open note draft after a failed checklist toggle", async () => {
    const initial = surface([module("m", "- [ ] Draft approved by Maya", 0)]);
    const store = fakeStore(initial);
    store.writeModule.mockRejectedValueOnce(new Error("Checklist save unavailable"));
    store.openSurface.mockImplementationOnce(async () => { store.setActive({ ...initial }); });
    show(store);
    button("Add note").click(); await settled();
    const note = document.querySelector<HTMLInputElement>('input[placeholder="Note on this module"]')!;
    note.value = "Unsaved checklist note";
    note.dispatchEvent(new Event("input", { bubbles: true }));
    await settled();
    document.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click();
    await settled();
    expect(store.writeModule).toHaveBeenCalledWith(SURFACE_ID, "m", "- [x] Draft approved by Maya", 0);
    expect(store.openSurface).toHaveBeenCalledWith(SURFACE_ID);
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("Checklist save unavailable");
    expect(document.querySelector<HTMLInputElement>('input[placeholder="Note on this module"]')?.value, "open note draft").toBe("Unsaved checklist note");
  });

  it.each(["rename", "resolution"] as const)("clears the failure notice after a successful %s retry", async (operation) => {
    const plan = module("m", "# Plan", 0);
    plan.annotations = [{ id: "note", module_id: "m", body: "Act on this", tags: [], author: "gateway", created_at: "2026-10-01T10:00:00Z", updated_at: "2026-10-01T10:00:00Z" }];
    const store = fakeStore(surface([plan]));
    const mutation = operation === "rename" ? store.renameSurface : store.resolveAnnotation;
    mutation.mockRejectedValueOnce(new Error("The change was not applied."));
    show(store);
    if (operation === "rename") { button("Rename surface").click(); await settled(); }
    const submit = () => button(operation === "rename" ? "Save title" : "Resolve").click();
    submit(); await settled();
    expect(document.querySelector('[role="alert"]')?.textContent).toContain("The change was not applied.");
    submit(); await settled();
    expect(mutation).toHaveBeenCalledTimes(2);
    expect(document.querySelector('[role="alert"]'), "successful retry failure notice").toBeNull();
  });

  it("requests resolution of an open note and filters recorded resolved notes", async () => {
    const m = module("m", "# Plan", 0);
    m.annotations = [
      { id: "note", module_id: "m", body: "Act on this", tags: [], author: "gateway", created_at: "2026-10-01T10:00:00Z", updated_at: "2026-10-01T10:00:00Z" },
      { id: "resolved", module_id: "m", body: "Already acted on", tags: [], author: "user", resolved_at: "2026-10-15T10:00:00Z", created_at: "2026-10-01T10:00:00Z", updated_at: "2026-10-15T10:00:00Z" },
    ];
    const resolveAnnotation = vi.fn(async () => {});
    const store = Object.assign(fakeStore(surface([m])), { resolveAnnotation });
    show(store); expect(document.body.textContent).toContain("Resolve"); button("Resolve").click(); await settled();
    expect(resolveAnnotation).toHaveBeenCalledWith(SURFACE_ID, "note");
    expect(document.body.textContent).toContain("Act on this");
    expect(document.body.textContent).not.toContain("Already acted on");
  });

  it("does not toggle a matching task example inside a nested code block", async () => {
    const source = "- [x] Parent\n\n  ```md\n  - [ ] Nested\n  ```\n\n  - [ ] Nested";
    const store = fakeStore(surface([module("m", source, 0)]));
    show(store);
    const boxes = document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect(boxes).toHaveLength(2);
    boxes[1].click(); await settled();
    expect(store.writeModule).toHaveBeenCalledWith(SURFACE_ID, "m", source.slice(0, source.lastIndexOf("[ ]")) + source.slice(source.lastIndexOf("[ ]")).replace("[ ]", "[x]"), 0);
  });
  it("maps quoted tasks without changing a later unquoted task", async () => {
    const source = "> - [ ] Quoted task\n\n- [ ] Later task";
    const store = fakeStore(surface([module("m", source, 0)]));
    show(store);
    const boxes = document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect(boxes).toHaveLength(2);
    boxes[0].click(); await settled();
    expect(store.writeModule).toHaveBeenCalledWith(SURFACE_ID, "m", "> - [x] Quoted task\n\n- [ ] Later task", 0);
  });
  it("preserves CRLF and Unicode when unticking a later task", async () => {
    const source = "- [ ] Café 🌿\r\n- [x] Retain";
    const store = fakeStore(surface([module("m", source, 0)]));
    show(store);
    document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[1].click(); await settled();
    expect(store.writeModule).toHaveBeenCalledWith(SURFACE_ID, "m", source.replace("[x] Retain", "[ ] Retain"), 0);
  });
  it("maps nested and ordered tasks to source while leaving code examples alone", async () => {
    const source = "```md\n- [ ] Example\n```\n\n- [x] First\n  - [ ] Nested\n\n1. [ ] Ordered";
    const store = fakeStore(surface([module("m", source, 0)]));
    show(store);
    const boxes = document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect(boxes).toHaveLength(3);
    boxes[1].click(); await settled();
    expect(store.writeModule).toHaveBeenLastCalledWith(SURFACE_ID, "m", source.replace("- [ ] Nested", "- [x] Nested"), 0);
    document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[0].click(); await settled();
    expect(store.writeModule).toHaveBeenLastCalledWith(SURFACE_ID, "m", source.replace("- [x] First", "- [ ] First"), 0);
    document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[2].click(); await settled();
    expect(store.writeModule).toHaveBeenLastCalledWith(SURFACE_ID, "m", source.replace("1. [ ] Ordered", "1. [x] Ordered"), 0);
  });

});

it("distinguishes identical library cards by their recorded save times", () => {
  const store = fakeStore(surface([]));
  const times = ["2026-10-01T10:00:00Z", "2026-10-02T10:00:00Z"];
  Object.assign(store, { library: times.map((updated_at, i) => ({ id: String(i), name: "Plan", description: "Plan", tags: [], updated_at, origin_topic_deleted: false })) });
  const component = mount(SurfaceLibrary, { target: document.body, props: { store, sessionId: null } });
  flushSync();
  try {
    const cards = Array.from(document.querySelectorAll(".entry"));
    expect(cards).toHaveLength(times.length);
    cards.forEach((card, index) => {
      const savedAt = card.querySelector("time");
      expect(savedAt?.dateTime, `save time on card ${index}`).toBe(times[index]);
      expect(savedAt?.textContent).toContain(times[index]);
    });
  }
  finally { unmount(component); document.body.innerHTML = ""; }
});
