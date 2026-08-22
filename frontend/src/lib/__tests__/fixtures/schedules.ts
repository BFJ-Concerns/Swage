import type { Schedule } from "../../protocol/Schedule";

export function makeSchedule(overrides: Partial<Schedule> = {}): Schedule {
  return {
    id: "schedule-1",
    message: "Review the queue.",
    status: "active",
    spec: { kind: "interval", every: "every 1 hours" },
    session: { policy: "fresh", space_id: "space-1", agent_id: "gateway" },
    creator: { kind: "user" },
    routing: {
      deliver_to_origin: false,
      home_target: null,
      explicit_targets: [],
      store_local_output: true,
    },
    gate: null,
    next_run_at: "2026-01-01T01:00:00Z",
    last_run_at: null,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}
