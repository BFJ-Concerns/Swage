---
description: How Swage runs workflow scripts — lifecycle, journalling, budget management, durable parking, and failure handling
---

# Workflow engine

Swage's workflow engine runs front-loaded JavaScript scripts that orchestrate
agent work. A script defines a run's entire plan — which agents to call, in
what order, with what budget — and the engine executes it inside a sandboxed
QuickJS runtime on a dedicated OS thread. Every non-deterministic host call
is journalled, so a crashed or restarted run replays from its journal and
picks up where it left off.

## Run lifecycle

A run progresses through these states:

| State | Meaning |
|-------|---------|
| `running` | The script is executing or replaying. |
| `completed` | The script returned a value. |
| `failed` | The script threw an error. A handover record preserves diagnostic context. |
| `cancelled` | An external cancellation stopped the run. |
| `interrupted` | The gateway shut down mid-run. Relaunched automatically on restart. |
| `parked` | A long timer tore the engine down. The wake loop relaunches it when the timer is due. |

Three entry points create runs — an HTTP endpoint with inline source, an HTTP
endpoint referencing a saved script, and a programmatic launcher used by agent
dispatch — and all converge on a shared `start_run` path that validates the
budget, enforces a hard limit of four concurrent active runs, and persists the
run record before launching.

The launch path (`launch_existing_run`) is shared between new and resumed runs.
It loads the journal replay, computes initial token and call counts from
completed entries, spawns the host-command loop, and starts a dedicated OS
thread running the QuickJS engine. When the engine finishes, the spawned
completion task resolves the terminal state — recording the output, the error,
or the handover depending on the outcome.

## Scripts

Scripts run in a stripped-down QuickJS sandbox. The runtime provides JSON, promises,
Map/Set, and regular expressions, but no `Date`, `setTimeout`, `fetch`, or
`process`. `Math.random` throws to prevent non-deterministic branching, since
replay depends on the script producing the same host-call sequence.

A script may open with `export const defaults = { ... }` to set default
options (agent name, model, schema) merged into every `agent()` and
`agentStart()` call. Call-site values override; unset fields fall through to
the default.

### Agent calls

`agent(prompt, options)` makes a synchronous (awaited) call and returns the
agent's output. `agentStart(prompt, options)` dispatches a call and returns a
handle immediately — the handle's `.done`, `.session`, and `.result` fields
update on completion. `wait({ watches, timerMs })` races a set of handles
against a journalled timer: the first handle to complete or the timer to fire
wins, and the result says which.

### Composition

`workflow(name, args)` calls a child workflow. It is only available when the
run's `composition` capability is enabled, and child runs always have
composition disabled — bounding nesting to one level.

### Utilities

`parallel(thunks)` runs thunks concurrently (failing thunks collapse to `null`
rather than aborting the batch). `pipeline(items, ...stages)` chains stages
per item with no cross-item barrier — a failing item stops at its failing
stage while others continue.

## Journal and replay

The journal is the mechanism that makes runs durable. Every host call follows
a write-ahead pattern: an intent row is created in status `started` before the
work begins, and the row is settled to `completed` or `failed` when the work
finishes. If the gateway crashes between intent and settlement, the row is
still `started` on resume and the work is re-executed.

### Sequence numbering

Sequence numbers are assigned in the host-command loop — the deterministic
boundary — not inside spawned tasks. This ensures the journal's ordering is
independent of Tokio's scheduler timing.

### Replay matching

Each journal entry stores an `args_hash`: the SHA-256 of the request's
canonical JSON. On replay, the current request's hash is compared against
the recorded entry. A mismatch produces a `NondeterministicReplayError`,
which surfaces in the script as a `NondeterministicCallError`. This catch
exists because replay depends on the script issuing the same host calls in
the same order — any deviation means the script's state has diverged from
the journal.

### Completion-order preservation

