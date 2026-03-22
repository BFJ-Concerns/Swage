/**
 * Tests for the HTTP API client for project CRUD and project-scoped sessions.
 *
 * All network calls are intercepted via a global fetch mock so these tests
 * run in isolation without a live gateway. Each test verifies the correct
 * URL, method, headers, body, and return type for one API function, plus
 * a shared error-throwing contract for non-2xx responses.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import {
  listProjects,
  createProject,
  getProject,
  updateProject,
  deleteProject,
  listProjectSessions,
  createProjectSession,
  setProjectsBaseUrl,
} from "../api/projects";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Build a minimal fetch Response stub. */
function mockResponse<T>(body: T, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    text: async () => JSON.stringify(body),
  } as unknown as Response;
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

beforeEach(() => {
  // Replace the global fetch with a vitest spy so tests control responses.
  vi.stubGlobal("fetch", vi.fn());
  // Ensure base URL is reset to same-origin default between tests.
  setProjectsBaseUrl("");
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// Sample fixtures
// ---------------------------------------------------------------------------

const sampleProject = {
  id: "proj-1",
  name: "My Project",
  slug: "my-project",
  workspace_path: "/home/user/my-project",
  created_at: "2026-01-01T00:00:00Z",
};

const sampleSession = {
  id: "sess-1",
  created_at: "2026-01-01T00:00:00Z",
  last_activity: "2026-01-01T01:00:00Z",
  message_preview: "Hello",
  has_interrupted_turn: false,
};

// ---------------------------------------------------------------------------
// listProjects
// ---------------------------------------------------------------------------

describe("listProjects", () => {
  it("sends GET /api/projects and unwraps the projects array", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ projects: [sampleProject] }),
    );

    const result = await listProjects();

    expect(fetchMock).toHaveBeenCalledWith("/api/projects", { method: "GET" });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("proj-1");
    expect(result[0].slug).toBe("my-project");
  });

  it("returns an empty array when there are no projects", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ projects: [] }));

    const result = await listProjects();
    expect(result).toEqual([]);
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));
    await expect(listProjects()).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// createProject
// ---------------------------------------------------------------------------

describe("createProject", () => {
  it("sends POST /api/projects with JSON body and returns the new project", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleProject, 201));

    const result = await createProject({
      name: "My Project",
      workspace_path: "/home/user/my-project",
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/projects",
      expect.objectContaining({
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: "My Project",
          workspace_path: "/home/user/my-project",
        }),
      }),
    );
    expect(result.id).toBe("proj-1");
    expect(result.slug).toBe("my-project");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 422));
    await expect(
      createProject({ name: "Bad", workspace_path: "/" }),
    ).rejects.toThrow("422");
  });
});

// ---------------------------------------------------------------------------
// getProject
// ---------------------------------------------------------------------------

describe("getProject", () => {
  it("sends GET /api/projects/:id and returns a Project", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleProject));

    const result = await getProject("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/projects/proj-1", {
      method: "GET",
    });
    expect(result.id).toBe("proj-1");
    expect(result.workspace_path).toBe("/home/user/my-project");
  });

  it("throws on 404", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));
    await expect(getProject("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// updateProject
// ---------------------------------------------------------------------------

describe("updateProject", () => {
  it("sends PUT /api/projects/:id with JSON body and returns the updated project", async () => {
    const fetchMock = vi.mocked(fetch);
    const updated = { ...sampleProject, name: "Renamed" };
    fetchMock.mockResolvedValueOnce(mockResponse(updated));

    const result = await updateProject("proj-1", { name: "Renamed" });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/projects/proj-1",
      expect.objectContaining({
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name: "Renamed" }),
      }),
    );
    expect(result.name).toBe("Renamed");
  });

  it("accepts a partial update with only workspace_path", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(mockResponse(sampleProject));

    await updateProject("proj-1", { workspace_path: "/new/path" });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/projects/proj-1",
      expect.objectContaining({
        body: JSON.stringify({ workspace_path: "/new/path" }),
      }),
    );
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 400));
    await expect(updateProject("proj-1", {})).rejects.toThrow("400");
  });
});

// ---------------------------------------------------------------------------
// deleteProject
// ---------------------------------------------------------------------------

describe("deleteProject", () => {
  it("sends DELETE /api/projects/:id and returns undefined", async () => {
    const fetchMock = vi.mocked(fetch);
    // Gateway returns 204 No Content on successful delete.
    fetchMock.mockResolvedValueOnce(mockResponse(null, 204));

    const result = await deleteProject("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/projects/proj-1", {
      method: "DELETE",
    });
    expect(result).toBeUndefined();
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 403));
    await expect(deleteProject("proj-1")).rejects.toThrow("403");
  });
});

// ---------------------------------------------------------------------------
// listProjectSessions
// ---------------------------------------------------------------------------

describe("listProjectSessions", () => {
  it("sends GET /api/projects/:id/sessions and unwraps the conversations array", async () => {
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockResolvedValueOnce(
      mockResponse({ conversations: [sampleSession] }),
    );

    const result = await listProjectSessions("proj-1");

    expect(fetchMock).toHaveBeenCalledWith("/api/projects/proj-1/sessions", {
      method: "GET",
    });
    expect(result).toHaveLength(1);
    expect(result[0].id).toBe("sess-1");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 404));
    await expect(listProjectSessions("missing")).rejects.toThrow("404");
  });
});

// ---------------------------------------------------------------------------
// createProjectSession
// ---------------------------------------------------------------------------

describe("createProjectSession", () => {
  it("sends POST /api/projects/:id/sessions and returns the new session", async () => {
    const fetchMock = vi.mocked(fetch);
    const created = {
      id: "sess-new",
      project_id: "proj-1",
      created_at: "2026-01-01T00:00:00Z",
      last_activity: "2026-01-01T00:00:00Z",
    };
    fetchMock.mockResolvedValueOnce(mockResponse(created, 201));

    const result = await createProjectSession("proj-1");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/projects/proj-1/sessions",
      expect.objectContaining({ method: "POST" }),
    );
    expect(result.id).toBe("sess-new");
    expect(result.project_id).toBe("proj-1");
  });

  it("throws on non-2xx response", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 500));
    await expect(createProjectSession("proj-1")).rejects.toThrow("500");
  });
});

// ---------------------------------------------------------------------------
// Configurable base URL
// ---------------------------------------------------------------------------

describe("setProjectsBaseUrl", () => {
  it("prepends the base URL to all requests", async () => {
    setProjectsBaseUrl("http://localhost:3000");
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({ projects: [] }));

    await listProjects();

    expect(vi.mocked(fetch)).toHaveBeenCalledWith(
      "http://localhost:3000/api/projects",
      expect.objectContaining({ method: "GET" }),
    );
  });
});
