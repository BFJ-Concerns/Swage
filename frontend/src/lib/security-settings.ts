import { writable } from "svelte/store";
import type { SecurityCheck } from "./protocol/SecurityCheck";
import type { RelaxedCheck } from "./protocol/RelaxedCheck";

export const CHECK_LABELS: Record<SecurityCheck, string> = {
  sandbox: "Sandbox",
  sandbox_confinement: "Sandbox confinement",
  approval_prompts: "Approval prompts",
  access_gate: "Access gate",
};
export function relaxedBy(entry: Pick<RelaxedCheck, "relaxed_by">): string {
  return entry.relaxed_by.kind === "topic" ? "relaxed by this topic" : "relaxed by the install";
}
export const securitySettingsRevision = writable(0);
export function securitySettingsChanged(): void {
  securitySettingsRevision.update((revision) => revision + 1);
}
