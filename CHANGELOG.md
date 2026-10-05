# Changelog

All notable changes to Swage are documented in this file.

## Unreleased

### Added

- Settings has a Memory tab listing every memory page, general and per topic, to read; a chat box there changes memory by instructing the curator, showing its reply and each page it changed. The curator's notice links open the page they name.
- Settings edits the web search provider and applies saved changes to new turns.
- The compaction threshold is configurable: `[context] compaction_threshold = { messages = N }` or `{ tokens = N }`, resolved through the configuration cascade (default: 100 messages). A session that keeps hitting walls — the same tool call failing again and again, or failed tool calls and failed turns piling up across its recent turns, provider timeouts and rate limits excluded — continues on a successor built from a blind diagnosis: separate readers each locate what went wrong in part of the history, a synthesis names the problems, and the successor starts from those notes with only the newest message kept verbatim. Every turn runs on such a successor while the walls remain among the recent turns: a turn whose recent walls are the ones the previous diagnosis answered starts from that diagnosis again, with every message since its split kept verbatim, new walls get a fresh diagnosis, and a turn with no wall left in view gets its full history back. The readers and the synthesis run on the session's model unless `[context] diagnosis_reader_model` or `diagnosis_synthesis_model` names a model entry or alias. The turn-context inspector shows the pass as a compaction with requested strategy `succession-degradation`, its diagnosis recording the walls answered and each model call's model, usage and report. It retains compaction decisions from the first assembly and the successor in pass order, even when their names match. Transient provider failures, including HTTP 5xx responses and malformed streams, do not trigger wall succession; provider refusals of the agent’s request still count as walls. If any diagnosis reader or the synthesis fails or returns an empty report, the turn falls back to the threshold path and compacts only when its context exceeds the threshold, retaining the failed diagnosis and call in its context record. Settings saves reject invalid compaction thresholds and diagnosis model names, including retained references to a removed model, before writing the configuration.
- Fresh scheduled sessions can run without a topic in the general scope, from Settings, schedule tools, or the API.
- The web UI's front door is the recent-sessions list with a **New session** control beneath it: choose a topic or none and the agent to run under, and the composer opens in that scope — the first message creates the conversation, so an abandoned start leaves nothing behind. The topic picker and the top-bar topic switcher are gone; a session's topic (or **General**) is named in the top bar and links to the topic's settings page, which now carries the topic's description, workspace relinking and deletion alongside its workspaces, default surfaces, tags and sessions. Every topic is listed in the settings sections.
- The shared surface library has its own page (`/library`), reachable from the top bar, the front door and the surfaces pane, where entries are filtered, added to the open session, and removed. Saving or removing an entry broadcasts `library_updated` on the work-surface channel, so open catalogues refresh without a reload.
- An open surface has an address (`/conversations/{id}/surfaces/{surface_id}`): surfaces can be linked to and restored with the browser's back button. Notes on a module can be edited in place.
- Conversation reads (`ConversationResponse`, `ConversationSummary`, `ConversationDetail`) carry the `agent_id` the session runs under. `GET /api/agent-definitions` lists the definitions a session without a topic can run under, and `GET /api/skills` without `topic_id` resolves the general scope's skills.
- The web UI opens to a recent-sessions list across every topic and the general scope, ordered by the latest user or assistant message, with a topic filter (**All sessions** by default) and an archived view; automated sessions — schedule runs, workflow workers, delegated children — are grouped beneath the conversation or schedule that caused them. Every session records its origin, and workflow runs record the session that dispatched them (`GET /api/sessions`).
- Sessions with no user or assistant message for a configurable number of days (`[sessions] archive_after_days`, default 7, set under Settings → Automation) are archived out of the recent list. Only a session with a turn executing is exempt; a new turn on an archived session returns it, and archiving cancels nothing.
- Surfaces belong to the session that created them and stay with its record. A surface can be saved to a shared library with a name, a description of what it does and when to use it, and tags; any session in any topic or the general scope can add a library entry as a surface of its own, receiving the saved content only within the entry's origin topic and the bare shape elsewhere. A topic can designate library entries as defaults that open in each of its new sessions, and carries tags that narrow the catalogue for its sessions. Agents browse the catalogue (descriptions and tags, never content) and add entries through the new `surface_library` tool, and save surfaces through `surface_write`.

