---
description: Settings memory view, operator history, turn context inspector, conversation branching, image attachments and reference admission, session-owned surfaces, the surface library and annotations, workspace relinking, delivery targets, devices (registration and pairing procedure, the dial-in connection, cancellation, and presence), tool-call recording and replay, turn completion states (incomplete, failed, interrupted, cancelled), hosted documentation and the docs_read tool, voice capabilities, gateway lifecycle (status and start recovery), wire error contract, route manifest and OpenAPI projection, verification gate, client-side route handling, the gateway's built-in tool inventory, and workspace tool output
---

# Interfaces and tooling

## Settings memory view

Settings has a **Memory** tab that lists every memory page — general and
per-topic — and lets the operator read any page in full. A chat box on the
tab changes memory by sending an instruction to the platform's curator; the
curator's reply and every page it changed (returned whole) are shown in the
same view.

Three API routes support this:

| Route | Method | Purpose |
|---|---|---|
| `/api/memory/pages?topic_id=` | GET | List every page in one scope (`MemoryPageListResponse`: `scope`, `pages[]` each with `reference` and `summary`) |
| `/api/memory/page?reference=&topic_id=` | GET | Read one page whole (`MemoryPageResponse`: `reference`, `summary`, `body`) |
| `/api/memory/instructions` | POST | Instruct the curator (`MemoryInstructionRequest`: `scope`, `instruction`) → `MemoryInstructionResponse`: `reply`, `pages_changed[]` (each a `MemoryPageResponse`), `curator_session_id`, `turn_id` |

All three routes are in the client route manifest. The scope selector
(`MemoryScopeSelector`) is `General` or `Topic { topic_id }`: absent
`topic_id` addresses the general memory. The curator is the only way a page
changes from Settings — there is no direct page-editing endpoint.

The curator's notices (from its background curation) link to the page they
name: the Settings route
`/settings/memory?scope=<general|topic_id>&reference=<ref>` opens the
Memory tab with that page selected. The `scope` parameter identifies
whether the page belongs to the general memory or a specific topic;
a reference alone does not identify a page across scopes.

## Operator history

The operator history pane reads `GET /api/operator/history`
(`OperatorHistoryResponse`) and the filterable audit record
(`GET /api/operator/audit-events`).

Configuration changes carry the whole prior and new state of the named
entity. The pane derives a per-field diff from those two snapshots:
nested settings are named by their path (e.g. "Context › Assembly"),
and each changed field shows its prior and new value in a table.
The whole prior and new objects are kept collapsed beneath each entry.

The pane also shows variant selections (location, variant, reason, session,
turn) and assembly decisions.

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

A `succession-degradation` compaction record's diagnosis details — the walls
answered, each model call (stage, model, usage, report), the synthesis text,
and the turn a reused diagnosis was originally written for — are held in the
turn's recorded context report, returned by `GET /api/turns/{id}/context`,
but are not rendered by the inspector panel. The report retains compaction
decisions from the first assembly and the successor in pass order, even when
their names match.

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
just-completed message without a surface reload — the `turn_id` guard at
`MessageDisplay.svelte:273` matches live messages and API-loaded messages
alike.

The `RouterOptions.conversationStore` interface (`router.ts:47`) is a
four-member contract — `activeConversationId`, `checkConversationExists`,
`clearActiveConversation`, `selectConversation` — decoupled from Svelte store
internals.

## Conversation branching

A conversation can be branched from any message. The conversation's detail
carries a `root_thread_id` — the thread it started on — and its `messages`
are that thread's messages. A branch is a sibling thread
(`ConversationBranchResponse`) that carries the parent's messages up to the
message it was branched from, then diverges. A branch's messages are read
from `GET /api/threads/{id}` (`ConversationThreadDetail`).

### Routes

| Route | Method | Purpose |
|---|---|---|
| `/api/conversations/{id}/branches` | GET | List a conversation's branches (`ConversationBranchListResponse`) |
| `/api/conversations/{id}/branches` | POST | Branch at a message (`CreateConversationBranchRequest`: `thread_id`, `from_message_id`) → `ConversationBranchResponse` (201) |
| `/api/threads/{id}` | GET | Read one thread with its messages and, for a branch, its lineage |
| `/api/threads/{id}/abandon` | POST | Abandon a branch: it keeps its history and lineage but takes no further turns |

