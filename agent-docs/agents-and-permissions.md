---
description: Agent definitions, operator guidance (AGENTS.md), tool permissions, approval mechanics, dispatch-bound grant intersection, effort resolution, topic-targeted session creation, context configuration, voice configuration, experimentation controls, and succession compaction
---

# Agents and permissions

## Agent definitions

An agent definition is a durable record that gives a session its character —
its instructions, model preference, tool permissions, skills, and optional
output schema. Each definition carries a stable `id` that sessions reference.

| Field | Purpose |
|---|---|
| `id` | Stable agent id; sessions reference it |
| `topic_id` | Absent = global; a topic UUID = topic-scoped, shadows a global definition with the same id within that topic |
| `display_name` | Human-readable label |
| `description` | Required free-text description |
| `instructions` | Appended after operator instructions; replaces outright when operator instructions are empty |
| `model_id` | Model preference; applied only when the named model id exists in the configured model entries, otherwise ignored with a warning |
| `tool_permissions` | Optional JSON permission overlay; intersects with the resolved config's grants — narrows, never widens |
| `skills` | Skill ids this definition activates |
| `output_schema` | Optional JSON Schema enforced on every final response |
| `output_max_retries` | Override for schema-validation retry limit |

### How the overlay works

When a session starts, the gateway resolves the file-based configuration
cascade (Global → Topic → Agent `.toml` files) into a resolved config, then
overlays the agent definition on top:

1. Instructions are appended after operator instructions.
2. Model preference takes effect only when the named model id exists in the
   configured model entries; an unrecognised model is logged as a warning and
   ignored.
3. Tool permissions are intersected — allow sets are intersected, deny sets are
   unioned. A malformed overlay is logged and ignored (the config's own grants
   stand).
4. Skills, output schema, and output retry limit are set directly from the
   definition.

### Creating and editing definitions

Definitions can be created and edited through two surfaces:

**API.** `POST /api/operator/agent-definitions` creates a definition;
`PUT /api/operator/agent-definitions/{agent_id}` replaces one. A complete
creation request:

```json
{
  "id": "reader",
  "display_name": "Read-only assistant",
  "description": "An agent that can search and read but not write.",
  "instructions": "You help the user find and read information. Do not modify files.",
  "tool_permissions": {
    "allow": ["knowledge_search", "knowledge_orient", "docs_read", "file_read", "file_list", "file_search", "url_fetch", "web_search"],
    "deny": [],
    "bundles": {},
    "require_approval": []
  }
}
```

`description` is required. `topic_id` scopes the definition to one topic;
omit it for a global definition. `tool_permissions` is a JSON object with
`allow`, `deny`, `bundles`, and `require_approval`. `bundles` maps bundle
names to tool-name arrays — it must be present (use `{}` when no custom
bundles are needed). The overlay intersects with the config cascade's
grants, so it can only narrow what the config allows. Fields not shown
above (`model_id`, `skills`, `output_schema`, `output_max_retries`) are
optional.

Conversational authoring of agent definitions is not yet available. The
API routes above are the supported surface.

## Effort

Reasoning effort is a single provider-neutral vocabulary of seven levels,
ordered lowest to highest:

| Level | Wire spelling |
|---|---|
| Off | `off` |
| Minimal | `minimal` |
| Low | `low` |
| Medium | `medium` |
| High | `high` |
| Xhigh | `xhigh` |
| Max | `max` |

### Where effort is set

Effort resolves through a three-step fallback:

1. **Per-turn override** — an explicit `effort_override` passed by the
   caller (workflow dispatches, delegated turns).
2. **Session metadata** — set at session creation via the `effort` field
   of `CreateConversationRequest`. Stored in the session's metadata JSON
   and read back on each turn.
3. **Model default** — `default_effort` on the model's catalogue entry
   (`ModelEndpointConfig` in config, `ModelSettings` on the wire). When
   set, every turn using that model inherits the default unless
   overridden above.

When none of these supplies a value, the turn sends no effort control.

