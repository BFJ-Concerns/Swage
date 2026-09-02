---
description: Run lifecycle, journal/replay, budget partitions, parking, wake gate, worker sessions, cancellation, failure handover, and carried workflows
---

# Workflow engine

The workflow engine executes front-loaded JavaScript scripts inside a
sandboxed QuickJS runtime. Each run is a single script evaluation; the host
(the gateway) journals every non-deterministic call so a crashed or parked
run replays from its journal.

Primary sources: `crates/swage-workflow/src/engine.rs` (the JS sandbox and
host bindings), `crates/swage-workflow/src/host.rs` (the `WorkflowHost`
trait), `crates/swage-gateway/src/workflow.rs` (journalling, parking,
budget, handover, wake loop).

## Run lifecycle

States: `running`, `completed`, `failed`, `cancelled`, `interrupted`,
`parked`.

Creation paths (all converge on the internal `start_run`):

- `start_inline_run` — HTTP, inline source and args.
- `start_saved_run` — HTTP, resolves a saved script's head version.
- `GatewayWorkflowLauncher::start_by_name` — programmatic, from agent
  dispatch. Carries `dispatch_bound_agent_ids`.

The `start_run` path checks an active-run cap of 4 and rejects new
runs when the cap is reached. Child runs created via
`complete_workflow_call_entry` and resumed runs bypass this check, and
concurrent `start_run` calls race the async mutex without
serialisation, so the cap is a soft admission gate on `start_run`
callers rather than a hard gateway-wide limit.

`start_run` validates the budget, creates a `NewWorkflowRun` record
(with `source_hash` as SHA-256 of source, `engine_version:
"rquickjs-0.12.0"`), then calls `launch_existing_run`.

`launch_existing_run` is the shared path for new and resumed runs:

1. Loads the journal replay from storage.
2. Computes `initial_agent_calls` and `initial_spent_tokens` from
   completed journal entries.
3. Spawns the host-command loop (`run_host_commands`).
4. Creates a `ChannelWorkflowHost` wrapping the command channel.
5. Determines `ScriptCapabilities` — `composition` is disabled for child
   runs (`parent_run_id.is_some()`).
6. Spawns a dedicated OS thread with a single-threaded Tokio runtime
   running `WorkflowEngine::execute_hosted`.
7. Spawns a Tokio task awaiting the engine result, then resolves the
   terminal state.

Terminal state resolution:

- Park token cancelled (cancellation token not cancelled) → run is
  **parked** (state already set by storage call).
- `Ok(json)` → `completed` with `final_output`.
- `WorkflowEngineError::Cancelled` → `cancelled`.
- Any other error → `failed` with `error_message` and a handover record.

## Scripts

Scripts are JavaScript evaluated in a sandboxed QuickJS runtime with only
these intrinsics: `Eval`, `Json`, `Promise`, `MapSet`, `RegExpCompiler`,
`RegExp`. `globalThis.eval` is deleted; `Math.random` throws
`NondeterministicCallError`. `Date`, `setTimeout`, `fetch`, `process` are
absent.

### Defaults header

A script may open with `export const defaults = { ... };` before its body. These
are merged into every `agent()` / `agentStart()` call — call-site values
override, but `undefined` at the call site falls back to the default.

### Global API