### Changed

- The operator history names each changed configuration field with its prior and new value — nested settings by their path, such as Context › Assembly — and keeps the whole prior and new objects collapsed one click away, so variant selections and assembly decisions sit close below.
- Per-turn sandboxing is off by default; a topic enables it with `[sandbox] enabled = true`. Local gateway turns with sandboxing disabled record a degraded execution boundary unless the install declares its own boundary.
- Unused keys in `[tools.search]` are refused at configuration load.
- Voice transcription and speech synthesis request the models set in `[voice]` (`transcription_model`, `speech_model`), editable under Settings → Voice (the Voice entry in the settings navigation, which states the prerequisite: an OpenAI-protocol default provider plus the two model ids), from the default provider's endpoint; they never use the chat model. With a model unset, that capability reports itself unavailable, and the voice capabilities route (`unavailable_reason`) says what to configure.
- Unsupported search `provider_type` values are refused when configuration loads and when settings are saved; the unused search `api_key` setting is removed.
- Sessions without an attached workspace say so in their orientation and name the scratch directory where tool paths resolve; attached workspaces and placed workers retain their working-directory line.
- The client OpenAPI document declares the recent-sessions `archived` query parameter as a boolean, matching the generated TypeScript client and gateway.
- Topic-local surface templates are replaced by the shared surface library; surfaces are listed and created per session (`/api/sessions/{id}/surfaces`) rather than per topic, and a schedule may deliver to a surface only when it continues the session that owns it.
- The container is now called a topic and the shared work view a surface everywhere — storage, API routes, tools, events, config directories (`~/.swage/topics/`), UI, and documentation. No aliases are retained, so stores and config directories from before this change are not read; private alpha starts from a fresh rebuild.

### Fixed

