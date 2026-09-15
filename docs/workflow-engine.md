---
description: How Swage runs workflow scripts — lifecycle, journalling, budget management (including per-route), run detail, terminal account (how a run ended, worker standings, partial results), durable parking, and failure handling
---

# Workflow engine

Swage's workflow engine runs front-loaded JavaScript scripts that orchestrate
agent work. A script defines a run's entire plan — which agents to call, in
what order, with what structure — and the engine executes it inside a sandboxed
QuickJS runtime on a dedicated OS thread. Every non-deterministic host call is journalled, so a crashed or
restarted run replays from its journal and picks up where it left off.
The one deliberate exception is the synchronous budget accessors
`budget.spent()` and `budget.remaining()` — these are live reads against
an atomic counter, not journal entries, so they reflect end-state values
during replay and must not be used for branching. Where a script needs a
replay-safe budget reading, `budgetSnapshot()` provides a journalled
snapshot that replays identically.

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
budget, checks an active-run cap of four, and persists the
run record before launching. The cap is a soft admission gate on `start_run`
callers: child runs created through composition and resumed runs bypass it,
and concurrent `start_run` calls race the async mutex without serialisation.

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

A script may open with a `export const meta = { ... };` header declaring
portable metadata: `name`, `description`, `task` (an opaque identity for
grouping runs of the same task), and `argumentSchema` (a JSON Schema
governing the workflow's arguments). The meta header is parsed at save time;
the caged runtime never sees it.

A script may open with `export const defaults = { ... };` to set default
options (agent name, model, schema) merged into every `agent()` and
`agentStart()` call. Call-site values override; unset fields fall through to
the default.

### Agent calls

`agent(prompt, options)` makes a synchronous (awaited) call and returns the
agent's output. When a call supplies `schema` and every validation attempt is
exhausted, it returns `null`; provider, turn, budget, and other call failures
still throw. `agentStart(prompt, options)` dispatches a call and returns a
handle immediately — the handle's `.done`, `.session`, `.result`, and
`.workingDirectory` fields update on completion, with the same `null` result
for schema exhaustion. `workingDirectory` is the host-resolved directory where
the child ran, including a provisioned isolated worktree. `wait({ watches,
timerMs })` races a set of handles against a journalled timer: the first handle
to complete or the timer to fire wins, and the result says which.

The optional `tools` array narrows a fresh worker call to the named tools. It
is intersected with the worker's effective grants and therefore cannot grant
new authority; continuations retain the allow-list chosen at worker creation.

Setting `identity: true` on a call opts it into an envelope that preserves the
call's identity through fan-in — when schema validation is exhausted, the
failure returns the call's `label` and `phase` rather than collapsing to `null`
inside `parallel()` or `pipeline()`. The `phase` option groups calls in the
journal, and `assembly` selects the context assembly strategy for the worker.

### Held workers

Use `session()` when several stages should return to the same worker and keep
its accumulated conversation. Creation fixes the worker's agent, task, tools,
skills, model, effort, working directory, and isolation. The model and effort
are resolved when the worker opens, so a later settings change cannot move the
conversation to another route. Per-turn controls remain separate:

```js
const reviewer = await session({ agent: "reviewer", model: "careful" });
await send(reviewer, "Inspect the evidence.");

const reading = await staleness(reviewer);
if (reading.stale !== true) {
  await send(reviewer, "Now assess the conclusion.", { maxAttempts: 2 });
}
await close(reviewer);
```

`send()` accepts `schema`, `maxAttempts`, `timeoutMs`, and `label`. Turns are
sequential: a second turn cannot start while the worker is busy. Effective
authority is checked again for every turn even though the requested tools and
skills stay fixed.

`staleness()` returns `{stale, idleMs, thresholdMs, route}`. It measures idle
time from the last completed turn against the worker route's configured
threshold. A `stale` value of `null` means the route has no threshold, so the
verdict is unknown rather than fresh. The reading is journalled and therefore
replays identically.

Call `close()` when the work is finished. Swage also closes open workers when
the run returns, throws, or is cancelled; parking retains them so the run can
resume. Repeated `close()` calls with the same worker object are harmless.

### Structured output extraction

When a call supplies `schema`, the worker's response is scanned for JSON
values — bare, inside a Markdown fence, or preceded by prose. Every
non-overlapping JSON value found in the text is validated against the schema
in source order, and the final schema-valid value wins. A model may reason
through provisional values before delivering its result, so the extractor
tolerates surrounding text and intermediate candidates rather than requiring
the response to be pure JSON. When no value passes validation after the
configured retries (`maxAttempts`), the call returns `null` for `agent()`, or
the handle's `.result` is `null` for `agentStart()`.

### Model fallback

`agent()`, `agentStart()`, and `session()` accept `fallbackModel`, a catalogue
model id. For a held worker it is a creation-only option, fixed with the rest
of the worker's execution envelope. It is used only when the requested model
is unavailable or the provider refuses or fails it before output; schema
exhaustion and failures after output still return the established `null` result
or throw. The completed call record's `served_model` names the model that
actually served the call. Continuation calls ignore `fallbackModel` and never
make a fallback attempt; they use the worker's persisted model selection,
including a fallback selected at creation.

### Composition

`workflow(name, args)` calls a child workflow. It is only available when the
run's `composition` capability is enabled, and child runs always have
composition disabled — bounding nesting to one level.

### Utilities

`parallel(thunks)` runs thunks concurrently; a thunk whose worker fails
collapses to `null` rather than aborting the batch. `pipeline(items,
...stages)` chains stages per item with no cross-item barrier — an item whose
worker fails stops at its failing stage while others continue.

Author errors are the exception to that swallowing. An unknown or invalid
agent option (`InvalidAgentOptionError`), an invalid prompt
(`InvalidAgentPromptError`), a schema that does not compile
(`InvalidAgentSchemaError`), an unresolvable `workflow()` reference
(`WorkflowReferenceError`), and non-JSON `workflow()` arguments
(`WorkflowArgumentsError`) are all raised before the call spawns anything;
inside `parallel` or `pipeline` they propagate out and fail the run, so a
broken call is never reported as a failed worker slot.

## Journal and replay

The journal is the mechanism that makes runs durable. Every host call follows
a write-ahead pattern: an intent row is created in status `started` before the
work begins, and the row is settled to `completed`, `failed`, or `cut_short`
when the work finishes. `cut_short` means the run's own end stopped the
entry's work — distinct from `failed`, which is the work's own failure. If
the gateway crashes between intent and settlement, the row is still `started`
on resume and the work is re-executed.

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
replay. `IdempotencyKey` resends directly without probing. Re-delivery is only safe
when the receiver implements duplicate suppression keyed on the idempotency
key; the strategy assumes this, it does not provide it.
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

### Per-route budgets

Token budgets can be partitioned by provider route. The `per_route` map on
`WorkflowBudget` assigns each named provider entry its own ceiling
(`max_total_tokens`) and optional closing reserve (`reserved_closing_tokens`).
Every agent call is admitted against the ledger for its resolved provider
route: calls on one entry never consume another entry's allowance. Routes
without a `per_route` entry fall back to the top-level `max_total_tokens`.

Declared fallback models that resolve to a routed provider entry are admitted
against that entry's ledger, not the primary model's. When a call's usage is
unavailable, the closing is charged only to the call's own route.

`budgetSnapshot()` returns per-route balances in its `per_route` map when
per-route budgets are configured. Each entry reports `spent`, `remaining`,
`closing_remaining`, and `usage_unavailable_calls`. The terminal budget record
at run completion carries the same per-route breakdown.

At run creation, every `per_route` key must name a provider entry; a route's
`reserved_closing_tokens` requires `max_total_tokens` on the same entry and
cannot exceed it.

### Wind-down

Each new working-partition agent call reserves one equal share of the working
partition per declared `max_agent_calls`, or per `max_concurrent_agent_calls`
slot when no total call count is declared. The reservation is an admission
estimate; the call's actual token usage replaces it at settlement. After
wind-down, closing-partition calls use a larger estimate computed from the full
`max_total_tokens` ceiling rather than the working partition's share — shrinking
the estimate a second time would admit work the closing partition cannot fund.

Wind-down triggers when a new agent call or a journalled `budgetSnapshot()`
finds the working partition's remaining capacity has dropped below that
estimate. At that point:

1. A `wind_down` journal entry is written with a budget reading.
2. The `closing_limit` latches to `spent + reserved_closing_tokens`.

When an agent call triggers the transition, that call is refused with
`BudgetWindDownError`. When `budgetSnapshot()` triggers it, the snapshot
succeeds and returns a reading with `winding_down: true` — it is the next
agent call that is refused, not the snapshot itself.

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
`{spent, remaining, working_remaining, closing_remaining, winding_down,
per_route, usage_unavailable_calls}` — and is the only budget function safe
for branching, because replay returns the recorded reading. The `per_route`
field appears when per-route budgets are configured. The synchronous
`budget.spent()` and `budget.remaining()` read live counters that reflect
end-state values during replay, so branching on them would produce
non-deterministic results.

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
failure to its journal entry. The host returns that reference structurally; the
engine preserves it in a structured error variant, and the terminal-state
handler uses it to reconstruct the handover.

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

- **Active-run cap**: `start_run` checks the count of active runs and rejects
  new runs beyond four. Child runs and resumed runs bypass this check.
- **Agent-call semaphore**: each run holds a per-run semaphore (default size 4)
  gating concurrent agent dispatches.
- **In-flight counter**: tracks outstanding agent calls, workflow calls, and
  gate evaluations. The park guard refuses to tear the engine down while this
  counter is above zero.
- **Agent-call counter**: a lifetime count checked against the budget's
  `max_agent_calls` cap.

## Run detail

A run's detail view returns every journal entry with full call-level
information. Each `agent_call` entry carries a call detail whose fields
tell the story of one worker from dispatch to settlement.

### Dispatch record

Every agent call records a dispatch before any provider work begins. The
dispatch captures the prompt, the agent definition, the requested model and
effort (before the turn resolves them), the provisioned placement (directory
and write access), the call-scoped tool grants after intersection with the
dispatching context's authority, and the resolved attempts — one per attempt
the call ran, each stamped with the model, effort, session, thread, and turn
the attempt actually used. A call interrupted by a crash and re-dispatched
on replay records both attempts, so the record keeps the interrupted work
rather than only the attempt that settled.

### Provider rounds

Each call detail carries the provider rounds its turn ran — the sequence of
requests to the model provider. Each round records its attempt number, round
number within the attempt, start and completion timestamps, and outcome. The
outcome distinguishes a response (with a count of tool calls — zero when the
model answered directly), a failure, an interruption (the request went out
but the attempt that made it stopped), and an outstanding request still
waiting for a response. A round the run's end interrupted has a completion
timestamp marking when that became true, not when a response arrived.

Provider rounds are absent on the live entry event (which fires as the entry
settles) but always present in the run detail.

### Usage

A call's token usage is either known (input and output token counts from the
provider) or unavailable with a reason. The two cases are distinct: a call
whose provider reported no usage is not a call that spent nothing. The
reasons are `provider_reported_no_usage` (the turn completed but the provider
sent no counts) and `call_did_not_settle` (the call never settled, so no
usage was collected).

