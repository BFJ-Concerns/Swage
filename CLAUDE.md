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
- `swage` — legacy façade crate (edition 2021)

Plus: `frontend/` (Svelte 5 SPA), `desktop/` (Node), `xtask/` (build tasks), `docs/`.

## Domain Model

The HTTP API speaks of "conversations"; storage uses Session + a default Thread.

- Session → Thread → Turn hierarchy. `agent_id` lives on all records.
- Messages belong to threads (flat, position-ordered), not turns.
- Thread state is enforced by storage (Idle/Processing/Completed/Interrupted); turn state is consumer-set.
- The storage supertrait composes `SessionStore` + `ThreadStore` + `TurnStore` + `MessageStore` + `ToolCallStore` + `SkillActivationStore` + `ApprovalStore`. `ProjectStore` is separate.
- A default project is auto-created in migration — every conversation belongs to a project.
- One active turn per conversation at a time.

## Gotchas

- Edition 2024 makes `std::env::set_var` and `remove_var` `unsafe`. Tests need an `unsafe { ... }` block.
- PyO3 timeouts use `PyErr_SetInterrupt`, not `PyThreadState_SetAsyncExc` — the latter caused SIGSEGV.
- Config tests can pollute each other via env vars when the workspace runs in parallel.
