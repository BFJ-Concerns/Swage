---
description: Network binding, the Settings memory view, operator history, the web UI's context inspector, conversation branching, image attachments and reference admission, surfaces, workspace relinking, delivery targets, devices (registration, the dial-in connection, cancellation, and presence), tool-call recording and replay, turn completion states (incomplete, failed, interrupted, cancelled), hosted documentation, voice capabilities, gateway lifecycle (status and start recovery), the API error contract, wire-contract architecture with the route manifest and its projections, verification gate, route handling, the gateway's built-in tool inventory, and workspace tool output
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
For systemd-managed gateways, the [service restart rules](../README.md#network-binding)
apply instead of per-call overrides.

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

## Settings memory view

The **Memory** tab in Settings lists every memory page — general and
per-topic — and lets you read any page in full. A chat box at the bottom of
the tab changes memory by sending an instruction to the platform's curator.
The curator processes the instruction and returns its reply alongside every
page it changed, shown in the same view so you can see exactly what was
updated.

The curator's background notices link directly to the page they name:
clicking a notice opens the Memory tab with that page selected.

Memory is the only way the operator reads and edits pages from Settings; there
is no direct page-editing interface — every change runs through the curator.

## Operator history

The operator history pane shows a timeline of configuration changes,
variant selections, and assembly decisions, all read from the gateway's
local audit trail.

Each configuration change entry names the changed fields with their prior
and new values. Nested settings appear by their path — for example,
"Context > Assembly" — so you can see at a glance what moved. The
whole prior and new objects are kept collapsed beneath each entry for
the complete picture.

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

When a compaction record carries strategy `succession-degradation`
(wall-triggered succession), the full diagnosis details — which walls were
answered, each model call's stage and usage, the synthesis text, and the turn
a reused diagnosis was written for — are held in the turn's recorded context
report, which `GET /api/turns/{id}/context` returns, but are not shown in the
inspector panel. The report retains compaction decisions from the first
assembly and the successor in pass order, even when their names match.

The "context" link appears as soon as a streamed message finishes — the
streaming store exposes the turn ID when the turn completes, so there is
no need to reload the surface for the link to appear. The terminal UI has a
comparable inspector toggled by a keybinding, showing a proportional fill bar
per element.

## Conversation branching

Any message in a conversation can be branched from, creating a sibling thread
that carries the history up to that message and then diverges. The
conversation's main line is its root thread; its messages are what the
conversation detail returns. A branch's messages are read separately through
the thread endpoint.

Branching and switching happen in the web UI's conversation channel: every
recorded message has a branch action, a thread switcher shows when branches
exist, and the composer sends on whichever thread is active. A branch can be
abandoned — a terminal state that keeps its history and lineage but takes no
further turns. Both branching and abandoning are recorded in the session's
audit trail.

Each branch listing entry shows which message the branch was taken from, a
preview of that message's opening (so a person can tell branches apart), and
the thread's state: `idle`, `processing`, `interrupted`, `completed`, or
`abandoned`.

## Image attachments

A message can include images alongside its text. The REST turn-submission
endpoint and the WebSocket send event both accept an `attachments` array,
where each entry is a tagged content part. The only attachment kind today is
`image_url` — an image addressed by a URL (either `https://` or a `data:`
URL carrying the bytes inline).

The gateway checks two things before anything is persisted: whether the
configured provider supports vision at all, and whether it can resolve the
specific URL form the client submitted. A provider that does not support
vision refuses the turn outright. A provider that supports vision but
cannot resolve the reference — for example, a web URL sent to a Google
provider that only accepts inline data or Google File API URIs — is also
refused, with a message advising a base64 `data:` URL instead. Both checks
return a 422 validation error before the turn reaches the database.

Which protocols accept images, and which URL forms they accept:

| Protocol | Vision | Accepted references |
|---|---|---|
| Chat Completions (OpenAI) | Yes | Any `https` or `data:` URL |
| OpenRouter | Yes | Any `https` or `data:` URL (same adapter) |
| OpenAI Responses | Yes | Any `https` or `data:` URL |
| Anthropic | Yes | Any `https` or `data:` URL |
| Google | Yes | `data:` URLs (carried inline) and Google File API URIs; ordinary web URLs are refused |
| Codex | No | — |

When an image is present, each adapter converts it to the provider's native
wire format — a typed content array for Chat Completions, an `input_image`
part for OpenAI Responses, an `image` block for Anthropic, and `inlineData`
or `fileData` for Google. Text-only messages keep the plain string form.
Stored messages carry their attachments as records that survive round-trips
through the API.

## Provider credentials

A provider entry has two credential fields, checked in order:

1. **`api_key_secret_ref`** — a `secret://` reference resolved through the
   gateway's secret manager. Every access is audited and the resolved value
   is registered for redaction. This is the governed path.
2. **`api_key_env`** — a direct environment-variable read (`std::env::var`)
   at startup. This is the legacy path; the gateway logs a warning when it is
   used. It does not go through the secret manager and is neither audited nor
   redacted.

When both are set, `api_key_secret_ref` wins.

### Secret manager and backends

The secret manager resolves `secret://` references with an audit event on
every access and automatic redaction of resolved values. Three backends
determine where a secret's value lives:

- **Managed** — encrypted at rest in the gateway's own SQLite store. Used by
  the generate-secret path; the create-secret path takes `backend_kind` from
  the caller.
- **Environment** — the secret names an environment variable, read at
  resolution time.
- **File** — the secret names a file on the gateway host, re-read on every
  access so externally refreshed tokens stay current. A JSON pointer
  (RFC 6901) can optionally select a single field from a JSON file rather
  than using the whole file's content.

### Registering a file-backed secret

Create the secret through the API (`POST /api/secrets`), specifying the file
path, the JSON pointer if the file is a JSON document, and
`"provider_key"` as the material kind:

```json
{
  "name": "my-service-token",
  "material_kind": "provider_key",
  "owner_kind": "operator",
  "backend_kind": "file",
  "scope_kind": "provider",
  "scope_id": "my-provider",
  "external_reference": "/path/to/token-file.json",
  "metadata": { "json_pointer": "/tokens/access_token" }
}
```

The response includes a `reference` field (e.g.
`secret://file/a1b2c3d4-e5f6-7890-abcd-ef1234567890`). Point the provider
entry's `api_key_secret_ref` at it to bring the credential under the
governed path:

```toml
[providers.entries.my-provider]
protocol = "openai"
api_key_secret_ref = "secret://file/a1b2c3d4-e5f6-7890-abcd-ef1234567890"
```

### Codex auth store

The file backend is how the gateway governs credentials that a companion
tool manages itself. The Codex CLI keeps its ChatGPT OAuth tokens in an
`auth.json` file and refreshes them on its own schedule. At startup, the
gateway registers the Codex access and refresh tokens as file-backed secrets
pointing into that document, so they resolve, audit, and redact through the
governed system without being copied out of the CLI's own store.

## Surfaces

Surfaces are a session's work surfaces, assembled from ordered module
instances. A surface belongs to the session that created it and stays with
that session's record: another session in the same topic does not see it. An
agent or workflow can write to a surface's modules, and users can annotate
individual modules with notes.

### The shared library

Either party can preserve a useful surface in the shared library, giving it a
name, a description of what it does and when to use it, and tags. Saving
copies the surface's modules as they stand and opens nothing anywhere; the
surface itself is untouched. Any session — in any topic, or without one — can
add a library entry as a new surface of its own, seeded from the entry's
modules and recording the entry as its origin. The audit trail records a
`surface_created` event with the library origin and seeding mode, and saving
a surface to the library records its own `surface_saved_to_library` event. Content travels with an entry
only within the topic it was saved from: added to a session in another topic
or in the general scope, the shape comes over and each module starts empty
for the destination's own data. The copies are the session's own from birth;
later edits on either side do not cross, and deleting an entry leaves every
surface added from it in place.

The library's catalogue lists names, descriptions and tags, never module
content. A topic can designate library entries as its defaults, which open
in each of the topic's new person-opened sessions (sessions with origin
`direct`); schedule runs and other automated sessions do not open default
surfaces, and the general scope designates no defaults. Any other entry is
added on demand.