All four routes are in the client route manifest.

### Branch listing

Each `ConversationBranchResponse` carries `child_thread_id`,
`parent_thread_id`, `branched_from_message_id`, the
`branched_from_preview` (the opening of the branch-point message, absent
when that message no longer exists), and the thread's `state`.

### Thread states

A thread's `state` is one of `idle`, `processing`, `interrupted`,
`completed`, or `abandoned`. Abandoning is terminal: the branch keeps its
history and lineage and refuses further turns.

### Message identity

Every `MessageView` carries its own `id` and `thread_id`. The `id` is what
a branch is taken from (`from_message_id` in the branch request).

### Audit

Both branching and abandoning land in the session's audit trail.

## Image attachments

A message may carry content parts beside its text, submitted as the
`attachments` array on `SubmitTurnRequest` (REST) or the `SendMessage`
WebSocket event. Each attachment is a tagged `MessageAttachment`
(`swage-protocol::client::turn`); the only kind at this tree is `image_url`,
carrying a URL (https or `data:` inline).

### Intake check

`check_attachment_support` (`turn.rs:158`) runs before anything is persisted.
For each attachment:

1. An `image_url` with an empty URL is rejected as a `ValidationError`.
2. An `image_url` when the configured provider's `WorkflowCapabilities.vision()`
   is `false` is rejected as a `ValidationError`.
3. An `image_url` whose URL the provider cannot resolve — checked by
   `WorkflowCapabilities::accepts_image_reference` — is rejected as a
   `ValidationError` with a message advising a base64 `data:` URL instead.

Step 3 is the per-reference gate: vision support alone does not admit every
URL form. The check runs before the turn is persisted, so an unusable
reference never reaches the database or the provider.

### Which protocols claim vision

`WorkflowCapabilities::for_protocol` (`capabilities.rs:51`) sets the
`images` field per protocol. `vision()` returns `true` when `images` is
`Some`. The `images` value also determines which URL forms the intake gate
accepts:

| Protocol | Vision | Image references | Wire shape |
|---|---|---|---|
| `OpenAi` (Chat Completions) | Yes | `AnyUrl` — any `https` or `data:` URL | Typed content-part array: `{"type":"image_url","image_url":{"url":"…"}}` |
| `OpenRouter` | Yes | `AnyUrl` | Same Chat Completions adapter |
| `OpenAiResponses` (Responses) | Yes | `AnyUrl` | `{"type":"input_image","image_url":"…"}` |
| `Anthropic` | Yes | `AnyUrl` | `{"type":"image","source":{"type":"url","url":"…"}}` for URLs; `{"type":"image","source":{"type":"base64","media_type":"…","data":"…"}}` for `data:` URLs |
| `Google` | Yes | `InlineOrHostedFile` — `data:` URLs (carried as `inlineData`) and Google File API URIs (`https://generativelanguage.googleapis.com/…`, carried as `fileData.fileUri`); arbitrary web URLs are refused at intake | `inlineData` blob or `fileData` reference |
| `Codex` | No | — | App Server integration; assembled text only |

Vision is claimed only where the adapter's message conversion maps
`ContentPart::ImageUrl` to the wire body. The capability is resolved once at
startup from the default provider's protocol and stored in
`AppState.workflow_capabilities`.

### Storage

Stored messages carry attachments as opaque JSON in the `attachments`
column of the `messages` table (`TEXT NOT NULL DEFAULT '[]'`). The
`MessageView` returned by the API includes the `attachments` array.
Text-only messages keep the plain string form on the wire; messages with
image parts use the provider-specific typed content array.

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

## Surfaces

Surfaces are a session's work surfaces, assembled from ordered module
instances. Each surface belongs to one session (`session_id` on
`SurfaceResponse`; `sessions.session_id` in storage, cascading with the
session) and carries its own module list, annotations, and provenance.
`GET/POST /api/sessions/{session_id}/surfaces` list and create a session's
surfaces.

### The shared library

