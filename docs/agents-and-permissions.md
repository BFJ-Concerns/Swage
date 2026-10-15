---
description: How Swage defines agents (including creating them through the API), reads operator guidance from AGENTS.md, resolves tool permissions, enforces approval, isolates tool execution, handles delegated grant inheritance, resolves effort per model, configures context assembly and compaction, controls experimentation and variant selection, handles wall-triggered succession, and configures voice transcription and synthesis
---

# Agents and permissions

Swage governs what every agent session can do through a layered permission
system: agent definitions narrow what the file-based config cascade grants,
delegation chains narrow further, and attended sessions can require human
approval for configured tool calls.

## Agent definitions

An agent definition is a durable record stored in the database that gives a
session its character — its instructions, model preference, tool permissions,
skills, and optional output schema. Each definition carries a stable `id`
that sessions reference.

Definitions can be **global** or **topic-scoped**. A topic-scoped definition
with the same id as a global one shadows it within that topic, so the same
agent id can behave differently per topic without requiring separate ids.

### How the overlay works

At each turn, the gateway overlays the session's topic and agent layers
(their `config.toml` files and `instructions/` directories, Global → Topic →
Agent) on the running configuration — the global layer as loaded at startup
and replaced by Settings saves — then overlays the agent definition on top.
The overlay:

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

A malformed permission overlay is logged and ignored — the config's own
grants stand rather than the session silently running with an unintended
permission set.

### Creating and editing definitions

Definitions can be created and edited through two surfaces.

**Through the API.** `POST /api/operator/agent-definitions` creates a
definition; `PUT /api/operator/agent-definitions/{agent_id}` replaces one.
A complete creation request:

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
omit it for a global definition. The `tool_permissions` object carries
`allow`, `deny`, `bundles`, and `require_approval`. `bundles` maps bundle
names to tool-name arrays — it must be present (use `{}` when no custom
bundles are needed). The overlay intersects with the config cascade's
grants, so it can only narrow what the config allows, never widen it; its
`require_approval` is not applied, and the approval list comes from the
config files.
Optional fields not shown above include `model_id`, `skills`,
`output_schema`, and `output_max_retries`.

Conversational authoring of agent definitions is not yet available. The
API routes above are the supported surface.

## Effort