When a topic is deleted, the entries saved from it remain in the catalogue —
marked with `origin_topic_deleted` — and other topics' default designations
of those entries stay intact. A surface seeded from an entry whose origin
topic has been deleted carries its shape only, because the entry's content
travels to no session once its origin topic is gone. A surface's record of
which entry it was seeded from (`seeded_from_name`) persists even after that
entry is deleted from the library.

The web UI shows the catalogue on the **Library** page, reachable from the
top bar of any session, from the front door, and from the surfaces pane.
From a session the page adds an entry as one of that session's surfaces and
opens it; from the front door it is read-only until a session is in view.
Entries can be removed there. A save or removal — from any session, by the
user or an agent — reaches every open catalogue live, without a reload.

### Annotations

Annotations are notes attached to individual modules of a surface. Each note
carries a body and optional tags. Updating an annotation replaces its body
and tags together; the prior text is preserved in the surface's provenance
trail. Deleting an annotation removes it permanently. The surfaces pane
offers all three: **Add note** on a module, and **Edit** and **Remove** on
each note.

### Surface addresses

An open surface has an address — `/conversations/{id}/surfaces/{surface_id}`
— so a surface can be linked to, bookmarked, and returned to with the
browser's back button. Opening a surface from the pane or adding one from
the library moves the address to it; loading an address opens the surface
it names once the session's surfaces are listed.

