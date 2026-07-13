import type {
  AttachmentQueuedResponse,
  CreateAttachmentRequest,
  RecoveryExportListResponse,
} from "../types";
import type { VoiceCapabilitiesResponse } from "../protocol/VoiceCapabilitiesResponse";
import type { CreateScheduleRequest } from "../protocol/CreateScheduleRequest";
import type { Schedule } from "../protocol/Schedule";

let baseUrl = "";

export function setWorkflowBaseUrl(url: string): void {
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

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunkSize = 0x8000;
  for (let index = 0; index < bytes.length; index += chunkSize) {
    const chunk = bytes.subarray(index, index + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary);
}

export async function createAttachment(
  payload: CreateAttachmentRequest,
): Promise<AttachmentQueuedResponse> {
  const response = await request("/api/attachments", "POST", payload);
  return response.json() as Promise<AttachmentQueuedResponse>;
}

export async function createSchedule(
  payload: CreateScheduleRequest,
): Promise<Schedule> {
  const response = await request("/api/schedules", "POST", payload);
  return response.json() as Promise<Schedule>;
}

export async function fetchVoiceCapabilities(): Promise<VoiceCapabilitiesResponse> {
  const response = await request("/api/voice/capabilities", "GET");
  return response.json() as Promise<VoiceCapabilitiesResponse>;
}

export async function exportRecoveryBundle(): Promise<Record<string, unknown>> {
  const response = await request("/api/recovery/export", "GET");
  return response.json() as Promise<Record<string, unknown>>;
}

export async function listRecoveryExports(): Promise<RecoveryExportListResponse> {
  const response = await request("/api/recovery/exports", "GET");
  return response.json() as Promise<RecoveryExportListResponse>;
}

export async function downloadRecoveryExport(exportId: string): Promise<Record<string, unknown>> {
  const response = await request(`/api/recovery/exports/${encodeURIComponent(exportId)}`, "GET");
  return response.json() as Promise<Record<string, unknown>>;
}
