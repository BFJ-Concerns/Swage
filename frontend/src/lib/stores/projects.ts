// Project store — owns the project list, current selection, and CRUD state.
//
// Components read from this store and emit events that trigger updates.
// The store calls the HTTP API client to persist changes and fetch data.

import { fromStore, get, writable } from "svelte/store";

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
  // Back the custom API with real Svelte stores so project changes propagate
  // into mounted components after async actions complete.
  const projectsStore = writable<Project[]>([]);
  const currentProjectStore = writable<Project | null>(null);
  const isLoadingStore = writable(false);

  const projectsState = fromStore(projectsStore);
  const currentProjectState = fromStore(currentProjectStore);
  const isLoadingState = fromStore(isLoadingStore);

  return {
    // ---------------------------------------------------------------------------
    // Reactive state (read-only to callers via readonly interface)
    // ---------------------------------------------------------------------------

    get projects() {
      return projectsState.current;
    },

    get currentProject() {
      return currentProjectState.current;
    },

    get isLoading() {
      return isLoadingState.current;
    },

    // ---------------------------------------------------------------------------
    // Actions
    // ---------------------------------------------------------------------------

    async loadProjects() {
      isLoadingStore.set(true);
      try {
        const projects = await api.listProjects();
        projectsStore.set(projects);

        // Auto-select the default project on load. The default project is
        // identified by name so it works regardless of its generated ID.
        const defaultProject = projects.find((p) => p.is_default);
        if (defaultProject) {
          currentProjectStore.set(defaultProject);
        }
      } finally {
        // Always clear loading — even on error, so the UI isn't stuck.
        isLoadingStore.set(false);
      }
    },

    async selectProject(id) {
      const projects = get(projectsStore);
      const found = projects.find((p) => p.id === id);
      if (!found) {
        throw new Error(`Project not found: ${id}`);
      }
      // Selection is purely local — no extra round-trip needed since we
      // already have the full project data from loadProjects.
      currentProjectStore.set(found);
    },

    async createProject(name, workspacePath) {
      const created = await api.createProject({
        name,
        workspace_path: workspacePath,
      });

      projectsStore.update((projects) => [...projects, created]);
      currentProjectStore.set(created);
    },

    async deleteProject(id) {
      const projects = get(projectsStore);
      const target = projects.find((p) => p.id === id);

      // Guard: the default project is the fallback for all conversations
      // and cannot be removed — doing so would leave the system in an
      // inconsistent state.
      if (target?.is_default) {
        throw new Error("Cannot delete the default project");
      }

      await api.deleteProject(id);

      const remaining = projects.filter((p) => p.id !== id);
      projectsStore.set(remaining);

      // If the deleted project was current, move focus to the next one.
      if (get(currentProjectStore)?.id === id) {
        currentProjectStore.set(remaining.length > 0 ? remaining[0] : null);
      }
    },
  };
}