`POST /api/surfaces/{surface_id}/save` (`SaveSurfaceToLibraryRequest`: `name`,
`description`, `tags`) copies a surface's modules into a library entry and
records the origin topic (`origin_topic_id`, absent for the general scope).
The description is required — it is what an agent chooses by.
`GET /api/surface-library?tag=` is the catalogue (`LibraryListResponse`:
names, descriptions, tags, origin; no module content);
`GET /api/surface-library/{entry_id}` reads an entry with its module rows;
`DELETE` removes it, leaving surfaces added from it untouched.

Creating a surface with `library_entry_id` set seeds it from the entry
(`library_entry_id`, `seeded_at`, and `seeded_from_name` on
`SurfaceResponse`). Seeding carries the saved content only when the
session's topic is the entry's origin (`LibrarySeeding::WithContent`); any
other topic or the general scope receives the shape with empty modules
(`ShapeOnly`). The audit event `surface_created` records which under
`seeded_from.seeding`. The `seeded_from_name` persists even after the
library entry it was seeded from is deleted.

`GET/POST /api/topics/{topic_id}/default-surfaces` and
`DELETE /api/topics/{topic_id}/default-surfaces/{entry_id}` manage a topic's
default entries; the gateway opens them in every person-opened session of
the topic (sessions with origin `direct`). Schedule runs and other
automated sessions do not open default surfaces, and the general scope
designates no defaults. `topics.tags` (`UpdateTopicRequest.tags`) are the
labels the `surface_library` tool narrows the catalogue by for a topic's
sessions.

Library entries carry an `origin_topic_deleted` boolean, set when the entry's
origin topic is deleted. Deleting a topic keeps the entries saved from it
and other topics' default designations of those entries intact — the
catalogue marks the origin as deleted (`origin_topic_deleted: true`), and a
surface seeded from such an entry carries its shape only, because the
entry's content travels to no session once its origin topic is gone.

A save or delete broadcasts `ServerEvent::LibraryUpdated { entry_id }` on
the work-surface channel (`/api/work-surface/ws`), delivered like
`surface_updated`: identity only, no content. The frontend's surfaces store
answers it by refetching the catalogue, so the library page
(`/library`, `SurfaceLibrary.svelte`) and the surfaces pane's picker show a
new entry without a reload. From a session the page adds an entry through
the ordinary create route and navigates to the new surface; from the front
door (no session) it is read-only.

### Annotations

Annotations are notes hung on individual modules of a surface, anchored at
module grain — there is no per-surface or intra-module positioning.
`CreateSurfaceAnnotationRequest` takes `module_id`, `body`, and optional
`tags`. `UpdateSurfaceAnnotationRequest` replaces body and tags together; the
displaced text survives in the surface's provenance trail.

Deleting an annotation returns `DeleteSurfaceAnnotationResponse { deleted:
true }`. The delete is permanent — there is no soft-delete or archive. The
surfaces pane (`SurfacesPane.svelte`) exposes create, update
(`store.updateAnnotation` → `PUT /api/surface-annotations/{id}`) and delete.

### Surface addresses

`/conversations/{id}/surfaces/{surface_id}` (`Route.surfaceId`;
`conversationPath(id, surfaceId)` in `router.ts`) addresses an open surface.
`SurfacesPane` opens the routed surface once the session's list is loaded
and reports the surface the user opens (`onOpenSurface`) so the shell keeps
the URL in step; a deep link or Back restores the surface with the session.

### Work-surface reconnect

When a WebSocket connection to a surface's work surface drops and reconnects,
the frontend refetches the surface state to pick up any writes that landed
while disconnected. `library_updated` rides the same channel and refetches
the catalogue.

## Workspace relinking

When a workspace directory moves on disk, the attachment can be relinked to
its new path without detaching and re-attaching:
`PUT /api/topics/{topic_id}/workspaces/{workspace_id}/relink`
(`RelinkWorkspaceAttachmentRequest`: `path`). Relinking updates only the
path and clears the missing flag; the attachment's label, configured
permissions, trust state, and default status are preserved.

## Delivery targets

A run's output can be routed to one or more destinations through
`DeliveryRoutingRequest` (`swage-protocol::client::delivery`). The four
target kinds:

| Kind | Fields | Destination |
|---|---|---|
| `channel` | `integration`, `account`, `conversation` | An external channel (Telegram, dev DM/thread) |
| `local_storage` | — | The gateway's local store, as a log record |
| `surface` | `surface_id` | A surface owned by the continuing session (schedules) or by a session in the run's topic (workflows) |
| `webhook` | `url` | An HTTP endpoint |

`store_local_output` on the routing request adds a `local_storage` target
alongside any explicit targets, so the output is always retained locally
when set.

### No-delivery records

When a schedule run produces no output delivery, the channel gateway
records a no-delivery entry (`record_no_delivery`,
`channel_gateway.rs:777`) with target `{"kind": "no_delivery"}`, status
`"not_delivered"`, and the reason as the error field. Three outcomes
produce this record:

| Outcome | Run state | Reason recorded |
|---|---|---|
| Gate skip | `skipped` (`skip_reason: gate`) | The gate model's message, or "schedule gate decided no work was needed" |
| Overlap skip | `skipped` (`skip_reason: overlap`) | "schedule run overlapped an in-flight run" |
| Failure before delivery | `failed` (`error_message` set) | The failure message from the run body |

`SkipReason` (`swage-protocol::client::schedule`) is the enum behind the
first two. A run that fails after delivery has already been attempted does
not receive a no-delivery record — `delivery_attempted` on the failure
controls this (`schedule.rs:1267`).

## Devices

A device registers once, naming the gateway secret its credential resolves
against and the workspace it operates in, then dials into the gateway over
a persistent connection. Nothing listens on the device: the device opens the
connection, advertises what it can run, and the gateway invokes operations
back over the same socket. `swage device connect` runs a machine as a device.

### Registration and pairing

A device can join in two ways: direct registration or pairing.

**Direct registration** — `POST /api/devices/register` with the device
identity, the gateway secret its credential resolves against, and the
workspace it operates in.

**Pairing** is a two-step alternative:

1. **Create the token file on the gateway host.** Write a plain-text file
   holding only the shared token value (the file is trimmed on read, so a
   trailing newline is fine):

   ```bash
   # On the gateway host
   head -c 32 /dev/urandom | base64 > /var/lib/swage/device-tokens/workshop.token
   ```

2. **Register the token file as a secret.** `POST /api/secrets`:

   ```json
   {
     "name": "workshop-token",
     "material_kind": "shared_token",
     "owner_kind": "operator",
     "backend_kind": "file",
     "scope_kind": "device",
     "scope_id": "workshop",
     "external_reference": "/var/lib/swage/device-tokens/workshop.token"
   }
   ```

   No `metadata` or `json_pointer` — the entire file is the token. The
   response carries the `secret_id`.

3. **Issue a pairing code.** `POST /api/onboarding/devices/pairing-codes`:

   ```json
   {
     "expires_in_seconds": 600
   }
   ```

   `expires_in_seconds` is optional (default 600 — ten minutes). The
   response carries the `pairing_code` and `expires_at`.

4. **Redeem the code.** `POST /api/onboarding/devices/redeem`:

   ```json
   {
     "pairing_code": "<code from step 3>",
     "device_id": "workshop",
     "shared_token_secret_ref": "secret://file/<secret-id>",
     "workspace_root": "/home/user/projects"
   }
   ```

   This registers and trusts the device in one step. `label` (optional) and
   `kind` (defaults to `"attached_device"`) can also be included.

5. **Create the same token file on the device machine.** Copy or recreate
   the file so the device holds the same value the gateway resolves:

   ```bash
   # On the device machine — same content as the gateway's file
   cat > /home/user/.swage/workshop.token
   ```

6. **Connect.** Run the device:

   ```bash
   swage device connect \
     --gateway http://gateway.local:3000 \
     --device-id workshop \
     --shared-token-file /home/user/.swage/workshop.token \
     --workspace /home/user/projects
   ```

   `--shared-token-file` points at the device's local copy. The process
   reads it, dials the gateway, presents the token for comparison against
   the registered secret, advertises the workspace's tools, and serves
   operations until interrupted.

