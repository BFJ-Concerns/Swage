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

The gateway daemonises and logs to `gateway.log` in its data root —
`~/.swage/` by default, `$SWAGE_HOME` when set. Stop it with the same binary
you started:

```bash
./target/debug/swage gateway stop
```

The CLI finds the running gateway through a metadata file in
`$XDG_RUNTIME_DIR/swage/` (falling back to the data root's `run/`
directory), so run `stop` with the same environment you started with.

## The first turn may ask about sandboxing

Local tool calls that are eligible for sandboxed execution run inside a
[bubblewrap](https://github.com/containers/bubblewrap) sandbox by default —
install the `bubblewrap` package to get isolation. Tools that require live
gateway state or host facilities execute directly in the gateway process,
and sessions targeting a non-local device use that device's execution path;
those calls do not receive the gateway's bubblewrap and Landlock isolation.

If sandbox setup fails (most commonly `bwrap` is not installed), what happens
next depends on whether anyone is there to answer. A turn you started from a
connected client pauses with an approval: *"Sandbox setup failed. Approve
running this turn without sandbox isolation?"* Approving runs that turn
unsandboxed and records the degradation in the audit trail; rejecting fails
the turn. A scheduled or delegated turn has nobody to ask, so it fails
outright.

To settle the question in configuration instead, opt in to unsandboxed
execution explicitly:

```toml
[sandbox]
allow_degraded_execution = true
```

Running without isolation always needs that opt-in, whether you chose it or
sandbox setup failed. So turning sandboxing off takes both keys —
`enabled = false` on its own makes every turn fail:

```toml
[sandbox]
enabled = false
allow_degraded_execution = true
```

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
```

Set the key in the gateway's environment before starting it. Providers and
models can also be managed from the web UI under Settings → Providers.

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
- `spaces/<space>/` — each space's world: its own `instructions/`,
  `skills/`, `agents/<agent>/` (per-agent `instructions/` and `skills/`),
  memory, and knowledge-base bodies. The agent directory name
  `__workflow_ephemeral__` is reserved for bare workflow workers and is excluded
  from authored agent configuration and instructions.
- `swage.db` — the SQLite store: conversations, turns, the audit trail,
  knowledge base, workflow runs. The one local record everything is
  reconstructable from.

The configuration cascade is Global → Space → Agent: the more specific
level extends or shadows the more general one. `AGENTS.md` extends the
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
`gateway restart`, and `gateway install`.

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

Three pages in [`docs/`](docs/) cover the parts of the system a newcomer
meets first:

- [Workflow engine](docs/workflow-engine.md) — how a workflow script runs,
  how a run survives a restart, and how its token budget is spent.
- [Agents and permissions](docs/agents-and-permissions.md) — agent
  definitions, operator guidance (`AGENTS.md`), what a tool call is allowed
  to do, approvals, and sandbox isolation.
- [Interfaces and tooling](docs/interfaces-and-tooling.md) — the web UI's
  turn context inspector, the wire contract, and the verification gate.

[`docs/README.md`](docs/README.md) indexes the set, and
[`docs/architecture-flow.svg`](docs/architecture-flow.svg) is the
system diagram.

## Development

`just verify` runs the full gate (format, clippy, tests, frontend checks,
projection freshness). See `CLAUDE.md` for repository conventions.