Annotation addresses stop at module grain: there is no way to annotate the
surface as a whole or a position inside a module's content.

### Work-surface reconnect

When a WebSocket connection to a surface's work surface drops and reconnects,
the surface state is refetched to pick up any writes that landed while
disconnected. The same channel carries library changes, so a catalogue on
screen refetches when an entry is saved or removed anywhere.

## Workspace relinking

When a workspace directory moves on disk, the attachment can be relinked to
its new path without detaching and re-attaching. Relinking updates only the
path and clears the missing flag; the attachment's label, configured
permissions, trust state, and default status are preserved.

## Delivery targets

A run's output can be routed to one or more destinations. The four target
kinds are: an external channel (Telegram, dev DM or thread), the gateway's
local store (as a log record), a surface owned by a session in the run's
topic, and a webhook URL. Setting `store_local_output` on the routing request adds a
local-storage target alongside any explicit targets.

### No-delivery records

When a schedule run produces no output delivery, the gateway records a
no-delivery entry so the run history accounts for every tick. Three
outcomes produce this record:

- **Gate skip** — the gate model decided no work was needed. The run is
  marked `skipped` with skip reason `gate`.
- **Overlap skip** — a previous run was still executing when this run came
  due. The run is marked `skipped` with skip reason `overlap`.
- **Failure before delivery** — the run failed before delivery was
  attempted. The run is marked `failed` with an error message describing
  the failure.

A run that fails after delivery has already been attempted does not
receive a no-delivery record — the delivery records themselves account
for it.

## Devices

A device registers once, naming the gateway secret its credential resolves
against and the workspace it operates in, then dials into the gateway over a
persistent connection. Nothing listens on the device side: the device opens
the connection, advertises what it can run, and the gateway invokes operations
back over the same socket. The `swage device connect` command runs a machine
as a device.

### Registration and pairing

A device can join in two ways. Direct registration
(`POST /api/devices/register`) takes the device identity, the gateway secret
its credential resolves against, and the workspace it operates in. Pairing
is a two-step alternative: the gateway issues a one-time pairing code
(`POST /api/onboarding/devices/pairing-codes`), and the device redeems it
(`POST /api/onboarding/devices/redeem`) to register and trust itself in one
step. Pairing codes expire after a configurable interval (default ten
minutes).

In both paths the `kind` field defaults to `attached_device` and names the
device type; registering with kind `local` is refused, since a local device
has no remote producer. The same constraint applies to recovery imports, so
the remote-device registry never holds a local entry. What a device can run
is not registered — the device advertises its capabilities each time it
connects.

### The connection

