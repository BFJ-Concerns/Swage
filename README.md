# Swage

**A self-hosted platform for running AI agents you can see inside.**

Swage is an always-on gateway that runs agents on your own hardware — in
conversation, on a schedule, or as scripted multi-agent workflows — and
records everything they do in one local audit trail. Every turn's context is
assembled deliberately and kept, so you can open any reply afterwards and see
exactly what the model was shown, which tools it called, what it was allowed
to do, and why.

It is written in Rust (gateway, agent runtime, workflow engine, sandbox) with
a Svelte 5 web UI and an Electron desktop shell.

![A Swage conversation: the agent writes and runs a Monte Carlo script with its shell tool, then reports the results as a table](docs/images/conversation.png)

> [!WARNING]
> **Swage is very early alpha.** It is used daily by its author, but it is not
> ready for anyone else to depend on:
>
> - **No compatibility promises.** Storage formats, configuration and APIs
>   change without migrations or deprecation periods; an upgrade may mean
>   starting from a fresh data directory.
> - **Single operator, Linux only.** One person's gateway on their own
>   machine, by design. Sandboxing relies on Linux (bubblewrap, Landlock).
> - **No authentication.** Run it on loopback or a network you fully trust.

## Why Swage

Most agent tools are either a chat window over an API or a framework you
assemble yourself. Swage is a long-running service with opinions about three
things those tend to leave out:

- **Context is governed, not accumulated.** Each turn's context — orientation,
  operator instructions, `AGENTS.md` guidance, skills, memory, history — is
  composed by a named assembly that records what it included, what it left
  out, and why. The web UI's turn inspector shows that record for any reply.
  Assemblies are swappable, and can be scripted in JavaScript.
- **Everything is reconstructable.** Turns, tool calls, approvals, assembly
  decisions, configuration changes and workflow journals all land in one
  local SQLite store, and the record never leaves the machine. Network
  traffic is what you enable: model providers, plus tools and channels such
  as URL fetch, web search and Telegram.
- **Built for experiments.** Behaviour can be expressed as variants selected
  per session behind a stable surface — today, context assembly, the working
  norms a model is given, task prompts and the memory curator's prompt — so
  two ways of doing something can be compared on real work. Neither the user nor
  the model sees which variant is running, every selection is recorded, and
  experimentation can be switched off globally or per location for new
  sessions.

<p align="center">
  <img src="docs/images/turn-context.png" alt="The turn context inspector: which assembly built the turn, each context element, where it came from, and its estimated token cost" width="85%">
  <br><em>Every reply links to the context it was built from.</em>
</p>

## What it does today

| Area | What's there |
|---|---|
| **Providers** | Anthropic, OpenAI Chat Completions and Responses, Google, OpenRouter, and the built-in Codex app server (temporarily disabled); retries; interactive turns honour the selected model's provider; per-model reasoning effort |
| **Tools** | File read/write/edit/search, shell, URL fetch, web search¹, a Chromium browser¹, Python execution¹, memory, a knowledge base with citations, schedules, workflows, and agent-built *surfaces* |
| **Safety** | Per-tool permissions with interactive approvals, grants that narrow as work is delegated, per-turn bubblewrap + Landlock sandboxing, and credential leak detection with redaction |
| **Workflows** | A JavaScript workflow engine (QuickJS) whose runs are journalled: a run interrupted by a crash or restart replays and carries on. Token budgets per provider route, durable parking on long timers, and runs that pause for a human decision |
| **Automation** | Schedules that start or continue sessions, external channels (Telegram¹), delivery targets, and remote *devices* that dial in and execute tools on other machines |
| **Context** | Scriptable context assembly, compaction, and recovery for sessions that keep failing: separate readers diagnose what went wrong and the session continues from their notes |
| **Interfaces** | Web UI, desktop shell and CLI; a wire contract defined once in Rust and projected to TypeScript, JSON Schema and OpenAPI |

