---
description: Agent definitions, tool permissions, approval mechanics, dispatch-bound grant intersection, and space-targeted session creation
---

# Agents and permissions

## Agent definitions

A durable agent definition is a DB-stored record (`AgentDefinition` in
`swage-storage`) that gives a session its character. Each definition carries:

| Field | Purpose |
|---|---|
| `id` | Stable agent id; sessions carry it as `sessions.agent_id` |
| `space_id` | `None` = global; `Some(uuid)` = space-scoped, shadows a global definition with the same id within that space |
| `display_name` | Human-readable label |
| `description` | Free-text description |
| `instructions` | Appended after operator instructions (never precede them); replaces outright when operator instructions are empty |
| `model_id` | Model preference; applied only when the model id exists in `config.models.entries`, otherwise ignored with a warning |
| `tool_permissions` | Optional JSON overlay (serialised `ToolPermissionConfig`); intersects with the resolved config's grants — narrows, never widens |
| `skills` | Skill ids this definition activates |
| `output_schema` | Optional JSON Schema enforced on every final response |
| `output_max_retries` | Override for schema-validation retry limit |

### Overlay onto the config cascade

`apply_agent_definition` (in `turn/configuration.rs`) overlays a definition
onto the session's resolved file config (Global → Space → Agent `.toml`
files). The overlay:

1. Appends instructions after operator instructions.
2. Sets the default model if the id is present in `config.models.entries`.
3. Intersects tool permissions via `intersect_tool_permissions` — allow sets
   are intersected, deny sets are unioned. A malformed overlay is logged and
   ignored (the config's own grants stand).
4. Sets `declared_skills` and, when present, `output_schema` and
   `output_max_retries`.

## Tool permission model

### Resolution order

`check_permission` (`swage-tool/src/permission.rs`) evaluates in strict
order:

1. **Deny wins.** If the tool is in `deny`, return `PermissionDenied(ExplicitlyDenied)`.
2. **Allow gate.** The tool must match the `allow` list — by direct name,
   `"*"` wildcard, or bundle expansion. No match → `PermissionDenied(NotAllowed)`.
3. **Approval check.** If the tool matches a `require_approval` entry
   (direct or bundle-expanded), return `RequiresApproval` with optional
   per-entry `timeout_secs` and `timeout_policy` overrides.
4. **Allow.** Return `Allowed`.

`require_approval` entries never grant access — they only gate tools already
in the allow set. An entry for a tool outside `allow` is inert.

### Defaults

The file-config default (`ToolPermissionConfig::default()` in
`swage-config/src/model.rs`) is:

- `allow: ["*"]` — all tools permitted
- `deny: []`
- Eight high-risk tools gated behind `require_approval`:
  `shell`, `file_edit`, `file_write`, `python_execute`, `browser_click`,
  `browser_type`, `browser_select`, `browser_scroll`
- Default bundles: `browsing`, `browser_actions`, `scheduling`,
  `knowledge-read`, `knowledge-write`, `knowledge-curation`,
  `knowledge-citation`, `knowledge-orientation-curation`

### Effect of a restricted allow list

When an agent definition sets `allow: ["tool_a", "tool_b"]`, the overlay
intersects with the config's default `["*"]`. Since `*` on the config side
defers to the overlay, the effective allow becomes `["tool_a", "tool_b"]`.
The default `require_approval` entries for `shell`, `file_edit`, etc. remain
in the config but are inert: `check_permission` rejects those tools at the
allow gate (step 2) before reaching the approval check (step 3). Setting
`allow: []` yields zero permitted tools.

## Dispatch-bound grant intersection

When an agent delegates a child, the child's effective permissions are the
intersection of its own config and every ancestor dispatcher's grants.

### Lineage

`GatewayDelegationHandler.handle_delegation` (in `turn/delegation.rs`):

1. Loads the target agent's file config and overlays its durable definition
   (`apply_agent_definition`).
2. Fetches the sender's dispatch-bound lineage from session metadata
   (`dispatch_bound_agent_ids`), then appends the sender's own agent id.
3. Calls `apply_dispatch_bounds` with the full lineage — each ancestor's
   resolved config (file config + its own definition) is loaded and applied
   in order via `apply_dispatch_bound`.
4. Stores the lineage in the child's session metadata so further delegation
   continues to narrow.

### Intersection mechanics

`apply_dispatch_bound` (`turn/configuration.rs`) operates on **expanded**
allow sets (bundles resolved to concrete tool names before intersecting,
preventing a shared bundle label from smuggling a wider definition):

1. **Allow intersection.** Both worker and bound allow lists are expanded
   through their respective bundles. Set-intersection produces the effective
   allow. Wildcard on either side defers to the other.
2. **Deny union.** The bound's denies are appended.
3. **Worker approval pruning.** The worker's own `require_approval` entries
   are explicitly filtered to only tools in the effective allow set — unlike
   the agent-definition overlay, where inert entries simply remain.
4. **Bound approval carry-forward.** The bound's `require_approval` entries
   are expanded through the bound's own bundles and carried onto the worker
   for any tool that is (a) in the effective allow set and (b) not already
   approval-gated by the worker. Carried entries use the worker's own
   timeout defaults.

### Audit record

`effective_tool_permissions_record` produces a JSON object with all bundle
labels expanded to concrete tool names:

```json
{ "allow": [...], "deny": [...], "require_approval": [...] }
```

This record is attached to the delegated assembly report
(`DelegatedAssembly.report`) and emitted via the `ContextAssembled` server
event for audit and inspection. It is not injected into the child's model
context — enforcement is handled at the dispatch/permission seam via
`check_permission`, not via prompt text.

## Space-targeted session creation

`CreateConversationRequest` (`swage-protocol/src/client/conversation.rs`)
carries `#[serde(deny_unknown_fields)]`. Its only fields are
`selected_model_id` and `agent_id` (both optional). Any unrecognised field —
including `space_id` — is rejected at deserialisation as a 422 error. This
is a structural guarantee from `deny_unknown_fields`, not a
space_id-specific validation.

Space-targeted creation uses a separate path-based endpoint:

```
POST /api/spaces/{space_id}/sessions
```

The handler (`create_space_session` in `handlers.rs`) accepts the same
`CreateConversationRequest` body (or no body) and creates the session
directly in the named space, returning 201 on success or 404 if the space is
not found.

## Sandbox and approval flow

### Sandbox isolation

Local tool calls that are eligible for sandboxed execution run inside a
two-layer sandbox (`swage-sandbox`):

1. **Bubblewrap (bwrap)** — Linux namespace isolation (PID, network, mount,
   user, IPC, UTS). `--unshare-all` with `--die-with-parent`. Read-only
   root bind with read-write overlays for the working directory and
   configured mounts.
2. **Landlock** — kernel-enforced per-path filesystem access control,
   applied inside the child process. Supports ABI 1 (Linux 5.13+), ABI 2
   (5.19+, adds `REFER`), ABI 3 (6.2+, adds `TRUNCATE`). Degrades
   gracefully on older kernels. Read-only system paths: `/bin`, `/lib`,
   `/lib64`, `/usr/bin`, `/usr/lib`, `/usr/lib64`, `/etc/ld.so.cache`.
   Always-writable paths: the working directory and `/tmp`, plus any
   configured write paths. Restrictions are irrevocable within the process.

The execution boundary is selected before this sandbox path. When a session
targets a non-local device, tool calls go to that device. On a local device,
tools that require gateway state or host facilities execute directly in the
gateway process; tools with resolved secret parameters also stay on the host
so their raw values do not cross sandbox IPC. These host/device executions
are not confined by the bubblewrap and Landlock layers described here.

Credential isolation strips `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and
`GOOGLE_API_KEY` from the sandbox child environment. Provider
`api_key_env` names are configurable, however, and any differently named
environment variable is not stripped by this mechanism. LLM completions use
the host IPC proxy, so the child does not need provider credentials for
completion requests.

### Sandbox configuration

| Option | Type | Default |
|---|---|---|
| `sandbox.enabled` | bool | `true` |
| `sandbox.allow_degraded_execution` | bool | `false` |

Setting `sandbox.enabled = false` does not by itself disable sandboxing —
it tells the dispatcher to skip sandbox setup, which counts as degraded
execution. The dispatcher requires the explicit opt-in
`sandbox.allow_degraded_execution = true` before it will proceed without a
sandbox. Without that opt-in, dispatcher construction fails with
`SandboxError::Unavailable`. To run unsandboxed, set both:

```toml
[sandbox]
enabled = false
allow_degraded_execution = true
```

When `enabled` is true and sandbox setup fails at runtime, the recovery
path depends on the turn's attendance:

- **Attended turns** (interactive client connected) — the turn pauses with an
  approval prompt asking whether to proceed unsandboxed. The client may
  approve (the turn retries with degraded execution), reject (the turn is
  marked Failed), or the prompt may time out or be interrupted.
- **Unattended or delegated turns** — the turn is blocked and marked Failed.
  The degradation is recorded in the audit trail via an
  `execution_isolation_degraded` event.

`allow_degraded_execution = true` also opts in to unsandboxed execution
without the approval prompt when sandbox setup fails at runtime, for both
attended and unattended turns. The degradation is recorded in the audit
trail regardless of how it was authorised.

### Approval channel

When `check_permission` returns `RequiresApproval`, the turn pauses and
sends an `ApprovalRequest` to the connected client. The client responds with
a decision (`Approved` or `Rejected`) and a scope.

**Scopes** (`ApprovalDecisionScope`):

| Scope | Persistence | Reach |
|---|---|---|
| `Once` | None — applies to the single pending call | This call only |
| `Session` | In-memory `session_approval_grants`, until the gateway exits | Any session in this gateway process |
| `Permanent` | In-memory + persisted to disk | This space |
| `PermanentGlobal` | In-memory + persisted to disk across all spaces | All spaces |

`Session` is process-scoped, not session-scoped, despite the name.
`ApprovalGrantKey` carries no session identifier, and `from_approval` sets
`space_id: None` for tool and hook approvals, so the grant is stored under a
global key. `build_permission_config` drops the matching `require_approval`
entry whenever that key is present, for every dispatcher built afterwards —
in any session, any space, until the gateway process exits. Existing
dispatchers keep their original config; the grant takes effect on the next
one built.

Both `Permanent` tool grants and location grants (workspace attachment trust)
are keyed by `space_id`. `Session` grants are global: tool and hook approvals
get `space_id: None` from `from_approval`, and location approvals explicitly
use a global location key. `PermanentGlobal` grants are global as well.

### Timeout and rejection

| Setting | Default | Source |
|---|---|---|
| `hitl.timeout` | `300` seconds (5 minutes) | `HitlConfig::default()` |
| `hitl.timeout_policy` | `Reject` | `HitlConfig::default()` |

Per-tool overrides via `RequireApprovalEntry::WithOptions { timeout, timeout_policy }`.

**Timeout policies:**

- `Reject` — treat timeout as rejection; the agent receives feedback and the
  turn continues.
- `Interrupt` — mark the turn as Interrupted; no further tool calls or
  completions are issued.