The device dials `GET /api/devices/connect`, a WebSocket endpoint. It
opens with a Hello frame carrying its identity, the shared credential
(resolved against the secret named at registration), and the tools it
can run. The gateway validates the credential and answers Welcome — which
tells the device how often to heartbeat — or Rejected (closing the
connection).

After the welcome the gateway invokes operations over the socket, each
carrying a correlation ID. The device settles each operation with a result,
a failure message, or a cancellation acknowledgement. The gateway can also
cancel an operation it sent, when the turn that requested it is interrupted.

### Presence

Presence begins when the gateway accepts the device's hello — the gateway
records the device as connected before sending the welcome. Subsequent
heartbeats keep the device connected; the gateway marks it stale after three
missed intervals, and disconnected when the WebSocket closes.

An operator can also close a device's connection from the gateway side with
`DELETE /api/presence/devices/{id}`, which disconnects the device
immediately.

Every connect, disconnect, invocation, and settlement lands in the audit
trail under the device's ID.

### Cancellation

When a turn is cancelled, the gateway sends a Cancel frame for each
in-flight device operation. For local shell commands, cancellation kills the
entire process group — the command and all its descendants are stopped
immediately.

## Tool-call recording and replay

Tool calls are recorded in two phases. A record is created when the model
selects the tool call, before execution begins. Once execution completes,
the record is settled with the result. This means a tool call that was
selected but never finished — interrupted by cancellation, a crash, or a
turn that ended before the result could be recorded — has a record with no
result.

At turn startup, unsettled tool calls from a prior interrupted turn are
replayed into the conversation history so the model sees what happened.
Completed tool calls replay with their recorded results. A tool call that
was interrupted or whose result could not be recorded replays with an
explicit unknown-outcome text rather than replaying as though it returned
successfully. A tool call stopped with its turn settles as cancelled. When a
tool-call result cannot be recorded (the settlement write fails), the turn
ends as failed — the result is not silently dropped.

Stopping a turn ends a shell command running inside the sandbox — in both
bubblewrap and fallback modes — and its tool-call record settles as
cancelled.

## Turn completion states

A turn passes through several states as it runs. The two worth knowing
about at the end are:

- **Incomplete** — the reply was cut short by the output token limit. The
  partial text the model produced is kept, and the conversation shows an
  incomplete marker with the cause.
- **Failed** — an error ended the turn. A failed turn keeps its explanation
  and any tool calls it made, visible in the conversation after a reload.
  The web UI offers **Retry** on a failed turn.

An interrupted or cancelled turn also keeps its tool calls and any
explanation text, attached to the request it answered.

Failed shell commands and workflow runs show as failed on their tool
rows, both live and after reload.

## Gateway lifecycle

### Status

`swage gateway status` reports the running gateway's state — PID, address,
uptime, and health — or prints "Gateway is not running" and exits
successfully when no gateway is found. Stale metadata (a PID that is no
longer alive) is cleaned up automatically.

### Start recovery

`swage gateway start` cleans up after a hard kill: if the previous
gateway's process has terminated but its metadata file remains, the stale
file is removed and a fresh start proceeds. If a running gateway has lost
its metadata (for example after a metadata directory was cleaned), the
start command probes the configured address, recovers the metadata, and
reports the gateway as already running rather than starting a duplicate.

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

