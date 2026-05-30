# Agent Notes

Shared context for agentic coding tools. `AGENTS.md` symlinks to this file — edit either path.

Build, test, and run instructions live in a project skill, not here.

## Workspace

Rust 2024 edition (requires 1.85+). Crates under `crates/`:

- `swage-core` — shared types (`ChatMessage`, `SwageError`, `LlmRequest`/`Response`, `StreamChunk`)
- `swage-safety` — credential leak detection (Aho-Corasick + regex)
- `swage-hook` — lifecycle hook registry
- `swage-provider` — LLM providers (OpenAI/Anthropic/Google) and reliability decorators
- `swage-tool` — `Tool` trait, registry, permissions, dispatch pipeline
- `swage-storage` — SQLite WAL persistence; Session/Thread/Turn/Message/ToolCall/Approval stores
- `swage-agent` — runtime: builder, turn loop, hooks, compaction
- `swage-sandbox` — bubblewrap sandboxing
- `swage-config` — TOML config hierarchy (Global → Project → Agent)
- `swage-context` — context assembly engine (typed elements, compaction)
- `swage-gateway` — axum HTTP/WS server, frontend embed, combined CLI binary
- `swage-python` — PyO3 embedding for programmable assembly and tools
- `swage-client` — typed gateway client (shared by CLI and TUI)
- `swage-tui` — terminal UI (ratatui + crossterm)
- `swage-topology` — graph/topology primitives
- `swage` — façade crate: single `use swage::...` entry point re-exporting the underlying crates, plus `SwageBuilder` and convenience constructors

Plus: `frontend/` (Svelte 5 SPA), `desktop/` (Node), `xtask/` (build tasks), `docs/`.

## Domain Model

The HTTP API speaks of "conversations"; storage uses Session + a default Thread.

- Session → Thread → Turn hierarchy. `agent_id` lives on all records.
- Messages belong to threads (flat, position-ordered), not turns.
- Thread state is enforced by storage (Idle/Processing/Completed/Interrupted); turn state is consumer-set.
- The storage supertrait composes `SessionStore` + `ThreadStore` + `TurnStore` + `MessageStore` + `ToolCallStore` + `SkillActivationStore` + `ApprovalStore`. `ProjectStore` is separate.
- A default project is auto-created in migration — every conversation belongs to a project.
- One active turn per conversation at a time.

## Working Principles

How to extend a workspace that spans many crates and several surfaces
(gateway, TUI, CLI, web). These are about *where* behaviour should live.

### Prefer the general path over the special case

Unless it is genuinely unavoidable, don't add a special case where a general
one would serve. When a requirement could be met either by a one-off branch or
by extending a shared path that future work can reuse, extend the shared path.
Special cases multiply — each becomes the template for the next, and the
general capability that would have absorbed all of them never gets built. If a
special case truly is unavoidable, say why at the call site.

The turn loop is this principle in practice. Turn processing lives in one
place — `Agent::process_turn_streaming` (`swage-agent`) — and surfaces consume
its `AgentEvent` stream as data rather than reimplementing the loop: the
gateway forwards events to clients, it doesn't run its own. Injection points
like `ToolExecutionStrategy` (`swage-tool`) let a surface add behaviour — the
gateway's secret resolution and sandbox execution — through the shared dispatch
pipeline instead of forking it. New surface behaviour should arrive the same
way: as data on the shared path, not parallel code per surface.

### Name model-facing surfaces by function

Anything the model reads should present itself by what it *is* or *does*, not
by the component, codename, or mechanism behind it. This covers labels and
headings (a context section title), the values inside them (a status, a
category), and the framing around them (a tool description, the opening line of
an agent prompt). The model can't distinguish an internal label from genuine
guidance: `## Conversation Summary` says what the block is;
`## Conversation Summary (Structured Extraction)` leaks the mechanism that
produced it and tells the model nothing useful.

Keep internal identifiers — agent ids, struct names, context element keys like
`"prompts"` and `"history"` — in the code, where they address developers. Where
the same concept crosses into the model's context, describe it by function. The
built-in tool descriptions get this right (`file_read`: "Read the contents of a
file in the workspace."); the assembly element keys stay inside the report and
never reach the model.

## Gotchas

- Edition 2024 makes `std::env::set_var` and `remove_var` `unsafe`. Tests need an `unsafe { ... }` block.
- PyO3 timeouts use `PyErr_SetInterrupt`, not `PyThreadState_SetAsyncExc` — the latter caused SIGSEGV.
- Config tests can pollute each other via env vars when the workspace runs in parallel.