### Termination

A call's termination records how the worker ended:

- **`outstanding`** — the worker is still working.
- **`settled`** — the worker ran to its own end, successfully or not.
- **`failed`** — the worker's own work failed, with a message.
- **`cut_short`** — the run ended while this worker was still working. The
  cause names the circumstance: `run_cancelled`, `run_returned`, `run_failed`,
  or `run_wound_down`. An optional `detail` field carries the settling error
  written over the worker, when the terminal account has one to preserve.
- **`unsettled`** — nothing ever recorded an outcome for this worker — the
  run's end left no process alive to settle it.

The `cut_short` status also appears on the journal entry itself — the entry
settles as `cut_short` rather than `failed`, keeping the run's own end
distinct from a worker failure.

### Liveness stamps

Each call detail carries liveness timestamps: `started_at` marks when the
worker's dispatch window opened (captured between requesting and entering
provider dispatch), and `first_evidence_at` marks when the first provider
evidence arrived. `first_evidence_at` is absent when the worker settled
without producing any evidence — refused at admission, or cut short before a
response. The stamps are persisted at settlement and appear on the live run
surface during execution.

### Worktree completion

When a worker ran in an isolated worktree, the call detail and journal result
carry the worktree's final Git state: `base_commit` (the commit the worktree
was cut from), `tip_commit` (HEAD when the worker settled), `changed` (whether
commits exist beyond the base), and `removed` (whether the worktree was cleaned
up after settlement). The completion is resolved after the worker settles by
reading the worktree's state through the worktree host.