The report's `provider_visible` object also records the resolved effort for
the turn — what was requested, what the adapter sent, and any clamp or
substitution — so an operator can see how effort was resolved for any past
turn through the `GET /api/turns/{id}/context` endpoint (the web UI's
inspector panel does not yet render this field). See
[Agents and permissions — Effort](agents-and-permissions.md#effort)
for the full resolution rules.

The assembly report used by the context inspector is part of the client
surface. Both the stored-context response and the `context_assembled`
WebSocket event use that protocol type, and the frontend imports its generated
TypeScript projection.

The crate is organised into four surfaces, each emitting its own schema
document:

- **Client** — conversations, streaming, approvals, schedules, knowledge,
  voice, workflows, channels, delivery, topics, settings, materials, recovery,
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
surface: the topic CRUD routes are the first entries, and routes not yet in
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

The gate runs eight stages in order. The first six abort immediately on
failure — a formatting problem or a type error is not worth running the
full test suite against. The last two always run, and their results are
combined:

1. **Tracked-local-state check** — catches runtime state directories that were
   accidentally committed.
2. **Format check** — Rust formatting and Justfile formatting.
3. **Type check** — `cargo check` across the workspace plus the frontend's
   type checker.
4. **Frontend design-token check** — rejects literal visual values in
   components where a semantic design token is required.
5. **Shipped-workflow parse check** — verifies that every shipped workflow
   script parses without error.
6. **Clippy** — with warnings treated as errors.
7. **Test suite** — Rust tests, frontend tests, and the desktop smoke test.
   Individual failures are collected rather than aborting on the first. The
   Rust test runner is selected by the `SWAGE_TEST_RUNNER` environment
   variable: `nextest` runs tests under `cargo nextest run` with a `ci`
   profile (which retries flaky tests) plus a separate doctest stage;
   `cargo` or unset runs `cargo test --workspace --no-fail-fast`.
8. **Projection freshness** — a `git diff` that verifies the committed
   TypeScript bindings and JSON Schemas match what the tests just regenerated.

When both the test suite and the projection check fail, the gate reports both
sets of errors in a single message. CI runs the same `just verify` recipe, so
a green local run means CI will pass.

Other useful recipes:

- `just check-gate` — the first six static stages above, before any tests.
- `just check` — the narrower Rust and frontend type check only.
- `just test` — the test suite alone, with the same failure-aggregation
  behaviour as the verify gate.
- `just build-dev-embedded` — a debug build with the current frontend baked
  into the gateway binary.

## Client-side route handling

The web UI uses a minimal, framework-free SPA router. It pattern-matches the
browser's URL path against the application's routes (the front door,
conversations and their surfaces, the surface library, workflow runs,
settings) and keeps the active conversation stable across browser navigation.

When the browser navigates to a conversation URL that is already the active
conversation — the common case for back/forward navigation — the router skips
the full conversation load. Instead, it runs a lightweight existence check to
confirm the conversation has not been deleted. If it has, the router clears
the active conversation and shows a "not found" notice. If the check fails
transiently (a network error, a momentary 503), the router keeps the
conversation in place rather than evicting working state.

For a conversation URL that is not the currently active one, the router loads
it normally. A 404 falls back to the root with a notice; other errors
propagate. When a scope was in view, the composer stays open in it so the
next message starts a fresh session there; otherwise the front door serves.

The router's dependency on the conversation store is a narrow injected
interface — four members rather than a coupling to the store's internals.
Disconnecting the WebSocket is part of clearing the active conversation, so
browser navigation to a deleted conversation does not leave an orphaned
connection.

## Hosted documentation

The gateway compiles the agent-facing documentation set (the `agent-docs/`
directory at build time) into its binary and serves it read-only. Each
document is addressed by name — the file stem of its source — and the set
is flat, with no subdirectories.

`GET /api/docs` lists the documents with their titles and descriptions;
`GET /api/docs/{name}` returns one document's full Markdown body.  Both
endpoints are in the client route manifest, so they appear in the OpenAPI
document and the generated TypeScript client.

Agents access the same set through the `docs_read` tool: calling it with
no arguments lists the documents, and calling it with a name reads one. A
long document pages by byte offset, the same continuation shape
`url_fetch` uses.

The compiled set is byte-identical to the repository source by test, and
every document must carry a title and a description — both are enforced
by the gateway's test suite.

## Voice capabilities

The voice capabilities endpoint reports what the current runtime can do with
audio. `GET /api/voice/capabilities` returns:

- **`transcription`** and **`synthesis`** — booleans for whether speech-to-text
  and text-to-speech are available.
- **`unavailable_reason`** — when either capability is off, the reason and
  what to address: the default provider may be absent, its credentials
  unavailable, its protocol incompatible (only OpenAI-protocol providers
  serve audio), or a `[voice]` model id may be unset. Absent when both
  capabilities are available.
- **`default_voice`** and **`supported_voices`** — the default synthesis
  voice and the available voice list. The OpenAI provider reports both
  regardless of whether synthesis is configured.

The two audio routes — `POST /api/voice/transcriptions` and
`POST /api/voice/speech` — send requests to the default provider's
OpenAI-protocol audio endpoints using the model ids configured in `[voice]`.
A request to an unavailable capability returns a 422 validation error. See
[Agents and permissions — Voice configuration](agents-and-permissions.md#voice-configuration)
for the configuration keys.

## Gateway tools

The gateway ships a set of built-in tools that agents call during turns. Most
tools execute directly in the gateway process, bypassing the bubblewrap sandbox
(`requires_device_execution: true` in the `Tool` trait). Two exceptions —
`shell` and `memory` — are sandbox-eligible and run through the sandbox when
one is available.

**Filesystem and shell** — `file_read`, `file_write`, `file_edit`, `file_list`,
`file_search`, `shell`, and `python_execute`. Of these, `shell` is one of the
two sandbox-eligible tools.

**Browser** — `browser_navigate`, `browser_snapshot`, `browser_click`,
`browser_type`, `browser_select`, and `browser_scroll`.

**Knowledge base** — `knowledge_search`, `knowledge_record`, `knowledge_curate`,
`knowledge_cite`, `knowledge_orient`, and `knowledge_curate_orientation`.

**Workflow** — `workflow_run` (start a workflow), `workflow_list` (list saved
scripts), `workflow_save` (save a script), `workflow_run_status` (list the
topic's runs that have not ended with the events each is waiting on, or read
one run in any state), and `workflow_run_account` (read a run's terminal
account — see
[Workflow engine — Terminal account](workflow-engine.md#terminal-account)).
`workflow_run_status` and `workflow_run_account` are scoped to the caller's
session topic: a run in another topic reads as absent rather than forbidden.

**Surfaces and delivery** — `surface_read`, `surface_write`, `surface_library`, and `run_event_post`.

**Scheduling** — `schedule` and `wake_me_later`.

**Session and memory** — `session_values`, `agent_memory`, `tool_result_read`,
`task_definition`, and `memory`. The `memory` tool reads and writes the
shared memory files of the session's scope — the topic's memory when a
topic is selected, the general memory otherwise. It is one of the two
sandbox-eligible tools.

**Delegation** — `delegate` dispatches a child turn to a named agent definition.

**External** — `url_fetch` and `web_search`.

**Documentation** — `docs_read` lists or reads the gateway's hosted
documentation (see [Hosted documentation](#hosted-documentation)).

### Workspace tool output

Several tools produce file artefacts as a side effect of their work. When a
session has an attached workspace, these artefacts land at:

```
.swage/tool-output/<turn_id>/<call_id>/<filename>
```

`shell` writes stdout and stderr artefacts only when the output stream was
truncated — a stream that fits in the tool result is reported inline and the
artefact file is removed. Three browser action tools — `browser_click`,
`browser_select`, and `browser_scroll` — write a post-action screenshot when
the browser screenshot policy is not Off; `browser_type` never captures.
`browser_snapshot` writes a full
accessibility snapshot as text only when the rendered snapshot is truncated;
otherwise the result is inline with no artefact file. `url_fetch` writes
`raw-body.html` only when the HTML body exceeds its capture limit; a body
that fits is reported inline and the artefact file is removed.

The path the tool result reports is relative to the workspace root (e.g.
`.swage/tool-output/turn/call/stdout.txt`). `file_read` can open text
artefacts directly, but it refuses binary content — so a screenshot written
by a browser action tool is not readable through `file_read`. The
`<turn_id>/<call_id>` structure keeps repeated calls from overwriting each
other.

The first time a tool writes output, the gateway creates
`.swage/tool-output/` and places a `.gitignore` containing `*` inside it.
This keeps every file under that directory out of `git status`, so tool
artefacts never appear as untracked files in the workspace's repository.

Sessions without an attached workspace write their tool output into the
session's scratch directory instead.

Operators can register additional tools through plugins; plugin tool names come
from the plugin manifest rather than the gateway's built-in set.