Reasoning effort is expressed as a single provider-neutral vocabulary of seven
levels: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`. The gateway
resolves the chosen level once per turn against the model's declared capability,
so every provider adapter receives a control it can express on the wire — no
adapter re-interprets the intent.

### Setting effort

Effort can be set in three places, checked in this order:

1. **Per-turn override** — passed explicitly by workflow dispatches and
   delegated turns.
2. **Session level** — the `effort` field in the session-creation request.
   Stored in the session's metadata and used for every turn in that session
   unless overridden.
3. **Model default** — the `default_effort` field on a model's catalogue
   entry. When set, every session using that model inherits the default.

When none of these supplies a value, no effort control is sent.

The web UI's **Settings → Providers** panel exposes a **Default effort**
dropdown on each model entry, with all seven levels plus "No effort". The
same setting can be written in `config.toml`:

A model entry with a default effort looks like this:

```toml
[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-5-5"
default_effort = "high"
```

### How resolution works

Each model declares what kind of effort control its provider accepts — an
`EffortCapability` — either through the catalogue entry's
`capabilities.effort` field or through the provider adapter's built-in
declaration for models it serves:

- **Named levels** — the control takes specific levels from a fixed set and
  optionally supports a true disable (`off`).
- **Budget** — the control takes a numeric thinking-budget in tokens; the
  six non-off levels spread monotonically across the declared range. `off`
  is handled separately — it sends the model's own disable when available,
  or clamps to the minimum budget otherwise.
- **None** — no effort control exists; the intent is omitted.

The resolution rules are:

- A level the model supports natively passes through unchanged.
- An unsupported level clamps downward to the nearest supported level, or to
  the lowest available level if nothing lower exists.
- `off` sends the model's own disable when it has one; otherwise it clamps
  honestly to the lowest supported level.
- A budget-only model receives a proportional budget — a higher intent never
  resolves to a smaller number.
- A capability that cannot be honoured (an empty level set, an inverted budget
  range) is refused at configuration load, not at resolution time.

### What the adapters declare

| Adapter | Levels | Can disable |
|---|---|---|
| Anthropic | `low`, `medium`, `high`, `max` | Yes (Claude models only) |
| Chat Completions (OpenAI) | `minimal`, `low`, `medium`, `high`, `xhigh` | Yes (reasoning models only; non-reasoning models declare no control) |
| Responses (OpenAI) | Same as Chat Completions | Same |
| Google | Budget: Gemini 2.5 Flash `1`–`24576` tokens, Flash Lite `512`–`24576` tokens | Yes (Flash families only; other Google models declare no control) |
| Codex | `low`, `medium`, `high`, `xhigh` | No (GPT-5.x models only; others declare no control) |

A catalogue entry's `capabilities.effort` overrides the adapter's declaration
for that model, so an operator can declare a custom capability for a
self-hosted or proxied endpoint.

### Audit trail

Every turn that resolves effort records the outcome in the assembly report
under `provider_visible.effort`: what was requested, what control the adapter
sent, and — when the two differ — what the request was clamped to and why.
This data is stored in the report JSON and available through the
`GET /api/turns/{id}/context` endpoint; the web UI's
[turn context inspector](interfaces-and-tooling.md#turn-context-inspector)
does not yet render it.

## What agents are available

A fresh installation's agent registry holds one entry: the shipped default
agent (`"gateway"`, display name **Swage default**). No other agent
definitions ship with the gateway — a shipped workflow's internal roles (the
research sweep's researcher, organiser, and synthesist) are bare dispatches
inside the workflow script, not standalone definitions.

The start-conversation picker in the web UI lists this default plus any
operator-authored definitions visible to the topic. The `agent_id` field of
`CreateConversationRequest` selects one of these — `"gateway"` or an
operator-authored definition visible to the target topic; the gateway
rejects anything else with a validation error.

## Operator guidance (`AGENTS.md`)

Alongside the `instructions/` directory, Swage reads a file named
`AGENTS.md` from the root of each guidance directory and folds its content
into every turn's prompt as an operator-authored instruction rule.

### Sources

Two kinds of root are checked, in this order:

1. **Config root** (`~/.swage/` or `$SWAGE_HOME`) — always read. This is
   global guidance that applies to every session.
2. **Trusted, readable workspace roots** — the active workspace and any
   workspace attachments that are both trusted and readable. Each
   contributes its own `AGENTS.md` if one exists.

A workspace root that is not trusted, or that is trusted but not readable
(e.g. a write-only attachment), contributes nothing — its `AGENTS.md` is
never read. There is no separate setting to disable global guidance.

### Ordering

Global guidance appears before workspace guidance. Both layers append after
the resolved instruction baseline (the merged `instructions/` content),
extending it without replacing it. Agent-definition instructions then
append after operator guidance, so the full chain is: resolved instructions
→ global `AGENTS.md` → workspace `AGENTS.md` → agent-definition
instructions.

### Error handling

A missing `AGENTS.md` is silently skipped. A path that exists but is not a
regular file is also skipped. An unreadable file is logged as a warning and
skipped — guidance never fails a turn.

### Visibility

The turn context inspector's prompts element shows the provenance of each
instruction rule by hierarchy level. Operator guidance from the config root
appears as "Global"; guidance from workspace roots appears as "Topic".

## Search configuration

To enable web search, configure a SearxNG instance in `config.toml`:

```toml
[tools.search]
provider_type = "searxng"
base_url = "http://localhost:8080"
```

SearxNG is the only supported backend. `provider_type` and `base_url` are the
only search configuration fields; `base_url` names your SearxNG instance.
Unsupported provider types and unrecognised keys are refused when
configuration loads and when settings are saved. The web search provider can
also be edited in the web UI under **Settings**; saved changes apply to new
turns.

## Tool permission model

### Permission bypass

The top-level `dangerously_skip_permissions = true` setting bypasses this
permission model rather than participating in its resolution rules. If it is
enabled in either the session-resolved configuration or the gateway's startup
configuration, the effective tool policy becomes wildcard allow with no
denies or approval requirements. The turn also auto-approves the remaining
tool, location, and hook gates, including prompts for out-of-workspace
locations. The deny, allow, and approval guarantees below apply only while
this setting is false in both configurations.

### Default grants

A fresh configuration allows all tools (`allow: ["*"]`), denies none, and
requires approval for the eight tools that act outside the conversation:
`shell`, `file_edit`, `file_write`, `python_execute`, `browser_click`,
`browser_type`, `browser_select` and `browser_scroll`. In an attended session
each call to one of these pauses until you approve or reject it. This default
is defined in `ToolPermissionConfig::default()` and applies unless a config
file replaces the approval list — a `require_approval` list in a global,
topic or agent config file replaces the default list rather than adding to
it, so `require_approval = []` turns the prompts off. An agent definition's
permission overlay narrows grants only; its `require_approval` is not
applied.

### Resolution order

When a tool call is attempted, `check_permission` evaluates the resolved
permission config in a fixed order:

1. **Deny list.** If the tool appears in `deny`, the call is rejected
   outright — deny always wins.
2. **Allow gate.** The tool must appear in `allow`, either by direct name,
   the `"*"` wildcard, or through a named bundle that expands to include it.
   If no match is found, the call is rejected.
3. **Approval check.** In an attended session, if the tool matches a
   `require_approval` entry, the call pauses for human approval before
   proceeding. Unattended scheduled, channel, workflow-worker, and delegated
   turns auto-approve gates for calls that passed the deny and allow checks.
4. **Allowed.** The call proceeds.

Because the allow gate precedes the approval check, `require_approval`
entries never grant access on their own. They only gate tools that are already in the
allow set. An approval entry for a tool that is not allowed is inert — the
tool is rejected at step 2 before the approval check at step 3 is reached.

### What happens when an agent restricts its allow list

When an agent definition sets a narrow allow list — say
`allow: ["knowledge_search", "knowledge_orient"]` — the overlay intersects
it with the config's default `["*"]`. Since `*` on the config side defers to
the overlay, the effective allow becomes just those two tools. Setting
`allow: []` yields zero permitted tools — a fully locked-down agent.

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

## Topic-targeted session creation

A session is created in a specific topic through a path-based endpoint:

```
POST /api/topics/{topic_id}/sessions
```

The request body is the same `CreateConversationRequest` used elsewhere
(with optional `selected_model_id` and `agent_id` fields), or it can be
omitted entirely. The endpoint returns 201 on success or 404 if the topic is
not found.

There is no `topic_id` field in the request body. `CreateConversationRequest`
carries `deny_unknown_fields` — any unrecognised field in the JSON body,
`topic_id` included, is rejected at deserialisation as a 422 error. This is
a structural guarantee from serde's `deny_unknown_fields` attribute, not a
topic_id-specific validation check: the same rejection applies to any
unexpected field.

## Sandbox isolation

When sandboxing is enabled, eligible local tool calls run inside a two-layer
sandbox that confines what the sandbox child process can reach.

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

Sandboxing is on by default, so the gateway host needs
[bubblewrap](https://github.com/containers/bubblewrap) installed. Two config
options govern sandbox behaviour:

| Option | Default | Effect |
|---|---|---|
| `sandbox.enabled` | `true` | Whether to set up sandbox isolation for each turn |
| `sandbox.allow_degraded_execution` | `false` | Whether to allow unsandboxed execution after sandbox setup fails |

With sandboxing disabled, local gateway turns run without sandbox isolation
and record a degraded boundary with reason
`sandbox_disabled_by_configuration`. `allow_degraded_execution` is not
required for this configuration choice. Installs with a declared boundary,
such as a container image, retain that boundary instead; their sandbox
settings do not change it.

To turn sandboxing off for a topic, add this to its `config.toml`:

```toml
[sandbox]
enabled = false
```

When sandboxing is enabled and sandbox setup fails at runtime — most often
because `bwrap` is not installed — the recovery path depends on whether the
turn is attended:

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
| `Permanent` | In-memory and persisted to disk | All matching calls in this topic while the gateway runs; after restart, subject to agent-level `require_approval` configuration |
| `PermanentGlobal` | In-memory and persisted to disk across all topics | All matching calls everywhere while the gateway runs; after restart, subject to topic- and agent-level `require_approval` configuration |

`Session` reaches further than its name suggests. The grant is not recorded
against the session that approved it, or against a topic: it is held in the
gateway process and consulted whenever a new turn dispatcher is built. Every
turn started after the approval — in any conversation, in any topic — will
run that tool without prompting, until the gateway is restarted. Turns
already under way keep the permissions they started with. If you want an
approval confined to the work in front of you, `Once` is the only scope that
gives it.

For tool approvals, permanent persistence removes the tool from the
`require_approval` list in the selected topic or global TOML file. That list
uses replacement semantics in the Global → Topic → Agent cascade. An agent
file can therefore restore a gate persisted at topic scope, and a topic or
agent file can restore one persisted globally. The in-memory grant masks that
narrower requirement until the gateway exits; after a restart, the narrower
configuration applies again.

`Permanent` and `PermanentGlobal` also handle location-scoped approvals —
trusting a workspace attachment so that file operations within it no longer
prompt. For tool and location approvals alike, `Permanent` applies only in
the approving topic, while `PermanentGlobal` applies across all topics.

### Timeout behaviour

Approval requests time out after **300 seconds** (five minutes) by default,
governed by the `hitl.timeout` setting. The default timeout policy is
**Reject**: a timed-out request is treated as a rejection, the agent receives
feedback, and the turn continues. The alternative policy, **Interrupt**, marks
the entire turn as interrupted — no further tool calls or completions are
issued.

Per-tool overrides use an object entry in `require_approval` instead of a
plain string:

```toml
require_approval = ["write_file", { tool = "execute_command", timeout = 600 }]
```

The object form accepts `tool` (required), `timeout` (seconds), and
`timeout_policy` for individual tools.

## Context configuration

The `[context]` section of the configuration file controls how the gateway
assembles and compacts conversation context. Every key cascades through the
file hierarchy (Global → Topic → Agent), so a topic-level file can override
the global setting, and an agent-level file can override the topic's.

| Key | Default | Purpose |
|---|---|---|
| `assembly` | `"default"` | Which named context assembly to use |
| `alternatives` | `[]` | Hidden alternative assemblies available for local selection |
| `timeout` | `5` (seconds, 1–60) | How long assembly may take before timing out |
| `budget` | unset | Token budget for the assembled context; must be positive when set |
| `compaction_threshold` | unset (100 messages) | When to compact — `{ messages = N }` or `{ tokens = N }` |
| `diagnosis_reader_model` | unset (session's model) | Model for the diagnosis readers during wall succession |
| `diagnosis_synthesis_model` | unset (session's model) | Model for the diagnosis synthesis during wall succession |

The Settings page validates these values before saving: a
`compaction_threshold` with a non-positive count is rejected, and a diagnosis
model name that does not match any model entry or alias in the configuration
being saved is rejected.

## Voice configuration

Speech transcription and synthesis are optional capabilities that use the
default provider's OpenAI-protocol audio endpoints — they never route through
the chat model.

Two keys in the `[voice]` section control which models are requested:

```toml
[voice]
transcription_model = "whisper-1"   # sent to /audio/transcriptions
speech_model = "tts-1"              # sent to /audio/speech
```

Each key names a model id the provider serves for that audio task. When a
capability is unavailable — because the default provider is absent, its
credentials are unavailable, its protocol has no audio API, or a model id is
unset — `GET /api/voice/capabilities` reports it with an `unavailable_reason`
naming what to address (see
[Interfaces and tooling — Voice capabilities](interfaces-and-tooling.md#voice-capabilities)).

Both ids can be edited in the web UI under **Settings → Voice**, which also
states the prerequisite (an OpenAI-protocol default provider plus the two
model ids). A save applies without restarting the gateway.

## Experimentation controls

Swage can run alternative implementations behind a stable surface — a
different context assembly, a different working-norms text — selecting one
per session and recording every decision in the audit trail. The
`[experimentation]` section of `config.toml` governs this.

### Selection lifetime

A session draws its variant the first time it reaches a location, records
that decision, and keeps it for the rest of its life. Configuration changes
— the global switch, a location's `enabled` flag, a `forced_variant` — bind
only for sessions that reach the location after the change; a session that
already recorded a selection keeps serving it.

### When changes take effect

The web UI's Settings page does not expose experimentation controls, so
changes are made by editing `config.toml`. Restart the gateway to apply
them:

- **CLI / systemd:** run `swage gateway restart`.
- **Desktop app:** quit and reopen the app.
- **Standalone process:** stop and start the process.

Only the global `config.toml` needs the restart: it is read when the gateway
starts and rewritten by Settings saves, which apply at once. A topic's or
agent's `config.toml` and `instructions/` are read at each turn and overlaid
on the running configuration, so an edit there applies to the next turn
under that scope, in every session alike, with no restart.

### Disabling experimentation

A global switch turns all variant selection off:

```toml
[experimentation]
enabled = false
```

When disabled, every location uses its default variant for sessions that
have not already recorded a selection, and the decision is recorded as such. Experimentation is on by default. Restart the gateway
after changing this setting.

### Per-location controls

Each experiment location — identified by a stable string such as
`context.assembly` or `context.working_norms` — can be individually
controlled:

```toml
[experimentation.locations."context.working_norms"]
enabled = true
forced_variant = "family-fallback"
```

- **`enabled`** — defaults to `true`. Set to `false` to disable variant
  selection for this location only; the default variant serves.
- **`forced_variant`** — pins the location to a specific named variant for
  every session that reaches it after the change. The variant must be registered at the location's choice point;
  an unrecognised name is refused. Remove the key to return to random
  selection.

Restart the gateway after changing either key.

The shipped locations are `context.assembly`, `context.working_norms`,
`task.prompt`, and `prompts.memory_curator`.

### Alternatives

What can be forced or drawn at a location depends on the alternatives
registered there. For `context.assembly`, alternatives are operator-authored:
each is a `<name>.js` file placed in an `assemblies/` directory under the
config hierarchy (global, topic, or agent — the most specific wins). List a
new assembly in `[context] alternatives` so the selection loop includes it:

```toml
[context]
assembly = "default"
alternatives = ["my-variant"]
```

The assembly script runs in a caged QuickJS runtime (no IO, no network, no
timers) and composes the turn's context from the elements the gateway built.
The script receives four globals:

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
source. For `context.working_norms`, the registered variants are
`authored-v1` (the default) and `family-fallback`. These cannot be extended
through configuration.

### Operator history

Every variant decision — whether drawn randomly, forced, or recorded as
disabled — appears in the operator history pane. Each entry names the
location, the variant that served, the reason (such as `drawn`, `forced`, or
`disabled`), and the session and turn that produced it.

When the selection cannot be recorded (for example because the audit store
is temporarily unavailable), the turn falls back to the default variant
without writing a record. The gap in the history is how a later reader
identifies that recording failed.

### Cascading

Experimentation settings cascade through the file hierarchy (Global → Topic
→ Agent). Per-location entries merge by location id across layers, so a topic
file can force a variant the global file leaves to random selection.

## Succession compaction

When the turn's assembled message list exceeds the compaction threshold
(default: 100 messages), the turn runs on a **successor** — a fresh context where the
older part of the conversation history is replaced by a summary, the newer
part is kept verbatim, and every other context element (orientation, norms,
prompts, briefings, skills) is rendered afresh from its sources. The summary
is written against the carried elements, so it records what the conversation
established rather than restating what the context already says.

### Wall-triggered succession

A session that keeps hitting walls continues on a successor built from a
blind diagnosis, regardless of how large the context is. Two triggers are
checked within the six most recent finished turns:

- Three failures of the identical tool call (same tool, same arguments)
  anywhere in that window, or
- at least five failed tool calls and failed turns combined.

The first trigger takes precedence when both are present.

When walls trigger succession, separate readers each examine part of the
history to locate what went wrong, and a synthesis combines their reports into
the notes the successor carries. While the walls remain in the recent window:

- A turn whose walls match the set the previous diagnosis answered carries
  that same diagnosis again, without repeating the model calls.
- A turn whose recent window no longer shows any wall gets its full history
  back.

The diagnosis models are selected by model catalogue entry or alias, resolved
the same way as ordinary turns resolve a model. When neither key is set, the
session's own model handles both stages.

If a diagnosis reader or the synthesis fails or returns an empty report, the
turn falls back to threshold-only compaction — it compacts only when the
context exceeds the threshold — and the failed attempt is retained in the
context record.

Transient provider failures (rate limits, timeouts, HTTP 5xx responses,
malformed streams, and connection errors) do not trigger wall succession.
Provider refusals of the agent's request do count.