The web UI's Settings → Providers panel exposes a **Default effort**
dropdown on each model entry, with all seven levels plus "No effort".
This sets `default_effort` on the model's config entry.

Config example:

```toml
[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-5"
default_effort = "high"
```

### Resolution

Each model declares an `EffortCapability` — either through the catalogue
entry's `capabilities.effort` field (config or wire), or through the
provider adapter's built-in declaration for models it serves. Three
capability shapes:

| Shape | Fields | Behaviour |
|---|---|---|
| `Levels` | `levels: [Effort]`, `can_disable: bool` | The control takes named levels from a fixed set. `can_disable` governs whether `off` sends the model's own disable or clamps to the lowest supported level. |
| `Budget` | `min_tokens: u32`, `max_tokens: u32`, `can_disable: bool` | The control takes a numeric thinking budget. The six non-off levels spread monotonically across the declared range. |
| `None` | — | No effort control; every intent is omitted from the request. |

`resolve_effort` (`swage-core/src/effort.rs`) is the single resolution
point. Rules:

- A level the model supports natively passes through.
- An unsupported level clamps to the nearest supported level below the
  request, or to the lowest supported level when nothing lower exists.
- `off` sends the model's disable when `can_disable` is true; otherwise
  clamps to the lowest level with a note.
- A budget model receives a monotone budget across its range (higher
  intent never resolves to a smaller budget).
- A model declaring `None` has the control omitted entirely.

A capability that cannot be honoured (empty levels, inverted budget
range) is refused at config validation (`EffortCapability::validate`),
not at resolution time.

### Adapter declarations

The built-in adapters declare:

| Adapter | Capability |
|---|---|
| Anthropic | Levels: `low`, `medium`, `high`, `max`; `can_disable: true` (Claude models only; non-Claude models return `None`) |
| Chat Completions (OpenAI) | Levels: `minimal`, `low`, `medium`, `high`, `xhigh`; `can_disable: true` (reasoning models only; non-reasoning models return `None`) |
| Responses (OpenAI) | Same as Chat Completions |
| Google | Budget: Gemini 2.5 Flash `1`–`24576` tokens, Flash Lite `512`–`24576` tokens; `can_disable: true`. Non-Flash Google models return `None`. |
| Codex | Levels: `low`, `medium`, `high`, `xhigh`; `can_disable: false` (GPT-5.x models only; others return `None`) |

A catalogue entry's `capabilities.effort` overrides the adapter's
declaration for that model.

### Audit trail

The turn's assembly report records the resolution under
`provider_visible.effort` as a `ResolvedEffort`:

```json
{
  "requested": "max",
  "control": { "kind": "level", "level": "xhigh" },
  "clamped_to": "xhigh",
  "note": "max unsupported; clamped to xhigh"
}
```

When the request was honoured as-is, `clamped_to` and `note` are absent.
This data is stored in the assembly report JSON and available through
`GET /api/turns/{id}/context`; the web UI's context inspector panel
does not yet render it.

## Effective agent registry

A fresh installation's effective agent registry holds one entry: the
shipped default (`"gateway"`, display name "Swage default"). No other
agent definitions ship with the gateway. The start-conversation picker
lists this default plus any operator-authored definitions visible to
the topic (via `GET /api/topics/{topicId}/agent-definitions`).
`CreateConversationRequest`'s `agent_id` field selects one of these:
`"gateway"` or an operator-authored definition visible to the target
topic; the gateway rejects anything else with a validation error.

A shipped workflow's roles (the research sweep's researcher, organiser,
and synthesist) are bare dispatches inside the workflow script, not
standalone agent definitions — they do not appear in the registry or the
picker.

## Operator guidance (`AGENTS.md`)

Operator-authored guidance is a second instruction source, parallel to the
`instructions/` directory. A file named `AGENTS.md` at the root of a
guidance directory is read, and its content enters every turn as an
instruction rule in the prompts element.

### Where it is read from

Two kinds of root are checked, in this order:

1. **Config root** (`~/.swage/` or `$SWAGE_HOME`) — always read. The
   resulting rule carries the `Global` provenance level.
