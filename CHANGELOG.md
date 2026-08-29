# Changelog

All notable changes to Swage are documented in this file.

## Unreleased

### Added

- Agents can now list the workflows available to their session before starting one.
- Asynchronous turn callers can select a configured model for one request, with the effective selection retained in that turn's audit snapshot.
- Context assembly alternatives can now be configured under `context.alternatives` for controlled selection experiments.
- Fresh spaces now include immutable researcher, organiser, and synthesist agent definitions for the shipped research-sweep workflow, with same-named authored definitions taking precedence.
- Knowledge captures made by workflow workers now carry their journalled call's identity, so a sweep interrupted mid-gather resumes without duplicating any source's observation.
- Research sweeps now author evidence-backed orientation cards during the organising pass and report claims the synthesist withheld for lack of supporting evidence.
- Research sweeps now derive gathered sources, failures, and withheld claims from the knowledge store through a read-only journalled script query.
- Asynchronous turn callers can provide optional focus instructions for context-compaction summaries, with the instructions retained in the compaction audit record.
- Operators can inspect recorded configuration changes, assembly decisions, and variant selections in a read-only history pane.
- Providers can now count prompt tokens under their own tokenizer where the vendor offers it (Anthropic), with decorated provider stacks forwarding the operation and estimate-based callers unaffected.
- Detached gateway starts now accept `--log-file`, report the chosen output destination, and explain foreground output handling in command help.
- Agents can route scheduled-run output to channels, local storage, and webhooks when creating or updating schedules.
- New conversations can choose a reasoning effort from the main composer, while each configured model can provide the inherited default.
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

- Context compaction now uses provider-native prompt token counts when available, while retaining conservative local screening and estimate fallback when counting is unavailable.
- Async turn submissions now reject unrecognised request fields instead of silently ignoring them.
- JavaScript context assemblies now expose `render()` only on template-backed briefing elements; calling it on literal elements fails the script and records the fallback to the default assembly.
- Gateway lifecycle defaults now use the descriptive `set-defaults` and `clear-defaults` commands; the terminal client is `chat`, and shell scripts are produced by `generate-completion`.
- Anthropic requests now use the native beta endpoint.
- Client-facing settings, asynchronous turns, context assembly reports, materials, attachments, and recovery listings now use the shared Rust wire contract and its generated TypeScript and JSON Schema projections.
- Delivery-log entries now identify the schedule run and turn that produced them when that attribution is available.
- Unknown keys under `[generation.presets.*]` and `[models.entries.*.capabilities]` now fail configuration loading instead of being ignored. Most other sections already rejected unknown keys through validation and still do; per-model pricing entries remain lenient.
- Tool approval prompting now defaults to disabled and remains available through explicit operator configuration.

### Fixed

- Gateway startup now reclaims worker worktrees and their Git registrations left behind by an unclean exit.
- Provider-facing tool schemas now retain stripped validation requirements in their descriptions, helping models form valid schedule delivery targets on the first call.
- Portable JavaScript workflow `meta` headers now seed saved script details, and the unused workflow-script `agent_id` query has been removed.
- Reserved workflow workers no longer load configuration, skills, named context elements, or JavaScript assemblies from an `__workflow_ephemeral__` agent directory, and that directory's skills are no longer listed by the skills API.
- Sandboxed dispatched-agent shell tools now mount the minimal device filesystem over the read-only root and add narrow per-node device rules to the Landlock ruleset, restoring standard device access for ordinary Git initialisation and commits.
- The conversation surface control no longer appears active while settings are open.
- Surface navigation now marks Settings, rather than a hidden conversation or operator surface, as active while it is open.
- Reconnecting after a dropped socket now clears only the captured live turn when its refreshed conversation is terminal.
- Gateway startup now directs legacy environment credentials to their governed secret-management routes.
- Missing file-like frontend paths now return HTTP 404 instead of a successful SPA shell.
- Turn-context inspection now shows exact built-in orientation and working-norm excerpts plus active skill names, while leaving operator and conversation content unpreviewed.
- Resumed workflow runs now park durably when a long timer follows an agent, child-workflow, or gate completion.
- The first rendered web frame now uses the light theme's complete token set before display preferences apply.
- Primary actions in space operations and schedule automation now use an AA-compliant light-theme treatment.
- Tool-call and connection-error failure states now use the shared error treatment instead of action orange; connection loss and reconnection keep their ordinary connectivity treatment.
- Inline-turn and approval errors now use the error-token wash in both themes.
- Workflow scripts now reject unrecognised `agent()` and `agentStart()` option keys before dispatch instead of silently dropping them.
- Reloading the web UI now returns to the open space instead of the space picker.
- Operator schedules now show their complete execution target from every space.
- The space picker now describes only the space behaviour the product provides.
- Session-scoped approval grants now apply only to the conversation session that granted them.
- Explicitly disabling the sandbox no longer also requires the degraded-execution opt-in.
- Gateways started with distinct explicit data roots now keep separate runtime metadata and report their own addresses.
- Obsolete private-alpha data roots now receive a clear rebuild instruction instead of a raw missing-workflow-table error.
- Chat Completions streams now deliver every parallel tool call accumulated before completion.
- Nullable object-valued tool parameters no longer fail nested required-field validation when set to null.
- Gateway start and restart failures now describe already-running and restart-refusal states truthfully.
- Bare skill commands now run as persisted conversation turns instead of leaving the composer waiting indefinitely after activation.
- Restarting a gateway now preserves its effective data root and runtime directory, and leaves the running gateway untouched when those values cannot be recovered.
- Workflow scripts now reject non-string `agent` and `agentStart` prompts before dispatch instead of silently sending coerced text such as `[object Object]` to workers.
- Starting a new conversation now returns the composer reasoning effort to **Model default** instead of carrying over the previous conversation's choice.
- Channel delivery totals and latest-delivery times now include only responses that were actually sent.
- Conversations with unrecognised model selections no longer apply the configured default model's reasoning effort.
- Rejected malformed tool calls no longer replay into provider history and prevent the conversation from continuing.
- Configured workflow token ceilings no longer prevent permitted parallel agent calls from sharing the working allowance while preserving the closing reserve.
- Development-channel no-ops now appear as `not_sent` with an explanation instead of claiming successful delivery.
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
