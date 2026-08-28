// HTTP API client for pages — agent-run tabs on a space.
//
// Wraps the gateway's page endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type { PageListResponse } from "../protocol/PageListResponse";
import type { PageResponse } from "../protocol/PageResponse";
import type { PageModuleView } from "../protocol/PageModuleView";
import type { WritePageModuleRequest } from "../protocol/WritePageModuleRequest";

let baseUrl = "";

export function setBaseUrl(url: string): void {
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
    const text = await response.text();
    throw new Error(`${method} ${path} failed (${response.status}): ${text}`);
  }
  return response;
}

export async function listPages(spaceId: string): Promise<PageListResponse> {
  const response = await request(`/api/spaces/${spaceId}/pages`, "GET");
  return (await response.json()) as PageListResponse;
}

export async function getPage(pageId: string): Promise<PageResponse> {
  const response = await request(`/api/pages/${pageId}`, "GET");
  return (await response.json()) as PageResponse;
}

export async function createPage(
  spaceId: string,
  title: string,
): Promise<PageResponse> {
  const response = await request(`/api/spaces/${spaceId}/pages`, "POST", {
    title,
  });
  return (await response.json()) as PageResponse;
}

export async function writePageModule(
  pageId: string,
  body: WritePageModuleRequest,
): Promise<PageModuleView> {
  const response = await request(`/api/pages/${pageId}/modules`, "POST", body);
  return (await response.json()) as PageModuleView;
}
