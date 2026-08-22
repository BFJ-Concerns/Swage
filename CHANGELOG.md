# Changelog

All notable changes to Swage are documented in this file.

## Unreleased

### Added

- Agents can now run newly authored workflow source directly without saving or registering it first.
- Durable named task definitions bundle instructions, an output contract, a model preference, and narrowing permissions for workflow and delegated child dispatches.
- Workflow and delegation authors can place individual workers in a chosen working directory or a runtime-provisioned isolated git worktree.
- Workflow agent calls now honour per-call reasoning effort and host-enforced timeout options.
- Workflow scripts can now dispatch ephemeral workers by prompt and model options without naming an agent definition.
- Skills can carry immediately runnable workflow scripts, with same-named authored workflows taking precedence.
- Fresh installations discover and activate the shipped development-process skills, with same-named operator skills taking precedence.
- Models now receive per-model native-harness scaffolds with family fallback, with each scaffold's divergence register recorded.
- Dispatches can name context elements, variable bindings, and an assembly for the child.
- Named reusable Markdown context elements now resolve through the global, space, and agent configuration hierarchy, with live session values rendered into each briefing at composition time.
- Workflow runs can wait on external events, answered through operator or granted agent posts.
- Session leads can save named values for their session and use them to fill parameterised context elements at composition time.
- Fresh installations now include front-loaded orchestration, research sweep, and segment orchestration workflows as immutable built-ins.
- Settings now lists the effective agent definitions for the selected space, with read-only source and configuration details for each winner.
- New conversations can now choose from the effective agent definitions available to their space, including the shipped default.
- Sessions now receive operator guidance from standard `AGENTS.md` files: a global file in the Swage config root and one at the root of each trusted, readable attached workspace, layered after the baseline instructions (global before workspace) and visible in the turn context inspector. Untrusted workspaces contribute nothing.

### Changed

- Client-facing settings, asynchronous turns, context assembly reports, materials, attachments, and recovery listings now use the shared Rust wire contract and its generated TypeScript and JSON Schema projections.
- Unknown keys under `[generation.presets.*]` and `[models.entries.*.capabilities]` now fail configuration loading instead of being ignored. Most other sections already rejected unknown keys through validation and still do; per-model pricing entries remain lenient.
- Tool approval prompting now defaults to disabled and remains available through explicit operator configuration.

### Fixed

- A delayed not-found response for an earlier conversation no longer clears a conversation selected while that check was pending.
- Browser back and forward navigation now restores the active conversation route when loading the destination fails.
- Provider fallback catalogues are current, attendance audit records distinguish interactive turns, and the context inspector identifies per-element token counts as non-additive estimates.
- Retained built-in workflow IDs now return not found after a same-named authored workflow shadows them, while the authored workflow remains readable and launchable by its own ID.
- Sandbox startup no longer fails when `TMPDIR` has a long path.
- Sandbox startup now falls back from an overlong `XDG_RUNTIME_DIR` socket path and reports how to correct a path that cannot fit.
- Generated TypeScript turn-poll responses now expose token counts as numbers rather than bigints.
- Fresh-session schedules created from Settings now use the currently selected space.
- Agent definition details now show the configured maximum output retries.
- Switching spaces no longer carries an unavailable agent into a new conversation.
- A temporary agent-inventory failure at startup no longer hides the available gateway.
- Streamed tool calls now remain attached to their assistant turn after completion and appear before the response both immediately and after reloading conversation history.

## 0.2.0 — 2026-08-13

### Added

- Long-running orchestration runs can now coordinate dependent tasks with bounded parallelism, worker restatement, iterative verification, and explicit acceptance before results are adopted.
- Runs now carry decisions and outstanding events between short lead sessions, re-evaluate stale decisions against changed state, and produce a handover when the work budget is exhausted.
- Workflow runs now retain completion, failure, and escalation updates until a lead wake delivers them, including across gateway restarts, with wake decisions and pending-update counts exposed in run details.
- Workflow runs can now park during long timer waits, free gateway resources while parked, and resume automatically when due or after a gateway restart.
- Workflow scripts can now record budget snapshots so budget-based decisions remain consistent when runs replay.
- Workflow budgets can reserve tokens for closing work, and budget snapshots now show separate working and closing allowances together with whether wind-down has begun.
- Delegated runs now retain context and effective-permission audit records for later inspection.
- Workflow scripts can now continue a worker in the same session by reusing the handle returned by an earlier worker call.
- Turn records now include redacted provider response metadata, including status, request identifiers, stop reasons, usage, and content-block sizes, without retaining response content.
- Sessions now receive working guidance tailored to the configured model family, with common guidance retained for unrecognised models.

### Changed

- Structured-output turns now provide the configured JSON Schema to the model on initial requests and validation retries, keeping responses aligned with the declared shape.
- Session orientation now explains what activity is recorded and that managed secret values are redacted from the local record.
- Interrupted timer waits now resume with their remaining duration instead of restarting from the full delay.

### Fixed

- The conversation composer now reflects newly saved provider and model settings without requiring a page reload.
- Fresh-session schedules can now use the calling conversation's space without requiring its UUID.
- Workflow runs now retain their reserved token allowance for final closing work, even when earlier agent calls are still running.
- Failed workflow runs now retain a durable handover identifying the failed call and the events delivered or still pending.
- Provider responses that complete without usable text or tool output are now treated as failures, allowing retries or failover instead of silently producing an empty result.
- Workflow wake gates now honour configured models on non-default providers.
- Failed conversation turns no longer leave the conversation blocked from starting another turn.
- Completed assistant replies now retain their turn identity, so their assembled context can be opened immediately from the conversation.
- Tools requiring approval are now available only when explicitly allowed, while allowed tools still pause for human approval before execution.
- Turns with an unknown forced variant now fail with a clear configuration error before any model request is sent.
- Conversation and space-session creation now rejects undeclared request fields instead of silently ignoring them, preventing accidental creation in the wrong space.
- Delegated and dispatched workers now remain limited to the tools and approvals granted by every agent in their dispatch chain, including after nested delegation and after permissions change while a run is paused.
- Workflow runs now replay consistently across gateway restarts and parked waits, without resurrecting runs cancelled concurrently.