Concurrent host calls (agent calls, workflow calls, gate evaluations) can
complete in any order. The original run records the order in a
`completed_order` column. On replay, completed entries wait until a rank
counter reaches their position, so the script sees the same interleaving it
saw originally. Without this, a script that branches on which call finished
first could diverge on replay.

### Timer journal entries

Timers have a three-state lifecycle. An armed timer creates a `started`
entry. If the timer wins its `wait()` race, `confirm_timer` settles the entry
to `completed`. If a handle completion wins, `abandon_timer` leaves the entry
as `started` — the journal records which leg actually won.

### Effect recovery

Delivery effects carry an `idempotency_key` and an `effect_kind` that
determines the recovery strategy when a `started` effect entry is found on
replay. `IdempotencyKey` resends directly (the key makes re-delivery safe).
`CheckBeforeRedo` probes for evidence of prior delivery first, falling back
to resend only when the probe finds nothing.

## Budget partitions

A run's token budget is carved into two partitions from a single
`max_total_tokens` ceiling:

- **Working partition** (`max_total_tokens - reserved_closing_tokens`): funds
  ordinary agent calls during the run's main work.
- **Closing partition** (`reserved_closing_tokens`): held back for agent calls
  after wind-down, so the script can still perform summary or clean-up work.

The split is configured through `WorkflowBudget`, which also carries
`max_agent_calls` (a hard cap on the number of calls) and
`max_concurrent_agent_calls` (the concurrency semaphore size, defaulting
to 4).

### Wind-down

Wind-down triggers when a new agent call (or a journalled `budgetSnapshot()`)
finds the working partition's remaining capacity has dropped below one per-call
reservation estimate. At that point:

1. A `wind_down` journal entry is written with a budget reading.
2. The `closing_limit` latches to `spent + reserved_closing_tokens`.
3. The triggering call is refused with `BudgetWindDownError`.

After wind-down, new agent calls are admitted against `closing_limit` rather
than `max_total_tokens`. When the closing partition is also exhausted, calls
are refused with `BudgetExceededError`.

### In-flight working calls

Working-partition calls admitted before wind-down but settling after it do not
consume the closing allowance. Their actual token spend extends
`closing_limit`, so the closing partition is not penalised for work the
run had already committed to.

During replay, a working call may settle before the replay cursor reaches the
recorded wind-down transition. A `replayed_working_credit` accumulator holds
this spend until `closing_limit` is constructed. Replay computes the limit from
the journal's completed agent-call token counts before the transition sequence
(not the live spent counter), plus the reserved closing tokens and any
accumulated replay credit.

### Budget reads

`budgetSnapshot()` returns a journalled reading —
`{spent, remaining, working_remaining, closing_remaining, winding_down}` — and
is the only budget function safe for branching, because replay returns the
recorded reading. The synchronous `budget.spent()` and `budget.remaining()`
read live counters that reflect end-state values during replay, so branching
on them would produce non-deterministic results.

## Durable run parking

Long timers park the run to disk rather than holding an engine thread open.
The threshold is one minute (`PARK_TIMER_THRESHOLD_MS = 60_000`); shorter
sleeps run in-process.

### Parking conditions

A timer parks the run only when all three conditions hold:

1. The remaining sleep is at least one minute.
2. No other timer is armed (an armed sibling would die unfired).
3. No host work — agent calls, workflow calls, gate evaluations — is
   outstanding (an in-flight call would be double-executed by wake replay).

When parking fires, the storage layer sets the run to `parked` with a
`wake_at` timestamp, and the `park` cancellation token tears the engine down.
If further timers drain from the queue while parking is in progress, they join:
`wake_at` is lowered to the earliest due time.

### The wake loop

A background loop ticks every five seconds. Each pass lists interrupted runs
and parked runs whose `wake_at` has arrived, and relaunches each by replaying
its journal. The first pass runs immediately at gateway startup — the boot
resume — picking up both stale interrupted runs and any parked runs that
became due while the gateway was down.

