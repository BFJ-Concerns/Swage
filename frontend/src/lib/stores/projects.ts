// Project store — owns the project list, current selection, and CRUD state.
//
// Components read from this store and emit events that trigger updates.
// The store calls the HTTP API client to persist changes and fetch data.

import type { Project } from "../types";
import * as api from "../api/projects";

export interface ProjectStore {
  readonly projects: Project[];
  readonly currentProject: Project | null;
  readonly isLoading: boolean;
  loadProjects(): Promise<void>;
  selectProject(id: string): Promise<void>;
  createProject(name: string, workspacePath: string): Promise<void>;
  deleteProject(id: string): Promise<void>;
}

export function createProjectStore(): ProjectStore {
  // Per-instance state — each call creates a fresh, isolated store.
  // Components read these through the getter properties below;
  // Svelte's fine-grained reactivity tracks property reads automatically.
  let projects: Project[] = [];
  let currentProject: Project | null = null;
  let isLoading = false;

  return {
    // ---------------------------------------------------------------------------
    // Reactive state (read-only to callers via readonly interface)
    // ---------------------------------------------------------------------------

    get projects() {
      return projects;
    },

    get currentProject() {
      return currentProject;
    },

    get isLoading() {
      return isLoading;
    },

    // ---------------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------------

    async loadProjects() {
      isLoading = true;
      try {
        projects = await api.listProjects();

        // Auto-select the default project on load. The default project is
        // identified by name so it works regardless of its generated ID.
        const defaultProject = projects.find((p) => p.is_default);
        if (defaultProject) {
          currentProject = defaultProject;
        }
      } finally {
        // Always clear loading — even on error, so the UI isn't stuck.
        isLoading = false;
      }
    },

    async selectProject(id) {
      const found = projects.find((p) => p.id === id);
      if (!found) {
        throw new Error(`Project not found: ${id}`);
      }
      // Selection is purely local — no extra round-trip needed since we
      // already have the full project data from loadProjects.
      currentProject = found;
    },

    async createProject(name, workspacePath) {
      const created = await api.createProject({
        name,
        workspace_path: workspacePath,
      });

      projects = [...projects, created];
      currentProject = created;
    },

    async deleteProject(id) {
      const target = projects.find((p) => p.id === id);

      // Guard: the default project is the fallback for all conversations
      // and cannot be removed — doing so would leave the system in an
      // inconsistent state.
      if (target?.is_default) {
        throw new Error("Cannot delete the default project");
      }

      await api.deleteProject(id);

      const remaining = projects.filter((p) => p.id !== id);
      projects = remaining;

      // If the deleted project was current, move focus to the next one.
      if (currentProject?.id === id) {
        currentProject = remaining.length > 0 ? remaining[0] : null;
      }
    },
  };
}
