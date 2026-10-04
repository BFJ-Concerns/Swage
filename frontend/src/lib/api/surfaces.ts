// HTTP API client for surfaces — a session's work surfaces — and the shared
// library they are preserved in.
//
// Wraps the gateway's surface endpoints with typed request/response handling.
// All functions throw on HTTP errors so callers can handle failures uniformly.

import type { SurfaceListResponse } from "../protocol/SurfaceListResponse";
import type { SurfaceResponse } from "../protocol/SurfaceResponse";
import type { WriteSurfaceModuleRequest } from "../protocol/WriteSurfaceModuleRequest";
import type { WriteSurfaceModuleResponse } from "../protocol/WriteSurfaceModuleResponse";
import type { DeleteSurfaceModuleResponse } from "../protocol/DeleteSurfaceModuleResponse";
import type { SurfaceAnnotationView } from "../protocol/SurfaceAnnotationView";
import type { CreateSurfaceAnnotationRequest } from "../protocol/CreateSurfaceAnnotationRequest";
import type { UpdateSurfaceAnnotationRequest } from "../protocol/UpdateSurfaceAnnotationRequest";
import type { LibraryListResponse } from "../protocol/LibraryListResponse";
import type { LibraryEntryResponse } from "../protocol/LibraryEntryResponse";
import type { SaveSurfaceToLibraryRequest } from "../protocol/SaveSurfaceToLibraryRequest";
import * as generated from "../protocol/client";

export function setBaseUrl(url: string): void {
  generated.setClientBaseUrl(url);
}

export async function listSurfaces(sessionId: string): Promise<SurfaceListResponse> {
  return generated.listSurfaces(sessionId);
}

export async function getSurface(surfaceId: string): Promise<SurfaceResponse> {
  return generated.getSurface(surfaceId);
}

// A library entry seeds the new surface's modules and is recorded as its
// origin; omitting it creates a blank surface.
export async function createSurface(
  sessionId: string,
  title: string,
  libraryEntryId?: string,
): Promise<SurfaceResponse> {
  return generated.createSurface(sessionId, { title, library_entry_id: libraryEntryId });
}

/** The library's catalogue: names, descriptions and tags, never module content. */
export async function listLibrary(tag?: string): Promise<LibraryListResponse> {
  return generated.listLibrary(tag);
}

export async function getLibraryEntry(entryId: string): Promise<LibraryEntryResponse> {
  return generated.getLibraryEntry(entryId);
}

export async function saveSurfaceToLibrary(
  surfaceId: string,
  body: SaveSurfaceToLibraryRequest,
): Promise<LibraryEntryResponse> {
  return generated.saveSurfaceToLibrary(surfaceId, body);
}

export async function deleteLibraryEntry(entryId: string): Promise<void> {
  await generated.deleteLibraryEntry(entryId);
}

export async function listTopicDefaultSurfaces(topicId: string): Promise<LibraryListResponse> {
  return generated.listTopicDefaultSurfaces(topicId);
}

export async function addTopicDefaultSurface(
  topicId: string,
  libraryEntryId: string,
): Promise<LibraryListResponse> {
  return generated.addTopicDefaultSurface(topicId, { library_entry_id: libraryEntryId });
}

export async function removeTopicDefaultSurface(
  topicId: string,
  entryId: string,
): Promise<void> {
  await generated.removeTopicDefaultSurface(topicId, entryId);
}

export async function writeSurfaceModule(
  surfaceId: string,
  body: WriteSurfaceModuleRequest,
): Promise<WriteSurfaceModuleResponse> {
  return generated.writeSurfaceModule(surfaceId, body);
}

export async function deleteSurfaceModule(
  surfaceId: string,
  moduleId: string,
): Promise<DeleteSurfaceModuleResponse> {
  return generated.deleteSurfaceModule(surfaceId, moduleId);
}

export async function moveSurfaceModule(
  surfaceId: string,
  moduleId: string,
  position: number,
): Promise<SurfaceResponse> {
  return generated.moveSurfaceModule(surfaceId, moduleId, { position });
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
