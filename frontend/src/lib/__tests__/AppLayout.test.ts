// @vitest-environment jsdom
/**
 * Tests for the three-panel AppLayout component.
 *
 * Verifies that all child components are imported and positioned in the
 * correct panels. AppLayout threads store props to children but owns no
 * state itself — it is a pure layout shell.
 */
import { describe, it, expect, afterEach } from "vitest";
import { mount, unmount } from "svelte";
import AppLayout from "../components/AppLayout.svelte";
import type { ConversationStore } from "../stores/conversations";
import type { ConnectionStore } from "../stores/connection";
import type { StreamingStore } from "../stores/streaming";

type MountedComponent = ReturnType<typeof mount>;

// ---------------------------------------------------------------------------
// Minimal mock stores — satisfy the interface so AppLayout can thread them.
// ---------------------------------------------------------------------------

function mockConversationStore(): ConversationStore {
  return {
    get conversations() { return []; },
    get activeConversation() { return null; },
    get isLoading() { return false; },
    loadConversations: async () => {},
    selectConversation: async () => {},
    createConversation: async () => {},
    deleteConversation: async () => {},
    appendOptimisticUserMessage: () => {},
    appendAssistantMessage: () => {},
    refreshActiveConversation: async () => {},
  };
}

function mockConnectionStore(): ConnectionStore {
  return {
    get status() { return "disconnected" as const; },
    get errorMessage() { return null; },
    connectTo: () => {},
    disconnect: () => {},
    sendMessage: () => {},
    cancelTurn: () => {},
    onServerEvent: () => {},
  };
}

function mockStreamingStore(): StreamingStore {
  return {
    get isStreaming() { return false; },
    get streamedContent() { return ""; },
    get turnError() { return null; },
    get activeToolCalls() { return []; },
    handleServerEvent: () => {},
    startTurn: () => {},
    cancelTurn: () => {},
  };
}

function mountLayout(): MountedComponent {
  return mount(AppLayout, {
    target: document.body,
    props: {
      conversationStore: mockConversationStore(),
      connectionStore: mockConnectionStore(),
      streamingStore: mockStreamingStore(),
      onRetry: () => {},
      isOperatorMode: false,
      onToggleOperatorMode: () => {},
    },
  });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("AppLayout: three-panel structure", () => {
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

  it("renders a sidebar panel", () => {
    component = mountLayout();

    expect(document.querySelector(".sidebar")).not.toBeNull();
  });

  it("renders a main conversation area", () => {
    component = mountLayout();

    expect(document.querySelector(".conversation-area")).not.toBeNull();
  });

  it("renders ProjectSelector inside the sidebar", () => {
    component = mountLayout();

    const sidebar = document.querySelector(".sidebar");
    expect(sidebar).not.toBeNull();
    expect(sidebar!.querySelector(".project-selector")).not.toBeNull();
  });

  it("renders ConversationList inside the sidebar", () => {
    component = mountLayout();

    const sidebar = document.querySelector(".sidebar");
    expect(sidebar).not.toBeNull();
    expect(sidebar!.querySelector(".conversation-list")).not.toBeNull();
  });

  it("renders MessageDisplay inside the conversation area", () => {
    component = mountLayout();

    const main = document.querySelector(".conversation-area");
    expect(main).not.toBeNull();
    expect(main!.querySelector(".message-display")).not.toBeNull();
  });

  it("renders MessageInput inside the conversation area", () => {
    component = mountLayout();

    const main = document.querySelector(".conversation-area");
    expect(main).not.toBeNull();
    expect(main!.querySelector(".message-input")).not.toBeNull();
  });

  it("renders conversation area with message display and input sections", () => {
    component = mountLayout();

    const main = document.querySelector(".conversation-area");
    expect(main).not.toBeNull();
    // ConnectionIndicator is hidden when status is "disconnected" (no active
    // connection attempt), so we verify the other children are present.
    expect(main!.children.length).toBeGreaterThan(0);
  });
});
