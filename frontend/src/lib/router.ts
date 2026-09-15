// Client-side URL router for conversation navigation.
//
// Manages browser history and maps URLs to active conversation state.
// This is a minimal SPA router — no framework dependency needed.

// Settings sub-pages reachable from the overlay nav. Providers, agents,
// generation presets, automation, recovery, and display are wired; the
// remainder are rendered as non-functional shells until backends exist.
export type SettingsTab =
  | "providers"
  | "agents"
  | "commands"
  | "skills"
  | "generation"
  | "automation"
  | "plugins"
  | "channels"
  | "devices"
  | "voice"
  | "recovery"
  | "display"
  | "account";

export type TopicSettingsTab = "overview" | "workspaces" | "sessions";

export interface Route {
  path: string;
  conversationId: string | null;
  // Set when the URL names a workflow run, so the run's recorded journal
  // and terminal account can be read back on its own surface.
  workflowRunId: string | null;
  settingsTab: SettingsTab | null;
  settingsTopicSlug: string | null;
  settingsTopicTab: TopicSettingsTab | null;
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
    readonly activeConversationId: string | null;
    checkConversationExists(id: string): Promise<void>;
    clearActiveConversation(): void;
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
 *   /workflow-runs/:id      → read back that run's record and terminal account
 *   anything else           → treated as root (unrecognised paths get SPA fallback)
 */
export function parseRoute(pathname: string): Route {
  const match = pathname.match(/^\/conversations\/([^/]+)$/);
  if (match) {
    return {
      path: pathname,
      conversationId: match[1],
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      notFound: false,
    };
  }
  const workflowRunMatch = pathname.match(/^\/workflow-runs\/([^/]+)$/);
  if (workflowRunMatch) {
    return {
      path: pathname,
      conversationId: null,
      workflowRunId: decodeURIComponent(workflowRunMatch[1]),
      settingsTab: null,
      settingsTopicSlug: null,
      settingsTopicTab: null,
      notFound: false,
    };
  }
  const topicSettingsMatch = pathname.match(
    /^\/settings\/topic\/([^/]+)(?:\/(overview|workspaces|sessions))?$/,
  );
  if (topicSettingsMatch) {
    return {
      path: pathname,
      conversationId: null,
      workflowRunId: null,
      settingsTab: null,
      settingsTopicSlug: decodeURIComponent(topicSettingsMatch[1]),
      settingsTopicTab: (topicSettingsMatch[2] as TopicSettingsTab | undefined) ?? "overview",
      notFound: false,
    };
  }
  const settingsMatch = pathname.match(
    /^\/settings(?:\/(providers|agents|commands|skills|generation|automation|plugins|channels|devices|voice|recovery|display|account))?$/,
  );
  if (settingsMatch) {
    return {
      path: pathname,
      conversationId: null,
      workflowRunId: null,
      settingsTab: (settingsMatch[1] as SettingsTab | undefined) ?? "providers",
      settingsTopicSlug: null,
      settingsTopicTab: null,
      notFound: false,
    };
  }
  return {
    path: "/",
    conversationId: null,
    workflowRunId: null,
    settingsTab: null,
    settingsTopicSlug: null,
    settingsTopicTab: null,
    notFound: false,
  };
}

// ---------------------------------------------------------------------------
// Router factory
// ---------------------------------------------------------------------------

export function createRouter(options?: RouterOptions): Router {
  // Seed from the current URL so the initial state is always consistent.
  let currentRoute: Route = parseRoute(window.location.pathname);
  let navigationGeneration = 0;

  function updateRoute(route: Route): void {
    currentRoute = route;
    options?.onRouteChange?.(route);
  }

  // Called on every URL change (pushState + popstate).
  async function applyLocation(generation: number): Promise<void> {
    const route = parseRoute(window.location.pathname);
    updateRoute(route);

    if (route.conversationId && options?.conversationStore) {
      const isActiveConversation =
        route.conversationId === options.conversationStore.activeConversationId;
      try {
        if (isActiveConversation) {
          await options.conversationStore.checkConversationExists(route.conversationId);
        } else {
          await options.conversationStore.selectConversation(route.conversationId);
        }
      } catch (error) {
        if (generation !== navigationGeneration) {
          return;
        }
        if (error instanceof Error && error.message.startsWith("HTTP 404 ")) {
          if (isActiveConversation) {
            options.conversationStore.clearActiveConversation();
          }
          // The ID in the URL is stale (conversation deleted or never existed).
          // Fall back to the root and flag the not-found state so the UI
          // can show an appropriate notice.
          updateRoute({
            path: "/",
            conversationId: null,
            workflowRunId: null,
            settingsTab: null,
            settingsTopicSlug: null,
            settingsTopicTab: null,
            notFound: true,
          });
          window.history.replaceState(null, "", "/");
        } else if (isActiveConversation) {
          // A transient existence-check failure must not evict a working
          // conversation or reject from the asynchronous popstate listener.
          return;
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
      navigationGeneration += 1;
      if (window.location.pathname !== path) {
        window.history.pushState(null, "", path);
      }
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
        const generation = ++navigationGeneration;
        void applyLocation(generation).catch(() => {
          if (generation !== navigationGeneration) {
            return;
          }
          const activeConversationId = options?.conversationStore?.activeConversationId;
          const fallbackPath = activeConversationId
            ? `/conversations/${activeConversationId}`
            : "/";
          window.history.replaceState(null, "", fallbackPath);
          updateRoute(parseRoute(fallbackPath));
        });
      });

      // Apply the initial URL — this kicks off conversation selection if
      // the URL contains an ID (e.g. on page refresh or direct navigation).
      await applyLocation(++navigationGeneration);
    },
  };
}
