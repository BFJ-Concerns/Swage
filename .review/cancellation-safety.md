# Cancellation and abandonment safety

## The concern

A Rust future can be dropped at any `.await`. When a future is *abandoned* —
a losing `tokio::select!` branch, a fired `tokio::time::sleep`/`timeout`, a
`JoinHandle::abort()`, or a shutdown — partway through a sequence of writes that
were meant to be indivisible, it leaves shared or persisted state half-applied.
The compiler cannot see that those writes were meant to be atomic; that
invariant lives in the author's head. There is no compile error, no panic, and
no failing test unless one was written specifically to force the cancellation.
The bug surfaces only intermittently, under load, timeout, or shutdown, as an
inconsistent Session→Thread→Turn state machine or a dangling persisted row.

This brief judges whether async code keeps its shared and persisted state
consistent across every point at which its future could be abandoned. It is not a ban on `select!`, `timeout`, or `abort` — those
are used correctly throughout — and it is not about data races or `Send`/`Sync`,
which the compiler already enforces.

## What good looks like

The repo has an established discipline; all code should match it.

- **Cooperative cancellation, not abandonment.** Long-running work threads a
  `tokio_util::sync::CancellationToken` and checks it at a chosen-safe point —
  the agent turn loop `select!`s on `cancel_token.cancelled()` and, on that
  branch, leaves storage in a consistent terminal state before returning. The
  point is that the author chose *where* the future may stop, rather than
  letting it stop wherever it happened to suspend.
- **Cancel-then-drain on shutdown.** `shutdown_active_turns` cancels the active
  turn tokens and then sleeps briefly so each task can write its own final
  state — it deliberately does not `abort()` the tasks out from under their
  writes. That is the model: signal cancellation cooperatively; don't drop
  futures mid-mutation.
- **Await-then-commit.** Where a handler must change several pieces of state, it
  does its `.await`s first, then commits the change with as little awaiting
  between the first and last write as possible — ideally a single synchronous
  step or one transaction. The gateway approval wait is close to this shape:
  it `select!`s on decision/timeout/cancel and then each arm, including the
  cancelled arm, commits a consistent outcome.
- **Recovery is a net, not a licence.** Startup reconciliation
  (`recover_orphaned_turns`) repairs turn state left dangling by an unclean
  exit. Rely on it as a backstop, not as permission to leave in-flight writes
  non-atomic — it only reconciles the state it knows about.

## What to flag

The core fingerprint — flag when all three hold:

1. an operation performs **more than one** mutation of shared or persisted state
   (any pair of: `update_turn_state`, `update_state` on a thread,
   `update_approval_state`, `update_scheduled_run_state`, `create_*`/`update_*`
   storage writes, a channel send that commits a decision, a write through a
   shared `Mutex`/`RwLock`), **and**
2. there is an `.await` between the first and last of those mutations, **and**
3. the operation can be abandoned — it is inside, or reachable from, a
   `select!` arm, a `timeout`/`sleep` race, a task whose `JoinHandle` is
   `abort()`ed, or any cancellation/shutdown path.

When you find a match, the question to answer is: *if the future is dropped at
each `.await` between the first and last write, is every observable state
combination still consistent?* If you cannot convince yourself it is, flag it.

Concrete signals in this codebase:

- **A paired turn/thread write split by an `.await`** — e.g. setting a turn to a
  terminal state and then resetting its thread to `Idle` as two separate awaited
  calls. If the future is dropped between them, the turn is terminal but the
  thread stays `Processing` and rejects future turns. Safe only if no
  abandonment can occur between the two writes; say why if you conclude it's
  safe.
- **A background loop that `abort()`s rather than cancels.** A task spawned and
  later torn down with `JoinHandle::abort()` (rather than a cooperative token)
  whose body performs a multi-write persisted sequence across awaits. `abort()`
  drops the body at whatever await it is suspended on; any half-written sequence
  with no reconciliation on the next run is a hazard. The scheduled-run and
  ingestion loops are the shape to scrutinise.
- **A lock guard held across an `.await`.** A `tokio::sync::Mutex`/`RwLock`
  guard taken with `.lock().await` / `.write().await` and still held while a
  later `.await` runs, with a mutation in between. This compiles and Clippy's
  `await_holding_lock` does not fire on async guards, so there is no signal:
  if the future is dropped while the guard is held mid-mutation, the next holder
  observes a partial write. Flag unless the critical section is provably
  drop-safe.
- **A future built and then dropped without `.await` or `spawn`** — constructed
  for its effect but never driven, so the effect silently never happens.
- **`.unwrap()`/`.expect()` reachable from a raced or timed-out arm**, where the
  panic only triggers on the abandonment path and so escapes ordinary tests.

A site that matches the fingerprint but carries **no deterministic
cancel-at-await test** is itself a finding. The oracle for this concern is a
test that drives the operation, forces cancellation at each await point, and
asserts the persisted state is still consistent — there is no other reliable
signal. The storage-failure injection pattern in `swage-agent/src/cancel.rs`
(a wrapper storage that fails a chosen write) is the calibration shape for
writing one. Require such a test for every flagged site and every fix; a fix
without it has not been shown to work.

## Preferred fixes (recommend the highest that fits)

1. **Single-owner / actor.** State owned by one task; others send messages down
   a channel; the owner mutates synchronously between receives. The codebase
   already leans this way — turn work is driven as an `AgentEvent` stream over
   an `mpsc` channel rather than shared mutable state. Extending that boundary
   makes the bug unwriteable. Note the trade: it adds backpressure/deadlock
   considerations — but those are loud and testable, an acceptable swap for a
   silent intermittent hazard.
2. **Await-then-commit.** Restructure so all awaits precede a single
   synchronous commit (one transaction, one atomic swap, one channel send).
3. **Cooperative cancellation.** Where work must be interruptible, thread a
   `CancellationToken` and check it at a safe point instead of permitting an
   `abort()` or a bare `select!` drop — matching the turn loop and
   `shutdown_active_turns`.

## Out of scope

- Data races, `Send`/`Sync`, and memory safety — the compiler already prevents
  these, so they need no review here.
- Whether a primitive *name* appears (`select!`, `timeout`, `abort`,
  `std::sync::Mutex`); a short fully-synchronous critical section is fine. The
  concern is the cross-await write relationship, not the token.
- General correctness or logic bugs that don't arise from abandonment — a wrong
  result that would occur even if the future always ran to completion is not
  this concern.

## Severity

Treat a fingerprint match on a **persisted** state machine (turn, thread,
approval, scheduled-run) reachable from `abort()` or shutdown as serious — it
corrupts durable state and survives the process. A guard-across-await on
in-memory state is usually moderate. A missing cancel-at-await test on an
otherwise-plausible site is a real finding but lower severity than a
demonstrated half-write. Calibrate to whether inconsistent state can actually
escape the operation.