## Terminal account

When a run ends, the gateway assembles a terminal account — a single record
of what ended it, where every worker stood at that moment, and what completed
work survived. The account is absent while the run is still going; it appears
the moment the run reaches a terminal state.

### How a run ended

Every ending path names itself, so no reader has to infer a run's end from
silence or the absence of a result:

- **Returned** — the script returned its result.
- **Threw** — the script threw an error, or the engine failed the run.
- **Cancelled** — an operator or a parent run cancelled it.
- **Wound down** — the working token allowance ran out and the run ended under
  wind-down rather than on its own terms.
- **Interrupted without resume** — the gateway died with the run in flight and
  the run could not be resumed; the reason says why.

### Where every worker stood

The account partitions every worker the run dispatched into exactly one of
four standings: **done** (settled successfully), **failed** (the worker's own
work failed), **cut short** (the run's ending stopped the worker while it was
still working), and **unsettled** (nothing ever recorded an outcome — the
run's end left no process alive to settle it). Each entry names its directory,
its label, and — for workers that ran in an isolated worktree — the final Git
state.

### Partial results

A run that ended without its script returning also carries partial results:
the completed workers' output, recoverable as work rather than the run's
result, with a reason saying what stopped the run from producing one. Each
partial result names the worker, its output, and its directory.

### Reading the account

Three places expose the terminal account:

1. **The run detail** — the `GET /api/workflow-runs/{id}` response carries the
   account on the run record.
2. **The run view** — the web UI route `/workflow-runs/{id}` renders the ending,
   the worker partition, and partial results in one coherent view alongside
   the journal.
3. **The `workflow_run_account` tool** — an agent tool that reads the terminal
   account for one run, scoped to the caller's topic. Useful after a run ends
   without returning, to find what survived and where.

### Worker directory retention

When a run ends without returning, the worker directories its account names
are retained: the gateway claims them from the worktree host before recording
the account, so from the moment a reader can find them nothing takes them
away — not the gateway's shutdown, not the next one's startup sweep. A run
that returned reclaims its workers' directories normally.

A stale cancellation — a parked cancellation that loses to a resumed run —
withdraws only its own retention claim. If the resumed run returned, the
directories go back to the normal reclaim path; if it failed, its own
retention claim is preserved.

## Carried workflows

A workflow script can be carried by a skill instead of being authored as a
standalone row in a topic. In the workflow listing, a carried workflow's
`carrying_skill` field names the skill that provides it. Carried workflows are
immutable in place; an operator can shadow one by authoring a same-named
workflow in the topic.

Each shipped skill carries its own workflows: the development-process
skills — commission, maestro, assembly, and shakedown — carry their
pipelines and waves, and the research skill carries its sweep. When a
skill is active on a session, its carried workflows become available, and any available workflow
starts by name through `workflow_run`. The research skill carries
`research-sweep`.

### Running a research sweep

To run a research sweep, activate the research skill on the session, then
dispatch the carried workflow:

```json
{ "name": "research-sweep", "args": { "subject": "…", "purpose": "…", "maxSources": 8 } }
```

- **`subject`** — what to research, as the user phrased it.
- **`purpose`** — what the findings must serve; the synthesis is judged
  against this, so it should carry the user's actual need.
- **`maxSources`** — optional ceiling on sources gathered (default 8).

The sweep runs unattended: sources are gathered as immutable observations,
an organising pass signposts what landed using `knowledge_curate_orientation`
(which can author, supersede, and retire orientation cards), and a synthesis
pass delivers cited findings. The sweep's internal roles — scout, gatherers,
organiser, synthesist — are bare dispatches inside the script, not standalone
agent definitions.