2. **Trusted, readable workspace roots** — the active workspace and any
   workspace attachments that are both trusted and readable. Each produces
   a rule at the `Topic` provenance level.

A workspace root that is not trusted, or that is trusted but not readable
(e.g. a write-only attachment), contributes nothing: workspace content
enters the model's instructions only when both conditions are met. There is
no config knob to disable global guidance — an `AGENTS.md` at the config
root is always read.

### Ordering and layering

Global guidance appears before workspace guidance, so workspace rules
refine the operator's baseline rather than the reverse. Both layers append
after the resolved instruction baseline (the merged `instructions/`
content), extending it without replacing it. Agent-definition instructions
then append after operator guidance, preserving the full chain:
resolved instructions → operator guidance → agent-definition instructions.

### Error handling

An absent `AGENTS.md` is silently skipped — a root without a guidance file
simply contributes nothing. A path that exists but is not a regular file
(e.g. a directory named `AGENTS.md`) is treated the same way. An
unreadable or non-UTF-8 file is logged as a warning and skipped — guidance
never fails a turn.

### Code path

Both foreground and delegated turns call `operator_guidance_rules`
(in `turn/operator_guidance.rs`), which reads the config root's guidance
file then each trusted, readable workspace root's, and returns the ordered rule vec.
The reading itself is in `swage_config::agent_guidance::read_agent_guidance`.
The rules are passed to `build_assembly_elements` (in `turn/context.rs`),
which appends them after the resolved-instructions rule so the
`PromptsElement` emits them in the correct order.

### Visibility in the context inspector

The turn context inspector's prompts element shows the provenance of each
rule by hierarchy level — e.g. `prompt rules (3 total: 1 Global, 2 Topic)`.
Operator guidance rules from the config root appear as `Global`; those from
workspace roots appear as `Topic`.

## Search configuration

`[tools.search]` configures `web_search`. SearxNG is the only supported backend.
The only fields are `provider_type` (must be `"searxng"`) and `base_url` (the
SearxNG instance URL). Unsupported provider types and unrecognised keys are
refused when configuration loads and when settings are saved.

The web search provider can be edited in the web UI under **Settings**; saved
changes apply to new turns.

## Tool permission model

### Permission bypass

`dangerously_skip_permissions = true` is a top-level escape hatch from this
permission model, not another layer within it. If either the session-resolved
configuration or the gateway's startup configuration enables it,
`build_permission_config` returns wildcard allow with no denies or approval
requirements. The turn also pre-approves all remaining tool, location, and
hook gates, including out-of-workspace location prompts. The resolution rules
below apply only while this setting is false in both configurations.

### Resolution order

`check_permission` (`swage-tool/src/permission.rs`) evaluates in strict
order:

1. **Deny wins.** If the tool is in `deny`, return `PermissionDenied(ExplicitlyDenied)`.
2. **Allow gate.** The tool must match the `allow` list — by direct name,
   `"*"` wildcard, or bundle expansion. No match → `PermissionDenied(NotAllowed)`.
3. **Approval check.** If the tool matches a `require_approval` entry
   (direct or bundle-expanded), return `RequiresApproval` with optional
   per-entry `timeout_secs` and `timeout_policy` overrides. An attended turn
   pauses for the operator's decision. Unattended turns (scheduled, channel,
   workflow-worker, delegated) run with `approval_prompts_enabled = false`
   (`swage-agent/src/config.rs`), which pre-approves every gate, so they
   proceed on grants alone.
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
  `browser_type`, `browser_select`, `browser_scroll`. A `require_approval`
  list at any configuration level replaces this list (`merge.rs`), so
  `require_approval = []` removes the prompts.
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

## Topic-targeted session creation

`CreateConversationRequest` (`swage-protocol/src/client/conversation.rs`)
carries `#[serde(deny_unknown_fields)]`. Its only fields are
`selected_model_id` and `agent_id` (both optional). Any unrecognised field —
including `topic_id` — is rejected at deserialisation as a 422 error. This
is a structural guarantee from `deny_unknown_fields`, not a
topic_id-specific validation.