In both paths the `kind` field defaults to `"attached_device"` and names the
device type. A `kind` of `"local"` is refused: a local device has no remote
producer to register. The same constraint applies to recovery imports, so the
remote-device registry never holds a local entry. What a device can run is
not registered — the device advertises its capabilities each time it
connects.

### The connection

The device dials `GET /api/devices/connect` (WebSocket). The handshake:

1. The device sends a `Hello` frame: `device_id`, `shared_token` (resolved
   against the secret named at registration), `label`, and `capabilities`
   (the tools it can run).
2. The gateway validates the credential and answers `Welcome` (with
   `heartbeat_interval_secs`) or `Rejected` (with a message) and closes.

After the welcome the connection carries two frame types in each direction:

| Direction | Frame | Purpose |
|---|---|---|
| Gateway → device | `Operation` | Run `tool_name` with `params` in `context`; carries a `correlation_id` |
| Gateway → device | `Cancel` | Stop the operation `correlation_id` if still running |
| Device → gateway | `Result` | Operation `correlation_id` finished with `output` |
| Device → gateway | `Failure` | Operation `correlation_id` failed with `message` |
| Device → gateway | `Cancelled` | Operation `correlation_id` was stopped |
| Device → gateway | `Heartbeat` | Sent every `heartbeat_interval_secs` (default 10 s) |

All frames are tagged by `kind` (snake_case).

### Cancellation

When a turn is cancelled, the gateway sends `Cancel` for each in-flight
device operation. For local shell commands (`shell` tool), cancellation
kills the entire process group (`SIGKILL` to `-pid`), stopping the command
and all its descendants.

### Presence

Presence begins when the gateway accepts the device's hello: the gateway
records a heartbeat at the accepted hello, marking the device `connected`
before the welcome is sent. Subsequent heartbeats (sent every
`heartbeat_interval_secs`, default 10 s, as the welcome tells the device)
keep the device connected. The gateway marks a device `stale` after 3× the
heartbeat interval without a heartbeat, and `disconnected` when the
WebSocket closes.

The operator can close a device's connection from the gateway side with
`DELETE /api/presence/devices/{id}`, which disconnects the device and marks
it disconnected.

Every connect, disconnect, invocation, and settlement lands in the audit
trail under the device's id.

## Gateway lifecycle

### Status

`swage gateway status` reports the running gateway's state: PID, bound
address, uptime, and health. When no runtime metadata file exists and no
running gateway can be recovered at the configured address, it prints
"Gateway is not running" and exits successfully (exit 0). When stale
metadata points to a dead PID, it removes the stale file and exits with
an error (non-zero).

### Start recovery

`swage gateway start` checks for an existing gateway before spawning. If
runtime metadata names a PID that has terminated (even if the parent
process has not yet reaped it), it cleans up the stale metadata file and
proceeds with a fresh start. If no metadata file exists, it probes the
configured address for a running gateway whose metadata was lost, and if
found, writes recovery metadata and reports it as already running.

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
| Client | `src/client/` | `ClientSurface` | Conversations, streaming, approvals, schedules, knowledge, voice, workflows, channels, delivery, topics, settings, materials, recovery, context reports, provider sign-in |
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
cover the full client surface — the topic CRUD routes are the first entries;
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
| `just check-gate` | The six static stages below; no tests |
| `just check` | `cargo check --workspace` + frontend type check (`npm run check`) |
| `just test` | Full test suite with failure aggregation (see below) |
| `just verify` (alias: `just ci`) | `check-gate`, then the test and projection stages below; the gate CI runs |

### Verify stages

`check-gate` runs stages 1–6 and aborts on the first failure (`?` operator).
Stages 7–8 run regardless; their results are combined.

| # | Stage | Short-circuits? |
|---|---|---|
| 1 | Tracked-local-state check (`scripts/check-tracked-local-state.sh`) | Yes |
| 2 | Format check (`cargo fmt --all -- --check` + `just --fmt --check`) | Yes |
| 3 | Type check (`cargo check --workspace` + frontend) | Yes |
| 4 | Frontend design-token check | Yes |
| 5 | Shipped-workflow parse check (`check_shipped_workflows_parse`) | Yes |
| 6 | Clippy (`-D warnings`) | Yes |
| 7 | Test suite (Rust, frontend, desktop smoke) | No |
| 8 | Projection freshness (`git diff --exit-code` on generated files) | No |

