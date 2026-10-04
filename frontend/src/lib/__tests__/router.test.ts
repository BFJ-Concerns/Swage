/**
 * Tests for the client-side URL router.
 *
 * The router runs in jsdom so window.location, window.history, and popstate
 * are available. Each test resets location to "/" to prevent state leaking.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

import { ApiError } from "../api/error";
import { createRouter, parseRoute } from "../router";

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

beforeEach(() => {
  // Reset URL to a clean state before each test.
  window.history.replaceState(null, "", "/");
  // Clear all event listeners by replacing the window's popstate behaviour.
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// parseRoute (pure function — no browser state needed)
// ---------------------------------------------------------------------------

describe("parseRoute", () => {
  it("maps '/' to a root route with no conversation ID", () => {
    expect(parseRoute("/")).toEqual({
      path: "/",
      conversationId: null,
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("maps '/conversations/:id' to a route with that conversation ID", () => {
    expect(parseRoute("/conversations/abc-123")).toEqual({
      path: "/conversations/abc-123",
      conversationId: "abc-123",
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("maps '/workflow-runs/:id' to that run's own surface", () => {
    expect(parseRoute("/workflow-runs/run-42")).toEqual({
      path: "/workflow-runs/run-42",
      conversationId: null,
      surfaceId: null,
      library: false,
      workflowRunId: "run-42",
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("maps an unrecognised path to the root route", () => {
    expect(parseRoute("/other/path")).toEqual({
      path: "/",
      conversationId: null,
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("maps '' (empty string) to the root route", () => {
    expect(parseRoute("")).toEqual({
      path: "/",
      conversationId: null,
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("maps '/workflow-runs/:id' to a route with that run ID", () => {
    expect(parseRoute("/workflow-runs/run-9")).toEqual({
      path: "/workflow-runs/run-9",
      conversationId: null,
      surfaceId: null,
      library: false,
      workflowRunId: "run-9",
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("maps settings routes to their active tab", () => {
    expect(parseRoute("/settings/generation")).toEqual({
      path: "/settings/generation",
      conversationId: null,
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: "generation",
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("maps the agent definitions settings route", () => {
    expect(parseRoute("/settings/agents").settingsTab).toBe("agents");
  });

  it("maps the memory settings route and the page its notice link names", () => {
    expect(parseRoute("/settings/memory")).toMatchObject({ settingsTab: "memory", settingsMemoryPage: null });
    expect(parseRoute("/settings/memory?reference=general%2Fpeople%2Falice")).toMatchObject({
      path: "/settings/memory",
      settingsTab: "memory",
      settingsMemoryPage: { scope: null, reference: "general/people/alice" },
    });
    expect(parseRoute("/settings/memory?scope=topic-1&reference=topic%2Fconventions").settingsMemoryPage).toEqual({
      scope: "topic-1",
      reference: "topic/conventions",
    });
    expect(parseRoute("/settings/agents?reference=general%2Fprofile").settingsMemoryPage).toBeNull();
  });

  it("maps command and skill settings routes", () => {
    expect(parseRoute("/settings/commands").settingsTab).toBe("commands");
    expect(parseRoute("/settings/skills").settingsTab).toBe("skills");
  });

  it("maps topic settings routes to their topic slug and tab", () => {
    expect(parseRoute("/settings/topic/research/workspaces")).toEqual({
      path: "/settings/topic/research/workspaces",
      conversationId: null,
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: "research",
      settingsTopicTab: "workspaces",
      settingsMemoryPage: null,
      notFound: false,
    });
    expect(parseRoute("/settings/topic/research/surfaces")).toEqual({
      path: "/settings/topic/research/surfaces",
      conversationId: null,
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: "research",
      settingsTopicTab: "surfaces",
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("handles UUID-style conversation IDs", () => {
    const id = "00000000-0000-0000-0000-000000000001";
    const route = parseRoute(`/conversations/${id}`);
    expect(route.conversationId).toBe(id);
  });
});

// ---------------------------------------------------------------------------
// createRouter — initial state
// ---------------------------------------------------------------------------

describe("createRouter initial state", () => {
  it("initialises currentRoute from window.location.pathname", () => {
    window.history.replaceState(null, "", "/conversations/init-test");
    const router = createRouter();
    expect(router.currentRoute.conversationId).toBe("init-test");
  });

  it("opens the memory page a notice link names when the app loads at it", () => {
    window.history.replaceState(null, "", "/settings/memory?reference=general%2Fpeople%2Falice");
    const router = createRouter();
    expect(router.currentRoute.settingsTab).toBe("memory");
    expect(router.currentRoute.settingsMemoryPage).toEqual({ scope: null, reference: "general/people/alice" });
  });

  it("starts at root when URL is '/'", () => {
    const router = createRouter();
    expect(router.currentRoute.path).toBe("/");
    expect(router.currentRoute.conversationId).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// navigate
// ---------------------------------------------------------------------------

describe("navigate", () => {
  it("notifies its consumer with the new route", () => {
    const onRouteChange = vi.fn();
    const router = createRouter({ onRouteChange });

    router.navigate("/settings/providers");

    expect(onRouteChange).toHaveBeenCalledOnce();
    expect(onRouteChange).toHaveBeenCalledWith(router.currentRoute);
  });

  it("updates currentRoute to the new path", () => {
    const router = createRouter();
    router.navigate("/conversations/nav-test");
    expect(router.currentRoute.conversationId).toBe("nav-test");
  });

  it("pushes an entry to browser history", () => {
    const router = createRouter();
    const before = window.history.length;
    router.navigate("/conversations/hist-test");
    expect(window.history.length).toBe(before + 1);
  });

  it("does not push the current path but still updates currentRoute", () => {
    const router = createRouter();
    router.navigate("/conversations/previous");
    window.history.replaceState(null, "", "/");
    const before = window.history.length;

    router.navigate("/");

    expect(window.history.length).toBe(before);
    expect(router.currentRoute.path).toBe("/");
    expect(router.currentRoute.conversationId).toBeNull();
  });

  it("updates currentRoute to root when navigating to '/'", () => {
    const router = createRouter();
    router.navigate("/conversations/some-id");
    router.navigate("/");
    expect(router.currentRoute.conversationId).toBeNull();
    expect(router.currentRoute.path).toBe("/");
  });
});

// ---------------------------------------------------------------------------
// popstate (back/forward navigation)
// ---------------------------------------------------------------------------

describe("popstate", () => {
  it.each([
    ["a missing conversation", new ApiError(404, "not_found", "conversation x not found")],
    ["an id the gateway cannot parse", new ApiError(422, "validation_error", "Invalid URL")],
  ])("falls back to the root with a notice for %s", async (_label, failure) => {
    window.history.replaceState(null, "", "/conversations/not-an-id");
    const router = createRouter({
      conversationStore: {
        activeConversationId: null,
        checkConversationExists: async () => {},
        clearActiveConversation: vi.fn(),
        selectConversation: async () => {
          throw failure;
        },
      },
    });

    await router.init();

    expect(router.currentRoute.notFound).toBe(true);
    expect(router.currentRoute.path).toBe("/");
    expect(window.location.pathname).toBe("/");
  });

  it("ignores a stale 404 from an earlier non-active selection after newer navigation", async () => {
    window.history.replaceState(null, "", "/conversations/active-conv");
    let popstateHandler: EventListener | null = null;
    vi.spyOn(window, "addEventListener").mockImplementation((type, listener) => {
      if (type === "popstate") {
        popstateHandler =
          typeof listener === "function"
            ? listener
            : (event) => listener.handleEvent(event);
      }
    });
    let rejectEarlierSelection: (error: Error) => void = () => {
      throw new Error("earlier selection was not started");
    };
    let resolveNewerSelection: () => void = () => {
      throw new Error("newer selection was not started");
    };
    const selectConversation = vi.fn((id: string) => {
      if (id === "earlier-conv") {
        return new Promise<void>((_resolve, reject) => {
          rejectEarlierSelection = reject;
        });
      }
      return new Promise<void>((resolve) => {
        resolveNewerSelection = resolve;
      });
    });
    const router = createRouter({
      conversationStore: {
        activeConversationId: "active-conv",
        checkConversationExists: vi.fn().mockResolvedValue(undefined),
        clearActiveConversation: vi.fn(),
        selectConversation,
      },
    });
    await router.init();
    expect(popstateHandler).not.toBeNull();

    window.history.replaceState(null, "", "/conversations/earlier-conv");
    popstateHandler!(new PopStateEvent("popstate"));
    await vi.waitFor(() => expect(selectConversation).toHaveBeenCalledWith("earlier-conv"));
    window.history.replaceState(null, "", "/conversations/newer-conv");
    popstateHandler!(new PopStateEvent("popstate"));
    await vi.waitFor(() => expect(selectConversation).toHaveBeenCalledWith("newer-conv"));

    rejectEarlierSelection(
      new Error("HTTP 404 from GET /api/conversations/earlier-conv"),
    );
    await Promise.resolve();

    expect(window.location.pathname).toBe("/conversations/newer-conv");
    expect(router.currentRoute.conversationId).toBe("newer-conv");
    expect(router.currentRoute.notFound).toBe(false);

    resolveNewerSelection();
  });

  it("restores the active route when selecting browser history fails", async () => {
    window.history.replaceState(null, "", "/conversations/active-conv");
    const onRouteChange = vi.fn();
    const router = createRouter({
      onRouteChange,
      conversationStore: {
        activeConversationId: "active-conv",
        checkConversationExists: vi.fn().mockResolvedValue(undefined),
        clearActiveConversation: vi.fn(),
        selectConversation: vi.fn().mockRejectedValue(new Error("HTTP 503 gateway unavailable")),
      },
    });
    await router.init();
    onRouteChange.mockClear();

    window.history.replaceState(null, "", "/conversations/unavailable-conv");
    window.dispatchEvent(new PopStateEvent("popstate"));
    await vi.waitFor(() =>
      expect(window.location.pathname).toBe("/conversations/active-conv"),
    );

    expect(router.currentRoute.conversationId).toBe("active-conv");
    expect(onRouteChange).toHaveBeenLastCalledWith(router.currentRoute);
  });

  it("notifies its consumer after browser navigation", async () => {
    const onRouteChange = vi.fn();
    const router = createRouter({ onRouteChange });
    await router.init();
    onRouteChange.mockClear();

    window.history.replaceState(null, "", "/conversations/restored");
    window.dispatchEvent(new PopStateEvent("popstate"));

    expect(onRouteChange).toHaveBeenCalledOnce();
    expect(onRouteChange).toHaveBeenCalledWith(router.currentRoute);
  });

  it("updates currentRoute when the browser navigates back", () => {
    const router = createRouter();
    router.init();

    // Push a new entry, then simulate the browser going back.
    router.navigate("/conversations/forward");
    expect(router.currentRoute.conversationId).toBe("forward");

    // Simulate the browser restoring the previous URL ("/").
    window.history.replaceState(null, "", "/");
    window.dispatchEvent(new PopStateEvent("popstate"));

    expect(router.currentRoute.conversationId).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// init — conversation selection
// ---------------------------------------------------------------------------

describe("init with conversation store", () => {
  it("does not clear an active conversation when its stale check fails during route selection", async () => {
    window.history.replaceState(null, "", "/conversations/active-conv");
    let popstateHandler: EventListener | null = null;
    vi.spyOn(window, "addEventListener").mockImplementation((type, listener) => {
      if (type === "popstate") {
        popstateHandler =
          typeof listener === "function"
            ? listener
            : (event) => listener.handleEvent(event);
      }
    });
    let rejectExistenceCheck: (error: Error) => void = () => {
      throw new Error("existence check was not started");
    };
    let resolveSelection: () => void = () => {
      throw new Error("selection was not started");
    };
    const clearActiveConversation = vi.fn();
    const selectConversation = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveSelection = resolve;
        }),
    );
    const router = createRouter({
      conversationStore: {
        activeConversationId: "active-conv",
        checkConversationExists: vi.fn(
          () =>
            new Promise<void>((_resolve, reject) => {
              rejectExistenceCheck = reject;
            }),
        ),
        clearActiveConversation,
        selectConversation,
      },
    });

    const initialisation = router.init();
    expect(popstateHandler).not.toBeNull();
    window.history.replaceState(null, "", "/conversations/new-conv");
    popstateHandler!(new PopStateEvent("popstate"));
    await vi.waitFor(() => expect(selectConversation).toHaveBeenCalledWith("new-conv"));
    rejectExistenceCheck(new Error("HTTP 404 from GET /api/conversations/active-conv"));
    await initialisation;

    expect(clearActiveConversation).not.toHaveBeenCalled();
    expect(window.location.pathname).toBe("/conversations/new-conv");
    expect(router.currentRoute).toEqual({
      path: "/conversations/new-conv",
      conversationId: "new-conv",
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });

    resolveSelection();
  });

  it("does not clear a newer selection when an existence check reports 404", async () => {
    window.history.replaceState(null, "", "/conversations/first-conv");
    let rejectExistenceCheck: (error: Error) => void = () => {
      throw new Error("existence check was not started");
    };
    const checkConversationExists = vi.fn(
      () =>
        new Promise<void>((_resolve, reject) => {
          rejectExistenceCheck = reject;
        }),
    );
    const clearActiveConversation = vi.fn();
    const conversationStore = {
      activeConversationId: "first-conv" as string | null,
      checkConversationExists,
      clearActiveConversation,
      selectConversation: vi.fn().mockResolvedValue(undefined),
    };
    const router = createRouter({ conversationStore });

    const initialisation = router.init();
    await vi.waitFor(() => expect(checkConversationExists).toHaveBeenCalledOnce());
    conversationStore.activeConversationId = "second-conv";
    router.navigate("/conversations/second-conv");
    rejectExistenceCheck(
      new Error("HTTP 404 from GET /api/conversations/first-conv"),
    );
    await initialisation;

    expect(clearActiveConversation).not.toHaveBeenCalled();
    expect(window.location.pathname).toBe("/conversations/second-conv");
    expect(router.currentRoute).toEqual({
      path: "/conversations/second-conv",
      conversationId: "second-conv",
      surfaceId: null,
      library: false,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      settingsMemoryPage: null,
      notFound: false,
    });
  });

  it("calls selectConversation when URL contains a conversation ID", async () => {
    window.history.replaceState(null, "", "/conversations/init-conv");
    const checkConversationExists = vi.fn().mockResolvedValue(undefined);
    const clearActiveConversation = vi.fn();
    const selectConversation = vi.fn().mockResolvedValue(undefined);
    const router = createRouter({
      conversationStore: {
        activeConversationId: null,
        checkConversationExists,
        clearActiveConversation,
        selectConversation,
      },
    });

    await router.init();

    expect(selectConversation).toHaveBeenCalledWith("init-conv");
  });

  it("does not call selectConversation when URL is '/'", async () => {
    const checkConversationExists = vi.fn().mockResolvedValue(undefined);
    const clearActiveConversation = vi.fn();
    const selectConversation = vi.fn().mockResolvedValue(undefined);
    const router = createRouter({
      conversationStore: {
        activeConversationId: null,
        checkConversationExists,
        clearActiveConversation,
        selectConversation,
      },
    });

    await router.init();

    expect(selectConversation).not.toHaveBeenCalled();
  });

  it("does not select the conversation that is already active", async () => {
    window.history.replaceState(null, "", "/conversations/active-conv");
    const checkConversationExists = vi.fn().mockResolvedValue(undefined);
    const clearActiveConversation = vi.fn();
    const selectConversation = vi.fn().mockResolvedValue(undefined);
    const router = createRouter({
      conversationStore: {
        activeConversationId: "active-conv",
        checkConversationExists,
        clearActiveConversation,
        selectConversation,
      },
    });

    await router.init();

    expect(checkConversationExists).toHaveBeenCalledOnce();
    expect(checkConversationExists).toHaveBeenCalledWith("active-conv");
    expect(selectConversation).not.toHaveBeenCalled();
  });

  it("selects a routed conversation when a different one is active", async () => {
    window.history.replaceState(null, "", "/conversations/next-conv");
    const checkConversationExists = vi.fn().mockResolvedValue(undefined);
    const clearActiveConversation = vi.fn();
    const selectConversation = vi.fn().mockResolvedValue(undefined);
    const router = createRouter({
      conversationStore: {
        activeConversationId: "active-conv",
        checkConversationExists,
        clearActiveConversation,
        selectConversation,
      },
    });

    await router.init();

    expect(checkConversationExists).not.toHaveBeenCalled();
    expect(selectConversation).toHaveBeenCalledOnce();
    expect(selectConversation).toHaveBeenCalledWith("next-conv");
  });

  it("sets notFound and navigates to '/' when selectConversation rejects", async () => {
    window.history.replaceState(null, "", "/conversations/stale-id");
    const checkConversationExists = vi.fn().mockResolvedValue(undefined);
    const clearActiveConversation = vi.fn();
    const selectConversation = vi
      .fn()
      .mockRejectedValue(
        new Error("HTTP 404 from GET /api/conversations/stale-id"),
      );
    const router = createRouter({
      conversationStore: {
        activeConversationId: null,
        checkConversationExists,
        clearActiveConversation,
        selectConversation,
      },
    });

    await router.init();

    expect(router.currentRoute.notFound).toBe(true);
    expect(router.currentRoute.conversationId).toBeNull();
    expect(window.location.pathname).toBe("/");
  });

  it("clears an active conversation when its existence check returns 404", async () => {
    window.history.replaceState(null, "", "/conversations/deleted-id");
    const checkConversationExists = vi
      .fn()
      .mockRejectedValue(
        new Error("HTTP 404 from GET /api/conversations/deleted-id"),
      );
    const clearActiveConversation = vi.fn();
    const selectConversation = vi.fn().mockResolvedValue(undefined);
    const router = createRouter({
      conversationStore: {
        activeConversationId: "deleted-id",
        checkConversationExists,
        clearActiveConversation,
        selectConversation,
      },
    });

    await router.init();

    expect(clearActiveConversation).toHaveBeenCalledOnce();
    expect(router.currentRoute.notFound).toBe(true);
    expect(window.location.pathname).toBe("/");
  });

  it("keeps an active conversation when its existence check fails transiently", async () => {
    window.history.replaceState(null, "", "/conversations/active-id");
    const checkConversationExists = vi
      .fn()
      .mockRejectedValue(
        new Error("HTTP 503 from GET /api/conversations/active-id"),
      );
    const clearActiveConversation = vi.fn();
    const selectConversation = vi.fn().mockResolvedValue(undefined);
    const router = createRouter({
      conversationStore: {
        activeConversationId: "active-id",
        checkConversationExists,
        clearActiveConversation,
        selectConversation,
      },
    });

    await expect(router.init()).resolves.toBeUndefined();
    expect(clearActiveConversation).not.toHaveBeenCalled();
    expect(router.currentRoute.conversationId).toBe("active-id");
    expect(router.currentRoute.notFound).toBe(false);
  });

  it("rethrows non-404 failures during route restore", async () => {
    window.history.replaceState(null, "", "/conversations/outage-id");
    const checkConversationExists = vi.fn().mockResolvedValue(undefined);
    const clearActiveConversation = vi.fn();
    const selectConversation = vi
      .fn()
      .mockRejectedValue(
        new Error("HTTP 503 from GET /api/conversations/outage-id"),
      );
    const router = createRouter({
      conversationStore: {
        activeConversationId: null,
        checkConversationExists,
        clearActiveConversation,
        selectConversation,
      },
    });

    await expect(router.init()).rejects.toThrow("HTTP 503");
    expect(window.location.pathname).toBe("/conversations/outage-id");
    expect(router.currentRoute.conversationId).toBe("outage-id");
    expect(router.currentRoute.notFound).toBe(false);
  });
});