| Function | Returns | Notes |
|---|---|---|
| `agent(prompt, options?)` | `Promise<Value>` | Synchronous (awaited) agent call. Requires `options.agent`. |
| `agentStart(prompt, options?)` | Handle | Fire-and-forget dispatch. Handle has `.done`, `.session`, `.result`, `.label`, and `.workingDirectory` after completion; the latter is the host-resolved child directory, including a provisioned isolated worktree. |
| `wait({ watches?, timerMs? })` | `Promise<WakeResult>` | Races handle completions against a journalled timer. Returns `{wake: "completion", handle, value/error}` or `{wake: "timer"}`. |
| `workflow(name, args?)` | `Promise<Value>` | Child workflow call. Only present when `capabilities.composition` is true. |
| `deliver(payload, options?)` | `Promise<EffectReceipt>` | Delivery effect. Only present when `capabilities.delivery` is true. |
| `postEvent(kind, payload?)` | `Promise<Value>` | Appends to the run's durable pending-event queue. Kind: `"completion"`, `"failure"`, or `"escalation"`. |
| `gate(options?)` | `Promise<Value>` | Wake-gate evaluation. Options: `criteria` (string), `model` (optional override). |
| `deliverEvents(upToSeq)` | `Promise<Value>` | Stamps pending events as delivered up to a sequence number. |
| `parallel(thunks)` | `Promise<Array>` | `Promise.all` with per-thunk error swallowing (failing thunks collapse to `null`). |
| `pipeline(items, ...stages)` | `Promise<Array>` | Per-item stage chain; failing items collapse to `null` at the failing stage. |
| `budgetSnapshot()` | `Promise<Value>` | Journalled budget reading. Returns `{spent, remaining, working_remaining, closing_remaining, winding_down}`. |
| `budget.spent()` | `number` | Synchronous, not journalled — NOT safe for branching (replay sees end-state values). |
| `budget.remaining()` | `number \| undefined` | Synchronous, not journalled — NOT safe for branching. |
| `log(message)` | — | Records via `host.record("log", ...)`. `console.log/warn/error` alias this. |
| `phase(title)` | — | Records via `host.record("phase", ...)`. |

### Composition bounds

`ScriptCapabilities { delivery: bool, composition: bool }`. Child runs
have `composition: false`, bounding nesting to one level. `delivery`
defaults to `false`; controlled by the script's delivery grant.

## Journal and replay

Every host call that could produce a different result on replay is
journalled as a write-ahead intent — with one deliberate exception:
`budget.spent()` and `budget.remaining()` are live synchronous reads
against an atomic counter, not journalled commands. They exist for
informational display; branching on them is unsafe because replay sees
end-state values. The journalled route for budget reads is
`budgetSnapshot()`, which records the reading so replay returns the
same values the original run saw.

1. **Intent** — `create_workflow_journal_intent` creates a row in status
   `started`.
2. **Settlement** — `complete_workflow_journal_entry` (status `completed`
   with result) or `fail_workflow_journal_entry` (status `failed` with
   error).

Calls refused at admission (budget exceeded) use
`create_failed_workflow_journal_entry` — a single atomic write in status
`failed`.

### Sequence numbering

The `seq` counter is incremented at the channel boundary (the
host-command loop), not inside spawned tasks. This makes journal ordering
deterministic and independent of scheduler timing.

### Replay matching

Journal entries store `args_hash` — SHA-256 of the canonical JSON
serialisation of the request (computed by `journal_args_hash`). On
replay, the current request's hash is compared against the recorded
entry's hash. A mismatch produces `NondeterministicReplayError`.

### Replay behaviour by status

| Recorded status | Replay behaviour |
|---|---|
| `completed` | Return the recorded result without re-executing. Concurrent calls wait for completion-order release. |
| `failed` | Return the recorded error without re-executing. |
| `started` | The original run crashed between intent and settlement. Re-execute the work and settle the existing entry. |

### Completion-order preservation

Concurrent host calls can complete in any order. The original run's
`completed_order` column records the interleaving. On replay, completed
entries wait in `await_replay_release` until a rank counter reaches their
rank, preserving the original interleaving for deterministic scripts.

### Journal entry kinds

| Kind | Source | Notes |
|---|---|---|
| `agent_call` | `agent()` / `agentStart()` | Spawned on semaphore. Cancellation errors are NOT settled (re-executed on resume). |
| `effect` | `deliver()` | Carries `idempotency_key` and `effect_kind`. Recovery uses `recover_started_effect`. |
| `log` / `phase` | `log()` / `phase()` | Fire-and-forget. Immediately completed with `null`. |
| `workflow_call` | `workflow()` | Child runs found by `(parent_run_id, parent_seq)` to avoid duplicates. |
| `timer` | `wait()` | Three-state lifecycle: started → completed (timer won via `confirm_timer`) or stays started (timer lost via `abandon_timer`). |
| `event` | `postEvent()` / `deliverEvents()` | Event posts and delivery stamps. |
| `gate` | `gate()` | Wake-gate evaluations, spawned like agent calls. |
| `budget` | `budgetSnapshot()` | Recorded reading replayed, not re-read, to preserve branch determinism. |
| `wind_down` | Agent admission or `budgetSnapshot()` | Written when working allowance is exhausted. When triggered by an agent call, that call is refused; when triggered by `budgetSnapshot()`, the snapshot succeeds. |

