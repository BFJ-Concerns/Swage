// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";

import MessageInput from "../components/MessageInput.svelte";
import type { StreamingStore } from "../stores/streaming";

type MountedComponent = ReturnType<typeof mount>;

function makeStreamingStore(isStreaming = false): StreamingStore {
  return {
    get isStreaming() {
      return isStreaming;
    },
    get streamedContent() {
      return "";
    },
    get turnError() {
      return null;
    },
    get activeToolCalls() {
      return [];
    },
    handleServerEvent: vi.fn(),
    startTurn: vi.fn(),
    cancelTurn: vi.fn(),
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

  function mountComponent(store?: StreamingStore): StreamingStore {
    const resolvedStore = store ?? makeStreamingStore();
    component = mount(MessageInput, {
      target: document.body,
      props: { store: resolvedStore },
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

  it("trims whitespace before sending", () => {
    const store = mountComponent();

    setTextareaValue("   Hello, Swage   ");

    flushSync(() => {
      (document.querySelector(".send-btn") as HTMLButtonElement).click();
    });

    expect(store.startTurn).toHaveBeenCalledWith("Hello, Swage");
  });

  it("does not send whitespace-only input", () => {
    const store = mountComponent();

    setTextareaValue("   \n  ");

    flushSync(() => {
      (document.querySelector(".send-btn") as HTMLButtonElement).click();
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

    expect(store.startTurn).toHaveBeenCalledWith("Send via keyboard");
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

  it("clears the draft and re-focuses the textarea after send", () => {
    mountComponent();

    setTextareaValue("Refocus me");

    flushSync(() => {
      (document.querySelector(".send-btn") as HTMLButtonElement).click();
    });

    const input = textarea();

    expect(input.value).toBe("");
    expect(document.activeElement).toBe(input);
  });

  it("renders a stop button while streaming and cancels on click", () => {
    const store = mountComponent(makeStreamingStore(true));

    const stopButton = document.querySelector(".stop-btn") as HTMLButtonElement;
    expect(stopButton).not.toBeNull();

    flushSync(() => {
      stopButton.click();
    });

    expect(store.cancelTurn).toHaveBeenCalledOnce();
  });
});
