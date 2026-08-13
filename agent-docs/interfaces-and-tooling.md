# Interfaces and tooling

## Turn context inspector

Each assistant message with a `turn_id` shows a "context" button
(`MessageDisplay.svelte:273`). Clicking it sets the reactive `inspectedTurnId`
and opens `TurnContextInspector.svelte`, which fetches
`GET /api/turns/{id}/context` (`handlers.rs:1239`, route at `server.rs:246`).

The endpoint reads a JSON snapshot from the `context_snapshots` table
(`turn_id TEXT PRIMARY KEY REFERENCES turns(id) ON DELETE CASCADE`,
`report_json TEXT NOT NULL`, `created_at TEXT NOT NULL`) and returns
`{ "turn_id": ..., "report": ... }`. 404 means no snapshot exists for that
turn.

The inspector panel displays:

| Section | Shown when | Content |
|---|---|---|
| Summary | Always | Assembly name, total tokens, element count, duration |
| Elements | Always | Table: element name, source (with optional preview), token count |
| Compaction | `compaction_applied` is non-empty | Element name, requested vs actual strategy, tokens before/after, messages removed |
| Fallbacks | `errors` is non-empty | Error message and which fallback assembly was used |
| Warnings | `warnings` is non-empty | Each warning string |

The Rust-side `AssemblyReport` type lives in `swage-context` (`report.rs:81`),
not `swage-protocol` — it is persisted as opaque JSON, not projected as a
wire type. The frontend defines its own TypeScript transcription
(`api/turns.ts:43`).

`swage-tui` has a terminal-UI equivalent (`ui/inspector.rs:64`), toggled by
keybinding. It shows per-element fill bars and a budget percentage.

## Turn identity in streamed messages

The `turn_complete` WebSocket event carries the turn's `turn_id`
(`client/websocket.rs:134`). The streaming store passes it to
`conversationStore.appendAssistantMessage` at turn-complete
(`streaming.ts:256`), which merges it into the persisted message object
(`conversations.ts:90`). This lets the context-inspector button appear on a
just-completed message without a page reload — the `turn_id` guard at
`MessageDisplay.svelte:273` matches live messages and API-loaded messages
alike.

The `RouterOptions.conversationStore` interface (`router.ts:47`) is a
four-member contract — `activeConversationId`, `checkConversationExists`,
`clearActiveConversation`, `selectConversation` — decoupled from Svelte store
internals.

## Wire-contract architecture

All types that cross a process boundary live in `swage-protocol`
(`crates/swage-protocol/`). One named exception: the `AssemblyReport` shapes
live in `swage-context`, not `swage-protocol` — they are persisted as opaque
JSON with a hand-written frontend transcription (`api/turns.ts:43`); migration
into the protocol crate is queued.

The crate is organised into four surface modules:

| Surface | Module | Anchor type | Scope |
|---|---|---|---|
| Client | `src/client/` (directory, 11 submodules) | `ClientSurface` | Conversations, streaming, approvals, schedules, knowledge, voice, workflows, channels, delivery, spaces, provider sign-in |
| Hosting | `src/hosting.rs` | `HostingSurface` | Presence, operator overview, agent definitions, approvals, configuration |
| Device | `src/device.rs` | `DeviceSurface` | Device registration, remote tool execution |
| Channel | `src/channel.rs` | `ChannelSurface` | Inbound messages (DM/thread), delivery log, channel status |

Each anchor struct references every wire type on its surface, so a single
`schemars::schema_for!` call on the anchor produces the complete JSON Schema
document.

### Projections

| Projection | Output | Mechanism |
|---|---|---|
| TypeScript (ts-rs) | `frontend/src/lib/protocol/` (114 files at ref 2be38e3) | `projections.rs` test exports all four surfaces, then trims trailing whitespace |
| JSON Schema (schemars) | `crates/swage-protocol/schema/{client,hosting,device,channel}.json` | `projections.rs` tests per surface, via the anchor type |

Running `cargo test -p swage-protocol` regenerates all projections in place.
Drift shows as uncommitted changes, caught by the verify gate's
projection-freshness check.

> [!NOTE]
> The doc comment at `lib.rs:9-11` is stale — the committed schema documents
> and the four surface modules are authoritative.

## Verification gate

`xtask` (`xtask/src/main.rs`) orchestrates the verification gate; the
`Justfile` is a thin dispatch into `cargo run -p xtask -- <subcommand>`.
Most logic is Rust, but `xtask` shells out for some checks (e.g.
`scripts/check-tracked-local-state.sh`).

### Recipes

| Recipe | What it runs |
|---|---|
| `just check` | `cargo check --workspace` + frontend type check (`npm run check`) |
| `just test` | Full test suite with failure aggregation (see below) |
| `just verify` (alias: `just ci`) | The gate CI runs; stages below in order |

### Verify stages

Stages 1--4 abort on failure (`?` operator). Stages 5--6 run regardless;
their results are combined.

| # | Stage | Short-circuits? |
|---|---|---|
| 1 | Tracked-local-state check (`scripts/check-tracked-local-state.sh`) | Yes |
| 2 | Format check (`cargo fmt --all -- --check` + `just --fmt --check`) | Yes |
| 3 | Type check (`cargo check --workspace` + frontend) | Yes |
| 4 | Clippy (`-D warnings`) | Yes |
| 5 | Test suite (Rust with `--no-fail-fast`, frontend, desktop smoke) | No |
| 6 | Projection freshness (`git diff --exit-code` on generated files) | No |

When both tests and projections fail, both errors surface in one message.
The test suite aggregates Rust, frontend, and desktop-smoke failures into a
`Vec<String>` rather than aborting on the first.

The verify body runs inside `run_with_final_sweep`, which runs
`cargo sweep` after the gate (even on failure) and reports the artefact
budget.

## Client-side route preservation

The SPA router (`frontend/src/lib/router.ts`) is framework-free: `parseRoute`
pattern-matches the URL; `createRouter` manages `pushState` navigation and
`popstate` listening.

`applyLocation()` (`router.ts:130`) runs on every URL change. When the URL
contains a conversation ID:

- **Already active:** calls `checkConversationExists` — a lightweight `GET`
  that confirms the conversation exists without re-fetching it. On 404,
  clears the active conversation and falls back to root with a not-found
  notice. On transient failure (non-404), the conversation is kept and the
  error is swallowed.
- **Different conversation:** calls `selectConversation`, which fully loads
  it. On 404, falls back to root. On other errors, the error propagates.

The `conversationStore` dependency is injected as a four-member interface
(`router.ts:47`), wired in `App.svelte:170`. `clearActiveConversation`
disconnects the WebSocket before clearing the store, preventing orphaned
connections.

Route changes propagate `notFound` state to the UI, so the "conversation not
found" notice appears for `popstate`-triggered navigations and initial page
loads alike.
