---
name: tool-creator
description: Design, add or change a Swage runtime tool — a `Tool` implementation the product's agents call by name — so that it is registered, reachable and chosen correctly. Use when asked to add a tool, merge or split tools, rename one, change a tool's parameters, return value, errors, permissions or secret handling, or fix a tool the model picks wrongly, calls with the wrong arguments or never calls. Not for a coding tool's own tools, and not for the product's skills (swage-skill-creator) or agents (swage-agent-creator).
metadata:
  audience: maintainers
  workflow: tool-authoring
---

# Tool creator

A Swage tool is a Rust type implementing the `Tool` trait
(`crates/swage-tool/src/tool.rs`): a `name`, a `description`, a JSON Schema
of `parameters`, and `execute`. The model sees only the first three, through
`definition()`, which forwards `name`, `description` and `input_schema` to
the provider and nothing else. It chooses a tool and fills in its arguments
from those alone, so most of the design is in them.

The built-in tools are the house style for naming, schemas and wiring. Read
two that resemble the one in hand before writing.

## Where things are

- The trait and its optional methods, each documented where it is declared:
  `crates/swage-tool/src/tool.rs`.
- The framework, in `crates/swage-tool/src/`: the registry (`registry.rs`),
  the dispatch pipeline (`dispatch.rs`), permissions (`permission.rs`),
  parameter validation (`validation.rs`), redaction (`params.rs`), rate
  limits (`rate_limit.rs`) and the delegate tool (`delegate.rs`).
- The built-in tools: `crates/swage-gateway/src/tool/`, one file for a tool
  or a family of them.
- `ToolError`: `crates/swage-core/src/error.rs`.

## Wiring a tool

A tool that compiles can still be unregistered, unreachable or absent where
it runs, and nothing at build time says so. A new built-in needs all of
these in the same change:

1. **Registration.** Construct it in the list that
   `build_tool_dispatcher_with_browser` assembles in
   `crates/swage-gateway/src/startup.rs`. Shell and the file tools are in
   `workspace_tools` beside it, the set a device process also runs.
   `ToolDispatcher::allow_all` registers everything in the list through
   `ToolRegistry::register_builtin`.
2. **The reserved name.** Add the name to `PROTECTED_NAMES` in
   `crates/swage-tool/src/registry.rs`. `ToolRegistry::register`, the entry
   point for a tool that is not built in, refuses a name on that list, and
   that refusal is what stops such a tool taking a built-in's name.
3. **Where it executes.** A tool whose `requires_device_execution()` returns
   false can be sent to the sandbox child process, which builds its own tool
   list in `build_sandbox_agent_tools` (`crates/swage-gateway/src/main.rs`).
   A tool missing from that list passes its schema and permission checks in
   the gateway and then fails when the call runs sandboxed. So either add it
   there or make it return true. It must return true when it needs the
   gateway's state, the host's network, or typed errors the sandbox channel
   cannot carry.
4. **Reach.** Dispatch is default-deny, so the agents meant to call the tool
   need a permission entry that grants it (`references/tool-design.md`,
   *Permissions*).

`Tool::name()` is the tool's identity everywhere: in the registry, in
`PROTECTED_NAMES`, in permission entries and bundles, in agent definitions'
permissions, in skills' `tools:` lists, in the orientation text and in the
docs. A rename changes every one of them in the same change and leaves no
alias behind. Search the tree for the old name.

Check the result with `just check`, the tests of the crate you touched
(`just test-lib <package> <filter>`), and the gateway tests that list the
registered tools, which a new or renamed tool usually has to join.

## The two kinds of work

**Design** decides what tools exist and what each takes, returns, costs and
may be called by. Read `references/tool-design.md` when adding a tool,
merging or splitting tools, or changing parameters, return values, errors,
permissions or secret handling.

**Description** is the text the model chooses by: the `description()` string
and each parameter's `description`. Read `references/description-writing.md`
when writing or changing either, and use the swage-prompt-engineering skill
for the wording, since both are instruction text.

Before changing an existing tool, read the whole of it and find every place
its name appears. Say what its callers rely on today, so that the change can
be checked against it.

## When a tool is used badly

Find which of these it is before changing anything, because each has a
different fix and a redesign is rarely the first:

- **The model picks another tool.** The two descriptions do not tell it
  which applies. Read them side by side and make each say where the other
  takes over.
- **Right tool, wrong arguments.** A parameter's description leaves its
  meaning, unit or source open, or the schema allows values the tool does not
  accept.
- **Never called.** Either the agent has no permission entry granting it,
  or the description does not say when it applies, or the agent's
  instructions steer it away from that kind of action.
- **Called where it should not be.** The description promises more than the
  tool does, or does not say what it leaves to another tool.
