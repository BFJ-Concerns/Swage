# Swage

A context-governed agent platform for your own hardware: an always-on
gateway running agents that work on schedules and on demand, with everything
the system does recorded locally and reconstructable afterwards.

## Build and run

Requires the Rust toolchain (see `rust-toolchain.toml`) and Node for the web
UI. The gateway binds to `127.0.0.1` (loopback) by default. An operator may
bind to a trusted private-network address such as a LAN or tailnet IP — see
[Network binding](#network-binding) below. Public exposure is not supported:
the gateway has no authentication layer, so binding to a public address
exposes the full API and every configured provider key to anyone who can
reach the port.

```bash
cargo build --bin swage
./target/debug/swage gateway start --port 3000

# Web UI (development)
cd frontend && npm install && npm run dev
```

The gateway detaches by default and prints its output destination when it
starts. Output goes to `gateway.log` in its data root (`~/.swage/` by default,
or `$SWAGE_HOME` when set). Choose another destination with `--log-file PATH`,
or use `--foreground` to keep the process and its output attached to the
terminal:

```bash
./target/debug/swage gateway start --log-file ./gateway-dev.log
./target/debug/swage gateway start --foreground
```

Stop it with the same binary you started:

```bash
./target/debug/swage gateway stop
```

The CLI finds the running gateway through a metadata file in
`$XDG_RUNTIME_DIR/swage/` (falling back to the data root's `run/`
directory), so run `stop` with the same environment you started with.

## Sandbox configuration

Per-turn sandboxing is off by default. Local tools run without bubblewrap
isolation, and each such turn records a degraded execution boundary with the
reason `sandbox_disabled_by_configuration`.

To enable sandboxing for a topic, add this to its `config.toml` and install
[bubblewrap](https://github.com/containers/bubblewrap):

```toml
[sandbox]
enabled = true
```

Sandbox-eligible local tool calls then run inside the sandbox. Tools that
require live gateway state or host facilities execute directly in the
gateway process, and sessions targeting a non-local device use that device's
execution path; those calls do not receive the gateway's bubblewrap and
Landlock isolation.

If enabled sandbox setup fails, what happens next depends on whether anyone
is there to answer. A turn you started from a connected client pauses with
an approval: *"Sandbox setup failed. Approve running this turn without sandbox
isolation?"* Approving runs that turn unsandboxed and records the degradation
in the audit trail; rejecting fails the turn. A scheduled or delegated turn
has nobody to ask, so it fails outright.

Setting `sandbox.allow_degraded_execution = true` permits unsandboxed
execution after a sandbox setup failure without that approval. It is not
needed when sandboxing is disabled by configuration.

[docs/agents-and-permissions.md](docs/agents-and-permissions.md) covers what
the sandbox confines and how tool approvals work.

## Configure a provider first

A fresh install runs, but no turn can complete until a model provider is
configured — the composer will say so and point at Settings. Configuration
lives in `~/.swage/config.toml`. A minimal working example:

```toml
[providers]
default = "anthropic"

[providers.entries.anthropic]
protocol = "anthropic"                # or "openai" for OpenAI-shaped endpoints
api_key_env = "ANTHROPIC_API_KEY"     # read from the environment at startup
# base_url = "https://..."            # only for proxies or self-hosted routes

[models]
default = "sonnet"

[models.entries.sonnet]
provider_id = "anthropic"
model_id = "claude-sonnet-5"
default_effort = "high"            # optional: off|minimal|low|medium|high|xhigh|max
```

`default_effort` sets the reasoning effort for every turn using this model
unless overridden at session start. Seven levels are available, from `off`
(disable reasoning where the model supports it) through `max`. The gateway
resolves the chosen level against the model's declared capability — see
[Agents and permissions — Effort](docs/agents-and-permissions.md#effort).

Set the key in the gateway's environment before starting it. Providers and
models can also be managed from the web UI under Settings → Providers.

### Credentials beyond environment variables

The `api_key_env` field shown above is a legacy convenience: a direct
environment-variable read at startup, with a deprecation warning. The
governed alternative is `api_key_secret_ref`, which resolves through the
gateway's secret manager — every access is audited and the resolved value is
redacted from logs.

The secret manager supports three backends: `managed` (encrypted in the
gateway's store), `env` (an environment variable), and `file` (a file on
the gateway host, re-read on every access so externally refreshed tokens
stay current). A file-backed secret can optionally narrow into a JSON
document with a JSON pointer — useful when a tool's own auth store keeps
multiple tokens in one file. The Codex CLI's OAuth tokens are registered
this way at startup, so they resolve and redact through the governed system
without being copied out of the harness's own `auth.json`.

See [Provider credentials](docs/interfaces-and-tooling.md#provider-credentials)
for the creation journey and request shape.

### Voice

Speech transcription and synthesis are optional. They need an
OpenAI-protocol default provider whose endpoint serves the audio APIs, plus
the model to request for each:

```toml
[voice]
transcription_model = "whisper-1"   # sent to /audio/transcriptions
speech_model = "tts-1"              # sent to /audio/speech
```

Voice never uses the chat model. With a model unset, that capability is off,
and `GET /api/voice/capabilities` says what to configure. Both ids can be set
in the web UI under **Settings → Voice** (the Voice entry in the settings
navigation), which also states this prerequisite; a save applies without a
restart.

`SWAGE_HOME` overrides the whole data root (config, database, logs) — useful
for throwaway instances: `SWAGE_HOME=/tmp/swage-test swage gateway start`.

## Where things live on disk

Everything is under `~/.swage/` (or `$SWAGE_HOME`):

- `config.toml` — providers, models, sandboxing, and platform settings.
- `AGENTS.md` — global operator guidance, read into every turn as an
  instruction rule. Workspace roots may carry their own `AGENTS.md` too —
  see [Operator guidance](docs/agents-and-permissions.md#operator-guidance-agentsmd).
- `instructions/` — global operator instructions, as `.md` files composed in
  filename order (`01-base.md`, `02-style.md`, …). They layer *after* the
  built-in orientation: every session already knows what it is running on,
  where it is, and what tools it has.
- `skills/<name>/` — global skills, one directory per skill with frontmatter.
- `general/` — the general scope: where a session started without a topic
  keeps its memory, knowledge-base bodies and `agents/<agent>/` overlays. It
  has no scope-level `config.toml` or `instructions/` — its cascade is
  Global → Agent.
- `scratch/` — scoped scratch directories for sessions without an attached
  workspace. `scratch/general/sessions/<session_id>` for topicless sessions;
  `scratch/topics/<topic>/sessions/<session_id>` for topic sessions.
- `topics/<topic>/` — each topic's world: its own `instructions/`,
  `skills/`, `agents/<agent>/` (per-agent `instructions/` and `skills/`),
  memory, and knowledge-base bodies. The agent directory name
  `__workflow_ephemeral__` is reserved for bare workflow workers and is excluded
  from authored agent configuration and instructions.
- `swage.db` — the SQLite store: conversations, turns, the audit trail,
  knowledge base, workflow runs. The one local record everything is
  reconstructable from. At open, the gateway checks the store's recorded
  migration history against its own list. Two mismatches refuse the open:
  a recorded migration name this build does not know (the store was written
  by a newer build — the message advises running that build, or rebuilding
  the data root), and a known migration missing before a later recorded one
  (the history is not one this build produced — the message advises
  rebuilding the data root). Neither case applies anything half-finished.

The gateway logs the resolved data root and whether `$SWAGE_HOME`
supplied it at startup, so a gateway that fell back to the default
`~/.swage/` says so in the log.

The configuration cascade is Global → Topic → Agent for a session in a
topic and Global → Agent for one without: the more specific level extends
or shadows the more general one. `AGENTS.md` extends the
cascade (global then workspace) rather than participating in the shadowing
— both levels are always additive.

## Network binding

The gateway's listen address and port are configurable through `config.toml`
and CLI flags. The defaults are `127.0.0.1` (loopback) on port `3000`.

**Config file** (`~/.swage/config.toml` or `$SWAGE_HOME/config.toml`):

```toml
[server]
host = "192.0.2.30"   # a LAN or tailnet address
port = 3000
```

**CLI flags** (override the config file for a single invocation):

```bash
./target/debug/swage gateway start --host 192.0.2.30 --port 3000
```

The `--host` and `--port` flags are accepted by `gateway start`,
`gateway restart`, and `gateway set-defaults`. Saved defaults are reused by
`start`, `restart`, `status`, and `stop` until removed with
`gateway clear-defaults`; an explicit lifecycle flag overrides its saved
default.

On Linux, when the gateway is the main process of a systemd service,
`gateway restart` restarts that service and waits up to 30 seconds for the
new process to record its address and answer the health endpoint there.
Per-call restart overrides are refused; configure the service instead. A failed restart reports an error and does not restore
the previous executable or data automatically.

The terminal client is `swage chat`. Generate an installation script for
shell completion with `swage generate-completion bash` (or `zsh`/`fish`).

**Security boundary.** The gateway has no authentication. Binding to a
private-network address is an explicit operator choice that trusts every host
on that network with full API access and every configured provider key.
Public-internet exposure is not supported until an authentication layer
exists.

**Host firewall.** A gateway bound to a LAN address is still unreachable from
other devices if the host's own firewall drops inbound connections on the
chosen port. Verify that the firewall permits traffic to the port before
expecting LAN clients to connect.

## Isolated development instance

To run a second gateway beside a real installation, give it its own data
root, its own runtime directory (the CLI's instance discovery lives there —
two instances sharing one collide), and its own port:

```bash
SWAGE_HOME=/tmp/swage-dev \
XDG_RUNTIME_DIR=/tmp/swage-dev-run \
./target/debug/swage gateway start --host 127.0.0.1 --port 3100
```

Stop it with `gateway stop` under the same two variables. The isolated data
root starts empty, so give it its own `config.toml` (the provider example
above) before expecting turns to complete. The dev web UI
(`npm run dev`) serves <http://localhost:5173> and proxies API calls to the
gateway; point it at a non-default port with
`SWAGE_GATEWAY_URL=http://127.0.0.1:3100 npm run dev`.

## Going deeper

The [`docs/`](docs/) surfaces cover the parts of the system a newcomer meets
first:

- [First session](docs/first-session.md) — opening a topic, attaching a
  repository as a workspace, and trusting it so its guidance reaches agents.
- [Workflow engine](docs/workflow-engine.md) — how a workflow script runs,
  how a run survives a restart, how its token budget is spent (including
  per-route partitions), what each call's detail record contains, and the
  terminal account that records how a run ended, where its workers stood,
  and what work survived.
- [Agents and permissions](docs/agents-and-permissions.md) — agent
  definitions, operator guidance (`AGENTS.md`), effort resolution, what a
  tool call is allowed to do, approvals, and sandbox isolation.
- [Interfaces and tooling](docs/interfaces-and-tooling.md) — the web UI's
  turn context inspector, conversation branching, image attachments,
  surfaces, delivery targets, devices, hosted documentation, the API error
  contract, the wire contract and its projections, the verification gate,
  and the gateway's built-in tool inventory.

Every failed API response carries a `{"code","message"}` body whose code
vocabulary is defined in the protocol crate. The manifested client-surface
routes are documented in an OpenAPI 3.1 document at
`crates/swage-protocol/schema/client-openapi.json`, generated from a route
manifest that also drives the TypeScript client and a router-coverage test;
the manifest is growing to cover the full surface.

[`docs/README.md`](docs/README.md) indexes the set, and
[`docs/architecture-flow.svg`](docs/architecture-flow.svg) is the
system diagram.

## Development

`just check-gate` runs the fast, pre-test static gate; `just verify` runs that
gate plus tests and projection freshness. See `CLAUDE.md` for repository
conventions.
