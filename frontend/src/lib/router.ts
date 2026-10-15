// Client-side URL router for conversation navigation.
//
// Manages browser history and maps URLs to active conversation state.
// This is a minimal SPA router — no framework dependency needed.

import { ApiError } from "./api/error";

// Settings sub-surfaces reachable from the overlay nav. Providers, agents,
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
  | "memory"
  | "recovery"
  | "display"
  | "account";

export type TopicSettingsTab = "overview" | "workspaces" | "sessions" | "surfaces";

export interface Route {
  path: string;
  conversationId: string | null;
  // Set with `conversationId` when the URL names one of the session's
  // surfaces, so a surface is a place the address bar can point at.
  surfaceId: string | null;
  // Set when the URL is the shared surface library's catalogue.
  library: boolean;
  // Set when the URL names a workflow run, so the run's recorded journal
  // and terminal account can be read back on its own surface.
  workflowRunId: string | null;
  settingsTab: SettingsTab | null;
  settingsTopicSlug: string | null;
  settingsTopicTab: TopicSettingsTab | null;
  // Set with the memory settings tab when the URL's `?reference=` names the
  // memory page to open — the target of the curator's change notice — with
  // `?scope=` (`general` or a topic id) naming the scope that holds it.
  settingsMemoryPage: MemoryPageAddress | null;
  // Set when a conversation ID from the URL was not found in the gateway.
  // The route falls back to "/" and the UI shows a "not found" notice.
  notFound: boolean;
}

// The route every pattern starts from: the front door, nothing selected.
function rootRoute(pathname = "/", notFound = false): Route {
  return {
    path: pathname,
    conversationId: null,
    surfaceId: null,
    library: false,
    workflowRunId: null,
    settingsTab: null,
    settingsTopicSlug: null,
    settingsTopicTab: null,
    settingsMemoryPage: null,
    notFound,
  };
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
  onNavigationError?: (error: unknown) => void;
}

// ---------------------------------------------------------------------------
// URL parsing
// ---------------------------------------------------------------------------

/** A memory page as a notice link addresses it; a null scope opens the first scope listing it. */
export interface MemoryPageAddress {
  // `general`, a topic id, or null when the link names no scope.
  scope: string | null;
  reference: string;
}

function memoryPageAddress(query: URLSearchParams): MemoryPageAddress | null {
  const reference = query.get("reference");
  return reference ? { scope: query.get("scope"), reference } : null;
}

/**
 * Derive a Route from a URL path, optionally carrying a query string.
 *
 * Recognised patterns:
 *   /                                   → root, no active conversation
 *   /conversations/:id                  → load conversation with that ID
 *   /conversations/:id/surfaces/:sid    → that conversation with one of its surfaces open
 *   /library                            → the shared surface library's catalogue
 *   /workflow-runs/:id                  → read back that run's record and terminal account
 *   /settings/memory?reference=:ref     → the memory settings tab with that page open
 *   anything else                       → treated as root (unrecognised paths get SPA fallback)
 */
export function parseRoute(location: string): Route {
  const queryStart = location.indexOf("?");
  const pathname = queryStart === -1 ? location : location.slice(0, queryStart);
  const query = new URLSearchParams(queryStart === -1 ? "" : location.slice(queryStart + 1));
  const match = pathname.match(/^\/conversations\/([^/]+)(?:\/surfaces\/([^/]+))?$/);
  if (match) {
    return {
      ...rootRoute(pathname),
      conversationId: match[1],
      surfaceId: match[2] === undefined ? null : decodeURIComponent(match[2]),
    };
  }
  if (pathname === "/library") {
    return { ...rootRoute(pathname), library: true };
  }
  const workflowRunMatch = pathname.match(/^\/workflow-runs\/([^/]+)$/);
  if (workflowRunMatch) {
    return {
      ...rootRoute(pathname),
      workflowRunId: decodeURIComponent(workflowRunMatch[1]),
    };
  }
  const topicSettingsMatch = pathname.match(
    /^\/settings\/topic\/([^/]+)(?:\/(overview|workspaces|sessions|surfaces))?$/,
  );
  if (topicSettingsMatch) {
    return {
      ...rootRoute(pathname),
      settingsTopicSlug: decodeURIComponent(topicSettingsMatch[1]),
      settingsTopicTab: (topicSettingsMatch[2] as TopicSettingsTab | undefined) ?? "overview",
    };
  }
  const settingsMatch = pathname.match(
    /^\/settings(?:\/(providers|agents|commands|skills|generation|automation|plugins|channels|devices|voice|memory|recovery|display|account))?$/,
  );
  if (settingsMatch) {
    const settingsTab = (settingsMatch[1] as SettingsTab | undefined) ?? "providers";
    return {
      ...rootRoute(pathname),
      settingsTab,
      settingsMemoryPage: settingsTab === "memory" ? memoryPageAddress(query) : null,
    };
  }
  return rootRoute();
}

// A conversation the URL names is gone (404) or was never an id at all
// (422: the gateway could not parse it). Either way the link is stale. The
// gateway's contract error carries the status; an error outside the contract
// carries it in its message.
function isNotAConversation(error: Error): boolean {
  if (error instanceof ApiError) return error.status === 404 || error.status === 422;
  return error.message.startsWith("HTTP 404 ") || error.message.startsWith("HTTP 422 ");
}

/** The address of a conversation, or of one of its surfaces. */
export function conversationPath(conversationId: string, surfaceId?: string | null): string {
  const base = `/conversations/${encodeURIComponent(conversationId)}`;
  return surfaceId ? `${base}/surfaces/${encodeURIComponent(surfaceId)}` : base;
}

// ---------------------------------------------------------------------------
// Router factory
// ---------------------------------------------------------------------------

// The address bar's path and query — the part of the URL a route reads.
function currentLocation(): string {
  return window.location.pathname + window.location.search;
}

export function createRouter(options?: RouterOptions): Router {
  // Seed from the current URL so the initial state is always consistent.
  let currentRoute: Route = parseRoute(currentLocation());
  let navigationGeneration = 0;

  function updateRoute(route: Route): void {
    currentRoute = route;
    options?.onRouteChange?.(route);
  }

  // Called on every URL change (pushState + popstate).
  async function applyLocation(generation: number): Promise<void> {
    const route = parseRoute(currentLocation());
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
        if (error instanceof Error && isNotAConversation(error)) {
          if (isActiveConversation) {
            options.conversationStore.clearActiveConversation();
          }
          // The ID in the URL is stale (conversation deleted or never existed).
          // Fall back to the root and flag the not-found state so the UI
          // can show an appropriate notice.
          updateRoute(rootRoute("/", true));
          window.history.replaceState(null, "", "/");
        } else if (options?.onNavigationError) {
          options.onNavigationError(error);
          const fallback = options.conversationStore.activeConversationId;
          const fallbackPath = fallback ? conversationPath(fallback) : "/";
          window.history.replaceState(null, "", fallbackPath);
          updateRoute(parseRoute(fallbackPath));
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
      if (currentLocation() !== path) {
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
      // the URL contains an ID (e.g. on surface refresh or direct navigation).
      await applyLocation(++navigationGeneration);
    },
  };
}
