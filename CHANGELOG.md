# Changelog

All notable changes to Swage are documented in this file.

## Unreleased

### Added

- New conversations can now choose from the effective agent definitions available to their space, including the shipped default.
- Sessions now receive operator guidance from standard `AGENTS.md` files: a global file in the Swage config root and one at the root of each trusted, readable attached workspace, layered after the baseline instructions (global before workspace) and visible in the turn context inspector. Untrusted workspaces contribute nothing.

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