Topic-targeted creation uses a separate path-based endpoint:

```
POST /api/topics/{topic_id}/sessions
```

The handler (`create_topic_session` in `handlers.rs`) accepts the same
`CreateConversationRequest` body (or no body) and creates the session
directly in the named topic, returning 201 on success or 404 if the topic is
not found.

## Sandbox and approval flow

### Sandbox isolation

Sandboxing is on by default: eligible local tool calls run inside a two-layer
sandbox (`swage-sandbox`):

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

With no sandbox setting in the configuration cascade, per-turn sandboxing is
on (`SandboxConfig::default()` in `swage-protocol/src/client/settings.rs`). A
topic inherits sandbox settings from the global configuration; an agent can
override them. When `sandbox.enabled` resolves to `false`, local gateway turns
run unsandboxed without requiring `allow_degraded_execution`. The persisted
turn boundary is `Degraded` with reason `sandbox_disabled_by_configuration`.
A declared install boundary, such as a container image, takes precedence over
sandbox settings and remains the recorded boundary.

To turn sandboxing off for a topic, add this to its `config.toml`:

```toml
[sandbox]
enabled = false
```

When `enabled` is true and sandbox setup fails at runtime — most often
because `bwrap` is not installed — the recovery path depends on the turn's
attendance:

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
| `Permanent` | In-memory + persisted to disk | This topic while the gateway runs; after restart, subject to agent-level `require_approval` configuration |
| `PermanentGlobal` | In-memory + persisted to disk across all topics | All topics while the gateway runs; after restart, subject to topic- and agent-level `require_approval` configuration |

`Session` is process-scoped, not session-scoped, despite the name.
`ApprovalGrantKey` carries no session identifier, and `from_approval` sets
`topic_id: None` for tool and hook approvals, so the grant is stored under a
global key. `build_permission_config` drops the matching `require_approval`
entry whenever that key is present, for every dispatcher built afterwards —
in any session, any topic, until the gateway process exits. Existing
dispatchers keep their original config; the grant takes effect on the next
one built.

For tool approvals, the persisted reach in the table is conditional after a
restart. Persistence removes the tool from `require_approval` in the topic or
global TOML file. That field uses replacement semantics in the Global → Topic
→ Agent cascade, so an agent file can replace a persisted topic list, and a
topic or agent file can replace a persisted global list. The process-local
grant masks such a narrower approval requirement until the gateway exits;
after restart, the narrower configuration takes effect again.

`Permanent` and `PermanentGlobal` also handle location-scoped approvals
(workspace attachment trust). For tool and location approvals alike,
`Permanent` grants carry the approving topic's `topic_id`, while
`PermanentGlobal` grants use `topic_id: None`. `Session` grants are also
global: tool and hook approvals get `topic_id: None` from `from_approval`,
and location approvals explicitly use a global location key.

### Timeout and rejection

| Setting | Default |
|---|---|
| `hitl.timeout` | `300` seconds (5 minutes) |
| `hitl.timeout_policy` | `Reject` |

Per-tool overrides use an object entry in `require_approval` instead of a
plain string:

```toml
require_approval = ["write_file", { tool = "execute_command", timeout = 600 }]
```

The object form accepts `tool` (required), `timeout` (seconds), and
`timeout_policy` for individual tools.

**Timeout policies:**

- `Reject` — treat timeout as rejection; the agent receives feedback and the
  turn continues.
- `Interrupt` — mark the turn as Interrupted; no further tool calls or
  completions are issued.

## Context configuration

