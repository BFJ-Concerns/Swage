/**
 * Tests for the client-side URL router.
 *
 * The router runs in jsdom so window.location, window.history, and popstate
 * are available. Each test resets location to "/" to prevent state leaking.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

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
      notFound: false,
    });
  });

  it("maps '/conversations/:id' to a route with that conversation ID", () => {
    expect(parseRoute("/conversations/abc-123")).toEqual({
      path: "/conversations/abc-123",
      conversationId: "abc-123",
      notFound: false,
    });
  });

  it("maps an unrecognised path to the root route", () => {
    expect(parseRoute("/other/path")).toEqual({
      path: "/",
      conversationId: null,
      notFound: false,
    });
  });

  it("maps '' (empty string) to the root route", () => {
    expect(parseRoute("")).toEqual({
      path: "/",
      conversationId: null,
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
  it("calls selectConversation when URL contains a conversation ID", async () => {
    window.history.replaceState(null, "", "/conversations/init-conv");
    const selectConversation = vi.fn().mockResolvedValue(undefined);
    const router = createRouter({ conversationStore: { selectConversation } });

    await router.init();

    expect(selectConversation).toHaveBeenCalledWith("init-conv");
  });

  it("does not call selectConversation when URL is '/'", async () => {
    const selectConversation = vi.fn().mockResolvedValue(undefined);
    const router = createRouter({ conversationStore: { selectConversation } });

    await router.init();

    expect(selectConversation).not.toHaveBeenCalled();
  });

  it("sets notFound and navigates to '/' when selectConversation rejects", async () => {
    window.history.replaceState(null, "", "/conversations/stale-id");
    const selectConversation = vi.fn().mockRejectedValue(new Error("not found"));
    const router = createRouter({ conversationStore: { selectConversation } });

    await router.init();

    expect(router.currentRoute.notFound).toBe(true);
    expect(router.currentRoute.conversationId).toBeNull();
    expect(window.location.pathname).toBe("/");
  });
});
