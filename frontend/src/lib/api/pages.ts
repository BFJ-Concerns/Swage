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
import * as generated from "../protocol/client";
import type { PageTemplateListResponse } from "../protocol/PageTemplateListResponse";
import type { PageTemplateResponse } from "../protocol/PageTemplateResponse";
import type { CreatePageTemplateRequest } from "../protocol/CreatePageTemplateRequest";

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
  return generated.listPageTemplates(spaceId);
}

export async function createPageTemplate(
  spaceId: string,
  body: CreatePageTemplateRequest,
): Promise<PageTemplateResponse> {
  return generated.createPageTemplate(spaceId, body);
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
