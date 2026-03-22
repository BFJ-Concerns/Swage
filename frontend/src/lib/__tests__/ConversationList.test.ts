// @vitest-environment jsdom
/**
 * Tests for the ConversationList sidebar component.
 *
 * The component reads from the conversation store prop and renders
 * a clickable list with create, delete, empty state, and active highlight.
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import { mount, unmount, flushSync } from "svelte";
import ConversationList from "../components/ConversationList.svelte";
import type { ConversationStore } from "../stores/conversations";
import type { ConversationSummary, ConversationDetail } from "../types";

type MountedComponent = ReturnType<typeof mount>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeConversation(overrides: Partial<ConversationSummary> = {}): ConversationSummary {
  return {
    id: "conv-1",
    created_at: "2026-03-20T08:00:00Z",
    last_activity: "2026-03-20T08:00:00Z",
    message_preview: "Hello there",
    has_interrupted_turn: false,
    ...overrides,
  };
}

function makeStore(
  conversations: ConversationSummary[] = [],
  active: ConversationDetail | null = null,
): ConversationStore {
  return {
    conversations,
    activeConversation: active,
    isLoading: false,
    loadConversations: vi.fn().mockResolvedValue(undefined),
    selectConversation: vi.fn().mockResolvedValue(undefined),
    createConversation: vi.fn().mockResolvedValue(undefined),
    deleteConversation: vi.fn().mockResolvedValue(undefined),
    addMessage: vi.fn(),
    refreshActiveConversation: vi.fn().mockResolvedValue(undefined),
  };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("ConversationList", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    while (document.body.firstChild) {
      document.body.removeChild(document.body.firstChild);
    }
  });

  // --- Empty state ---

  it("shows empty state message when there are no conversations", () => {
    const store = makeStore([]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const el = document.querySelector(".conversation-list");
    expect(el).not.toBeNull();
    // Should show a helpful empty state, not the raw placeholder.
    const text = el!.textContent!.toLowerCase();
    expect(text).toMatch(/start|new|no conversation/i);
    // Should NOT show the raw stub placeholder text.
    expect(el!.textContent).not.toContain("not yet implemented");
  });

  // --- Conversation entries ---

  it("renders one entry per conversation", () => {
    const store = makeStore([
      makeConversation({ id: "a" }),
      makeConversation({ id: "b" }),
      makeConversation({ id: "c" }),
    ]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    // Component uses <li class="conversation-item"> for each entry.
    const entries = document.querySelectorAll(".conversation-item");
    expect(entries).toHaveLength(3);
  });

  it("shows the message preview when available", () => {
    const store = makeStore([makeConversation({ message_preview: "Hello there" })]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const el = document.querySelector(".conversation-list");
    expect(el!.textContent).toContain("Hello there");
  });

  it("shows a fallback label when the preview is null", () => {
    const store = makeStore([makeConversation({ message_preview: null })]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    // "New conversation" should appear in place of a preview.
    const entry = document.querySelector(".conversation-item");
    expect(entry).not.toBeNull();
    const text = entry!.textContent!.toLowerCase();
    expect(text).toMatch(/new conversation/i);
  });

  it("shows a timestamp for each conversation", () => {
    const store = makeStore([makeConversation()]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const timestamp = document.querySelector(".conversation-item .timestamp");
    expect(timestamp).not.toBeNull();
    expect(timestamp!.textContent!.trim().length).toBeGreaterThan(0);
  });

  // --- Interrupted turn indicator ---

  it("shows an interrupted turn indicator when has_interrupted_turn is true", () => {
    const store = makeStore([makeConversation({ has_interrupted_turn: true })]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const entry = document.querySelector(".conversation-item");
    const hasIndicator =
      entry!.querySelector(".interrupted") !== null ||
      entry!.classList.contains("interrupted");
    expect(hasIndicator).toBe(true);
  });

  it("does not show the interrupted indicator for normal conversations", () => {
    const store = makeStore([makeConversation({ has_interrupted_turn: false })]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const entry = document.querySelector(".conversation-item");
    expect(entry!.querySelector(".interrupted")).toBeNull();
    expect(entry!.classList.contains("interrupted")).toBe(false);
  });

  // --- Active highlight ---

  it("applies an active style to the selected conversation", () => {
    const conv = makeConversation({ id: "active-one" });
    const activeDetail: ConversationDetail = {
      id: "active-one",
      project_id: "proj-1",
      created_at: conv.created_at,
      last_activity: conv.last_activity,
      messages: [],
    };
    const store = makeStore([conv], activeDetail);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const entry = document.querySelector(".conversation-item");
    expect(entry!.classList.contains("active")).toBe(true);
  });

  it("does not mark inactive conversations as active", () => {
    const conversations = [
      makeConversation({ id: "a" }),
      makeConversation({ id: "b" }),
    ];
    const activeDetail: ConversationDetail = {
      id: "a",
      project_id: "proj-1",
      created_at: conversations[0].created_at,
      last_activity: conversations[0].last_activity,
      messages: [],
    };
    const store = makeStore(conversations, activeDetail);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const entries = document.querySelectorAll(".conversation-item");
    expect(entries).toHaveLength(2);
    expect(entries[0].classList.contains("active")).toBe(true);
    expect(entries[1].classList.contains("active")).toBe(false);
  });

  // --- New conversation button ---

  it("renders a 'New conversation' button", () => {
    const store = makeStore([]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    // Component uses class="new-btn" for the create action.
    const newButton = document.querySelector("button.new-btn");
    expect(newButton).not.toBeNull();
  });

  it("calls store.createConversation when the new button is clicked", async () => {
    const store = makeStore([]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const newButton = document.querySelector("button.new-btn") as HTMLButtonElement;
    expect(newButton).not.toBeNull();
    newButton.click();
    await Promise.resolve();
    expect(store.createConversation).toHaveBeenCalledOnce();
  });

  // --- Delete with inline confirmation ---

  it("renders a delete button per conversation entry", () => {
    const store = makeStore([makeConversation()]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    // Component uses class="delete-btn" for the per-item delete action.
    const deleteBtn = document.querySelector(".conversation-item .delete-btn");
    expect(deleteBtn).not.toBeNull();
  });

  it("shows an inline confirm step after clicking delete", () => {
    const store = makeStore([makeConversation({ id: "x" })]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    // First click: show confirmation — must NOT call deleteConversation yet.
    const deleteBtn = document.querySelector(
      ".conversation-item .delete-btn",
    ) as HTMLButtonElement;
    flushSync(() => deleteBtn.click());

    // Confirmation UI should now be visible (.delete-confirm span with buttons).
    const confirmEl = document.querySelector(".delete-confirm");
    expect(confirmEl).not.toBeNull();
    expect(store.deleteConversation).not.toHaveBeenCalled();
  });

  it("calls store.deleteConversation after confirming delete", async () => {
    const store = makeStore([makeConversation({ id: "x" })]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    // First click: request deletion — flushSync to render the confirm step.
    const deleteBtn = document.querySelector(
      ".conversation-item .delete-btn",
    ) as HTMLButtonElement;
    flushSync(() => deleteBtn.click());

    // Second click: the first button inside .delete-confirm is the confirm action.
    const confirmBtn = document.querySelector(
      ".delete-confirm .confirm-btn",
    ) as HTMLButtonElement;
    expect(confirmBtn).not.toBeNull();
    confirmBtn.click();
    await Promise.resolve();

    expect(store.deleteConversation).toHaveBeenCalledWith("x");
  });

  // --- Select on click ---

  it("calls store.selectConversation when a conversation entry is clicked", async () => {
    const conv = makeConversation({ id: "conv-click-test" });
    const store = makeStore([conv]);
    component = mount(ConversationList, { target: document.body, props: { store } });

    const entry = document.querySelector(".conversation-item") as HTMLElement;
    entry.click();
    await Promise.resolve();

    expect(store.selectConversation).toHaveBeenCalledWith("conv-click-test");
  });
});