¹ Needs something outside Swage; see [Optional capabilities](#optional-capabilities).

## Quick start

Swage targets Linux. Install the Rust toolchain with
[rustup](https://rustup.rs/) (the version is pinned by
`rust-toolchain.toml`) and **Node 24 with npm**. The frontend dependencies
support Node 22 from **22.13.0**, or **24 and later**; Node 24 is recommended.

Building needs a C toolchain, `pkg-config`, and OpenSSL development headers.
The per-turn sandbox needs
[bubblewrap](https://github.com/containers/bubblewrap). On Debian/Ubuntu,
install these packages, plus Git for cloning the repository:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev bubblewrap git ca-certificates
```

Clone the repository, build the binary, and add its directory to this
terminal's `PATH`:

```bash
git clone https://github.com/BFJ-Concerns/Swage.git && cd Swage
cargo build --bin swage
export PATH="$PWD/target/debug:$PATH"
```

The `PATH` change applies to this terminal only. In each new terminal,
repeat the `export PATH` command from the repository root. Examples using
`./target/debug/swage` also assume the repository root.

Start the gateway; it can run before a provider is configured:

```bash
./target/debug/swage gateway start          # listens on 127.0.0.1:3000

# In another terminal, from the repository root:
# install and build the frontend, then serve the web UI on http://localhost:5173
(cd frontend && npm ci && npm run build && npm run dev)
```

Open `http://localhost:5173`, then **Settings → Providers**:

1. Choose **Add provider** and the kind for your endpoint. For GPT-family
   models over the Responses API, choose **OpenAI Responses**
   (`protocol = "openai-responses"`). **OpenAI** and **OpenAI compatible**
   use Chat Completions (`protocol = "openai"`). For an OpenAI-family proxy,
   fill in its **Base URL**, including its API prefix such as `/v1`;
   other protocols use different paths (see [Provider setup](docs/interfaces-and-tooling.md#provider-setup)).
   For **OpenAI compatible**, enter a provider name and URL, then choose
   **Connect** to open its fields.
2. Paste the provider key into **API key** and wait for the saved notice
   and stored credential indicator. Settings stores an encrypted governed
   secret and writes `api_key_secret_ref`; no key export is needed.
3. Choose **Refresh** under **Discover**, then add a model, or enter a model
   ID your provider serves beside **Add model**. Choose **Use** on its row
   to make it the default and wait for autosave confirmation.

Provider and model changes saved here apply to new turns without a gateway
restart. For file-based configuration, see
[Provider setup](docs/interfaces-and-tooling.md#provider-setup); restart the
gateway after hand-editing `config.toml`. A model's `provider_id` selects
its route, including the saved protocol and URL. An unusable route fails
visibly instead of silently using another configured provider.

Choose **Close settings (×)**, then **Start** under **New session**, and ask
for a small tool action, such as using the shell to print the working
directory. Handle any sandbox or tool approval when it appears, after
provider setup. [First session](docs/first-session.md) covers topics,
workspaces and trust; none is required for a first conversation.

To try Swage without touching a real installation, point `SWAGE_HOME` at a
throwaway directory: `SWAGE_HOME=/tmp/swage-try ./target/debug/swage gateway start`.

## Documentation

- [First session](docs/first-session.md) — topics, attaching a repository as
  a workspace, and trusting it.
- [Agents and permissions](docs/agents-and-permissions.md) — agent
  definitions, `AGENTS.md` guidance, effort, tool permissions, approvals,
  sandboxing and experimentation controls.
- [Workflow engine](docs/workflow-engine.md) — how scripts run, replay after a
  restart, spend their budgets, park, and account for how they ended.
- [Interfaces and tooling](docs/interfaces-and-tooling.md) — the turn
  inspector, branching, surfaces, devices, credentials, the API error
  contract, and the published API reference.

## Running a gateway

### Lifecycle

The gateway detaches by default and writes its log to `gateway.log` in its
data root (`~/.swage/`, or `$SWAGE_HOME` when set). Use `--log-file PATH` for
another destination, or `--foreground` to keep it attached:

```bash
./target/debug/swage gateway start --foreground
./target/debug/swage gateway stop
```

The CLI finds the running gateway through a metadata file: under
`$SWAGE_HOME/run/` when `SWAGE_HOME` is set, otherwise under
`$XDG_RUNTIME_DIR/swage/`, falling back to `~/.swage/run/`. Run `stop` with
the same environment you started with. When
the gateway is the main process of a systemd service, `gateway restart`
restarts that service and waits for the new process to answer its health
endpoint. `swage generate-completion bash` (or `zsh`/`fish`) prints a shell
completion script.

### Credentials

Use **Settings → Providers** to store a governed credential, or configure
`api_key_secret_ref` with an existing secret reference. See
[Provider credentials](docs/interfaces-and-tooling.md#provider-credentials)
for secret backends, the labelled legacy `api_key_env` route and supported
Codex-family API routes. [Codex auth store](docs/interfaces-and-tooling.md#codex-auth-store)
gives the disabled integration's credential location.

### Sandboxing and approvals

Per-turn sandboxing is on by default: sandbox-eligible tool calls run under
bubblewrap and Landlock. Tools that need live gateway state run in the gateway
process, and sessions targeting a remote device use that device's execution
path. If sandbox setup fails — most often because bubblewrap is not
installed — a turn you started yourself pauses to ask whether to continue
unsandboxed; a scheduled or delegated turn, with nobody to ask, fails. The
failure reason quotes the cause. `sandbox.allow_degraded_execution = true`
skips that approval.

To run a topic without the sandbox, set this in its `config.toml`; its turns
then record a degraded execution boundary so the audit trail says they ran
unconfined:

```toml
[sandbox]
enabled = false
```

By default, `shell`, `file_edit`, `file_write` and `python_execute` ask for
your approval before each call in a session you are attending. Browser
clicks, typing, selections and scrolling do not ask for approval by default.
Scheduled, channel and delegated work runs on its grants alone.
[Agents and permissions](docs/agents-and-permissions.md#sandbox-isolation)
covers what the sandbox confines, how approvals work and how to change which
tools ask.

### Optional capabilities

These register or work only when their prerequisite is present:

| Capability | Prerequisite |
|---|---|
| Web search | A self-hosted [SearxNG](https://github.com/searxng/searxng) instance, configured under `[tools.search]` with `provider_type = "searxng"` and its `base_url` ([details](docs/agents-and-permissions.md#search-configuration)) |
| Browser tool | Chromium or Chrome on `PATH`, or its path in `SWAGE_BROWSER_BIN` |
| Python execution | `python3` on `PATH` |
| Telegram channel | A bot token from BotFather, named under `[channels.telegram]` as `bot_token_env = "<VARIABLE>"` |
| Voice | An OpenAI-protocol provider that serves the audio APIs (below) |

### Voice

Speech transcription and synthesis are optional. They need an
OpenAI-protocol default provider whose endpoint serves the audio APIs, plus a
model for each, set in `config.toml` or under **Settings → Voice**:

```toml
[voice]
transcription_model = "whisper-1"
speech_model = "tts-1"
```

### Where things live

Everything is under `~/.swage/` (or `$SWAGE_HOME`):

- `config.toml` — providers, models, sandboxing and platform settings.
- `AGENTS.md` and `instructions/*.md` — operator guidance read into every
  turn, layered after the built-in orientation. Workspaces may carry their own
  `AGENTS.md`.
- `skills/<name>/` — skills, one directory each.
- `topics/<topic>/` — a topic's own instructions, skills, per-agent overlays,
  memory and knowledge base.
- `general/` and `scratch/` — memory for sessions without a topic, and scratch
  directories for sessions without a workspace.
- `swage.db` — the SQLite store: conversations, turns, the audit trail, the
  knowledge base and workflow runs. The store refuses to open if its migration
  history does not match the build, rather than applying anything
  half-finished.

Configuration cascades Global → Topic → Agent; the more specific level
extends or shadows the more general one.

### Network binding

The gateway listens on `127.0.0.1:3000` by default. Set `[server] host` and
`port` in `config.toml`, or pass `--host`/`--port` to `gateway start`:

```bash
./target/debug/swage gateway start --host 192.0.2.10 --port 3000
```

**The gateway has no authentication.** Binding to a private-network address
trusts every host on that network with the full API and every configured
provider key. Public-internet exposure is not supported. Remember that the
host firewall must also allow the port before LAN clients can connect.

### A second, isolated instance

Give a development gateway its own data root and port, so it never opens
your real store or collides with the running instance; `SWAGE_HOME` also
gives it its own metadata file:

```bash
SWAGE_HOME=/tmp/swage-dev ./target/debug/swage gateway start --port 3100

# Point the dev web UI at it
(cd frontend && SWAGE_GATEWAY_URL=http://127.0.0.1:3100 npm run dev)
```

## Licence

Copyright © 2026 BFJ Concerns.

Swage is free software: you can redistribute it and/or modify it under the
terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version. See [LICENSE](LICENSE) for the full text.

The AGPL extends the GPL's source obligation to network use: if you run a
modified Swage as a service that others interact with, you must offer them
the source of your modified version.

Bug reports, questions and pull requests are welcome. Contributions are
accepted under the same licence.
