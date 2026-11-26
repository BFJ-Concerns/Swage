---
description: How Swage defines agents (including conversational authoring and the API), reads operator guidance from AGENTS.md, resolves tool permissions, enforces approval, isolates tool execution, handles delegated grant inheritance, resolves effort per model, configures context assembly and compaction, controls experimentation and variant selection, handles wall-triggered succession, and configures voice transcription and synthesis
---

# Agents and permissions

Swage governs what every agent session can do through a layered permission
system: agent definitions narrow what the file-based config cascade grants,
delegation chains narrow further, and attended sessions can require human
approval for configured tool calls.

## Agent definitions

An agent definition is a reusable configuration that gives a session its
character — its instructions, model preference, tool permissions,
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

**In conversation.** Ask an agent to create a named agent you can choose for
future sessions. For example:

> Create a global agent called Reader, id reader, that searches and reads
> files without changing them. Give it standing instructions to avoid
> modifications and allow only file_read, file_list and file_search.

To edit it, ask: “Change Reader's instructions to cite file paths in answers,
keeping its other settings.” The agent can list definitions to find the id
behind a display name. Creating refuses an id that already exists at that
scope; editing changes only the fields supplied and refuses a missing
definition. Supplied instructions replace the whole text, so a revision
must retain any instructions you want to keep. Optional fields can be
cleared; the name, description and instructions cannot be cleared with
`null`.

The agent needs the `agent_definition` tool. Its shipped bundle,
`definition-authoring`, also includes `task_definition` for delegated work
contracts. In **Settings → Automation**, add `definition-authoring` to
**Tool allow rules** if you use restricted rules; the default `*` already
allows it. If you have overridden the shipped bundle mapping, **Tool bundles**
must map `definition-authoring` to `["agent_definition", "task_definition"]`.
Denies and narrower agent or delegation grants still apply. An agent that
lacks this tool cannot grant itself access.

Definitions are global by default. Ask for a topic-specific definition to
limit it to the conversation's topic; the tool cannot target another topic.
In a conversation without a topic, this option limits it to the general
scope. A topic-specific definition hides a global definition with the same id
there. Say which scope you mean when editing or deleting one. Deleting is
permanent, though the definition can be recreated.

Tool permissions can only narrow what the configuration grants. The shipped
**Swage default** and **Memory curator** cannot be changed or deleted through
this tool. Invalid ids, blank display names, empty or duplicate skill names,
malformed permission overlays and invalid output schemas are refused.
Ids use lowercase ASCII letters, digits and hyphens. Saving a model preference
outside the configured catalogue is refused with the available ids; choose
the model when starting a session.
At run time, a saved model preference no longer in the catalogue leaves the
resolved default model in place.
Approval requirements belong in configuration files, not in a definition.

After creating or editing, reload the page. Open **Settings → Agents** to
read the definition's instructions, model preference, tool permissions,
skills and output contract. Its name is also available under **Agent** in
the new-session picker for the scope where it is visible. Creating a
definition does not switch the conversation you are in to that agent.

