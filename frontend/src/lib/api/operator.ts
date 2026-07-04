import type { OperatorOverview } from "../types";
import type { ScheduleListResponse } from "../protocol/ScheduleListResponse";
import type { ScheduleRunListResponse } from "../protocol/ScheduleRunListResponse";
import type { TriggerScheduleResponse } from "../protocol/TriggerScheduleResponse";

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
  scope: "once" | "session" | "permanent" | "permanent_global" = "once",
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

export async function listSchedules(): Promise<ScheduleListResponse> {
  const response = await request("/api/schedules", "GET");
  return response.json() as Promise<ScheduleListResponse>;
}

export async function triggerSchedule(
  scheduleId: string,
): Promise<TriggerScheduleResponse> {
  const response = await request(`/api/schedules/${scheduleId}/trigger`, "POST");
  return response.json() as Promise<TriggerScheduleResponse>;
}

export async function pauseSchedule(scheduleId: string): Promise<void> {
  await request(`/api/schedules/${scheduleId}/pause`, "POST");
}

export async function resumeSchedule(scheduleId: string): Promise<void> {
  await request(`/api/schedules/${scheduleId}/resume`, "POST");
}

export async function fetchScheduleRuns(
  scheduleId: string,
): Promise<ScheduleRunListResponse> {
  const response = await request(`/api/schedules/${scheduleId}/runs`, "GET");
  return response.json() as Promise<ScheduleRunListResponse>;
}
