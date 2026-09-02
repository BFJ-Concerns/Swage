---
description: Network binding, the web UI's context inspector, image attachments, the API error contract, wire-contract architecture with the route manifest and its projections, verification gate, and route handling
---

# Interfaces and tooling

## Network binding

The gateway listens on a single TCP address configured through `ServerConfig`
(`crates/swage-config/src/model.rs`). The two knobs are **host** (default
`127.0.0.1`) and **port** (default `3000`).

### Configuration

Set the address in `config.toml`:

```toml
[server]
host = "192.0.2.30"
port = 3000
```

Or override per invocation with CLI flags:

```bash
swage gateway start --host 192.0.2.30 --port 3000
```

CLI flags take precedence over `config.toml`. Both `--host` and `--port` are
accepted by `gateway start`, `gateway restart`, and `gateway set-defaults`.
The last command saves lifecycle defaults which `start`, `restart`, `status`,
and `stop` reuse; `gateway clear-defaults` removes them.

Detached starts write output to the data root's `gateway.log` and print that
destination after startup. Pass `--log-file PATH` to choose another file, or
`--foreground` to leave the process and output attached to the terminal.

### Bind address resolution

At startup the gateway parses `host:port` into a `SocketAddr` and binds a TCP
listener to it (`startup.rs`). If `host` is not a valid IP address or the port
is already in use, the gateway exits with a diagnostic. The runtime-metadata
file written for CLI discovery records the actual bound address, which may
differ from the configured one when port `0` (OS-assigned) is used.

### Security boundary

The gateway has no authentication layer. Binding to any address other than
loopback exposes the full API — including every configured provider key — to
every host that can reach the port. The intended use is trusted
private-network addresses (a LAN segment, a tailnet, or similar).
Public-internet binding is not supported until authentication exists.

The host machine's own firewall must also permit inbound connections on the
chosen port — a correctly bound gateway is unreachable from the LAN if the
firewall drops the traffic.

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

## Image attachments

A message can include images alongside its text. The REST turn-submission
endpoint and the WebSocket send event both accept an `attachments` array,
where each entry is a tagged content part. The only attachment kind today is
`image_url` — an image addressed by a URL (either `https://` or a `data:`
URL carrying the bytes inline).

The gateway checks whether the configured provider can consume image parts
before anything is persisted. If the provider's protocol does not support
vision, the turn is refused immediately rather than the image being silently
dropped.

Which protocols accept images depends on whether the provider adapter
actually maps image content parts to the wire body. At this tree, only two
protocols claim vision:

| Protocol | Vision | Notes |
|---|---|---|
| Chat Completions (`openai`) | Yes | Maps image parts as typed content arrays |
| OpenRouter | Yes | Uses the same Chat Completions adapter |
| OpenAI Responses | No | Text parts only in the current adapter |
| Anthropic | No | Text parts only in the current adapter |
| Google | No | Text parts only in the current adapter |
| Codex | No | Assembled text only |

When an image is present, the Chat Completions adapter sends the message
content as a JSON array of typed parts rather than a plain string, keeping
text-only messages in the original string form. Stored messages carry their
attachments as records that survive round-trips through the API.

## Secret backends

Every credential the gateway uses — provider API keys, OAuth tokens,
service credentials — resolves through a single secret manager with an
audit event on every access and automatic redaction of resolved values.

Three backends determine where a secret's value lives:

- **Managed** — encrypted at rest in the gateway's own SQLite store. Used by
  the generate-secret path; the create-secret path takes `backend_kind` from
  the caller.
- **Environment** — the secret names an environment variable, read at
  resolution time. This is what `api_key_env` in the provider config uses.
- **File** — the secret names a file on the gateway host, re-read on every
  access so externally refreshed tokens stay current. A JSON pointer
  (RFC 6901) can optionally select a single field from a JSON file rather
  than using the whole file's content.

The file backend is how the gateway governs credentials that a companion
tool manages itself. The Codex CLI, for example, keeps its ChatGPT OAuth
tokens in an `auth.json` file and refreshes them on its own schedule. At
startup, the gateway registers the Codex access and refresh tokens as
file-backed secrets pointing into that document, so they resolve, audit, and
redact through the governed system without being copied out of the CLI's own
store.

## API error contract

Every failed HTTP response on the client surface carries the same shape: a
JSON object with `code` (a machine-readable identifier) and `message` (a
human-readable explanation). The code is the contract a client branches on;
the HTTP status is a mechanical projection of the code, never an independent
decision.

The code vocabulary is defined as an enum in the protocol crate. Each code
names exactly one HTTP status:

| Code | Status | Meaning |
|---|---|---|
| `malformed_body` | 400 | The request body was not valid JSON |
| `forbidden` | 403 | The caller may not perform this operation |
| `not_found` | 404 | The addressed entity does not exist |
| `conflict` | 409 | Collides with existing state |
| `turn_in_progress` | 409 | A turn is already running in this conversation |
| `already_connected` | 409 | The conversation already has a live connection |
| `unsupported_media_type` | 415 | Missing `Content-Type: application/json` |
| `validation_error` | 422 | The body parsed but a field failed validation |
| `sign_in_unsupported` | 422 | The provider offers no sign-in of the requested kind |
| `internal_error` | 500 | An unexpected gateway failure |
| `provider_error` | 502 | The upstream model provider failed |
| `sign_in_failed` | 502 | The provider refused or could not complete sign-in |
| `provider_runtime_unavailable` | 503 | The provider runtime could not be reached |