### Timer replay

A replayed timer entry still in status `started` re-arms with the remaining
wall-clock time: a four-hour sleep interrupted at hour three owes one hour on
resume, not four.

## Run events and the wake gate

Detached worker children post events to the run's durable pending queue via
`postEvent(kind, payload)`. Three event kinds are accepted: `completion`,
`failure`, and `escalation`. Events are journalled (exactly-once under replay)
and assigned a monotonic sequence number.

### Wake-gate evaluation

`gate({ criteria, model? })` evaluates accumulated pending events against the
script's criteria to decide whether a parked or waiting lead should wake:

1. Pending (undelivered) events are loaded from storage.
2. If none exist, the gate short-circuits with `wake: false`.
3. A model call evaluates the events against the criteria, constrained to
   a 500-token response.
4. The model returns `{wake: bool, reasoning: string}`.
5. On parse failure, the gate fails open (`wake: true`).

The pending events ride along in the response, so a waking script can ground
its next decision without a second read. Gate evaluations are journalled — a
replayed gate returns its recorded verdict.

After acting on a wake, `deliverEvents(upToSeq)` stamps the consumed events as
delivered up to a sequence number, preventing them from triggering future gates.

## Worker session continuity

A worker's session survives across parked and replayed runs through
`AgentSessionRef`, a continuation token carrying `session_id` and `thread_id`.
The flow:

1. The first `agent()` or `agentStart()` call creates a new session and thread.
2. The host returns both IDs in `AgentCallResult`.
3. The engine surfaces them on the `agentStart` handle's `.session` field.
4. The script passes the token back in the next call's `options.session`.
5. The host validates the session was created by this run, then routes the turn
   to the existing session and thread.

Because the tokens come from journalled results, a replayed run rebuilds
identical tokens. The `session` field on `AgentCallRequest` uses
`skip_serializing_if` to omit it when unset — without this, the journal args
hash would change and replay would fail with a non-deterministic-call error.

## Cancellation

Cancellation propagates through a `CancellationToken` installed as the QuickJS
interrupt handler (checked between JS opcodes) and raced against the execution
future. The engine returns `Cancelled` and the run transitions to `cancelled`.

For parked runs, which have no engine to signal, `cancel_run` directly sets
the state to `cancelled` in storage.

## Failure attribution and handover records

When a run fails, a handover record preserves diagnostic context so the next
handler — a retry, a human operator, or a monitoring system — can understand
what happened without re-reading the journal.

### How attribution works

When a spawned agent task fails, it records a `FailedAgentCallRef` linking the
failure to its journal entry. The error message embeds this reference as
`AgentCallReference:{id}:{error}`. The engine captures the reference in a
structured error variant, and the terminal-state handler uses it to look up the
failed call and reconstruct the handover.

### The handover record

The full handover (`classification: "interrupted"`) contains:

- The failed call's journal entry ID, sequence, label, and error.
- Events delivered to the failed call (between the previous agent call and this
  one) and events still pending.
- The journal's last sequence number, giving the next handler a sense of how
  far the run progressed.

Two degraded classifications cover cases where full attribution is not
possible: `interrupted_unattributed` when no agent-call reference was
captured, and `reconstruction_failed` when the reference existed but the
journal read-back could not find the matching entry.

The handover is persisted in `workflow_runs.handover_json` and exposed on the
run's wire representation.

## Concurrency control

Four mechanisms bound concurrent work:

- **Active-run limit**: at most four runs execute concurrently across the
  gateway.
- **Agent-call semaphore**: each run holds a per-run semaphore (default size 4)
  gating concurrent agent dispatches.
- **In-flight counter**: tracks outstanding agent calls, workflow calls, and
  gate evaluations. The park guard refuses to tear the engine down while this
  counter is above zero.
- **Agent-call counter**: a lifetime count checked against the budget's
  `max_agent_calls` cap.
