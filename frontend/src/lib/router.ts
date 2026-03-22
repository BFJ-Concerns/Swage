// Client-side URL router for conversation navigation.
//
// Manages browser history and maps URLs to active conversation state.
// This is a minimal SPA router — no framework dependency needed.

export interface Route {
  path: string;
  conversationId: string | null;
  // Set when a conversation ID from the URL was not found in the gateway.
  // The route falls back to "/" and the UI shows a "not found" notice.
  notFound: boolean;
}

export interface Router {
  readonly currentRoute: Route;
  navigate(path: string): void;
  // Parse the initial URL, attach popstate listener, and select any
  // conversation referenced by the URL. Returns a Promise so callers
  // (App.svelte) can await initial selection before rendering.
  init(): Promise<void>;
}

export interface RouterOptions {
  // Injected at wiring time (App.svelte). Optional so the router can be
  // constructed and tested in isolation without a live store.
  conversationStore?: {
    selectConversation(id: string): Promise<void>;
  };
}

// ---------------------------------------------------------------------------
// URL parsing
// ---------------------------------------------------------------------------

/**
 * Derive a Route from a URL pathname.
 *
 * Recognised patterns:
 *   /                       → root, no active conversation
 *   /conversations/:id      → load conversation with that ID
 *   anything else           → treated as root (unrecognised paths get SPA fallback)
 */
export function parseRoute(pathname: string): Route {
  const match = pathname.match(/^\/conversations\/([^/]+)$/);
  if (match) {
    return { path: pathname, conversationId: match[1], notFound: false };
  }
  return { path: "/", conversationId: null, notFound: false };
}

// ---------------------------------------------------------------------------
// Router factory
// ---------------------------------------------------------------------------

export function createRouter(options?: RouterOptions): Router {
  // Seed from the current URL so the initial state is always consistent.
  let currentRoute: Route = parseRoute(window.location.pathname);

  // Called on every URL change (pushState + popstate).
  async function applyLocation(): Promise<void> {
    const route = parseRoute(window.location.pathname);
    currentRoute = route;

    if (route.conversationId && options?.conversationStore) {
      try {
        await options.conversationStore.selectConversation(route.conversationId);
      } catch {
        // The ID in the URL is stale (conversation deleted or never existed).
        // Fall back to the root and flag the not-found state so the UI
        // can show an appropriate notice.
        currentRoute = { path: "/", conversationId: null, notFound: true };
        window.history.replaceState(null, "", "/");
      }
    }
  }

  return {
    get currentRoute() {
      return currentRoute;
    },

    navigate(path: string): void {
      window.history.pushState(null, "", path);
      // navigate() is always intentional — no async store call needed here.
      // The calling code (e.g. selectConversation in conversation list) has
      // already updated the store; we just sync the URL.
      currentRoute = parseRoute(path);
    },

    async init(): Promise<void> {
      // Listen for browser back/forward navigation.
      window.addEventListener("popstate", () => {
        // popstate is synchronous in terms of the URL update; async selection
        // runs in the background and updates currentRoute when it settles.
        applyLocation();
      });

      // Apply the initial URL — this kicks off conversation selection if
      // the URL contains an ID (e.g. on page refresh or direct navigation).
      await applyLocation();
    },
  };
}