### Effect recovery

When a `deliver` entry is found in status `started` during replay, the
recovery strategy depends on `EffectKind`:

- `IdempotencyKey` — resend directly without probing. Re-delivery is
  only safe when the receiver implements duplicate suppression keyed on
  the idempotency key; the strategy assumes this, it does not provide it.
- `CheckBeforeRedo` — probe first; if evidence of prior delivery exists,
  use it; otherwise resend.

## Budget partitions

`WorkflowBudget` fields:

| Field | Type | Purpose |
|---|---|---|
| `max_agent_calls` | `Option<u32>` | Hard cap on agent call count (excluding budget-refused calls). |
| `max_total_tokens` | `Option<u64>` | Total token ceiling across all agent calls. |
| `reserved_closing_tokens` | `Option<u64>` | Tokens held back from ordinary work for calls after wind-down. Part of, not in addition to, `max_total_tokens`. |
| `max_concurrent_agent_calls` | `Option<u32>` | Concurrency semaphore size (default 4). |

### Two partitions

The budget is carved from `max_total_tokens`:

- **Working partition**: `max_total_tokens - reserved_closing_tokens`.
  Funds ordinary agent calls.
- **Closing partition**: `reserved_closing_tokens`. Funds agent calls
  after wind-down.

### Wind-down trigger

Wind-down latches when a new agent call or a `budgetSnapshot()` finds
the working partition's remaining capacity has dropped below one
per-call reservation estimate. The trigger:

1. Computes `working_limit = limit - reserve`.
2. Computes `committed = spent + working_reserved + closing_reserved`.
3. If `working_limit - committed < reservation`:
   - Writes a `wind_down` journal entry with a budget reading.
   - Latches `closing_limit = spent + reserve`.

When an agent call triggers this transition, that call is refused with
`BudgetWindDownError`. When `budgetSnapshot()` triggers the transition,
the snapshot succeeds and returns a reading with `winding_down: true` —
it is the next agent call that is refused, not the snapshot itself.

Once `winding_down` is true, new agent calls are admitted against
`closing_limit`. If `closing_limit - (spent + closing_reserved) <
reservation`, the call is refused with `BudgetExceededError`.

### In-flight working calls after wind-down

Working-partition calls admitted before wind-down that settle after it
extend `closing_limit` by their actual spend. This prevents working calls
from consuming the closing allowance — their spend is added to the
ceiling, not counted against it.

During replay, a working call may settle before the replay cursor
reaches the recorded wind-down transition. The `replayed_working_credit`
field accumulates this spend and is folded into `closing_limit` when
wind-down latches.

Replay constructs `closing_limit` from
`completed_agent_tokens_before(replay, transition_seq) +
reserved_closing_tokens + replayed_working_credit`, scanning the journal
for completed agent-call token counts before the transition — not from
the live `accounting.spent` counter.

### Token accounting

`TokenBudgetState` fields:

| Field | Purpose |
|---|---|
| `spent` | Total tokens consumed by settled agent calls. |
| `working_reserved` | Tokens reserved by in-flight working-partition calls (estimated). |
| `closing_reserved` | Tokens reserved by in-flight closing-partition calls. |
| `closing_limit` | Once wind-down latches: `spent_at_transition + reserved_closing_tokens`. `None` before wind-down. |
| `replayed_working_credit` | Accumulated spend from replayed working calls that settle before the wind-down latch. |

Per-call reservation estimate: `token_reservation_per_call(limit, budget)`
computes `(limit / calls).max(1)` where `calls` is `max_agent_calls`
when set **and non-zero**, otherwise
`max(max_concurrent_agent_calls.unwrap_or(4), 1)`. The final `.max(1)`
clamp ensures every admitted call reserves at least one token.