The Rust test runner is selected by the `SWAGE_TEST_RUNNER` environment
variable. The Justfile defaults it to `nextest`, so `just verify` runs
nextest unless overridden. When set to `nextest` (the default), Rust tests
run under `cargo nextest run` with a `ci` profile (which retries flaky
tests), followed by a separate `cargo test --workspace --doc` stage for
doctests (nextest cannot run doctests). When set to `cargo`, Rust tests run
under `cargo test --workspace --no-fail-fast`.

When both tests and projections fail, both errors surface in one message.
The test suite aggregates Rust, frontend, and desktop-smoke failures rather
than aborting on the first.

The verify body runs inside `run_with_final_sweep`, which runs
`cargo sweep` after the gate (even on failure) and reports the artefact
budget.

## Client-side route preservation

The SPA router (`frontend/src/lib/router.ts`) is framework-free: `parseRoute`
pattern-matches the URL (`/`, `/conversations/{id}`,
`/conversations/{id}/surfaces/{surface_id}`, `/library`,
`/workflow-runs/{id}`, `/settings/...`, `/settings/topic/{slug}/...`);
`createRouter` manages `pushState` navigation and `popstate` listening.

`applyLocation()` runs on every URL change. When the URL contains a
conversation ID:

- **Already active:** calls `checkConversationExists` — a lightweight `GET`
  that confirms the conversation exists without re-fetching it. On 404,
  clears the active conversation and falls back to root with a not-found
  notice. On transient failure (non-404), the conversation is kept and the
  error is swallowed.
- **Different conversation:** calls `selectConversation`, which fully loads
  it. On 404, falls back to root. On other errors, the error propagates.

The `conversationStore` dependency is injected as a four-member interface,
wired in `App.svelte`. `clearActiveConversation` disconnects the WebSocket
before clearing the store, preventing orphaned connections.

Route changes propagate `notFound` state to the UI, so the "conversation not
found" notice appears for `popstate`-triggered navigations and initial surface
loads alike. When a scope was in view, `App.svelte` keeps a pending start in
it (the composer stays open; the next message creates a session there);
otherwise the front door serves.

Session creation is fenced against a scope change: `createAndNavigate`
captures a scope generation before the request and hands the store a
`shouldInstall` check, so a session created for one scope is neither listed
nor activated once the user has moved to another (it exists in the scope it
was asked for, and the recent list finds it).

## Tool-call recording and replay

Tool calls are recorded in two phases. A record is created when the model
selects the tool call (`create_tool_call`), before execution begins. Once
execution completes, the record is settled with the result
(`update_tool_call_result`). This means a tool call that was selected but
never finished — interrupted by cancellation, a crash, or a turn that ended
before the result could be recorded — has a record with no result.

At turn startup, unsettled tool calls from a prior interrupted turn are
replayed into the conversation history so the model sees what happened.
Completed tool calls replay with their recorded results. A tool call that
was interrupted or whose result could not be recorded replays with an
explicit unknown-outcome text rather than replaying as though it returned
successfully:

- An interrupted call (no result, no status): `"tool call did not report a
  result; its outcome is unknown"` with status `"interrupted"`.
- A call stopped with its turn: status `"cancelled"`.
- A call that ran but whose result could not be recorded: `"tool call ran,
  but its result could not be recorded"` with status `"result_unavailable"`.

When a tool-call result cannot be recorded (the settlement write fails),
the turn ends as `Failed` — the result is not silently dropped.

Stopping a turn ends a shell command running inside the sandbox — in both
bubblewrap and fallback modes — and its tool-call record settles as
`"cancelled"`.

## Turn completion states

`TurnState` (`swage-protocol::client::conversation`) names the states a
turn passes through:

| State | Meaning |
|---|---|
| `Pending` | Queued but not yet started |
| `Processing` | In flight |
| `Completed` | Finished successfully |
| `Incomplete` | The reply was cut short by the output token limit |
| `Failed` | An error ended the turn |
| `Interrupted` | Cancelled by the user or a restart |
| `AwaitingApproval` | Paused for a human approval decision |
| `Delegating` | Dispatching a child turn |