The `[context]` section of the configuration cascade (Global → Topic → Agent
`.toml` files) governs context assembly and compaction.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `assembly` | string | `"default"` | Named context assembly to use |
| `alternatives` | array of strings | `[]` | Named assemblies registered as hidden alternatives; selection records the chosen name locally |
| `timeout` | integer (1–60) | `5` | Assembly timeout in seconds |
| `budget` | integer or absent | absent | Context token budget; must be positive when set |
| `compaction_threshold` | `{ messages = N }` or `{ tokens = N }`, or absent | absent (100 messages) | Size past which a turn's context is compacted; `N` must be positive |
| `diagnosis_reader_model` | string or absent | absent (session's own model) | Model entry or alias for the diagnosis readers during wall succession |
| `diagnosis_synthesis_model` | string or absent | absent (session's own model) | Model entry or alias for the diagnosis synthesis during wall succession |

Every key cascades: a topic file overrides the global, an agent file overrides
the topic. Settings saves reject an invalid `compaction_threshold` (non-positive
count) and a `diagnosis_reader_model` or `diagnosis_synthesis_model` that names
neither a model entry nor an alias in the configuration being saved.

## Voice configuration

The `[voice]` section configures speech transcription and synthesis.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `transcription_model` | string or absent | absent | Model id sent to the default provider's `/audio/transcriptions` endpoint |
| `speech_model` | string or absent | absent | Model id sent to the default provider's `/audio/speech` endpoint |

Voice uses the default provider's OpenAI-protocol audio endpoints. It never
uses the chat model. When a capability is unavailable — because the default
provider is absent, its credentials are unavailable, its protocol has no
audio API, or a model id is unset — `GET /api/voice/capabilities` returns an
`unavailable_reason` naming what to address
(see [Interfaces and tooling — Voice capabilities](interfaces-and-tooling.md#voice-capabilities)).

Config example:

```toml
[voice]
transcription_model = "whisper-1"
speech_model = "tts-1"
```

Both ids can be edited in the web UI under **Settings → Voice**; a save
applies without a gateway restart.

## Experimentation controls

The `[experimentation]` section governs hidden runtime variant selection —
alternative implementations behind stable surfaces, selected per session and
recorded in the audit trail.

### Selection lifetime

A session draws its variant the first time it reaches a location, records
that decision, and keeps it for the rest of its life. Configuration changes
— the global switch, a location's `enabled` flag, a `forced_variant` — bind
only for sessions that reach the location after the change; a session that
already recorded a selection keeps serving it.

### When changes take effect

The Settings save route does not include experimentation fields, so changes
are made by editing `config.toml`. Restart the gateway to apply them:

- **CLI / systemd:** `swage gateway restart`.
- **Desktop app:** quit and reopen the app.
- **Standalone process:** stop and start the process.

Sessions whose topic or agent has its own `config.toml` re-resolve the full
configuration hierarchy (global file included) on each turn, so edits can
reach those sessions without a restart.

### Global switch

```toml
[experimentation]
enabled = true   # default; set to false to disable all variant selection
```

When `enabled` is `false`, every location returns its default variant to
sessions that have not already recorded a selection, and records the
decision as `disabled`. No new alternative is drawn or forced.
Restart the gateway after changing this key.

### Per-location controls

Each location has its own `enabled` flag and an optional `forced_variant`
pin:

```toml
[experimentation.locations."context.working_norms"]
enabled = true
forced_variant = "family-fallback"
```

- `enabled` — defaults to `true`. Setting it to `false` disables variant
  selection for this location only; the default variant serves and the
  decision records as `disabled`.
- `forced_variant` — pins this location to the named variant for every
  session that reaches it after the change.
  The variant must be registered at the location's choice point; an
  unrecognised name is refused. The decision records as `forced`. Omit or
  remove the key to return to random selection.

Restart the gateway after changing either key.

Location ids are application-defined strings. The shipped locations are
`context.assembly`, `context.working_norms`, `task.prompt`, and
`prompts.memory_curator`.

### Alternatives

What can be forced or drawn at a location depends on the alternatives
registered there. On this tree, alternatives are defined in code — each
location's choice point names its registered variants — except for
`context.assembly`, where the operator authors alternatives as JavaScript
files.

A context assembly alternative is a `<name>.js` file placed in an
`assemblies/` directory under the config hierarchy (global, topic, or
agent — the most specific wins). List it in `[context] alternatives` so the
selection loop includes it:

```toml
[context]
assembly = "default"
alternatives = ["my-variant"]
```

The script runs in a caged QuickJS runtime (no IO, no network, no timers)
with four globals:

- `elements` — the turn's context elements, read-only:
  `[{ name, priority, source, tokenCount, messages }]` where `messages` is
  `[{ role, text }]`.
- `ctx` — read-only assembly context. `ctx.variable(name)` returns the
  current session value or `undefined`.
- `add(element_or_messages)` — appends an element (pass an entry from
  `elements`) or a raw `[{ role, text }]` array to the output, in call
  order.
- `omit(name, reason)` — declares that the composition deliberately leaves
  the named element out. Any baseline element neither added nor declared
  through `omit` is restored with a warning.

A minimal reordering assembly (`assemblies/my-variant.js`):

```js
const byName = Object.fromEntries(elements.map(e => [e.name, e]));
add(byName.orientation);
add(byName.prompts);
add(byName.history);
```

A script that errors (missing file, syntax error, runtime error) falls back
to the built-in default assembly. A script that runs without error but
leaves out baseline elements has them restored by the baseline floor, with
warnings — except elements the script declares as omitted via `omit()`,
which stay out. If the final composition ends up genuinely empty (every
baseline element was declared omitted and the script added nothing else),
the assembly also falls back to the default.

The other shipped locations (`context.working_norms`, `task.prompt`,
`prompts.memory_curator`) have their alternatives defined in the gateway
source and cannot be extended through configuration. For
`context.working_norms` the registered variants are `authored-v1` (the
default) and `family-fallback`.

### How the effect shows in operator history

Every variant decision — drawn, forced, disabled, or default-only — is
recorded in the audit trail. The operator history pane
(`GET /api/operator/history`) lists each selection with the location, the
variant that served, the reason (`drawn`, `forced`, `disabled`, or
`default_only`), and the session and turn that produced it.

When selection recording fails (for example because the audit store is
temporarily unavailable), the turn falls back to the default variant
without writing a record. The absence of a record for a turn is how a
later reader identifies that recording failed — there is no
`record_unavailable` entry in history.

### Configuration cascade

Experimentation settings cascade through the file hierarchy (Global → Topic
→ Agent). Per-location entries merge by location id across layers: a topic
file can force a variant the global file leaves to chance, and an agent file
can disable a location the topic file enables.

## Succession compaction

When a turn's assembled message list exceeds the configured
`compaction_threshold` (default: 100 messages), the turn runs on a successor:
the older part of the conversation history is replaced by a summary, the newer
part is kept verbatim, and every other context element — orientation, norms,
prompts, briefings, skills — is rendered afresh from its sources.

### Wall-triggered succession

A session that keeps hitting walls continues on a successor built from a blind
diagnosis, regardless of the context's size. Two triggers are checked within the
6 most recent finished turns:

- 3 failures of the identical tool call (same tool, same arguments) anywhere in
  that window, or
- at least 5 failed tool calls and failed turns combined.

The first trigger is checked first; when both are present, the repeated-call
signal is reported.

The successor carries diagnosis notes in place of the summarised history. Each
turn runs on such a successor while the walls remain among its recent turns:

- **Reuse:** a turn whose recent walls match the set its predecessor's
  diagnosis answered carries that diagnosis again rather than writing a new
  one.
- **Restoration:** a turn with no wall left in the recent window gets its full
  history back.

The diagnosis readers and the synthesis are selected by model catalogue entry
or alias, resolved as ordinary turns resolve a model. When unset, the session's
own model is used.

If any diagnosis reader or the synthesis fails or returns an empty report, the
turn falls back to threshold-only compaction and compacts only when its context
exceeds the threshold.

Transient provider failures — rate limits, timeouts, HTTP 5xx responses,
malformed streams, and connection errors — do not trigger wall succession.
Provider refusals of the agent's request count as walls.

The compaction record carries strategy `succession-degradation` and includes a
diagnosis record with the walls answered and each model call's stage, model,
usage, report, and any failure reason. A reused diagnosis names the turn it
was originally written for. The assembly report retains compaction decisions
from the first assembly and the successor in pass order, even when their names
match.