- The workflow-authoring skill states event-kind names as the gateway accepts them: segments are joined by `.`, `:` or `/` only, with `_` and `-` allowed inside a segment but never separating one.
- Denying a tool approval identifies the user's denial in the tool card and the feedback sent to the model, distinct from an approval timeout.
- The session archive-delay field states its one-day minimum and visibly refuses smaller values without changing the saved delay.
- Queued messages and the turn they start reach every open view in order; a queued message no longer overtakes the reply before it.
- Provider refusals keep a visible account in the conversation with a way to retry or amend the request, instead of an empty reply. Exhausted empty provider responses retain their own failure account.
- Updating a schedule’s agent is refused with guidance to create a new schedule with that agent, rather than reporting success with the existing agent.
- Tool bundles appear in a stable order in Settings.
- After an approval decision, the model is told plainly whether the person approved or denied the tool call: a denied call reads as the user's final denial (it did not run and nothing is pending), and an approved call's result names the approval scope, once or for the session. Successful calls confirm they ran; calls refused by parameter validation say they were approved but did not run. Approval feedback leaves the tool payload, stored output metadata and hook input unchanged.
- Automation explains that Create schedule saves and activates a schedule without a gateway restart; the Autosave indication accompanies only the settings that save automatically.
- With no devices attached, the Operator Deck's Devices card says so and explains how to pair one with a one-time pairing code, matching the Approvals and Schedules cards' empty states.
- Tables in surface document modules show padded, bordered cells, so adjacent names, figures and statuses read as separate columns instead of running together.
- Assistant replies span the conversation column at wide windows, as tool blocks do: tables and code blocks use the available width, a table wider than the column scrolls inside itself instead of wrapping words mid-word, and prose keeps a readable line length.
- A workflow that fails on a named error keeps that error's name in the run record, so an unknown `agent()` option is recorded as `UnknownAgentOptionError` naming the option, and a missing agent as `AgentRequiredError`, instead of a plain `Error`.
- A scheduled run interrupted by a gateway stop records that nothing was delivered, with the interruption reason in its delivery account.
- With nothing awaiting approval, Operator shows Approvals as a slim full-width strip and gives Schedules the whole row, so schedule briefs and expanded run histories are no longer squeezed beside an empty half-width pane. A pending approval brings the side-by-side panel back.
- File search, read and edit recognise a binary file from its first 8 KiB and read no further, and file search reads text line by line. A search reaching multi-gigabyte model weights no longer holds each file whole in memory, which had grown the gateway past 20 GB.
- Underscores in fill-in blanks, such as `__:__`, render literally in assistant messages while ordinary underscore emphasis still renders as emphasis.
- PEM private keys are redacted whole, including their body and footer; truncated keys are redacted through the end of the input, and overlapping secret matches leave no plaintext tail.
- Retry badges name the model requested for the current turn.
- Stopping a turn ends a shell command running inside the sandbox, in both bubblewrap and fallback modes, and its call settles as cancelled.
- Failed shell commands and workflow runs show as failed on their tool rows, live and after reload.
- Paused schedules hide their inactive next-run time in Automation and Operator.
- A workflow call whose pinned model is unknown sends its own prompt to the declared fallback.
- Replies cut short by the output token limit retain their partial text and show an incomplete status with the cause in the conversation, delegated child record and parent completion notice.
- `gateway status` after a clean stop reports not running without an error and exits successfully; `gateway start` recovers after a hard kill even while the terminated process awaits reaping.
- A turn that failed, was interrupted or was cancelled keeps its explanation and its tool calls in the conversation after a reload, attached to the request it answered; a failed turn offers Retry.
- Tool output written into an attached workspace no longer appears as untracked files in that workspace’s Git repository.
- Session previews in conversation, topic and operator lists show the latest user or assistant message; tool and system messages do not replace the preview.
- A turn interrupted mid-tool resumes without repeating tools that had already completed: each tool call is recorded as soon as the model selects it and settled when it finishes, a call cut off by a restart is settled as interrupted and one stopped with its turn is settled as cancelled, and the resumed conversation replays those calls with their results.
- Shared memory notes now save to the session's topic, or to general memory when no topic is selected, so later sessions in that scope can read them.
- Stopping a turn stops the shell command and its child processes when it runs on a device or directly on the gateway without sandboxing.
- Relinking a moved workspace keeps its label, permissions, trust and working-folder selection; a failed relink leaves the attachment unchanged.
- Surfaces opened by a topic when a person starts a session now appear in the local audit trail with their library origin and module content, and notify open surface views. Saving a surface to the library records the entry and its audit event together.
- Fresh schedule runs no longer open a topic's person-facing default surfaces; those defaults open when a person starts a session.
- Deleting a topic leaves library entries saved from it and other topics' default designations intact; the catalogue identifies the deleted origin, and seeded surfaces retain the source entry's name after its removal.
- Starting a session and moving to another scope before the gateway answers no longer activates the late session where the user now stands; it exists in the scope it was asked for and the recent list finds it.
- The top bar and the two-column surfaces wrap and stack on phone-sized viewports instead of clipping at the right edge, so every control stays reachable.
- A topic's default working folder and an explicitly selected workspace now pass the same trust gate as every other attachment: an untrusted selection grants no file access, and revoking trust removes the access it granted while leaving its configured permissions in place.
- `gateway restart` now tells a restart that failed after stopping the gateway apart from one refused before it, so the caller knows whether a running gateway was lost.
- The event-posting tool now names the event kind and payload shape an assembly decision accepts, including each finding disposition's fields and values; its previous example led a lead to post an answer the hold ignored.
- The shipped assembly pipelines' holds now post and await namespaced event kinds (`decision.needed`, `decision.answer`); the engine rejected the previous bare kinds, so no hold could be raised or answered.
- Anthropic-shaped provider streams now reject malformed tool-use blocks without a name before they can enter replay.
- Ending a workflow now closes its unanswered decision holds, stopping their watchers and removing them from the answer queue whether the run completes, fails, or is cancelled; late answers are rejected without adding decision history.
- Answering a workflow's decision and cancelling the run it waits on now settle against each other: exactly one of them takes effect, the loser reports the state the winner left and starts nothing, an accepted answer and the parked run's readiness to act on it are recorded together so a crash between them cannot strand the verdict, and a cancelled run launches no engine afterwards.
- A stale cancellation that loses to a resumed workflow run now withdraws only its own worktree-retention claim, so the winning run's recoverable work remains available.

