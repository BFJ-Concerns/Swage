import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  approvePendingAction,
  disableHeartbeat,
  enableHeartbeat,
  fetchOperatorOverview,
  pauseScheduledJob,
  rejectPendingAction,
  resumeScheduledJob,
  setOperatorBaseUrl,
} from "../api/operator";

function mockResponse<T>(body: T, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
    text: async () => JSON.stringify(body),
  } as unknown as Response;
}

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn());
  setOperatorBaseUrl("");
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("fetchOperatorOverview", () => {
  it("sends GET /api/operator/overview and returns the parsed payload", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(
      mockResponse({
        gateway: {
          status: "running",
          health: "healthy",
          shutdown_requested: false,
          seconds_since_activity: 3,
          active_turn_count: 0,
          active_connection_count: 0,
        },
        sessions: [],
        approvals: [],
        scheduled_jobs: { jobs: [] },
        heartbeats: { heartbeats: [] },
        channels: { channels: [] },
        nodes: [],
      }),
    );

    const overview = await fetchOperatorOverview();

    expect(fetch).toHaveBeenCalledWith("/api/operator/overview", { method: "GET" });
    expect(overview.gateway.health).toBe("healthy");
  });
});

describe("operator actions", () => {
  it("approves a pending action", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 200));

    await approvePendingAction("approval-1", "approved in test");

    expect(fetch).toHaveBeenCalledWith("/api/approvals/approval-1", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        decision: "approved",
        scope: "once",
        reason: "approved in test",
      }),
    });
  });

  it("approves a pending action for the current session", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 200));

    await approvePendingAction("approval-1", "approved in test", "session");

    expect(fetch).toHaveBeenCalledWith("/api/approvals/approval-1", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        decision: "approved",
        scope: "session",
        reason: "approved in test",
      }),
    });
  });

  it("approves a pending action permanently", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 200));

    await approvePendingAction("approval-1", "approved in test", "permanent");

    expect(fetch).toHaveBeenCalledWith("/api/approvals/approval-1", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        decision: "approved",
        scope: "permanent",
        reason: "approved in test",
      }),
    });
  });

  it("approves a pending action permanently and globally", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 200));

    await approvePendingAction(
      "approval-1",
      "approved in test",
      "permanent_global",
    );

    expect(fetch).toHaveBeenCalledWith("/api/approvals/approval-1", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        decision: "approved",
        scope: "permanent_global",
        reason: "approved in test",
      }),
    });
  });

  it("rejects a pending action", async () => {
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 200));

    await rejectPendingAction("approval-1", "rejected in test");

    expect(fetch).toHaveBeenCalledWith("/api/approvals/approval-1", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        decision: "rejected",
        reason: "rejected in test",
      }),
    });
  });

  it("pauses and resumes a scheduled job", async () => {
    vi.mocked(fetch)
      .mockResolvedValueOnce(mockResponse({}, 200))
      .mockResolvedValueOnce(mockResponse({}, 200));

    await pauseScheduledJob("job-1");
    await resumeScheduledJob("job-1");

    expect(fetch).toHaveBeenNthCalledWith(1, "/api/scheduled-jobs/job-1/pause", {
      method: "POST",
    });
    expect(fetch).toHaveBeenNthCalledWith(2, "/api/scheduled-jobs/job-1/resume", {
      method: "POST",
    });
  });

  it("disables and enables a heartbeat", async () => {
    vi.mocked(fetch)
      .mockResolvedValueOnce(mockResponse({}, 200))
      .mockResolvedValueOnce(mockResponse({}, 200));

    await disableHeartbeat("hb-1");
    await enableHeartbeat("hb-1");

    expect(fetch).toHaveBeenNthCalledWith(1, "/api/heartbeats/hb-1/disable", {
      method: "POST",
    });
    expect(fetch).toHaveBeenNthCalledWith(2, "/api/heartbeats/hb-1/enable", {
      method: "POST",
    });
  });

  it("prepends the configured base URL", async () => {
    setOperatorBaseUrl("http://localhost:4000");
    vi.mocked(fetch).mockResolvedValueOnce(mockResponse({}, 200));

    await pauseScheduledJob("job-1");

    expect(fetch).toHaveBeenCalledWith(
      "http://localhost:4000/api/scheduled-jobs/job-1/pause",
      { method: "POST" },
    );
  });
});
