import type { DeliveryTarget } from "./protocol/DeliveryTarget";
import type { ScheduleRunDelivery } from "./protocol/ScheduleRunDelivery";

/** Where a run's output was routed, as the run's account names it. */
export function describeDeliveryDestination(target: DeliveryTarget | null | undefined): string {
  if (target === null || target === undefined) return "no destination";
  switch (target.kind) {
    case "channel":
      return `${target.integration} ${target.conversation}`;
    case "local_storage":
      return "local storage";
    case "surface":
      return `surface ${target.surface_id.slice(0, 8)}`;
    case "webhook":
      return `webhook ${target.url}`;
  }
}

/** How a routed destination ended, with its reason where there is one. */
export function describeDeliveryOutcome(delivery: ScheduleRunDelivery): string {
  switch (delivery.outcome) {
    case "sent":
      return "sent";
    case "not_sent":
      return delivery.error ? `not sent: ${delivery.error}` : "not sent";
    case "failed":
      return delivery.error ? `failed: ${delivery.error}` : "failed";
    case "not_delivered":
      return delivery.error ? `not delivered: ${delivery.error}` : "not delivered";
  }
}