### Added

- A conversation can start without a topic. It runs as a general assistant in the general scope — general memory, a cumulative general knowledge base, its own attachments, ordinary granted tools — and inherits no files, memory or context from any topic; its configuration is Global → Agent, and its orientation says that no topic is selected and no working directory is attached rather than implying an earlier topic is current. The web UI's topic picker offers **Start without a topic**, `POST /api/conversations` creates such a session, and every session-shaped response carries `topic_id` only when the session has one. The general scope's files live under `~/.swage/general/`; it is never listed as a topic and cannot be opened or deleted. The former default topic is gone.
- Attaching a workspace from a topic's Settings now offers the same file-change and trust choices as the topic picker, and the first attachment becomes the topic's working folder; the attachment list marks which one that is.
- On Linux, `gateway restart` on a gateway that is the main process of a systemd service now restarts that service and waits for the health endpoint, keeping the service's supervision instead of leaving a detached daemon and a failed unit behind. Per-call overrides such as `--port` are refused for a supervised gateway.
- A workflow can raise a decision it cannot make itself, naming the question and at least two verdicts it accepts, and park until an agent answers it through a granted tool; an answer outside the accepted set is refused with feedback and the decision keeps waiting, with the raise, the answer, and every refusal made while the decision remains open kept on the run record.
- The shakedown skill — the detection-only QA campaign that follows a discharged build ledger — now ships with the gateway alongside commission, maestro, and assembly, carrying its driver and judgement waves as workflows started by name.
- Agents can read which workflow runs in their topic are still going and what each is waiting on through the new `workflow_run_status` tool: every run that has not ended with its undelivered events — a pipeline's pending hold among them — or one named run in any state. The schedule gate's state digest now carries the same open runs and pending events, so a schedule that steers pipelines wakes on accumulated decisions rather than on the clock.
- A workflow run that ends by any path now leaves a terminal account: how it ended, and a partition of every worker it dispatched into done, failed, cut short, and unsettled, each with the directory its work was left in. A run that never returned also surfaces its completed workers' output as partial results with the reason it produced none, on the run detail, on the run's own view at `/workflow-runs/{id}`, and through the new `workflow_run_account` tool a lead agent can read; the run view loads one coherent record for its account and journal, and the worker directories such an account names are kept, surviving the gateway's shutdown and the next one's startup sweep.
- Workflow run details now show when each worker started and first produced evidence.
- Workflow token budgets enforce independent ceilings and closing reserves for each configured provider route; every call, including a declared fallback, is admitted against its provider route's ledger or the run-wide fallback ledger, while recovery preserves the admission already granted to interrupted calls.
- Workflow scripts can hold a worker open across successive turns — `session()` fixes its agent, model, effort, placement, and requested grants at creation, `send()` gives it another turn on the context it has already built, `staleness()` reports the recorded idle gap against the provider route's own threshold, and `close()` releases it; a run ending by any path closes the workers it still holds, and changing model or effort mid-session is refused so an escalation is always a fresh dispatch. A delegating agent can likewise name a child and come back to it later, landing the new task on that child's own session with everything it was told and answered before.
- Every workflow agent call is now recorded with the prompt and resolved options each attempt of it was dispatched under, the provider rounds every attempt ran with start, end, and outcome, and a usage record that reads as unavailable with a reason where the provider reported none. Unknown usage marks budget readings incomplete and closes the token dispatch gate instead of being treated as zero spend. A worker the run's own end cut short is recorded as cut short rather than as its own failure, and a call the run's end interrupted keeps that attempt's rounds beside those of the attempt replay dispatched after it. Run detail preserves structured prompt whitespace and wraps long tokens.
- Images now reach every selected provider protocol that advertises vision, and an image reference that provider cannot resolve is refused before the turn reaches it.
- Named context assemblies now keep the baseline a session starts with: an element the built-in assembly would have carried is restored when a script leaves it out, unless the script declares the omission and its reason, which the turn's context inspector shows.
- Workflow fan-in calls can opt into an identified result envelope, preserving each leg's label, phase, successful output, or failure account through parallel and pipeline collapse; malformed identity options fail before dispatch rather than being reported as worker failures.
- Schema-constrained workers now accept JSON values embedded in prose or Markdown fences and use the final value that fulfils their schema.
- Workflow run details now report each isolated worker worktree's base and tip commits plus whether it changed or was removed.
- Workflow calls can declare a fallback model for unavailable or pre-output failures, and their journal records the model that served.
- Effort intent now reaches Gemini 2.5 Flash-family budgets and Codex app-server turns through their native controls.
- Workflow scripts may declare an opaque task identity, retained on their run records and available when listing runs for the same task.
- Surface template creation records its complete ordered module configuration in the local audit trail.
- Users and agents can leave durable notes on a surface's modules, appearing on an open surface as they are written and readable by the agent alongside the module they hang on.
- Work surfaces receive server-wide surface signals over their own listen-only WebSocket, so an open surface stays current whether or not a conversation is selected.
- Workflow runs now journal their terminal budget balance.
- Workflows may declare an argument schema in portable metadata; saved runs validate it before starting and workflow listings expose the declared or unconstrained contract.
- The conversation now rides every work surface: surfaces carry the same live conversation beside them, with earlier conversations picked up and new ones started from the channel itself, so moving between surfaces neither leaves the conversation behind nor resets it.
- Workspace attachment, trust, and detach controls are now reachable from the selected topic's Settings workspaces section.
- Shipped development workflows are now syntax-checked at the gate, and the assembly build-and-verify workflow is runnable.
- Agents can now list the workflows available to their session before starting one.
- Agents can save and revise named workflow scripts for their topic, versioned alongside operator-authored ones; a workflow authored in a conversation never carries a delivery grant, so it cannot send anything off the machine, and a workflow that already delivers can only be revised by an operator.
- Asynchronous turn callers can select a configured model for one request, with the effective selection retained in that turn's audit snapshot.
- Context assembly alternatives can now be configured under `context.alternatives` for controlled selection experiments.
- The research sweep ships inside a research skill that carries its workflow; its researcher, organiser, and synthesist roles are dispatches inside that workflow rather than agent definitions, so a fresh installation lists only the shipped default agent and no compiled-in workflow catalogue. The workflow listing names each carried workflow's skill in place of the former built-in flag.
- Knowledge captures made by workflow workers now carry their journalled call's identity, so a sweep interrupted mid-gather resumes without duplicating any source's observation.
- Research sweeps now author evidence-backed orientation cards during the organising pass and report claims the synthesist withheld for lack of supporting evidence.
- Research sweeps now derive gathered sources and failures from the knowledge store through a read-only journalled script query.
- Asynchronous turn callers can provide optional focus instructions for context-compaction summaries, with the instructions retained in the compaction audit record.
- Operators can inspect recorded configuration changes, assembly decisions, and variant selections in a read-only history pane.
- Providers can now count prompt tokens under their own tokenizer where the vendor offers it (Anthropic), with decorated provider stacks forwarding the operation and estimate-based callers unaffected.
- Detached gateway starts now accept `--log-file`, report the chosen output destination, and explain foreground output handling in command help.
- Agents can route scheduled-run output to channels, local storage, and webhooks when creating or updating schedules.
- Schedules and workflows can deliver output to a surface in their own topic.
- `LocalStorage` remains the log target, retaining output in the local delivery record.
- Schedules now record why they made no delivery when overlap policy skips them, gates suppress them, or a run fails before dispatch.
- New conversations can choose a reasoning effort from the main composer, while each configured model can provide the inherited default.
- Agents can now run newly authored workflow source directly without saving or registering it first.
- Durable named task definitions bundle instructions, an output contract, a model preference, and narrowing permissions for workflow and delegated child dispatches.
- Workflow and delegation authors can place individual workers in a chosen working directory or a runtime-provisioned isolated git worktree.
- Dispatch results and the audit trail now name the directory where each child worker ran.
- Workflow agent calls now honour per-call reasoning effort and host-enforced timeout options.
- Workflow scripts can now dispatch ephemeral workers by prompt and model options without naming an agent definition.
- Skills can carry immediately runnable workflow scripts, with same-named authored workflows taking precedence.
- Fresh installations discover and activate the shipped development-process skills, with same-named operator skills taking precedence.
- Models now receive per-model native-harness scaffolds with family fallback, with each scaffold's divergence register recorded.
- Dispatches can name context elements, variable bindings, and an assembly for the child.
- Dispatched workers can be given named skills for one workflow or delegation call.
- Named reusable Markdown context elements now resolve through the global, topic, and agent configuration hierarchy, with live session values rendered into each briefing at composition time.
- Workflow runs can wait on external events, answered through operator or granted agent posts.
- Session leads can save named values for their session and use them to fill parameterised context elements at composition time.
- Fresh installations now include front-loaded orchestration, research sweep, and segment orchestration workflows as immutable built-ins.
- Settings now lists the effective agent definitions for the selected topic, with read-only source and configuration details for each winner.
- Settings now lists the selected topic's available slash commands and resolved skills, with read-only detail for each item.
- New conversations can now choose from the effective agent definitions available to their topic, including the shipped default, whether starting from an existing conversation or an empty topic.
- Sessions now receive operator guidance from standard `AGENTS.md` files: a global file in the Swage config root and one at the root of each trusted, readable attached workspace, layered after the baseline instructions (global before workspace) and visible in the turn context inspector. Untrusted workspaces contribute nothing.

