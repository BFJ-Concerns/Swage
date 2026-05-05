import type { OperatorOverview } from "../types";

let baseUrl = "";

export function setOperatorBaseUrl(url: string): void {
  baseUrl = url;
}

async function request(
  path: string,
  method: string,
  body?: unknown,
): Promise<Response> {
  const init: RequestInit = { method };

  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }

  const response = await fetch(`${baseUrl}${path}`, init);

  if (!response.ok) {
    throw new Error(`HTTP ${response.status} from ${method} ${path}`);
  }

  return response;
}

export async function fetchOperatorOverview(): Promise<OperatorOverview> {
  const response = await request("/api/operator/overview", "GET");
  return response.json() as Promise<OperatorOverview>;
}

export async function approvePendingAction(
  approvalId: string,
  reason?: string,
  scope: "once" | "turn" = "once",
): Promise<void> {
  await request(`/api/approvals/${approvalId}`, "POST", {
    decision: "approved",
    scope,
    reason,
  });
}

export async function rejectPendingAction(
  approvalId: string,
  reason?: string,
): Promise<void> {
  await request(`/api/approvals/${approvalId}`, "POST", {
    decision: "rejected",
    reason,
  });
}

export async function pauseScheduledJob(jobId: string): Promise<void> {
  await request(`/api/scheduled-jobs/${jobId}/pause`, "POST");
}

export async function resumeScheduledJob(jobId: string): Promise<void> {
  await request(`/api/scheduled-jobs/${jobId}/resume`, "POST");
}

export async function disableHeartbeat(heartbeatId: string): Promise<void> {
  await request(`/api/heartbeats/${heartbeatId}/disable`, "POST");
}

export async function enableHeartbeat(heartbeatId: string): Promise<void> {
  await request(`/api/heartbeats/${heartbeatId}/enable`, "POST");
}
