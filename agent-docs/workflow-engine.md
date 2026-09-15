---
description: Run lifecycle, journal/replay, budget partitions (including per-route), run detail (dispatch, provider rounds, usage, liveness, worktree completion, termination), terminal account (ending, worker partition, partial results, retention), parking, wake gate, worker sessions, cancellation, failure handover, and carried workflows
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

### Meta header

A script may open with `export const meta = { ... };` to declare portable
metadata. Fields: `name` (string), `description` (string), `task` (string —
opaque identity grouping runs of the same task), `argumentSchema` (JSON Schema
governing the workflow's arguments). The header is parsed at save time and
never exposed to the caged runtime.

### Defaults header

A script may open with `export const defaults = { ... };` before its body. These
are merged into every `agent()` / `agentStart()` call — call-site values
override, but `undefined` at the call site falls back to the default.

### Global API

| Function | Returns | Notes |
|---|---|---|
| `agent(prompt, options?)` | `Promise<Value>` | Synchronous (awaited) agent call. Requires `options.agent`. Options include `identity` (boolean — preserves the call's identity through fan-in), `phase` (string — groups the call in the journal), and `assembly` (string — selects the context assembly strategy). |
| `agentStart(prompt, options?)` | Handle | Fire-and-forget dispatch. Handle has `.done`, `.session`, `.result`, `.label`, and `.workingDirectory` after completion; the latter is the host-resolved child directory, including a provisioned isolated worktree. Accepts the same options as `agent()`. |
| `session(options?)` | `Promise<HeldWorker>` | Opens a held worker. Creation options: `agent`, `task`, `tools`, `skills`, `model`, `fallbackModel`, `effort`, `cwd`, `isolation`, `label`. |
| `send(worker, prompt, options?)` | `Promise<Value>` | Runs the held worker's next sequential turn. Turn options: `schema`, `maxAttempts`, `timeoutMs`, `label`. |
| `staleness(worker)` | `Promise<StalenessReading>` | Returns `{stale, idleMs, thresholdMs, route}` from a journalled reading. |
| `close(worker)` | `Promise<void>` | Closes a held worker. Repeated calls with the same JS worker object are no-ops. |
| `wait({ watches?, timerMs? })` | `Promise<WakeResult>` | Races handle completions against a journalled timer. Returns `{wake: "completion", handle, value/error}` or `{wake: "timer"}`. |
| `workflow(name, args?)` | `Promise<Value>` | Child workflow call. Only present when `capabilities.composition` is true. |
| `deliver(payload, options?)` | `Promise<EffectReceipt>` | Delivery effect. Only present when `capabilities.delivery` is true. |
| `postEvent(kind, payload?)` | `Promise<Value>` | Appends to the run's durable pending-event queue. Kind: `"completion"`, `"failure"`, or `"escalation"`. |
| `gate(options?)` | `Promise<Value>` | Wake-gate evaluation. Options: `criteria` (string), `model` (optional override). |
| `deliverEvents(upToSeq)` | `Promise<Value>` | Stamps pending events as delivered up to a sequence number. |
| `parallel(thunks)` | `Promise<Array>` | `Promise.all` with per-thunk worker-failure swallowing (a failed worker collapses its slot to `null`). A pre-spawn author error propagates and fails the run. |
| `pipeline(items, ...stages)` | `Promise<Array>` | Per-item stage chain; a failed worker collapses that item to `null` at the failing stage. A pre-spawn author error propagates and fails the run. |
| `budgetSnapshot()` | `Promise<Value>` | Journalled budget reading. Returns `{spent, remaining, working_remaining, closing_remaining, winding_down, per_route, usage_unavailable_calls}`. Per-route balances appear when `per_route` budgets are configured. |
| `budget.spent()` | `number` | Synchronous, not journalled — NOT safe for branching (replay sees end-state values). |
| `budget.remaining()` | `number \| undefined` | Synchronous, not journalled — NOT safe for branching. |
| `log(message)` | — | Records via `host.record("log", ...)`. `console.log/warn/error` alias this. |
| `phase(title)` | — | Records via `host.record("phase", ...)`. |

### Agent model fallback

`agent()`, `agentStart()`, and `session()` accept `fallbackModel`, a catalogue
model id. For a held worker it is a creation-only option, fixed with the rest of
the worker's execution envelope. The host uses it only when the requested model
is unavailable or the provider refuses or fails it before any output; schema
exhaustion and failures after output do not fall back. A completed `agent_call`
result records `served_model`, the catalogue model that actually served the
call. Continuation calls ignore `fallbackModel` and never make a fallback
attempt; they use the worker's persisted model selection, including a fallback
selected at creation.

### Structured output extraction

When a call supplies `schema`, the worker's response text is scanned for
JSON values — bare, inside a Markdown fence, or preceded by prose. Every
non-overlapping JSON value is validated against the schema in source order;
the final schema-valid value wins. A model may reason through provisional
values before delivering its result, so extraction tolerates surrounding
text and intermediate candidates. When no value passes validation after
`maxAttempts` retries, the call returns `null` (for `agent()`) or the
handle's `.result` is `null` (for `agentStart()`).

### Held workers

Use a held worker when successive tasks need the same worker's accumulated
conversation:

```js
const reviewer = await session({
  agent: "reviewer",
  skills: ["source-checking"],
  model: "careful",
});

await send(reviewer, "Inspect the evidence.");
const reading = await staleness(reviewer);
if (reading.stale !== true) {
  await send(reviewer, "Now assess the conclusion.", { maxAttempts: 2 });
}
await close(reviewer);
```

`session()` fixes `agent`, `task`, `tools`, `skills`, `model`, `effort`,
`cwd`, and `isolation` at creation. The host resolves an omitted model and
effort at that point, so later configuration changes do not move the worker to
a different route. Script defaults contribute only creation fields when
opening a worker; per-turn defaults such as `maxAttempts` do not make
`session()` fail.

`send()` accepts only `schema`, `maxAttempts`, `timeoutMs`, and `label` for the
new turn. A worker takes one turn at a time; a concurrent `send()` is refused
rather than queued. Each successful turn updates the worker's continuation
token and returns the turn output. Sending to a closed worker is refused.
Requested grants are fixed, but effective authority is re-derived at every
turn.

`staleness()` reports idle time since the last completed turn and compares it
with `worker_staleness_threshold_ms` on the worker's provider route. Test
`reading.stale === true` before treating the worker as stale. `null` means the
route has no threshold and the verdict is unknown; it does not mean fresh.
The reading is journalled, so replay returns the value on which the original
script branched.

Call `close()` on the success path so the journal records intent. Run-ending
return, throw, and cancellation also close every worker still open. Parking is
not a run-ending path: a parked run retains its workers for replay and resume.

### Identified fan-in

Setting `identity: true` on an `agent()` or `agentStart()` call opts it
into an envelope that preserves the call's identity through fan-in. When
a call with `identity: true` exhausts its schema validation attempts, it
returns a failure envelope carrying the call's `label` and `phase` rather
than collapsing to `null`. Inside `parallel()` and `pipeline()`, the
envelope is preserved so the script can distinguish which leg failed and
why.

### Pre-spawn author errors

Certain call options are validated before dispatch. An unknown or invalid
agent option (`InvalidAgentOptionError`), an invalid prompt
(`InvalidAgentPromptError`), a schema that does not compile
(`InvalidAgentSchemaError`), an unresolvable `workflow()` reference
(`WorkflowReferenceError`), and non-JSON `workflow()` arguments
(`WorkflowArgumentsError`) are all raised before the call spawns anything.
Inside `parallel()` or `pipeline()` a pre-spawn author error propagates
out and fails the run — it is never swallowed as a `null` worker slot.

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
| `cut_short` | The run's own end stopped this entry's work (distinct from `failed`, which is the work's own failure). Treated as a terminal record on replay. |
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
| `per_route` | `BTreeMap<String, WorkflowRouteBudget>` | Per-provider-entry token ledgers. A model resolves to the provider entry named by its catalogue configuration; calls on one entry never consume another entry's allowance. |
| `max_total_tokens` | `Option<u64>` | Fallback token ceiling for provider routes without a `per_route` entry. |
| `reserved_closing_tokens` | `Option<u64>` | Tokens held back from ordinary work for calls after wind-down. Part of, not in addition to, `max_total_tokens`. |
| `max_concurrent_agent_calls` | `Option<u32>` | Concurrency semaphore size (default 4). |

### Per-route budgets

Each entry in `per_route` names a provider entry and carries its own
`WorkflowRouteBudget`:

| Field | Type | Purpose |
|---|---|---|
| `max_total_tokens` | `Option<u64>` | Token ceiling for calls resolved to this provider entry. |
| `reserved_closing_tokens` | `Option<u64>` | Closing reserve for this route. Requires `max_total_tokens` on the same entry and cannot exceed it. |

Every agent call is admitted against the ledger governing its resolved
provider route. When a route has a `per_route` entry, calls on that route —
including declared fallbacks that resolve to it — are admitted against
that entry's ceiling. Routes without a `per_route` entry fall back to the
top-level `max_total_tokens`.

When usage is unavailable for a call (the provider reported no token counts),
the closing is charged only to the route the call resolved to. Route
exhaustion refuses new calls on that route with `BudgetExceededError`
without affecting other routes.

`budgetSnapshot()` returns per-route balances in a `per_route` map when
per-route budgets are configured. Each entry reports `{spent, remaining,
closing_remaining, usage_unavailable_calls}`. The terminal budget record
written at run completion carries the same per-route shape.

Validation at `start_run`: every `per_route` key must name a provider entry
in the topic's configuration; `reserved_closing_tokens` on a route requires
`max_total_tokens` on the same route and cannot exceed it.

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

## Run detail

The run detail (`WorkflowRunDetailResponse`) carries the run record, the
full journal, and wake metrics. Each `agent_call` journal entry carries a
`WorkflowAgentCallDetail` with the fields below.

### Dispatch record

`WorkflowAgentCallDispatch` is written before any provider work begins, so
a worker that dies mid-run still leaves evidence of the call it was serving.

| Field | Type | Notes |
|---|---|---|
| `prompt` | `String` | The prompt the call was dispatched with. |
| `agent_definition_id` | `Option<String>` | The agent definition used. |
| `requested_model` | `Option<String>` | The model the call named, before the turn resolved it. |
| `requested_effort` | `Option<Effort>` | The effort the call named, before the turn resolved it. |
| `placement` | `Option<WorkflowWorkerPlacementRecord>` | Where the worker runs (`path`, `can_write`), once provisioned. |
| `grants` | `Option<Vec<String>>` | Call-scoped tool grants after intersection with the dispatching context's authority. `None` means the call narrowed nothing. |
| `attempts` | `Vec<WorkflowAgentCallResolution>` | Every attempt the call ran, oldest first. Each records the resolved `model`, `effort`, `session_id`, `thread_id`, and `turn_id`. A call interrupted and re-dispatched by replay has more than one attempt. Empty until the first turn resolves. |

The dispatch record is absent only for calls journalled before this field
existed.

### Provider rounds

`provider_rounds` on the call detail is the sequence of provider request
rounds this call's turn ran, oldest first. Each `WorkflowProviderRound`:

| Field | Type | Notes |
|---|---|---|
| `attempt` | `u32` | Which attempt of the call ran this round, counting from 1. |
| `round` | `u32` | The round within this attempt. |
| `started_at` | `DateTime<Utc>` | When the request went to the provider. |
| `completed_at` | `Option<DateTime<Utc>>` | When the response arrived, or when interruption became final. `None` for outstanding rounds. |
| `outcome` | `WorkflowProviderRoundOutcome` | What came back. |

`WorkflowProviderRoundOutcome` variants:

| Variant | Meaning |
|---|---|
| `outstanding` | The request is in flight — no response has arrived. |
| `responded { tool_calls }` | A response arrived; `tool_calls` is the number of tool calls (zero when the model answered). |
| `failed { message }` | The provider request failed. |
| `interrupted` | The request went out but nothing can answer it any more — the attempt stopped. Distinct from `outstanding`. The round's end time is when that became true. |

`provider_rounds` is `None` on the live entry event (sent as the entry
settles); the run detail always carries them. An absent round record is
never read as a call that made no provider request.

### Usage

`WorkflowCallUsage` is a tagged enum on the `availability` field:

| Variant | Fields | Meaning |
|---|---|---|
| `known` | `input_tokens`, `output_tokens` | The provider reported token counts. |
| `unavailable` | `reason` | Usage is not known. |

`WorkflowUsageUnavailableReason` values: `provider_reported_no_usage` (the
turn completed but the provider reported no counts), `call_did_not_settle`
(the call never settled, so no usage was collected). A call whose provider
reported no usage is not a call that spent nothing — the two are different
facts and the record keeps them apart.

### Termination

`WorkflowWorkerTermination` is a tagged enum on the `termination` field:

| Variant | Meaning |
|---|---|
| `outstanding` | The worker is still working. |
| `settled` | The worker ran to its own end. |
| `failed { message }` | The worker's own work failed. |
| `cut_short { cause, detail }` | The run ended while this worker was still working. `detail` (optional) carries the settling error written over the worker when the terminal account has one to preserve. |
| `unsettled` | Nothing ever recorded an outcome — the run's end left no process alive to settle this worker. |

`WorkflowCutShortCause` values: `run_cancelled`, `run_returned`,
`run_failed`, `run_wound_down`.

### Liveness

`WorkflowWorkerLiveness` on the call detail:

| Field | Type | Notes |
|---|---|---|
| `started_at` | `DateTime<Utc>` | When the worker's dispatch window opened — captured between requesting and entering provider dispatch. |
| `first_evidence_at` | `Option<DateTime<Utc>>` | When the first provider evidence arrived. `None` if the worker settled without producing any evidence (e.g. refused at admission or cut short before a response). |

Liveness stamps are persisted at settlement and appear on the live run
surface during execution.

### Worktree completion

`WorkflowWorktreeCompletion` on the call detail and the `agent_call`
journal result:

| Field | Type | Notes |
|---|---|---|
| `base_commit` | `String` | The commit the isolated worktree was cut from. |
| `tip_commit` | `String` | The commit at the worktree's HEAD when the worker settled. |
| `changed` | `bool` | Whether the worktree has commits beyond `base_commit`. |
| `removed` | `bool` | Whether the worktree was cleaned up after settlement. |

The worktree completion is resolved after the worker settles: the host
calls `worktree_host.finish()` to read the final Git state, then records
it in the journal result.

## Terminal account

When a run ends, the gateway assembles a `WorkflowRunTerminalAccount` —
a single record of what ended it, where every worker stood, and what
work survived. It is absent while the run is still going and present
from the moment the run reaches a terminal state.

Primary source: `crates/swage-protocol/src/client/workflow.rs`
(`WorkflowRunTerminalAccount` and its children);
`crates/swage-gateway/src/workflow.rs` (`terminal_account_from_journal`,
`record_terminal_account`, `retained_placements`).

### Ending

`WorkflowRunEnding` is a tagged enum. Every ending path names itself, so
no reader infers a run's end from silence or the absence of a result:

| Variant | Meaning |
|---|---|
| `returned` | The script returned its result. |
| `threw { error }` | The script threw, or the engine failed the run. |
| `cancelled` | An operator or a parent run cancelled it. |
| `wound_down { error }` | The working token allowance ran out and the run ended under wind-down. |
| `interrupted_without_resume { reason }` | The gateway died with the run in flight and the run could not be resumed. `reason` says why the resume was refused. |

### Worker partition

`WorkflowWorkerPartition` sorts every worker the run dispatched into
exactly one list:

| List | Standing |
|---|---|
| `done` | The worker settled successfully. |
| `failed` | The worker's own work failed. |
| `cut_short` | The run's ending stopped the worker while it was still working. |
| `unsettled` | Nothing ever recorded an outcome — the run's end left no process alive to settle it. |

Each `WorkflowWorkerAccount` entry carries:

| Field | Type | Notes |
|---|---|---|
| `journal_entry_id` | `Uuid` | The journal entry for this worker's call. |
| `seq` | `u64` | Journal sequence number. |
| `label` | `Option<String>` | The call's label, when one was given. |
| `agent_definition_id` | `Option<String>` | The agent definition used. |
| `placement` | `Option<String>` | The directory the worker ran in — kept on disk when the run did not return, so its work is still there. |
| `worktree` | `Option<WorkflowWorktreeCompletion>` | The final Git state recorded when an isolated worker settled. |
| `termination` | `Option<WorkflowWorkerTermination>` | How the worker stopped. Absent for a worker that settled on its own account. |

### Partial results

`WorkflowPartialResults` is present exactly when the run ended without
its script returning. It surfaces the completed workers' output as
recoverable work — never the run's result — with a `reason` saying what
stopped the run from producing one.

Each `WorkflowPartialResult` carries:

| Field | Type | Notes |
|---|---|---|
| `journal_entry_id` | `Uuid` | The journal entry for this worker. |
| `seq` | `u64` | Journal sequence number. |
| `label` | `Option<String>` | The call's label. |
| `output` | `Value` | The completed worker's output. |
| `placement` | `Option<String>` | Where the work was done, still on disk. |

### Where the account is read

Three surfaces expose the terminal account:

1. **Run detail response** — `WorkflowRunDetailResponse` carries the
   `WorkflowRun`, which includes `terminal_account`. Returned by
   `GET /api/workflow-runs/{id}` (`server.rs:125`).
2. **Run view** — the frontend route `/workflow-runs/{id}` renders
   `WorkflowRunView.svelte`, which reads `run.terminal_account` and
   displays the ending, worker partition, and partial results in one
   coherent view alongside the journal.
3. **`workflow_run_account` tool** — a gateway tool
   (`crates/swage-gateway/src/tool/workflow_run_account.rs`) that reads
   the terminal account for one run. Scoped to the caller's session
   topic: a run in another topic reads as absent. Returns
   `{ run_id, script_name, state, ended: true, account }` when the run
   has ended, or `{ ended: false, note }` when it has not.

### Worker directory retention

When a run ends without returning, the directories its account names are
retained: the gateway claims them from the worktree host before
recording the account, so from the moment a reader can find them nothing
takes them away — not this gateway's shutdown, not the next one's
startup sweep.

A run that returned reclaims its workers' directories with the rest of
its scaffolding. The `retained_placements` function returns an empty
list for a `returned` ending and every named placement otherwise.

A stale cancellation — a parked cancellation that loses to a resumed
run — withdraws only its own retention claim. When the resumed run
returned, the stale cancellation's withdrawal restores the normal
reclaim path; when the resumed run failed, the winner's retention claim
is preserved.

## Carried workflows

A workflow script may be carried by a skill rather than authored as a
topic row. The `WorkflowScript` wire type carries a `carrying_skill`
field (`Option<String>`) naming the skill that provides the workflow
when it is not a standalone authored row. A carried workflow is
immutable in place; an operator can shadow it by authoring a same-named
workflow in the topic.

Each shipped skill carries its own workflows: the development-process
skills — commission, maestro, assembly, and shakedown — carry their
pipelines and waves, and the research skill carries its sweep. When a
skill is active on a session, its carried workflows become available, and any available
workflow starts by name through `workflow_run`. The research skill
(`shipped/skills/research/`) carries `research-sweep`.

### Running a research sweep

Activate the research skill on a session (via `activate_skill`), then
dispatch its carried workflow with `workflow_run`:

```json
{ "name": "research-sweep", "args": { "topic": "…", "purpose": "…", "maxSources": 8 } }
```

- `topic` — what to research, as the user phrased it.
- `purpose` — what the findings must serve; the synthesis is judged
  against this.
- `maxSources` — optional ceiling on sources gathered (default 8).

The sweep runs unattended: sources are gathered as immutable
observations, an organising pass signposts what landed using
`knowledge_curate_orientation` (which can author, supersede, and retire
orientation cards), and a synthesis pass delivers cited findings. The
sweep's roles — scout, gatherers, organiser, synthesist — are bare
dispatches inside the script, not standalone agent definitions.

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
