---
description: How Swage defines agents, resolves tool permissions, enforces approval, isolates tool execution, and handles delegated grant inheritance
---

# Agents and permissions

Swage governs what every agent session can do through a layered permission
system: agent definitions narrow what the file-based config cascade grants,
delegation chains narrow further, and human approval gates high-risk calls.

## Agent definitions

An agent definition is a durable record stored in the database that gives a
session its character — its instructions, model preference, tool permissions,
skills, and optional output schema. Each definition carries a stable `id`
that sessions reference.

Definitions can be **global** or **space-scoped**. A space-scoped definition
with the same id as a global one shadows it within that space, so the same
agent id can behave differently per space without requiring separate ids.

### How the overlay works

When a session starts, the gateway resolves the file-based configuration
cascade (Global → Space → Agent `.toml` files) into a `ResolvedConfig`, then
overlays the agent definition on top. The overlay:

- **Instructions** are appended after operator instructions, preserving the
  operator's ambient guidance while letting the definition extend it.
- **Model preference** takes effect only when the named model id exists in
  the configured model entries — an unconfigured model is logged as a warning
  and ignored, so the session falls back to the resolved default.
- **Tool permissions** are intersected with the config's grants. This can
  only narrow what the config allows, never widen it: the effective allow set
  is the intersection of both sides, and denies from either side stand.
- **Skills**, **output schema**, and **output retry limit** are set directly
  from the definition.

A malformed permission overlay (one that fails to parse as
`ToolPermissionConfig`) is logged and ignored — the config's own grants stand
rather than the session silently running with an unintended permission set.

## Tool permission model

### Default grants

A fresh configuration allows all tools (`allow: ["*"]`) and gates eight
high-risk tools behind human approval: `shell`, `file_edit`, `file_write`,
`python_execute`, `browser_click`, `browser_type`, `browser_select`, and
`browser_scroll`. This default is defined in `ToolPermissionConfig::default()`
and applies unless a config file or agent definition narrows it.

### Resolution order

When a tool call is attempted, `check_permission` evaluates the resolved
permission config in a fixed order:

1. **Deny list.** If the tool appears in `deny`, the call is rejected
   outright — deny always wins.
2. **Allow gate.** The tool must appear in `allow`, either by direct name,
   the `"*"` wildcard, or through a named bundle that expands to include it.
   If no match is found, the call is rejected.
3. **Approval check.** If the tool matches a `require_approval` entry, the
   call pauses for human approval before proceeding.
4. **Allowed.** The call proceeds.

Because the allow gate precedes the approval check, `require_approval`
entries never grant access on their own. They only gate tools that are already in the
allow set. An approval entry for a tool that is not allowed is inert — the
tool is rejected at step 2 before the approval check at step 3 is reached.

### What happens when an agent restricts its allow list

When an agent definition sets a narrow allow list — say
`allow: ["knowledge_search", "knowledge_orient"]` — the overlay intersects
it with the config's default `["*"]`. Since `*` on the config side defers to
the overlay, the effective allow becomes just those two tools. The eight
default `require_approval` entries remain in the config but are functionally
inert: any of those tools would be rejected at the allow gate before the
approval check. Setting `allow: []` yields zero permitted tools — a
fully locked-down agent.

## Dispatch-bound grant intersection

When an agent delegates work to a child — through a workflow dispatch or any
other delegation path — the child's effective permissions are confined to
what every agent in the dispatch chain was itself permitted. Permissions can
only narrow as the chain grows; a child never gains access its parent lacked.

### How the lineage builds

Each delegation appends the sender's agent id to a lineage stored in the
child's session metadata. When the child's turn configuration is resolved,
`apply_dispatch_bounds` walks this lineage, loading each ancestor's resolved
config (file config plus its own agent-definition overlay) and applying it as
a bound.

### Bundle expansion before intersection

The intersection operates on **expanded** allow sets: bundle labels are
resolved to concrete tool names through each side's own bundle definitions
before the sets are intersected. This prevents a shared bundle name from
smuggling a wider definition — if the worker's config defines
`"browsing"` as five tools and the dispatcher's defines it as two, only the
two that both sides agree on survive.

### Approval demands carry forward

Dispatch-bound intersection also handles approval requirements:

- The worker's own `require_approval` entries are pruned to only tools in the
  effective allow set — an explicit filtering step, unlike the agent-definition
  overlay where inert entries simply remain.
- The dispatcher's approval demands carry forward onto the worker for any
  tool that is in the effective allow set and not already approval-gated.
  A tool the dispatcher could only use with approval cannot run approval-free
  in a worker it dispatched.

### Audit record

After all bounds are applied, the gateway produces an
`effective_tool_permissions_record` — a JSON object listing the effective
allow, deny, and require_approval sets with all bundle labels expanded to
concrete tool names. This record is attached to the delegated assembly report
and emitted as part of the `ContextAssembled` event for audit and inspection.
It is not injected into the child's model context — enforcement is handled at
the dispatch and permission seam (`check_permission`), not via prompt text.

## Space-targeted session creation

A session is created in a specific space through a path-based endpoint:

```
POST /api/spaces/{space_id}/sessions
```

The request body is the same `CreateConversationRequest` used elsewhere
(with optional `selected_model_id` and `agent_id` fields), or it can be
omitted entirely. The endpoint returns 201 on success or 404 if the space is
not found.