Working-partition calls pass `limit = max_total_tokens - reserved_closing_tokens`
(the working partition's share). After wind-down, closing-partition calls
pass `limit = max_total_tokens` (the full ceiling), so their per-call
estimate is larger — shrinking the estimate a second time would admit
work that the closing partition cannot fund.

## Durable run parking

### Threshold

`PARK_TIMER_THRESHOLD_MS = 60_000` (one minute). Timers shorter than
this run as in-process sleeps.

### Parking conditions

A timer parks the run when all three hold:

1. Remaining sleep ≥ `PARK_TIMER_THRESHOLD_MS`.
2. No other timer is armed (`pending_timers` is empty).
3. No host work is outstanding (`in_flight == 0`).

### How parking works

1. `storage.set_workflow_run_parked(run_id, due)` sets state to `parked`
   with `wake_at`.
2. The `park` cancellation token fires, which triggers the `halt` token
   (combined with `cancellation`), stopping the engine.
3. The timer's reply sender is stashed (never resolved — the engine is
   being torn down).
4. Later timers arriving while parking join: their reply senders are
   stashed, and `wake_at` is lowered to `min(current_due, new_due)`.

Post-park, the run is removed from `active_workflow_runs`. No terminal
state update — the run is dormant, not finished.

### Resumption

The wake loop (`spawn_workflow_wake_loop`) runs on a 5-second tick
(`WORKFLOW_WAKE_TICK`). Each pass calls `resumable_workflow_runs`, which
lists:

- All `interrupted` runs.
- All `parked` runs whose `wake_at ≤ now`.

Each resumable run goes through `resume_workflow_run_record`:

1. Compare-and-swap state transition to `running`
   (`try_transition_workflow_run_state`).
2. `launch_existing_run` replays the journal and continues execution.

### Timer replay on wake

A replayed timer entry in status `started` re-arms with remaining time:
`effective_ms = ms - elapsed` where `elapsed` is wall-clock time since
`started_at`. A four-hour sleep interrupted at hour three owes one hour.

### Boot recovery

At gateway startup, `interrupt_running_workflow_runs` bulk-transitions
all `running` runs to `interrupted`. The wake loop's first pass
(immediate at spawn) picks these up alongside due parked runs and
relaunches them by replay.

## Run events and the wake gate

### Event posting

`postEvent(kind, payload)` appends to the run's durable pending-event
queue. Accepted kinds: `"completion"`, `"failure"`, `"escalation"`.
Events are journalled (exactly-once under replay) and assigned a
monotonic `seq`. Each event record links to its journal entry via
`journal_entry_id`.

### Wake-gate evaluation

`gate({ criteria, model? })` evaluates pending events against the
script's criteria:

1. Loads pending (undelivered) events from storage.
2. If no events: returns `{wake: false, reasoning: "no pending events"}`.
3. Builds a prompt with the criteria and a JSON array of pending events.
4. Calls a model (the gate model override, or the default model) with
   `max_tokens: 500`.
5. Parses a `{wake: bool, reasoning: string}` JSON from the response.
6. On parse failure, fails open: `wake: true`.
7. Returns `{wake, reasoning, events, read_up_to_seq}`.

The events ride along in the response so a waking script can ground the
lead without a second read.

Gate evaluations are journalled — a replayed gate returns its recorded
verdict.

### Event delivery

`deliverEvents(upToSeq)` stamps pending events up to a sequence number
as delivered. Journalled, idempotent.

## Worker session continuity

`AgentSessionRef { session_id: Uuid, thread_id: Uuid }` is the
continuation token.

Flow:

1. Host returns `session_id` and `thread_id` in `AgentCallResult`.
2. Engine wraps them in the `agentStart` handle's `.session` field.
3. Script passes `handle.session` back as `options.session` in the next
   `agent()` / `agentStart()` call.
4. `AgentCallRequest.session` carries it to the host, which routes the
   turn to the existing session/thread.

Session validation: `run_ephemeral_agent_call` verifies the session was
created by this run (checking completed `agent_call` journal entries for
a matching `session_id`). Unrecognised sessions are rejected.

