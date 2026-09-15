// HTTP API client for surfaces — agent-run tabs on a topic.
//
// Wraps the gateway's surface endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type { SurfaceListResponse } from "../protocol/SurfaceListResponse";
import type { SurfaceResponse } from "../protocol/SurfaceResponse";
import type { SurfaceModuleView } from "../protocol/SurfaceModuleView";
import type { WriteSurfaceModuleRequest } from "../protocol/WriteSurfaceModuleRequest";
import type { SurfaceAnnotationView } from "../protocol/SurfaceAnnotationView";
import type { CreateSurfaceAnnotationRequest } from "../protocol/CreateSurfaceAnnotationRequest";
import type { UpdateSurfaceAnnotationRequest } from "../protocol/UpdateSurfaceAnnotationRequest";
import * as generated from "../protocol/client";
import type { SurfaceTemplateListResponse } from "../protocol/SurfaceTemplateListResponse";
import type { SurfaceTemplateResponse } from "../protocol/SurfaceTemplateResponse";
import type { CreateSurfaceTemplateRequest } from "../protocol/CreateSurfaceTemplateRequest";

export function setBaseUrl(url: string): void {
  generated.setClientBaseUrl(url);
}

export async function listSurfaces(topicId: string): Promise<SurfaceListResponse> {
  return generated.listSurfaces(topicId);
}

export async function getSurface(surfaceId: string): Promise<SurfaceResponse> {
  return generated.getSurface(surfaceId);
}

// A template seeds the new surface's modules and is recorded as its origin;
// omitting it creates a blank surface.
export async function createSurface(
  topicId: string,
  title: string,
  templateId?: string,
): Promise<SurfaceResponse> {
  return generated.createSurface(topicId, { title, template_id: templateId });
}

export async function listSurfaceTemplates(
  topicId: string,
): Promise<SurfaceTemplateListResponse> {
  return generated.listSurfaceTemplates(topicId);
}

export async function createSurfaceTemplate(
  topicId: string,
  body: CreateSurfaceTemplateRequest,
): Promise<SurfaceTemplateResponse> {
  return generated.createSurfaceTemplate(topicId, body);
}

export async function writeSurfaceModule(
  surfaceId: string,
  body: WriteSurfaceModuleRequest,
): Promise<SurfaceModuleView> {
  return generated.writeSurfaceModule(surfaceId, body);
}

export async function createSurfaceAnnotation(
  surfaceId: string,
  body: CreateSurfaceAnnotationRequest,
): Promise<SurfaceAnnotationView> {
  return generated.createSurfaceAnnotation(surfaceId, body);
}

export async function updateSurfaceAnnotation(
  annotationId: string,
  body: UpdateSurfaceAnnotationRequest,
): Promise<SurfaceAnnotationView> {
  return generated.updateSurfaceAnnotation(annotationId, body);
}

export async function deleteSurfaceAnnotation(
  annotationId: string,
): Promise<void> {
  await generated.deleteSurfaceAnnotation(annotationId);
}