An incomplete turn is surfaced through two paths. The `turn_complete`
WebSocket event carries an `incomplete_reason` string naming the cause
(e.g. the output token limit), delivered live to connected clients. For
persistence, the agent appends a separate platform-origin message whose
content states the cause; this message carries `TurnState::Incomplete` and
is what the conversation shows after a reload. `MessageView` has no
`incomplete_reason` field — the cause is in the platform message's content.
The partial text the model produced is retained on the assistant message.

A failed, interrupted or cancelled turn keeps its explanation text and its
tool calls in the conversation after a reload. The tool calls are attached
to the request the turn answered, so the work the agent performed is
visible. A failed turn offers **Retry** in the web UI.

Failed shell commands and workflow runs show as failed on their tool rows,
both during the turn and after reload.

## Hosted documentation

The gateway compiles the agent-facing documentation set (`agent-docs/`) into
the binary (`crate::shipped_docs`) and serves it read-only. Documents are
addressed by name — the file stem of the source under `agent-docs/`. The set
is flat: no subdirectories.

| Route | Response |
|---|---|
| `GET /api/docs` | `DocumentListResponse`: each document's `name`, `title`, and `description` |
| `GET /api/docs/{name}` | `DocumentResponse`: the document's Markdown body, front matter included |

Both routes are in the client route manifest (`swage-protocol::client::docs`).

The `docs_read` tool (`tool/docs_read.rs`) exposes this to agents: call with
no arguments to list the documents with descriptions; call with a `name` to
read one. A document longer than the tool's output limit pages by byte offset
(`offset_bytes`), the same continuation shape `url_fetch` uses. The tool
executes in the gateway process (`requires_device_execution: true`).

The `catalogue_embeds_every_document_byte_for_byte` test
(`swage-gateway::shipped_docs`) proves each compiled document is
byte-identical to the repository source. The
`every_shipped_document_carries_a_title_and_description` test verifies the
metadata the listing returns.

## Voice capabilities

`GET /api/voice/capabilities` returns a `VoiceCapabilitiesResponse`
(`swage-protocol::client::voice`):

| Field | Type | Meaning |
|---|---|---|
| `transcription` | bool | Whether speech-to-text is available |
| `synthesis` | bool | Whether text-to-speech is available |
| `default_voice` | string or null | The default synthesis voice (e.g. `"alloy"`) |
| `supported_voices` | string[] | Available synthesis voices |
| `unavailable_reason` | string or null | What to configure to enable a missing capability; absent when both are available |

The `unavailable_reason` names what to address. Three cases:

- No OpenAI-protocol default provider is configured.
- The default provider's credentials are unavailable.
- The default provider uses a non-OpenAI protocol (Anthropic, Google, Codex)
  that serves no audio API.

