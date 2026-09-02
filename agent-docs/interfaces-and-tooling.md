---
description: Turn context inspector, image attachments, wire error contract, route manifest and OpenAPI projection, verification gate, and client-side route handling
---

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

`AssemblyReport` and `TurnContextResponse` live in
`swage-protocol::client::context`. The stored-context endpoint and
`ContextAssembled` WebSocket event use the same typed report; the frontend
imports the generated `AssemblyReport.ts` projection.

The report's `provider_visible` object also records the resolved effort
for the turn (see [Agents and permissions — Effort](agents-and-permissions.md#effort)):
what was requested, what the adapter sent, and any clamp or substitution.
This data is stored in the report JSON but is not yet rendered by the
web UI's inspector panel; it is available through the
`GET /api/turns/{id}/context` endpoint.

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

## Image attachments

A message may carry content parts beside its text, submitted as the
`attachments` array on `SubmitTurnRequest` (REST) or the `SendMessage`
WebSocket event. Each attachment is a tagged `MessageAttachment`
(`swage-protocol::client::turn`); the only kind at this tree is `image_url`,
carrying a URL (https or `data:` inline).

### Intake check

`check_attachment_support` (`turn.rs:154`) runs before anything is persisted.
For each attachment:

- An `image_url` with an empty URL is rejected as a `ValidationError`.
- An `image_url` when the configured provider's `WorkflowCapabilities.vision`
  is `false` is rejected as a `ValidationError`.

### Which protocols claim vision

`WorkflowCapabilities::for_protocol` (`capabilities.rs:30`) sets `vision`
per protocol:

| Protocol | `vision` | Reason |
|---|---|---|
| `OpenAi` | `true` | The Chat Completions adapter maps `ContentPart::ImageUrl` |
| `OpenRouter` | `true` | Same adapter (OpenAI-shaped envelope) |
| `OpenAiResponses` | `false` | Adapter converts text parts only |
| `Anthropic` | `false` | Adapter converts text parts only |
| `Google` | `false` | Adapter converts text parts only |
| `Codex` | `false` | App Server integration; assembled text only |

Vision is claimed only where the adapter's message conversion maps
`ContentPart::ImageUrl` to the wire body. The capability is resolved once at
startup from the default provider's protocol and stored in
`AppState.workflow_capabilities`.

### Wire shape

When image parts are present, the OpenAI adapter (`openai.rs`) emits a JSON
array of typed content parts (`[{"type":"text","text":"…"},
{"type":"image_url","image_url":{"url":"…"}}]`). Text-only messages keep
the plain string form, preserving wire compatibility.

Stored messages carry attachments as opaque JSON in the `attachments`
column of the `messages` table (`TEXT NOT NULL DEFAULT '[]'`). The
`MessageView` returned by the API includes the `attachments` array.

## Provider credentials

A provider entry in `config.toml` has two credential fields
(`ProviderEndpointConfig` in `swage-config/src/model.rs`):

| Field | Resolution | Audit/redaction |
|---|---|---|
| `api_key_secret_ref` | Resolved through the `SecretManager` by reference (`secret://{backend}/{id}`) with an audit event per access and the value registered for redaction | Yes |
| `api_key_env` | Direct `std::env::var` read at startup; legacy path, warned at startup (`startup.rs:449`) | No |

`resolve_provider_config` (`startup.rs:420`) checks `api_key_secret_ref`
first; if absent, falls back to `api_key_env`. The resolved secret must have
`material_kind` `"provider_key"`.

### Secret manager and backends

The `SecretManager` (`secret.rs`) resolves secrets by their `secret://`
reference, with an audit event per access and the resolved value registered
for redaction. A secret's `backend_kind` names where its value lives;
resolution reads the backend on every call, so material that changes
underneath is current at the next access.

| Backend | `backend_kind` | Value source |
|---|---|---|
| Managed | `managed` | Encrypted at rest in the gateway's SQLite store |
| Environment | `env` | `external_reference` names an environment variable |
| File | `file` | `external_reference` names a file path; `metadata.json_pointer` (RFC 6901) optionally selects one field of a JSON document |

### File backend

The file is re-read on every resolution. When `json_pointer` is absent, the
whole file (trimmed) is the value. When present, the file is parsed as JSON
and the pointer selects the field — a string value is used directly; any
other JSON type is serialised. The file's modification time is recorded as
the observation stamp.

### Registering a file-backed secret

Create the secret with `POST /api/secrets` (`server.rs:251`):

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

The request fields (`CreateSecretRequest` in `secret.rs:43`):

| Field | Required | Purpose |
|---|---|---|
| `name` | Yes | Human-readable label |
| `material_kind` | Yes | What the secret is — `"provider_key"` for provider credentials |
| `owner_kind` | Yes | Who owns it — `"operator"`, `"harness"`, etc. |
| `backend_kind` | Yes | `"file"`, `"env"`, or `"managed"` |
| `scope_kind` | Yes | Scope category — `"provider"`, `"global"`, etc. |
| `scope_id` | No | Scope within the category |
| `external_reference` | No | File path (for `file`) or env var name (for `env`) |
| `metadata` | No | Backend-specific; `{"json_pointer": "..."}` for `file` |
| `value` | No | The secret material (for `managed` backend) |

The response includes a `reference` field (`secret://file/{id}`). Point the
provider entry's `api_key_secret_ref` at this reference:

```toml
[providers.entries.my-provider]
protocol = "openai"
api_key_secret_ref = "secret://file/<returned-id>"
```

### Codex auth store registration

At startup (`startup.rs:208`) the gateway registers the Codex CLI's
`auth.json` under the file backend as two records:

| Name | Material kind | JSON pointer |
|---|---|---|
| `codex.access_token` | `oauth_access_token` | `/tokens/access_token` |
| `codex.refresh_token` | `oauth_refresh_token` | `/tokens/refresh_token` |

Registration is idempotent: an existing active record for either token is
re-pointed at the current path. Registration precedes sign-in, so the file
need not exist yet. Both tokens resolve, audit, and redact through the
governed system.

## Wire error contract

Every failed HTTP response on the client surface carries
`{"code":"<snake_case>","message":"…"}` — the `ApiErrorBody` type
(`swage-protocol::client::error`).

### ErrorCode enum

`ErrorCode` (`error.rs:21`) names one HTTP status each:

| Code | Status | Meaning |
|---|---|---|
| `malformed_body` | 400 | Body was not valid JSON or could not be read |
| `forbidden` | 403 | Caller may not perform this operation |
| `not_found` | 404 | Addressed entity does not exist |
| `conflict` | 409 | Collides with existing state |
| `turn_in_progress` | 409 | Conversation already has a turn in flight |
| `already_connected` | 409 | Conversation already has a live connection |
| `unsupported_media_type` | 415 | Missing `Content-Type: application/json` |
| `validation_error` | 422 | Body parsed but failed validation |
| `sign_in_unsupported` | 422 | Provider offers no sign-in flow of the requested kind |
| `internal_error` | 500 | Unexpected gateway failure |
| `provider_error` | 502 | Upstream model provider failed |
| `sign_in_failed` | 502 | Provider refused or could not complete sign-in |
| `provider_runtime_unavailable` | 503 | Provider runtime could not be started or reached |

The HTTP status is a projection of the code (`ErrorCode::status`), never an
independent fact. Codes serialise as `snake_case` strings via serde.

### Coverage

`GatewayError` maps exhaustively to an `ErrorCode` — a new variant must
choose one. The gateway's own `Json` extractor answers body-parsing
rejections (malformed JSON, unsupported media type, `deny_unknown_fields`
violations naming the rejected field) in the same `ApiErrorBody` shape.
Handlers construct refusals through `ApiError` rather than hand-built bodies.

### Client consumers

- **Rust client** — `ClientError::Gateway` variant parses the body into a
  typed error.
- **Frontend** — `ApiError` class (`frontend/src/lib/api/error.ts`) reads
  the `code` field. The generated TypeScript client's request helper raises
  `ApiError` on non-ok responses.

## Wire-contract architecture

All types that cross a process boundary live in `swage-protocol`
(`crates/swage-protocol/`). Gateway modules consume or re-export those Rust
types, and frontend wire types come from generated projections under
`frontend/src/lib/protocol/`.

The crate is organised into four surface modules:

| Surface | Module | Anchor type | Scope |
|---|---|---|---|
| Client | `src/client/` | `ClientSurface` | Conversations, streaming, approvals, schedules, knowledge, voice, workflows, channels, delivery, spaces, settings, materials, recovery, context reports, provider sign-in |
| Hosting | `src/hosting.rs` | `HostingSurface` | Presence, operator overview, agent definitions, approvals, configuration |
| Device | `src/device.rs` | `DeviceSurface` | Device registration, remote tool execution |
| Channel | `src/channel.rs` | `ChannelSurface` | Inbound messages (DM/thread), delivery log, channel status |

Each manually maintained anchor struct must reference every wire type on its
surface. A `schemars::schema_for!` call walks only the type graph reachable from
that anchor, so a new top-level type must be added to the anchor unless an
existing field already reaches it.

### Route manifest

`routes.rs` (`swage-protocol::client::routes`) carries a growing manifest of
client-surface HTTP routes: method, path template, path parameters, request
body schema name, and response body schema name. The manifest does not yet
cover the full client surface — the space CRUD routes are the first entries;
routes not yet in the manifest are registered directly on the router and are
not covered by the OpenAPI or generated-client projections. Failures are not
listed per route — every route answers a failure with `ApiErrorBody`,
projected once as the shared error response.

Adding a manifested route is one entry in the `routes()` function; the
OpenAPI document, the generated TypeScript client, and the router-coverage
test all read this table. The router test (`server.rs:418`) proves every
manifest route is registered on the axum router — a manifest entry with no
matching handler fails the test.

### Projections

| Projection | Output | Mechanism |
|---|---|---|
| TypeScript types (ts-rs) | `frontend/src/lib/protocol/` | `projections.rs` test manually calls `export_all` for each top-level export, then trims trailing whitespace |
| JSON Schema (schemars) | `crates/swage-protocol/schema/{client,hosting,device,channel}.json` | `projections.rs` tests per surface, via the anchor type |
| OpenAPI 3.1 (from route manifest) | `crates/swage-protocol/schema/client-openapi.json` | `projections.rs` test renders `openapi::document()`, which walks the manifest and reuses the client surface's JSON Schema as its `components/schemas` |
| TypeScript client (from route manifest) | `frontend/src/lib/protocol/client.ts` | `projections.rs` test renders `typescript_client::generate()` — one typed function per manifest route over a shared `request` helper that raises `ApiError` on non-ok responses |

Running `cargo test -p swage-protocol` regenerates all projections in place.
New top-level wire types must be added to the appropriate anchor for JSON
Schema and to the explicit `export_all` list for TypeScript unless already
reachable from an existing entry. New routes must be added to the `routes()`
manifest for the OpenAPI and client projections. Drift shows as uncommitted
changes, caught by the verify gate's projection-freshness check; an
unregistered type or unmanifested route creates no drift for that check to
detect.

## Verification gate

`xtask` (`xtask/src/main.rs`) orchestrates the verification gate; the
`Justfile` is a thin dispatch into `cargo run -p xtask -- <subcommand>`.
Most logic is Rust, but `xtask` shells out for some checks (e.g.
`scripts/check-tracked-local-state.sh`).

### Recipes

| Recipe | What it runs |
|---|---|
| `just check-gate` | Tracked-local-state, format, Rust/frontend type, frontend design-token, and clippy checks; no tests |
| `just check` | `cargo check --workspace` + frontend type check (`npm run check`) |
| `just test` | Full test suite with failure aggregation (see below) |
| `just verify` (alias: `just ci`) | `check-gate`, then the test and projection stages below; the gate CI runs |

### Verify stages

`check-gate` runs stages 1--5 and aborts on the first failure (`?` operator).
Stages 6--7 run regardless; their results are combined.

| # | Stage | Short-circuits? |
|---|---|---|
| 1 | Tracked-local-state check (`scripts/check-tracked-local-state.sh`) | Yes |
| 2 | Format check (`cargo fmt --all -- --check` + `just --fmt --check`) | Yes |
| 3 | Type check (`cargo check --workspace` + frontend) | Yes |
| 4 | Frontend design-token check | Yes |
| 5 | Clippy (`-D warnings`) | Yes |
| 6 | Test suite (Rust with `--no-fail-fast`, frontend, desktop smoke) | No |
| 7 | Projection freshness (`git diff --exit-code` on generated files) | No |

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