Every creation, edit and deletion records the whole definition before and
afterwards. In the operator view, choose **View history** and find
**Configuration changes**. Expand **Whole prior and new state** to see
**Prior state** and **New state**.

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
    "bundles": {}
  }
}
```

`description` is required. `topic_id` scopes the definition to one topic;
omit it for a global definition. The `tool_permissions` object carries
`allow`, `deny`, and `bundles`. `bundles` maps bundle
names to tool-name arrays — it must be present (use `{}` when no custom
bundles are needed). The overlay intersects with the config cascade's
grants, so it can only narrow what the config allows, never widen it.
An explicit `require_approval` field is refused before saving, including
an empty list. Set approval requirements in the config files.
Optional fields not shown above include `model_id`, `skills`,
`output_schema`, and `output_max_retries`.

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

A fresh installation includes **Swage default** (`gateway`) and **Memory
curator** (`memory-curator`). The curator is visible in agent settings but
cannot be selected for a new session. A shipped workflow's internal roles
are not standalone agent definitions.

After a reload, the new-session picker lists the default plus authored
definitions visible in the selected scope. Without a topic, it offers global
definitions; a topic also sees its own definitions, which hide global ones
with the same id. The `agent_id` field when creating a conversation selects
the default or a visible authored definition; other choices are refused.

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
requires approval for four tools: `shell`, `file_edit`, `file_write` and
`python_execute`. In an attended session each call to one of these pauses
until you approve or reject it. Browser actions do not require approval by
default. This default is defined in `ToolPermissionConfig::default()` and
applies unless a config
file replaces the approval list — a `require_approval` list in a global,
topic or agent config file replaces the default list rather than adding to
it, so `require_approval = []` turns the prompts off. An agent definition's
permission overlay narrows grants only; an explicit `require_approval`
field is refused before saving.

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
  overlay, which refuses an explicit approval field before saving.
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
PID, mount, user, IPC and UTS namespaces are unshared, and the network
namespace too unless the sandbox shares the host's network (see
*Network* below); the child dies with its parent (`--die-with-parent`).

The sandbox reads broadly and writes narrowly. Everything the host can see
is readable except the private paths it masks: SSH and GnuPG key material,
cloud and cluster credentials (`~/.aws`, `~/.azure`, `~/.config/gcloud`,
`~/.kube`), plaintext token files (`~/.netrc`, `~/.git-credentials`, the
`gh` and Forgejo token files), package-registry tokens (Cargo's
`credentials.toml`, `~/.npmrc`, `~/.pypirc`, `~/.gem/credentials`, Docker's
`config.json`), the desktop keyrings and `pass` store, and the gateway's own
data root. A masked
directory reads as empty and a masked file as `/dev/null`. Of `~/.ssh`, the
files that hold no key — `config`, `known_hosts` and the `.pub` halves —
stay readable. The shipped skills' files, written beneath the data root,
are readable and runnable but not writable. A grant that names the data
root reopens it.

Writes go only to the workspace (when its grant allows), the workspace's
git metadata where a linked worktree keeps it outside the tree, temporary
space, the shared build and package caches under the home (`~/.cache`,
`~/.cargo`, `~/.npm`, `~/.local/share/pnpm`, `~/go/pkg`) and any attachment
granted write. A cache that does not exist yet is created before the turn
where its tool is installed, so a first fetch can fill it; a token kept
inside a cache stays masked.

The child receives only named settings from the gateway's environment
(`PATH`, `HOME`, the locale, the XDG directories, `CARGO_HOME`, the
`RUSTUP_*` settings and git's configuration-file selectors) and never a
provider credential. LLM completions are proxied back through the host via
IPC, so the child does not need provider credentials.

### Git on the operator's behalf

Sandboxed git runs on the operator's own system and global git
configuration, located as the gateway's environment locates it, together
with the repository's own. Configuration carried in the gateway's
environment (`GIT_CONFIG_PARAMETERS`, `GIT_CONFIG_COUNT`) and repository
selectors (`GIT_DIR`, `GIT_WORK_TREE`) do not cross into the sandbox.

Authentication goes through the operator's agent and credential helpers,
never through key or token files inside the sandbox. Over SSH, the sandbox
is given the operator's SSH agent (`SSH_AUTH_SOCK`), and git's ssh reads
the operator's own `~/.ssh/config` alone (`ssh -F`) unless they set
`core.sshCommand`: inside the sandbox's user namespace the system's
root-owned ssh configuration shows as owned by nobody, which OpenSSH
refuses. Over HTTPS, the
sandboxed git's one credential helper asks the gateway through the turn's
private socket, and the gateway runs `git credential fill` on the host, so
the operator's own helpers answer with their own stores. A secret
registered with the gateway is never an answer: governed secrets do not
reach the sandbox. Nothing prompts, and every lookup is in the audit
trail.

Commit signing runs on the host: when the operator signs commits with SSH
(`gpg.format ssh` and a `user.signingkey`), the sandboxed git is configured
to sign with the `swage` binary as its signing program and with the
operator's public key as a literal; the program forwards each payload to
the gateway, which signs with the operator's key — the private file beside
the public one, or the operator's agent — and records every signing in the
audit trail with the key named and the turn attributed, under its
fingerprint. The configuration is read as the turn's workspace sees it,
conditional includes and all. A key the operator has not configured is
refused, and a host that cannot sign — a key with no readable public half,
a key named by a key command, a signing that waits on a hardware token or
a passphrase — fails the commit with the cause rather than leaving it
unsigned. An operator who signs with OpenPGP is served the same way: the
sandboxed git's signing program forwards each payload, and the gateway
signs with the operator's own `gpg` (`gpg.openpgp.program` or
`gpg.program`) and key — `user.signingkey`, or the committer identity git
signs as without one — through their gpg-agent; the keyring stays masked.
A format the host cannot forward (`x509`) fails the commit with that
cause, which the gateway log also gives at the turn's start.

### Network

A sandboxed command reaches the network as the host does, through the
operator's own proxy settings where there are any. `sandbox.network`
confines it:

| Value | A sandboxed command reaches |
|---|---|
| `open` (default) | the host's network, directly |
| `none` | nothing beyond its own loopback |
| `proxy` | only the gateway's recording proxy |

Under `proxy`, every destination is recorded in the audit trail as an
`egress_connection` event. A host matching `sandbox.egress_hosts` (an exact
host, or `*.suffix` for a host and every name beneath it) is reached; any
other host is put to the request judge (`[judge] model`), and without its
yes the connection is refused. With no judge configured, only the listed
hosts are reachable.

### Landlock

The inner layer applies Landlock — a kernel-enforced, per-path filesystem
access-control mechanism — with the same write set: everything is readable
and executable, and only the paths above are writable. It supports ABI 1
(Linux 5.13+), ABI 2 (5.19+, adding the `REFER` permission), and ABI 3
(6.2+, adding `TRUNCATE`), and degrades gracefully on kernels that predate
it. Landlock restrictions are irrevocable within the process.

### Configuration

Sandboxing is on by default, so the gateway host needs
[bubblewrap](https://github.com/containers/bubblewrap) installed. These
config options govern sandbox behaviour:

| Option | Default | Effect |
|---|---|---|
| `sandbox.enabled` | `true` | Whether to set up sandbox isolation for each turn |
| `sandbox.allow_degraded_execution` | `false` | Whether to allow unsandboxed execution after sandbox setup fails |
| `sandbox.network` | `open` | What a sandboxed command reaches on the network (see *Network*) |
| `sandbox.egress_hosts` | `[]` | Under `network = "proxy"`, the hosts reached without the judge |

With sandboxing disabled, local gateway turns, Python included, run without
sandbox isolation and record that the sandbox was switched off
(`sandbox_disabled`), which is distinct from degraded execution after a
sandbox failed to start. `allow_degraded_execution` is not required for
this configuration choice.

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

### Requests beyond the grants

A tool call that asks for a location outside the session's granted
locations is decided before it runs. In a session nobody attends — a turn
started by the API, a schedule, a workflow or a delegation — the request is
refused unless a judge allows it, and the agent is told why: with no judge
configured, the refusal says so. An attended session asks the person, as
above.

A judge is a model named in settings, off by default:

```toml
[judge]
model = "claude-haiku"   # a model catalogue key or alias
```

Given the tool, its arguments, the agent's stated justification and the
task the turn is doing, the judge answers yes or no with a reason. In an
unattended session it decides alone, and anything but a clear yes — a no,
an unreadable reply, a judge that could not be reached — refuses. In an
attended session the person is asked unless the judge is confident. A yes
lets that one request through and adds nothing to the session's grants.
Every judgement is recorded as a `request_judged` audit event with the
tool, the arguments, the justification, the verdict and the reason. Under
`sandbox.network = "proxy"` the same judge decides a host the listed hosts
do not cover. The judge's instruction is the `prompts.request_judge`
experiment location.

## Context configuration

The `[context]` section of the configuration file controls how the gateway
assembles and compacts conversation context. Every key cascades through the
file hierarchy (Global → Topic → Agent), so a topic-level file can override
the global setting, and an agent-level file can override the topic's.

| Key | Default | Purpose |
|---|---|---|
| `assembly` | `"default"` | Which named context assembly to use |
| `alternatives` | `[]` | Hidden alternative assemblies available for local selection |
| `timeout` | `5` (seconds, at least 1) | How long assembly may take before timing out |
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
`task.prompt`, `prompts.memory_curator`, `prompts.succession_summary` (the
instruction a conversation summary is written under, selected when a
session first needs a summary) and `prompts.request_judge` (the judge's
instruction).

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
`prompts.memory_curator`, `prompts.succession_summary`) have their alternatives defined in the gateway
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