When the provider is available but a `[voice]` model id is unset, the reason
names the missing key — e.g. "set `transcription_model` in the [voice]
settings to a model the default provider serves". See
[Agents and permissions — Voice configuration](agents-and-permissions.md#voice-configuration)
for the configuration keys.

The voice routes (`POST /api/voice/transcriptions`, `POST /api/voice/speech`)
use the configured model ids against the default provider's OpenAI-protocol
audio endpoints. A request to an unavailable capability returns a 422
validation error.

## Gateway tools

The gateway ships these built-in tools
(`crates/swage-gateway/src/tool/`). A tool's `requires_device_execution`
flag (`swage-tool::Tool` trait) determines whether it executes in the
gateway process or can run through the bubblewrap sandbox: `true` means
the tool must execute in the gateway process (bypassing the sandbox);
`false` (the trait default) means it is eligible for sandboxed execution.
All built-in tools set `true` except `shell` and `memory`, which are the
two sandbox-eligible tools.

### Filesystem

| Tool | Source |
|---|---|
| `file_read` | `file_read.rs` |
| `file_write` | `file_write.rs` |
| `file_edit` | `file_edit.rs` |
| `file_list` | `file_list.rs` |
| `file_search` | `file_search.rs` |
| `shell` | `shell.rs` — sandbox-eligible |
| `python_execute` | `python_execute.rs` |

### Browser

| Tool | Source |
|---|---|
| `browser_navigate` | `browser.rs` |
| `browser_snapshot` | `browser.rs` |
| `browser_click` | `browser.rs` |
| `browser_type` | `browser.rs` |
| `browser_select` | `browser.rs` |
| `browser_scroll` | `browser.rs` |

### Knowledge base

| Tool | Source |
|---|---|
| `knowledge_search` | `knowledge.rs` |
| `knowledge_record` | `knowledge.rs` |
| `knowledge_curate` | `knowledge.rs` |
| `knowledge_cite` | `knowledge.rs` |
| `knowledge_orient` | `knowledge.rs` |
| `knowledge_curate_orientation` | `knowledge.rs` |

### Workflow

| Tool | Source |
|---|---|
| `workflow_run` | `workflow_run.rs` |
| `workflow_list` | `workflow_list.rs` |
| `workflow_save` | `workflow_save.rs` |
| `workflow_run_account` | `workflow_run_account.rs` — reads a run's terminal account. Scoped to the caller's session topic; a run in another topic reads as absent. |
| `workflow_run_status` | `workflow_run_status.rs` — lists the topic's runs that have not ended, each with its undelivered events (a pipeline's pending hold among them), or reads one named run in any state. Same topic scoping as `workflow_run_account`. |

### Surfaces and delivery

| Tool | Source |
|---|---|
| `surface_read` | `surface.rs` — the calling session's surfaces |
| `surface_write` | `surface.rs` — create, write, annotate, or save to the library |
| `surface_library` | `surface.rs` — the catalogue narrowed by the session's topic tags, widened on request; add an entry to the session |
| `run_event_post` | `run_event.rs` |

### Scheduling

| Tool | Source |
|---|---|
| `schedule` | `schedule.rs` |
| `wake_me_later` | `schedule.rs` |

### Session and memory

| Tool | Source |
|---|---|
| `session_values` | `session_variable.rs` |
| `agent_memory` | `agent_memory.rs` |
| `memory` | `memory.rs` — sandbox-eligible; reads and writes the shared memory files of the session's scope — the topic's memory when a topic is selected, the general memory otherwise |
| `tool_result_read` | `tool_result_read.rs` |
| `task_definition` | `task_definition.rs` |

### Delegation

| Tool | Source |
|---|---|
| `delegate` | `swage-tool::delegate` — dispatches a child turn to a named agent definition. |

### External

| Tool | Source |
|---|---|
| `url_fetch` | `url_fetch.rs` |
| `web_search` | `web_search.rs` |

### Documentation

| Tool | Source |
|---|---|
| `docs_read` | `docs_read.rs` — list or read the gateway's hosted documentation; see [Hosted documentation](#hosted-documentation) |

### Workspace tool output

Tools that produce file artefacts write them into the attached workspace at:

```
.swage/tool-output/<turn_id>/<call_id>/<filename>
```

`shell` writes stdout and stderr artefacts only when the stream was truncated;
a non-truncated stream is reported inline and the artefact file is removed.
`browser_click`, `browser_select`, and `browser_scroll` write a post-action
screenshot when the browser screenshot policy is not `Off`; `browser_type`
never captures. `browser_snapshot` writes a full accessibility snapshot as
text only when the rendered snapshot is truncated; otherwise the result is
inline with no artefact. `url_fetch` writes `raw-body.html` only when the
HTML body exceeds its capture limit; a body that fits is reported inline and
the artefact file is removed.

The tool result reports the artefact's relative path (e.g.
`.swage/tool-output/turn/call/stdout.txt`). `file_read` can open text
artefacts directly; it refuses binary content, so screenshots are not
readable through `file_read`. The `<turn_id>/<call_id>` structure prevents
repeated calls from overwriting each other.

On first write, the gateway creates `.swage/tool-output/` and writes a
`.gitignore` containing `*` into it, so tool output never appears as
untracked files in the workspace's Git repository.

Sessions without an attached workspace write tool output into the session's
scratch directory instead.

Plugin tools (`plugin.rs`) are dynamically registered from operator
configuration; their names come from the plugin manifest.
