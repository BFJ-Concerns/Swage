// Client-side URL router for conversation navigation.
//
// Manages browser history and maps URLs to active conversation state.
// This is a minimal SPA router — no framework dependency needed.

// Settings sub-pages reachable from the overlay nav. The first three are wired
// to real backends (providers, generation presets, automation). The remainder
// are rendered as non-functional shells until backends exist — see the
// SettingsPage motes for the gap on each page.
export type SettingsTab =
  | "providers"
  | "generation"
  | "automation"
  | "plugins"
  | "channels"
  | "devices"
  | "voice"
  | "recovery"
  | "display"
  | "account";

export type SpaceSettingsTab = "overview" | "workspaces" | "sessions";

export interface Route {
  path: string;
  conversationId: string | null;
  settingsTab: SettingsTab | null;
  settingsSpaceSlug: string | null;
  settingsSpaceTab: SpaceSettingsTab | null;
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
  onRouteChange?: (route: Route) => void;
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
    return {
      path: pathname,
      conversationId: match[1],
      settingsTab: null,
      settingsSpaceSlug: null,
      settingsSpaceTab: null,
      notFound: false,
    };
  }
  const spaceSettingsMatch = pathname.match(
    /^\/settings\/space\/([^/]+)(?:\/(overview|workspaces|sessions))?$/,
  );
  if (spaceSettingsMatch) {
    return {
      path: pathname,
      conversationId: null,
      settingsTab: null,
      settingsSpaceSlug: decodeURIComponent(spaceSettingsMatch[1]),
      settingsSpaceTab: (spaceSettingsMatch[2] as SpaceSettingsTab | undefined) ?? "overview",
      notFound: false,
    };
  }
  const settingsMatch = pathname.match(
    /^\/settings(?:\/(providers|generation|automation|plugins|channels|devices|voice|recovery|display|account))?$/,
  );
  if (settingsMatch) {
    return {
      path: pathname,
      conversationId: null,
      settingsTab: (settingsMatch[1] as SettingsTab | undefined) ?? "providers",
      settingsSpaceSlug: null,
      settingsSpaceTab: null,
      notFound: false,
    };
  }
  return {
    path: "/",
    conversationId: null,
    settingsTab: null,
    settingsSpaceSlug: null,
    settingsSpaceTab: null,
    notFound: false,
  };
}

// ---------------------------------------------------------------------------
// Router factory
// ---------------------------------------------------------------------------

export function createRouter(options?: RouterOptions): Router {
  // Seed from the current URL so the initial state is always consistent.
  let currentRoute: Route = parseRoute(window.location.pathname);

  function updateRoute(route: Route): void {
    currentRoute = route;
    options?.onRouteChange?.(route);
  }

  // Called on every URL change (pushState + popstate).
  async function applyLocation(): Promise<void> {
    const route = parseRoute(window.location.pathname);
    updateRoute(route);

    if (route.conversationId && options?.conversationStore) {
      try {
        await options.conversationStore.selectConversation(route.conversationId);
      } catch (error) {
        if (error instanceof Error && error.message.startsWith("HTTP 404 ")) {
          // The ID in the URL is stale (conversation deleted or never existed).
          // Fall back to the root and flag the not-found state so the UI
          // can show an appropriate notice.
          updateRoute({
            path: "/",
            conversationId: null,
            settingsTab: null,
            settingsSpaceSlug: null,
            settingsSpaceTab: null,
            notFound: true,
          });
          window.history.replaceState(null, "", "/");
        } else {
          throw error;
        }
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
      updateRoute(parseRoute(path));
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