### Changed

- The shipped development-process pipelines now hold their workers across turns: build-verify and estate-unit keep one builder from restatement through build, fix rounds and a warm landing, docs-update keeps its writer likewise, and each keeps its cross-family verifier for every re-verification, so a fix round returns to the worker that holds what it built and a re-verification to the reader that derived the claims; a held worker's effort is fixed at creation, so fix rounds run at the build pin and escalation is a park and a re-brief. Every dispatch in every shipped workflow — the assembly pipelines, the commission and shakedown waves, the research sweep and the teardown audit — now writes a `[dispatch]` line to the run journal saying whether it continued a held worker or re-briefed a fresh one and why, with the staleness reading where the choice turned on it. The workflow-authoring skill documents the held-worker surface and the continue-or-re-brief rule.
- The `workspace.path` key is removed; workspace attachments and managed scratch roots define tool scope, and local execution is no longer listed as an attached device.
- Presence APIs and protocol types now consistently identify attached devices; the former machine-named routes and fields are removed in this private alpha.
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

- Workflow calls that name an unknown model now fail at that call instead of silently using the default model.
- Workflow call author errors now fail before a worker or child workflow starts, and propagate out of parallel and pipeline rather than being reported as a failed worker slot.
- New-conversation controls now create and navigate immediately.
- New-conversation agent pickers now show every available definition by name.
- Live replies now remain with their conversation while the user changes conversations or opens Operator.
- Workflow runs interrupted by a gateway restart now resume: startup recovery returns an orphaned worker thread from `processing` to `idle` so the recorded continuation can replay.
- Deleting a surface annotation now records its removed body and tags atomically, advances the surface timestamp, and rejects a concurrent second delete as not found.
- Open surfaces now refetch after their work-surface connection recovers, so updates missed while disconnected appear without a manual reload.
- Work-surface sockets now retain a surface update emitted while their WebSocket handshake is completing.
- One-shot self-wakes now retry after their active turn instead of being consumed as an overlap skip.
- Continued workflow workers now retain the skill selection made when they were created.
- Workflow terminal-budget audit entries now settle to failed after bounded completion-write retries, rather than remaining started.
- Carried workflows excluded for invalid metadata are now recorded in the local audit trail.
- Remote-device registration now refuses a Local target consistently, including pairing and recovery imports that have no local producer.
- Workflow-script updates now retain source-header descriptions, expose their whole delivery grant in configuration history, and return 404 for an absent script.
- Rejected tool approvals now render as failed calls in the conversation transcript.
- Immediate tool-approval rejections now settle reliably instead of timing out when they arrive as the prompt is shown.
- Gateway runtime discovery no longer reads a partially written metadata file during startup.
- Compaction audit records now count every source message replaced by a successful summary.
- Compaction preserves a native threshold breach when a later count is unavailable while retaining estimate fallback for providers without native counting.
- Context compaction now counts tokens with the selected completion model.
- Tool-schema guidance now describes every `allOf` branch as required together.
- Workflow metadata headers now accept valid unquoted keys that begin with `$`.
- Accepted sandbox-degradation approvals now resume attended turns so ordinary tool approval, execution, and assistant output can continue.
- Schema-constrained workflow agent calls now return `null` after exhausting validation attempts, while ordinary agent and turn failures still throw.
- Unattended turns now refuse a relative managed scratch root with an error naming how to configure a deliberate root, and record their resolved tool root in turn diagnostics.
- Terminal Chat Completions stream events now preserve reported token usage when it arrives alongside the finish reason.
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
- Primary actions in topic operations and schedule automation now use an AA-compliant light-theme treatment.
- Tool-call and connection-error failure states now use the shared error treatment instead of action orange; connection loss and reconnection keep their ordinary connectivity treatment.
- Inline-turn and approval errors now use the error-token wash in both themes.
- Workflow scripts now reject unrecognised `agent()` and `agentStart()` option keys before dispatch instead of silently dropping them.
- Reloading the web UI now returns to the open topic instead of the topic picker.
- Operator schedules now show their complete execution target from every topic.
- The topic picker now describes only the topic behaviour the product provides.
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
- Fresh-session schedules created from Settings now use the currently selected topic.
- Agent definition details now show the configured maximum output retries.
- Switching topics no longer carries an unavailable agent into a new conversation.
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

- The conversation composer now reflects newly saved provider and model settings without requiring a surface reload.
- Fresh-session schedules can now use the calling conversation's topic without requiring its UUID.
- Workflow runs now retain their reserved token allowance for final closing work, even when earlier agent calls are still running.
- Failed workflow runs now retain a durable handover identifying the failed call and the events delivered or still pending.
- Provider responses that complete without usable text or tool output are now treated as failures, allowing retries or failover instead of silently producing an empty result.
- Workflow wake gates now honour configured models on non-default providers.
- Failed conversation turns no longer leave the conversation blocked from starting another turn.
- Completed assistant replies now retain their turn identity, so their assembled context can be opened immediately from the conversation.
- Tools requiring approval are now available only when explicitly allowed, while allowed tools still pause for human approval before execution.
- Turns with an unknown forced variant now fail with a clear configuration error before any model request is sent.
- Conversation and topic-session creation now rejects undeclared request fields instead of silently ignoring them, preventing accidental creation in the wrong topic.
- Delegated and dispatched workers now remain limited to the tools and approvals granted by every agent in their dispatch chain, including after nested delegation and after permissions change while a run is paused.
- Workflow runs now replay consistently across gateway restarts and parked waits, without resurrecting runs cancelled concurrently.
