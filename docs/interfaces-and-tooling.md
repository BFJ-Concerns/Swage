---
description: The web UI's context inspector, wire-contract architecture, verification gate, and route handling
---

# Interfaces and tooling

## Turn context inspector

Assistant messages that carry a turn ID show a small "context" link in the web
UI. Older turns and turns that failed before assembly have no turn ID, so no
link appears for them. Clicking the link opens an inspector panel that shows
the assembly report for that turn — a
breakdown of every context element the system included, each element's token cost, whether any compaction was applied, and any warnings or errors the
assembly raised.

The inspector fetches its data from `GET /api/turns/{id}/context`. The gateway
stores each turn's assembly report as a JSON snapshot in the database when the turn runs, so the report reflects what the agent actually saw, not a
reconstruction.

The panel has five sections (the last three appear only when there is something
to show):

- **Summary** — assembly name, total token count, element count, and how long
  assembly took.
- **Elements** — a table listing each context element by name, its source, and
  its token count.
- **Compaction** — when the assembly compacted any element to fit the budget:
  which element, the strategy used, and the token count before and after.
- **Fallbacks** — errors that occurred during assembly, including which fallback
  assembly was used.
- **Warnings** — anything else the assembly flagged.

The "context" link appears as soon as a streamed message finishes — the
streaming store exposes the turn ID when the turn completes, so there is
no need to reload the page for the link to appear. The terminal UI has a
comparable inspector toggled by a keybinding, showing a proportional fill bar
per element.

## Wire-contract architecture

The architecture's rule is that all types crossing a process boundary — HTTP
request and response bodies, WebSocket events, device commands — are defined
once in the `swage-protocol` crate as plain serde types. Every consumer is a
derived projection: Rust crates depend on the protocol crate directly, the
frontend's TypeScript is generated from it via ts-rs, and the canonical
published artefact is the JSON Schema generated via schemars and committed
under `crates/swage-protocol/schema/`.

One known exception remains: the `AssemblyReport` type used by the context
inspector lives in `swage-context`, not `swage-protocol`. It is persisted as
opaque JSON and the frontend defines its own TypeScript transcription rather
than importing a generated projection. Migrating the report shapes into the
protocol crate's wire contract is queued but not yet done.

The crate is organised into four surfaces, each emitting its own schema
document:

- **Client** — conversations, streaming, approvals, schedules, knowledge,
  voice, workflows, channels, delivery, spaces, and provider sign-in. This is
  the largest surface, split across eleven submodules.
- **Hosting** — presence, operator overview, agent definitions, approvals,
  and configuration. The contract between the gateway and its hosting
  operator.
- **Device** — device registration and remote tool execution. The contract
  between the gateway and execution clients that dial in to run tool calls.
- **Channel** — inbound messages (direct and threaded), delivery log, and
  channel status. The contract external integrators speak.

Each surface module has an anchor struct whose fields reference every wire type
on that surface. A single schema-generation call on the anchor produces a
complete JSON Schema document with the full type graph, so adding a new type to
the surface is enough to include it in the schema — no manual registration.

Running `cargo test -p swage-protocol` regenerates all projections (TypeScript
bindings and JSON Schemas) in place. Any drift shows up as uncommitted changes
and is caught by the verification gate.

## Verification gate

Development verification runs through `just verify` (aliased as `just ci`).
The Justfile dispatches into `xtask`, a workspace crate that orchestrates the
gate. Most logic is Rust, but `xtask` shells out for some checks — for
example, the tracked-local-state check runs `scripts/check-tracked-local-state.sh`.

The gate runs six stages in order. The first four abort immediately on failure
— a formatting problem or a type error is not worth running the full test
suite against. The last two always run, and their results are combined:

1. **Tracked-local-state check** — catches runtime state directories that were
   accidentally committed.
2. **Format check** — Rust formatting and Justfile formatting.
3. **Type check** — `cargo check` across the workspace plus the frontend's
   type checker.
4. **Clippy** — with warnings treated as errors.
5. **Test suite** — Rust tests (with `--no-fail-fast` so all crates run even
   if one fails), frontend tests, and the desktop smoke test. Individual
   failures are collected rather than aborting on the first.
6. **Projection freshness** — a `git diff` that verifies the committed
   TypeScript bindings and JSON Schemas match what the tests just regenerated.

When both the test suite and the projection check fail, the gate reports both
sets of errors in a single message. CI runs the same `just verify` recipe, so
a green local run means CI will pass.

Other useful recipes:

- `just check` — a fast workspace type check (Rust and frontend) without
  linting or tests.
- `just test` — the test suite alone, with the same failure-aggregation
  behaviour as the verify gate.
- `just build-dev-embedded` — a debug build with the current frontend baked
  into the gateway binary.

## Client-side route handling

The web UI uses a minimal, framework-free SPA router. It pattern-matches the
browser's URL path against the application's routes (conversations, settings,
root) and keeps the active conversation stable across browser navigation.

When the browser navigates to a conversation URL that is already the active
conversation — the common case for back/forward navigation — the router skips
the full conversation load. Instead, it runs a lightweight existence check to
confirm the conversation has not been deleted. If it has, the router clears
the active conversation and shows a "not found" notice. If the check fails
transiently (a network error, a momentary 503), the router keeps the
conversation in place rather than evicting working state.

For a conversation URL that is not the currently active one, the router loads
it normally. A 404 falls back to the root with a notice; other errors
propagate.

The router's dependency on the conversation store is a narrow injected
interface — four members rather than a coupling to the store's internals.
Disconnecting the WebSocket is part of clearing the active conversation, so
browser navigation to a deleted conversation does not leave an orphaned
connection.
