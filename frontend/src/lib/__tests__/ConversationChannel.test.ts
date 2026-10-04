// @vitest-environment jsdom
/**
 * The conversation channel's thread controls: the switcher that lists the
 * root thread and every branch, the abandon control on an active branch,
 * and the composer giving way to a notice on an abandoned one.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import ConversationChannel from "../components/ConversationChannel.svelte";
import type { ConnectionStore } from "../stores/connection";
import type { ConversationStore } from "../stores/conversations";
import { createStreamingStore } from "../stores/streaming";
import type { ConversationBranch } from "../types";
import { makeConversationDetail } from "./fixtures/conversations";

type MountedComponent = ReturnType<typeof mount>;

function makeBranch(overrides: Partial<ConversationBranch> = {}): ConversationBranch {
  return {
    child_thread_id: "branch-1",
    parent_thread_id: "root-1",
    branched_from_message_id: "msg-1",
    branched_from_preview: "What about the other approach?",
    state: "idle",
    created_at: new Date().toISOString(),
    ...overrides,
  };
}

function makeConversationStore(
  branches: ConversationBranch[],
  activeThreadId: string | null,
): ConversationStore & { selectThread: ReturnType<typeof vi.fn>; abandonActiveBranch: ReturnType<typeof vi.fn> } {
  const detail = makeConversationDetail({ root_thread_id: "root-1" });
  return {
    get conversations() {
      return [];
    },
    get activeConversation() {
      return detail;
    },
    get isLoading() {
      return false;
    },
    get branches() {
      return branches;
    },
    get activeThreadId() {
      return activeThreadId;
    },
    loadConversations: async () => {},
    selectConversation: async () => {},
    createConversation: async () => {},
    deleteConversation: async () => {},
    clearActiveConversation: () => {},
    appendOptimisticUserMessage: () => {},
    appendAssistantMessage: () => {},
    appendRecordedMessage: () => {},
    refreshActiveConversation: async () => {},
    selectThread: vi.fn(async () => {}),
    branchFrom: vi.fn(async () => {}),
    abandonActiveBranch: vi.fn(async () => {}),
  };
}

function makeConnectionStore(): ConnectionStore {
  return {
    get status() {
      return "connected" as const;
    },
    get errorMessage() {
      return null;
    },
    connectTo: () => {},
    disconnect: () => {},
    sendMessage: () => {},
    cancelTurn: () => {},
    onServerEvent: () => {},
  };
}

function mountChannel(conversationStore: ConversationStore): MountedComponent {
  const component = mount(ConversationChannel, {
    target: document.body,
    props: {
      conversationStore,
      connectionStore: makeConnectionStore(),
      streamingStore: createStreamingStore(),
      onRetry: () => {},
      selectedAgentId: "",
    },
  });
  // The switcher's selection follows the store through an effect; settle
  // it so the mounted DOM reads as the store says.
  flushSync();
  return component;
}

describe("ConversationChannel thread controls", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) unmount(component);
    component = undefined;
    document.body.replaceChildren();
  });

  it("shows no thread switcher while the conversation has no branch", () => {
    component = mountChannel(makeConversationStore([], null));

    expect(document.querySelector('select[aria-label="Active thread"]')).toBeNull();
  });

  it("lists the main thread first and names each branch by the message it came from", () => {
    component = mountChannel(
      makeConversationStore(
        [makeBranch(), makeBranch({ child_thread_id: "branch-2", branched_from_preview: null, state: "abandoned" })],
        null,
      ),
    );

    const switcher = document.querySelector<HTMLSelectElement>('select[aria-label="Active thread"]');
    expect(switcher).not.toBeNull();
    expect(switcher!.value).toBe("");
    const labels = Array.from(switcher!.options, (option) => option.textContent);
    expect(labels[0]).toBe("Main thread");
    expect(labels[1]).toContain("Branch from “What about the other approach?”");
    expect(labels[2]).toContain("Branch");
    expect(labels[2]).toContain("abandoned");
    expect(document.querySelector('button[aria-label="Abandon branch"]')).toBeNull();
  });

  it("switching the selector selects that thread, and back to the main thread selects null", () => {
    const store = makeConversationStore([makeBranch()], null);
    component = mountChannel(store);

    const switcher = document.querySelector<HTMLSelectElement>('select[aria-label="Active thread"]')!;
    switcher.value = "branch-1";
    switcher.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();
    expect(store.selectThread).toHaveBeenCalledWith("branch-1");

    const onBranch = makeConversationStore([makeBranch()], "branch-1");
    unmount(component);
    document.body.replaceChildren();
    component = mountChannel(onBranch);
    const back = document.querySelector<HTMLSelectElement>('select[aria-label="Active thread"]')!;
    expect(back.value).toBe("branch-1");
    back.value = "";
    back.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();
    expect(onBranch.selectThread).toHaveBeenCalledWith(null);
  });

  it("returns the switcher to the thread in view when switching fails", async () => {
    const store = makeConversationStore([makeBranch()], null);
    store.selectThread.mockRejectedValueOnce(new Error("conversation is gone"));
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    component = mountChannel(store);

    const switcher = document.querySelector<HTMLSelectElement>('select[aria-label="Active thread"]')!;
    switcher.value = "branch-1";
    switcher.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();
    expect(store.selectThread).toHaveBeenCalledWith("branch-1");
    await vi.waitFor(() => expect(consoleError).toHaveBeenCalled());
    flushSync();
    expect(switcher.value).toBe("");
    consoleError.mockRestore();
  });

  it("offers to abandon the active branch and calls the store when asked", () => {
    const store = makeConversationStore([makeBranch()], "branch-1");
    component = mountChannel(store);

    const abandon = document.querySelector<HTMLButtonElement>('button[aria-label="Abandon branch"]');
    expect(abandon).not.toBeNull();
    abandon!.click();
    expect(store.abandonActiveBranch).toHaveBeenCalledOnce();
    expect(document.querySelector("textarea")).not.toBeNull();
  });

  it("withholds the abandon control while a turn is running on the branch", () => {
    const store = makeConversationStore([makeBranch()], "branch-1");
    const streamingStore = createStreamingStore({ conversationStore: store });
    component = mount(ConversationChannel, {
      target: document.body,
      props: {
        conversationStore: store,
        connectionStore: makeConnectionStore(),
        streamingStore,
        onRetry: () => {},
        selectedAgentId: "",
      },
    });
    flushSync();
    expect(document.querySelector('button[aria-label="Abandon branch"]')).not.toBeNull();

    streamingStore.startTurn("still thinking");
    flushSync();
    expect(document.querySelector('button[aria-label="Abandon branch"]')).toBeNull();
  });

  it("replaces the composer with a notice on an abandoned branch", () => {
    component = mountChannel(makeConversationStore([makeBranch({ state: "abandoned" })], "branch-1"));

    expect(document.querySelector('button[aria-label="Abandon branch"]')).toBeNull();
    expect(document.querySelector("textarea")).toBeNull();
    expect(document.querySelector('[role="status"]')?.textContent).toContain("abandoned");
  });
});
