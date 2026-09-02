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
import type { PageTemplateListResponse } from "../protocol/PageTemplateListResponse";
import type { PageTemplateResponse } from "../protocol/PageTemplateResponse";
import type { CreatePageTemplateRequest } from "../protocol/CreatePageTemplateRequest";
import * as generated from "../protocol/client";

async function templateRequest(
  path: string,
  method: string,
  body?: unknown,
): Promise<Response> {
  const init: RequestInit = { method };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const response = await fetch(path, init);
  if (!response.ok) {
    const text = await response.text();
    throw new Error(`${method} ${path} failed (${response.status}): ${text}`);
  }
  return response;
}

export function setBaseUrl(url: string): void {
  generated.setClientBaseUrl(url);
}

export async function listPages(spaceId: string): Promise<PageListResponse> {
  return generated.listPages(spaceId);
}

export async function getPage(pageId: string): Promise<PageResponse> {
  return generated.getPage(pageId);
}

// A template seeds the new page's modules and is recorded as its origin;
// omitting it creates a blank page.
export async function createPage(
  spaceId: string,
  title: string,
  templateId?: string,
): Promise<PageResponse> {
  return generated.createPage(spaceId, { title, template_id: templateId });
}

export async function listPageTemplates(
  spaceId: string,
): Promise<PageTemplateListResponse> {
  const response = await templateRequest(
    `/api/spaces/${spaceId}/page-templates`,
    "GET",
  );
  return (await response.json()) as PageTemplateListResponse;
}

export async function createPageTemplate(
  spaceId: string,
  body: CreatePageTemplateRequest,
): Promise<PageTemplateResponse> {
  const response = await templateRequest(
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
  return generated.writePageModule(pageId, body);
}

export async function createPageAnnotation(
  pageId: string,
  body: CreatePageAnnotationRequest,
): Promise<PageAnnotationView> {
  return generated.createPageAnnotation(pageId, body);
}

export async function updatePageAnnotation(
  annotationId: string,
  body: UpdatePageAnnotationRequest,
): Promise<PageAnnotationView> {
  return generated.updatePageAnnotation(annotationId, body);
}

export async function deletePageAnnotation(
  annotationId: string,
): Promise<void> {
  await generated.deletePageAnnotation(annotationId);
}