There is no `space_id` field in the request body. `CreateConversationRequest`
carries `deny_unknown_fields` — any unrecognised field in the JSON body,
`space_id` included, is rejected at deserialisation as a 422 error. This is
a structural guarantee from serde's `deny_unknown_fields` attribute, not a
space_id-specific validation check: the same rejection applies to any
unexpected field.

## Sandbox isolation

Local tool calls that are eligible for sandboxed execution run inside a
two-layer sandbox that confines what the sandbox child process can reach.

The executor chooses the host/device boundary before this sandbox path. If a
session targets a non-local device, tool calls are sent to that device. On a
local device, tools that need live gateway state or host facilities execute
directly in the gateway process; tools with resolved secret parameters also
stay on the host so raw secret values do not cross sandbox IPC. These direct
host and remote-device executions do not receive the bubblewrap and Landlock
isolation described below.

### Bubblewrap

The outer layer uses bubblewrap (bwrap) for Linux namespace isolation:
PID, network, mount, user, IPC, and UTS namespaces are all unshared
(`--unshare-all`), and the child dies with its parent
(`--die-with-parent`). The root filesystem is bind-mounted read-only, with
explicit read-write overlays for the working directory and any configured
additional mounts.

The sandbox launcher strips `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and
`GOOGLE_API_KEY` from the child's environment. This is a fixed list:
provider `api_key_env` names are configurable, and a differently named
environment variable is not stripped by this mechanism. LLM completions are
proxied back through the host via IPC, so the child does not need provider
credentials for completion requests.

### Landlock

The inner layer applies Landlock — a kernel-enforced, per-path filesystem
access-control mechanism. It supports ABI 1 (Linux 5.13+), ABI 2 (5.19+,
adding the `REFER` permission), and ABI 3 (6.2+, adding `TRUNCATE`), and
degrades gracefully on kernels that predate it.

System paths required for tool execution (`/bin`, `/lib`, `/lib64`,
`/usr/bin`, `/usr/lib`, `/usr/lib64`, `/etc/ld.so.cache`) are mounted
read-only. The working directory and `/tmp` are always writable, along with
any explicitly configured write paths. Landlock restrictions are irrevocable
within the process.

### Configuration

Sandboxing is enabled by default. Two config options govern sandbox
behaviour:

| Option | Default | Effect |
|---|---|---|
| `sandbox.enabled` | `true` | Whether to set up sandbox isolation for each turn |
| `sandbox.allow_degraded_execution` | `false` | Whether to allow execution without sandbox isolation |

Running without sandbox isolation is always treated as degraded execution,
whether you chose it or whether sandbox setup failed at runtime. The
gateway requires an explicit opt-in before it will proceed in either case:
`sandbox.allow_degraded_execution` must be set to `true`. Without it,
dispatcher construction fails.

To disable sandboxing entirely, set both options together:

```toml
[sandbox]
enabled = false
allow_degraded_execution = true
```

When sandboxing is enabled and sandbox setup fails at runtime, the recovery
path depends on whether the turn is attended:

- **Attended turns** (interactive client connected) receive an approval
  prompt asking whether to proceed without sandbox isolation. The client can
  approve, reject, or the prompt can time out — a rejected or timed-out
  prompt marks the turn as Failed.
- **Unattended or delegated turns** are blocked and marked Failed immediately.
  There is no approval prompt because no interactive client is available to
  answer it.

Setting `allow_degraded_execution` to `true` also bypasses the approval
prompt for runtime sandbox failures, for both attended and unattended turns.
Degraded execution is always recorded in the audit trail regardless of how
it was authorised.

## Approval flow

When `check_permission` determines a tool call requires approval, the turn
pauses and sends an approval request to the connected client. The client
responds with a decision and a scope that governs how far the approval
reaches.

### Scopes

| Scope | Persistence | Reach |
|---|---|---|
| `Once` | None | The single pending call only |
| `Session` | In-memory, until the gateway exits | All matching calls in any session in this gateway process |
| `Permanent` | In-memory and persisted to disk | All matching calls in this space |
| `PermanentGlobal` | In-memory and persisted to disk across all spaces | All matching calls everywhere |

`Session` reaches further than its name suggests. The grant is not recorded
against the session that approved it, or against a space: it is held in the
gateway process and consulted whenever a new turn dispatcher is built. Every
turn started after the approval — in any conversation, in any space — will
run that tool without prompting, until the gateway is restarted. Turns
already under way keep the permissions they started with. If you want an
approval confined to the work in front of you, `Once` is the only scope that
gives it.

`Permanent` and `PermanentGlobal` also handle location-scoped approvals —
trusting a workspace attachment so that file operations within it no longer
prompt. Location approvals are the only kind recorded against a particular
space.

### Timeout behaviour

Approval requests time out after **300 seconds** (five minutes) by default,
governed by the `hitl.timeout` setting. The default timeout policy is
**Reject**: a timed-out request is treated as a rejection, the agent receives
feedback, and the turn continues. The alternative policy, **Interrupt**, marks
the entire turn as interrupted — no further tool calls or completions are
issued.

Per-tool overrides are possible through the `WithOptions` variant of a
`require_approval` entry, which can set a custom `timeout` and
`timeout_policy` for individual tools.
