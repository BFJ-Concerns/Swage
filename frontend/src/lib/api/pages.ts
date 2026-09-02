// HTTP API client for pages — agent-run tabs on a space.
//
// Wraps the gateway's page endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type { PageListResponse } from "../protocol/PageListResponse";
import type { PageResponse } from "../protocol/PageResponse";
import type { PageModuleView } from "../protocol/PageModuleView";
import type { WritePageModuleRequest } from "../protocol/WritePageModuleRequest";
import type { PageAnnotationView } from "../protocol/PageAnnotationView";
import type { CreatePageAnnotationRequest } from "../protocol/CreatePageAnnotationRequest";
import type { UpdatePageAnnotationRequest } from "../protocol/UpdatePageAnnotationRequest";
import { ApiError } from "./error";
import type { PageTemplateListResponse } from "../protocol/PageTemplateListResponse";
import type { PageTemplateResponse } from "../protocol/PageTemplateResponse";
import type { CreatePageTemplateRequest } from "../protocol/CreatePageTemplateRequest";

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
    throw await ApiError.fromResponse(response, method, path);
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

// A template seeds the new page's modules and is recorded as its origin;
// omitting it creates a blank page.
export async function createPage(
  spaceId: string,
  title: string,
  templateId?: string,
): Promise<PageResponse> {
  const response = await request(`/api/spaces/${spaceId}/pages`, "POST", {
    title,
    template_id: templateId,
  });
  return (await response.json()) as PageResponse;
}

export async function listPageTemplates(
  spaceId: string,
): Promise<PageTemplateListResponse> {
  const response = await request(
    `/api/spaces/${spaceId}/page-templates`,
    "GET",
  );
  return (await response.json()) as PageTemplateListResponse;
}

export async function createPageTemplate(
  spaceId: string,
  body: CreatePageTemplateRequest,
): Promise<PageTemplateResponse> {
  const response = await request(
    `/api/spaces/${spaceId}/page-templates`,
    "POST",
    body,
  );
  return (await response.json()) as PageTemplateResponse;
}

export async function writePageModule(
  pageId: string,
  body: WritePageModuleRequest,
): Promise<PageModuleView> {
  const response = await request(`/api/pages/${pageId}/modules`, "POST", body);
  return (await response.json()) as PageModuleView;
}

export async function createPageAnnotation(
  pageId: string,
  body: CreatePageAnnotationRequest,
): Promise<PageAnnotationView> {
  const response = await request(
    `/api/pages/${pageId}/annotations`,
    "POST",
    body,
  );
  return (await response.json()) as PageAnnotationView;
}

export async function updatePageAnnotation(
  annotationId: string,
  body: UpdatePageAnnotationRequest,
): Promise<PageAnnotationView> {
  const response = await request(
    `/api/page-annotations/${annotationId}`,
    "POST",
    body,
  );
  return (await response.json()) as PageAnnotationView;
}

export async function deletePageAnnotation(
  annotationId: string,
): Promise<void> {
  await request(`/api/page-annotations/${annotationId}`, "DELETE");
}