Replay determinism: tokens come from journalled results, so a replayed
run rebuilds identical tokens. `AgentCallRequest.session` uses
`skip_serializing_if = "Option::is_none"` — without it, the args hash
would change and replay would fail.

## Cancellation

The `CancellationToken` is installed as the QuickJS interrupt handler
(checked between JS opcodes) and raced against the execution future via
`tokio::select!`. Result: `WorkflowEngineError::Cancelled`.

`cancel_run` cancels the token. For parked runs (no engine to signal),
it directly sets state to `cancelled` in storage.

## Failure attribution and handover records

When a run fails, a handover record preserves diagnostic context for the
next handler.

### Attribution path

1. A spawned agent task observes an error and creates a
   `FailedAgentCallRef { journal_entry_id, seq }`.
2. It inserts the ref into `failed_agent_calls` keyed by
   `journal_entry_id.to_string()`.
3. The host returns the reference structurally with the failure.
4. The engine captures the reference inside
   `WorkflowEngineError::AgentCall { host_reference, message }`.
5. At terminal state resolution, the `host_reference` looks up
   `failed_agent_calls` to recover the ref.

### Handover construction

`failed_agent_call_handover` reads the full journal, finds the matching
failed `agent_call` entry, computes `previous_agent_call_seq`, and
filters events into a delivery window:

- `"delivered_to_failed_call"` — events delivered between the previous
  agent call and the failed call.
- `"pending"` — undelivered events.

Result:

```json
{
  "classification": "interrupted",
  "failure": "Agent call {id} failed: {error}",
  "failed_call": {
    "journal_entry_id": "...",
    "seq": 0,
    "kind": "agent_call",
    "label": "...",
    "error": "..."
  },
  "last_journal_seq": 0,
  "events_delivered_to_failed_call": 0,
  "events_pending": 0,
  "events": []
}
```

### Classification values

| Classification | Condition |
|---|---|
| `interrupted` | Full handover — failed agent call identified, journal reconstructed. |
| `interrupted_unattributed` | No `host_reference`, or the reference was not in `failed_agent_calls`. |
| `reconstruction_failed` | Reference existed but journal reconstruction returned `None`. |

### Persistence

The handover is stored via `update_workflow_run_state` in the
`handover_json` column of `workflow_runs`.

## Concurrency control

| Mechanism | Scope | Default |
|---|---|---|
| Active-run cap | `start_run` callers | 4 (soft — bypassed by child runs and resumptions) |
| Agent-call semaphore | Per-run | `max_concurrent_agent_calls` (default 4) |
| `in_flight` counter | Per-run | Guards parking — no park while > 0 |
| `agent_calls` counter | Per-run lifetime | Checked against `max_agent_calls` budget |

The `in_flight` counter tracks agent calls, workflow calls, and gate
evaluations. It is incremented in the host-command loop (deterministic
boundary) and decremented in the spawned task on completion. The park
guard reads this counter — the journal can lag a dispatch, so it must not
be the guard.

## Carried workflows

A workflow script may be carried by a skill rather than authored as a
space row. The `WorkflowScript` wire type carries a `carrying_skill`
field (`Option<String>`) naming the skill that provides the workflow
when it is not a standalone authored row. A carried workflow is
immutable in place; an operator can shadow it by authoring a same-named
workflow in the space.

The shipped research skill (`shipped/skills/research/`) carries the
`research-sweep` workflow. No other workflows ship with the gateway.
A carried workflow's roles (the research sweep's researcher, organiser,
and synthesist) are bare dispatches inside the script, not standalone
agent definitions — they do not appear in the agent registry.

## Storage

SQLite. Key tables:

- `workflow_runs` — run records with `state`, `handover_json`,
  `wake_at`, `dispatch_bound_agent_ids_json`.
- `workflow_journal` — journal entries with `seq`, `completed_order`,
  `kind`, `status`, `args_hash`, `request_json`, `result_json`,
  `effect_kind`, `idempotency_key`.
- `workflow_run_events` — run events with `seq`, `kind`,
  `payload_json`, `delivered_wake_seq`, `delivered_at`. Partial index on
  `(run_id, seq) WHERE delivered_at IS NULL` for pending-event queries.