This contract is exhaustive: every gateway error variant maps to a code, so a
new kind of failure chooses its code at the point it is defined. Body-parsing
rejections — malformed JSON, wrong content type, unrecognised fields — answer
in the same shape, with the message naming the rejected field where
applicable. A client that reads only the JSON body loses nothing the HTTP
status would have told it.

The Rust client parses the body into a typed error variant; the frontend has
an `ApiError` class that extracts the code. The generated TypeScript client
raises `ApiError` on any non-ok response.

## Wire-contract architecture

The architecture's rule is that all types crossing a process boundary — HTTP
request and response bodies, WebSocket events, device commands — are defined
once in the `swage-protocol` crate as plain serde types. Every consumer is a
derived projection: Rust crates depend on the protocol crate directly, the
frontend's TypeScript is generated from it via ts-rs, and the canonical
published artefact is the JSON Schema generated via schemars and committed
under `crates/swage-protocol/schema/`.

The assembly report used by the context inspector is part of the client
surface. Both the stored-context response and the `context_assembled`
WebSocket event use that protocol type, and the frontend imports its generated
TypeScript projection.

The crate is organised into four surfaces, each emitting its own schema
document:

- **Client** — conversations, streaming, approvals, schedules, knowledge,
  voice, workflows, channels, delivery, spaces, settings, materials, recovery,
  context reports, and provider sign-in.
- **Hosting** — presence, operator overview, agent definitions, approvals,
  and configuration. The contract between the gateway and its hosting
  operator.
- **Device** — device registration and remote tool execution. The contract
  between the gateway and execution clients that dial in to run tool calls.
- **Channel** — inbound messages (direct and threaded), delivery log, and
  channel status. The contract external integrators speak.

Each surface module has a manually maintained anchor struct whose fields
reference every wire type on that surface. A schema-generation call walks the
type graph reachable from that anchor. A new top-level wire type must therefore
be added to the surface anchor (unless an existing anchor field already reaches
it) and to the explicit `ts-rs` export list in
`crates/swage-protocol/tests/projections.rs` (unless an existing export already
reaches it).

### Route manifest

The client surface is gaining a route manifest — a single table that names
each covered HTTP route once, with its method, path template, request and
response schema names. The manifest does not yet cover the full client
surface: the space CRUD routes are the first entries, and routes not yet in
the manifest are registered directly on the router without appearing in the
projections below. Two projections are generated from the manifest:

- An **OpenAPI 3.1 document** at `crates/swage-protocol/schema/client-openapi.json`,
  whose `components/schemas` are the client surface's own JSON Schema — so
  the OpenAPI document and the schema never disagree.
- A **generated TypeScript client** at `frontend/src/lib/protocol/client.ts`,
  with one typed function per manifested route over a shared request helper
  that raises the API error contract on failure.

Adding a manifested route is one entry in the manifest. A router test
verifies that every manifest route is actually registered on the gateway's
HTTP router — a manifest entry with no handler fails the test.

### Projections

Running `cargo test -p swage-protocol` regenerates all projections in place:
TypeScript type bindings, JSON Schemas, the OpenAPI document, and the
generated client. Any drift shows up as uncommitted changes and is caught by
the verification gate, but the freshness check cannot detect an omitted type
that was never made reachable, or a route that was never added to the
manifest.

## Verification gate

Development's fast, pre-test static gate is `just check-gate`; the full gate
is `just verify` (aliased as `just ci`). The Justfile dispatches into `xtask`,
a workspace crate that orchestrates the gate. Most logic is Rust, but `xtask`
shells out for some checks — for example, the tracked-local-state check runs
`scripts/check-tracked-local-state.sh`.

The gate runs seven stages in order. The first five abort immediately on failure
— a formatting problem or a type error is not worth running the full test
suite against. The last two always run, and their results are combined:

1. **Tracked-local-state check** — catches runtime state directories that were
   accidentally committed.
2. **Format check** — Rust formatting and Justfile formatting.
3. **Type check** — `cargo check` across the workspace plus the frontend's
   type checker.
4. **Frontend design-token check** — rejects literal visual values in
   components where a semantic design token is required.
5. **Clippy** — with warnings treated as errors.
6. **Test suite** — Rust tests (with `--no-fail-fast` so all crates run even
   if one fails), frontend tests, and the desktop smoke test. Individual
   failures are collected rather than aborting on the first.
7. **Projection freshness** — a `git diff` that verifies the committed
   TypeScript bindings and JSON Schemas match what the tests just regenerated.

When both the test suite and the projection check fail, the gate reports both
sets of errors in a single message. CI runs the same `just verify` recipe, so
a green local run means CI will pass.

Other useful recipes:

- `just check-gate` — the first five static stages above, before any tests.
- `just check` — the narrower Rust and frontend type check only.
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
