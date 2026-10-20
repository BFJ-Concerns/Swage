// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import { createClassComponent } from "svelte/legacy";

import MessageInput from "../components/MessageInput.svelte";
import { createStreamingStore } from "../stores/streaming";
import { createConversationStore } from "../stores/conversations";
import * as conversationsApi from "../api/conversations";
import type { ConversationDetail } from "../types";
import type { StreamingStore } from "../stores/streaming";

vi.mock("../api/conversations");

type MountedComponent = ReturnType<typeof mount>;

function makeStreamingStore(isStreaming = false): StreamingStore {
  return {
    get isStreaming() {
      return isStreaming;
    },
    get activeTurnGeneration() {
      return 0;
    },
    get streamedContent() {
      return "";
    },
    get turnError() {
      return null;
    },
    get turnStartedAt() {
      return null;
    },
    get activeToolCalls() {
      return [];
    },
    get providerRetry() {
      return null;
    },
    get pendingApproval() {
      return null;
    },
    handleServerEvent: vi.fn(),
    startTurn: vi.fn(),
    cancelTurn: vi.fn(),
    clearConnectionError: () => {},
    reconcileAfterReconnect: vi.fn(),
  };
}

describe("MessageInput", () => {
  let component: MountedComponent | undefined;

  afterEach(() => {
    if (component) {
      unmount(component);
      component = undefined;
    }
    document.body.innerHTML = "";
  });

  function mountComponent(
    store?: StreamingStore,
    props: {
      modelOptions?: string[];
      modelCatalogueReady?: boolean;
      canSelectAgent?: boolean;
      onNavigate?: (path: string) => void;
    } = {},
  ): StreamingStore {
    const resolvedStore = store ?? makeStreamingStore();
    component = mount(MessageInput, {
      target: document.body,
      props: { store: resolvedStore, ...props },
    });
    return resolvedStore;
  }

  function textarea(): HTMLTextAreaElement {
    return document.querySelector("textarea") as HTMLTextAreaElement;
  }

  function setTextareaValue(value: string): void {
    const input = textarea();
    flushSync(() => {
      input.value = value;
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  it.each(["delegation", "schedule"])("offers Stop for an opened processing %s turn and settles its cancellation", async (origin) => {
    vi.mocked(conversationsApi.getConversation).mockResolvedValue({
      id: "external", topic_id: null, created_at: "2026-10-20T00:00:00Z", last_activity: "2026-10-20T00:00:00Z",
      messages: [{ role: "user", origin, content: "Work in progress", timestamp: "2026-10-20T00:00:00Z", turn_state: "processing" }],
    } as ConversationDetail);
    const conversations = createConversationStore();
    await conversations.selectConversation("external");
    const cancelTurn = vi.fn();
    const store = createStreamingStore({ conversationStore: conversations, connectionStore: { sendMessage: vi.fn(), cancelTurn } });
    component = mount(MessageInput, { target: document.body, props: { store, sessionId: "external" } });
    flushSync();
    const stop = document.querySelector<HTMLButtonElement>(".action.stop");
    expect(stop?.textContent?.trim()).toBe("Stop");
    flushSync(() => stop!.click());
    expect(cancelTurn).toHaveBeenCalledWith("external");
    flushSync(() => store.handleServerEvent({ type: "cancelled" }, "external"));
    expect(conversations.activeConversation?.messages[0].turn_state).toBe("cancelled");
    expect(document.querySelector(".action.stop")).toBeNull();
    expect(document.querySelector(".action.send")).not.toBeNull();
  });

  it("trims whitespace before sending", () => {
    const store = mountComponent();

    setTextareaValue("   Hello, Swage   ");

    flushSync(() => {
      (document.querySelector(".action.send") as HTMLButtonElement).click();
    });

    expect(store.startTurn).toHaveBeenCalledWith("Hello, Swage", null, null, null);
  });

  it("does not send whitespace-only input", () => {
    const store = mountComponent();

    setTextareaValue("   \n  ");

    flushSync(() => {
      (document.querySelector(".action.send") as HTMLButtonElement).click();
    });

    expect(store.startTurn).not.toHaveBeenCalled();
  });

  it("sends on Enter without Shift", () => {
    const store = mountComponent();

    setTextareaValue("Send via keyboard");

    flushSync(() => {
      textarea().dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          bubbles: true,
          cancelable: true,
        }),
      );
    });

    expect(store.startTurn).toHaveBeenCalledWith("Send via keyboard", null, null, null);
  });

  it("does not send on Shift+Enter", () => {
    const store = mountComponent();

    setTextareaValue("Keep newline");

    flushSync(() => {
      textarea().dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          shiftKey: true,
          bubbles: true,
          cancelable: true,
        }),
      );
    });

    expect(store.startTurn).not.toHaveBeenCalled();
  });

  it("guides a user with a settled empty catalogue to provider settings", () => {
    const onNavigate = vi.fn();
    mountComponent(undefined, {
      modelOptions: [],
      modelCatalogueReady: true,
      onNavigate,
    });

    expect(document.querySelector(".model-guidance")?.textContent).toContain(
      "No models are configured",
    );

    flushSync(() => {
      (document.querySelector(".model-guidance button") as HTMLButtonElement).click();
    });

    expect(onNavigate).toHaveBeenCalledWith("/settings/providers");
  });

  it("prevents button and Enter submission when the settled catalogue is empty", () => {
    const store = mountComponent(undefined, {
      modelOptions: [],
      modelCatalogueReady: true,
    });
    setTextareaValue("Cannot send this");

    const sendButton = document.querySelector(".action.send") as HTMLButtonElement;
    expect(sendButton.disabled).toBe(true);

    flushSync(() => {
      sendButton.click();
      textarea().dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "Enter",
          bubbles: true,
          cancelable: true,
        }),
      );
    });

    expect(store.startTurn).not.toHaveBeenCalled();
  });

  it("keeps the model picker and dispatch behaviour for a populated catalogue", () => {
    const store = mountComponent(undefined, {
      modelOptions: ["provider/model-a", "provider/model-b"],
      modelCatalogueReady: true,
    });
    setTextareaValue("Use the selected model");

    const picker = document.querySelector('[aria-label="Model"]') as HTMLSelectElement;
    expect(document.querySelector(".model-guidance")).toBeNull();
    expect(Array.from(picker.options, (option) => option.value)).toEqual([
      "provider/model-a",
      "provider/model-b",
    ]);

    flushSync(() => {
      picker.value = "provider/model-b";
      picker.dispatchEvent(new Event("change", { bubbles: true }));
      (document.querySelector(".action.send") as HTMLButtonElement).click();
    });

    expect(store.startTurn).toHaveBeenCalledWith(
      "Use the selected model",
      "provider/model-b",
      null,
      null,
    );
  });

  it("forwards the rendered effort selection when starting a conversation", () => {
    const store = mountComponent(undefined, {
      modelOptions: ["provider/model-a"],
      modelCatalogueReady: true,
      canSelectAgent: true,
    });
    setTextareaValue("Use maximum effort");

    const picker = document.querySelector('[aria-label="Effort"]') as HTMLSelectElement;
    expect(Array.from(picker.options, (option) => option.value)).toEqual([
      "",
      "off",
      "minimal",
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);

    flushSync(() => {
      picker.value = "max";
      picker.dispatchEvent(new Event("change", { bubbles: true }));
      (document.querySelector(".action.send") as HTMLButtonElement).click();
    });

    expect(store.startTurn).toHaveBeenCalledWith(
      "Use maximum effort",
      "provider/model-a",
      null,
      "max",
    );
  });

  it("returns effort to the model default at a new session boundary", () => {
    const store = makeStreamingStore();
    const input = createClassComponent({
      component: MessageInput,
      target: document.body,
      props: {
        store,
        modelOptions: ["provider/model-a"],
        modelCatalogueReady: true,
        canSelectAgent: true,
        sessionId: "session-one",
      },
    });
    const effort = document.querySelector('[aria-label="Effort"]') as HTMLSelectElement;
    effort.value = "high";
    effort.dispatchEvent(new Event("change", { bubbles: true }));
    flushSync();

    input.$set({ sessionId: "session-two" });
    flushSync();

    expect((document.querySelector('[aria-label="Effort"]') as HTMLSelectElement).value).toBe("");
    setTextareaValue("Use the new session default");
    (document.querySelector(".action.send") as HTMLButtonElement).click();
    expect(store.startTurn).toHaveBeenCalledWith(
      "Use the new session default",
      "provider/model-a",
      null,
      null,
    );
    input.$destroy();
  });

  it("follows a changed session default while retaining a manual choice within that session", () => {
    const store = makeStreamingStore();
    const input = createClassComponent({
      component: MessageInput, target: document.body, props: {
        store, modelOptions: ["global", "topic-a", "topic-b"], selectedModelId: "global",
        modelCatalogueReady: false, sessionId: null as string | null,
      },
    });
    try {
      flushSync();
      input.$set({ selectedModelId: "topic-a" });
      flushSync();
      const picker = document.querySelector('[aria-label="Model"]') as HTMLSelectElement;
      expect(picker.value).toBe("topic-a");
      flushSync(() => { picker.value = "topic-b"; picker.dispatchEvent(new Event("change", { bubbles: true })); });
      input.$set({ modelCatalogueReady: true });
      flushSync();
      expect(picker.value).toBe("topic-b");
      input.$set({ selectedModelId: "global", sessionId: "new-session" });
      flushSync();
      expect(picker.value).toBe("global");
      setTextareaValue("New default");
      flushSync(() => (document.querySelector(".action.send") as HTMLButtonElement).click());
      expect(store.startTurn).toHaveBeenCalledWith("New default", "global", null, null);
    } finally {
      input.$destroy();
    }
  });

  it("clears the draft and re-focuses the textarea after send", () => {
    mountComponent();

    setTextareaValue("Refocus me");

    flushSync(() => {
      (document.querySelector(".action.send") as HTMLButtonElement).click();
    });

    const input = textarea();

    expect(input.value).toBe("");
    expect(document.activeElement).toBe(input);
  });

  it("renders a stop button while streaming and cancels on click", () => {
    const store = mountComponent(makeStreamingStore(true));

    const stopButton = document.querySelector(".action.stop") as HTMLButtonElement;
    expect(stopButton).not.toBeNull();

    flushSync(() => {
      stopButton.click();
    });

    expect(store.cancelTurn).toHaveBeenCalledOnce();
  });
});
