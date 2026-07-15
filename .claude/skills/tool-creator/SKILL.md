---
name: tool-creator
description: Use this skill to design, add, and improve Swage runtime tools — the `Tool` implementations in `crates/swage-tool/` and the built-in tools under `crates/swage-gateway/src/tool/`, which the LLM agent invokes by name through the dispatch pipeline. Use when the user wants to add a new tool, consolidate or split existing tools, rewrite tool or parameter descriptions because the model is choosing them poorly, fix tool naming/namespacing, design better tool return shapes, make tool error messages actionable, or decide a tool's permission and redaction behaviour. Not for Claude Code tools, and not for Swage runtime *skills* (use the skill-creator skill for those).
metadata:
  audience: maintainers
  workflow: tool-authoring
---

# Tool Creator

Guide the session through designing, adding, and revising the tools the Swage agent calls.

A Swage tool is a Rust type implementing the `Tool` trait in `crates/swage-tool/src/tool.rs`. The trait surface is small: `name`, `description`, `parameters` (JSON Schema), `execute`, plus optional `sensitive_keys`, `device_secret_keys`, `requires_device_execution`, and a `definition()` convenience the framework forwards to the provider. The model picks tools entirely from the `name`, `description`, and parameter schema it sees, so the value of the work sits in those three surfaces, not in `execute`.

The calibration point: the built-in tools under `crates/swage-gateway/src/tool/` are the house style for naming, schema shape, and dispatch wiring. Read a couple before writing a new one. Vague descriptions that merely restate the name are the failure mode this skill is here to break.

## Two Distinct Activities

Tool work splits cleanly into two activities. Treat them separately.

1. **Tool design** — what tools should exist, what shape they take, how they consolidate or split, what their parameters and return shapes look like, where they register, which agents are permitted to call them. See `references/tool-design.md`.
2. **Description writing** — how to write the `description()` string and the per-parameter descriptions so the model can pick the tool correctly and call it with the right arguments. See `references/description-writing.md`.

Most "the model isn't using my tool" problems are description-writing problems even when they look like design problems. Diagnose before redesigning.

## Where Tools Live

- The trait itself: `crates/swage-tool/src/tool.rs`.
- Shared tool framework: `crates/swage-tool/src/` — registry (`registry.rs`), dispatch pipeline (`dispatch.rs`), permissions (`permission.rs`), schema validation (`validation.rs`), parameter helpers and redaction (`params.rs`), rate limiting (`rate_limit.rs`), the delegate tool (`delegate.rs`).
- Built-in tool implementations: `crates/swage-gateway/src/tool/` — one file per tool (`file_read.rs`, `shell.rs`, `python_execute.rs`, and so on).
- `ToolError` lives in `crates/swage-core/src/error.rs`.
- The tool name the model sees comes from `Tool::name()`. It must match the registry registration, the protected-namespace entry (for a built-in), and every permission entry that refers to the tool.

## Core Workflow

1. Identify whether this is a new tool, an edit to an existing tool, a description rewrite, or a consolidation/split. The four take different routes.
2. Run the intake interview (below). Even a "just rewrite the description" task benefits from confirming what the tool is for and what the current model behaviour is.
3. For an edit or rewrite, read the current implementation in full before changing it — `name`, `description`, `parameters`, `execute`, the registry registration, and any permission config or test that references the tool name. Renames especially need every reference updated together.
4. Decide the design first (see `references/tool-design.md`): is this one tool or several, what does the parameter schema look like, what does the return value carry, which agents should be permitted to call it.
5. Write the description and parameter descriptions (see `references/description-writing.md`). This is the highest-leverage part of the work — give it real attention, not a tail-end pass.
6. Wire the tool: register it, add it to the protected namespace if it is a built-in, and confirm the permission model lets the intended agents reach it.
7. Summarise what changed — the design choice, the description, and the wiring — before writing live files.
8. After the edits land, verify them. A tool that compiles can still be unregistered, unadvertised, missing from the sandbox catalogue, or rejected at dispatch — none of which the compiler catches. Confirm the crate builds and the relevant tests pass through the project's `just` targets; at minimum run `just check` and the targeted tests for the touched crate, plus any test that enumerates the tool catalogue.

## Intake Interview

For new tools, edits, and rewrites alike:

1. What should calling this tool let the agent accomplish? Phrase it as a verb on a resource.
2. Which agents should be able to call it, and through what permission entry — a direct allow, a bundle, or behind an approval gate?
3. What does success look like — what does the return value carry, and what does the agent do with it next?
4. What are the side effects? Does the call write state, emit a message, hit an external service, cost money, become visible to the user, or affect other sessions or agents?
5. Which existing tools sit nearest to it? Could it be an `action` parameter on one of them instead of a new tool?
6. What does the model currently get wrong (for rewrites)? Wrong tool chosen, wrong parameters, wrong frequency, ignored entirely?
7. Does the tool touch secrets or sensitive values that need redaction (`sensitive_keys`) or host-side secret resolution (`device_secret_keys`)?

Run this as a back-and-forth. The questions exist to surface the design choice and the description targets — not to be filled in mechanically.

## Diagnose Before Redesigning

When the user reports a tool problem, separate diagnosis from the first guessed fix:

- **Wrong tool chosen** → usually a description disambiguation problem. Compare the descriptions of the candidates the model is confusing.
- **Right tool, wrong parameters** → usually a parameter description or schema problem. Look at the parameter `description:` fields and whether the value space is constrained (enums, format, units).
- **Tool not called at all** → either the agent's permission config does not allow it, the description is too generic to match, or the model has been told to avoid the kind of action it represents.
- **Tool called too often** → missing anti-trigger ("do not use this for X"), or the description claims more capability than the tool delivers.

Each branch routes to either `tool-design.md` or `description-writing.md`. Most route to description-writing.

## Core Checklist

Detailed checklists live in the references. This one is the minimum gate.

- [ ] the tool's purpose is named as a verb on a resource, and a near-lookalike has been identified or ruled out
- [ ] design choices are made (see `references/tool-design.md`): consolidation, namespacing, parameter schema, return shape, permission and redaction behaviour
- [ ] the description and per-parameter descriptions are written (see `references/description-writing.md`) and reviewed against their checklist
- [ ] the tool is registered, added to the protected namespace if built-in, and the permission model lets the intended agents reach it
- [ ] every reference to the tool name (registry, permission config, tests that enumerate the catalogue) has been updated together
- [ ] the change has been summarised to the user before live files are touched, including the design choice and the description
- [ ] after the edits, the crate builds and the relevant tests pass — including any catalogue/dispatch tests, and the sandbox path if the tool runs there (see *Wiring Checklist* in `references/tool-design.md`)

## References

Read these on demand:

- `references/tool-design.md` — choosing what tools should exist, shape of parameters and returns, namespacing, consolidation versus splitting, permission and redaction behaviour, error message design, registry wiring
- `references/description-writing.md` — how to write the `description()` string and per-parameter descriptions so the model picks and calls the tool correctly
