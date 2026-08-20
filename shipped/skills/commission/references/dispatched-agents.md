# Dispatched Work

Dispatch exists for one purpose: perspective unpolluted by this session's
context or this model's biases. The jobs stay in the room — what goes out is a
question, what comes back is evidence and viewpoints for the conversation. The
doctrine below governs all dispatch; the one move that runs in-context is
**verification**. The six cross-engine moves — the divergent survey, the blind
re-derivation, the draft review, the shedding-run grounding, the commitment
audit, and the
preservation passes — run on the
Ensemble runtime (the ensemble-workflow skill's launcher) and are documented in
[ensemble-workflows.md](ensemble-workflows.md).

## When to Dispatch

Dispatch is triggered, never scheduled. The trigger is importance: a part that
is core to what the thing *is*, or infrastructure the build genuinely hinges on.
Routine parts get the main agent's own judgement and no dispatch. Effort scales
with how load-bearing the item is, and there is no quota in either direction.

## Grounding Rules (embed in every dispatched prompt)

Whether the work goes to an in-context agent or a cross-engine Ensemble worker, the
prompt instructs it to:

- attach evidence to every claim, naming the source
- quote sources directly rather than paraphrasing them
- treat "I don't know" / "could not verify" as a valid, welcome finding
- separate observation from inference, and say which is which

These are the dispatched side of grounding; the main session's half — relaying
findings with their provenance and resolving the doubtful by checking — is the
relay discipline in `SKILL.md`. The Ensemble scripts carry these rules in
their worker prompts already; you embed them by hand only for the in-context
verification move below.

## Verify, Don't Transcribe (in-context)

Checkable claims get checked before they enter the README. This is
fact-checking, not divergent generation, so it stays lightweight and in the
session's own context — it does not need a cross-engine Ensemble run. Three
shapes of claim route differently; each shape's concrete action on this tool
is in the block below, never assumed from another tool's toolkit:

- a **single checkable fact** — the lightest check that settles it.
- a **broader *external* investigative question** — library capabilities, API
  behaviour, prior art, "does some existing thing already do this". This shape
  is outward-looking — what the world offers, not what our own code currently
  does — and its check carries the grounding rules.
- a **one-off fact about the user's own system or codebase** — a single "does
  our repo already do X?" raised in passing during capture, wanting a
  de-anchored read of the code. This is for the lone check; the shedding run's
  *sweep* of candidates against the build is not a one-off and does not come
  here — it runs as the cross-engine `grounding` workflow on the Ensemble
  runtime (see
  [shedding-workflow.md](shedding-workflow.md)).

A full multi-source research pass is overkill for capture-time verification;
keep this proportionate.

<!-- foundry:variant verification-dispatch start -->

On this target:

- a **single checkable fact** → an inline `WebSearch`.
- a **broader *external* investigative question** → a clean-context subagent
  dispatched with the `Agent`/`Task` tool, which carries the full session harness
  (web search, file reads); put the Grounding Rules in its prompt. It is properly
  de-anchored despite sharing this model's weights — the anchor lives in this
  session's context, not in the model. This tier is outward-looking (libraries,
  APIs, prior art), not "what does our own code do" — that is the next bullet.
- a **one-off fact about the user's own system or codebase** → invoke the
  `codebase-research` skill; it dispatches blind, read-only research agents by
  design — exactly the de-anchored perspective wanted here. Do not hand-roll your
  own investigation agents for this: the skill is where the blindness discipline
  and the cheap read-only agents live. (The shedding run's *sweep* of candidates
  against the build is not this one-off route — it runs as the `grounding`
  ensemble workflow.)

Treat "I don't know" / "could not verify" as a valid, welcome result from any of
these.

<!-- foundry:variant verification-dispatch end -->

## How Findings Come Back

A verification finding is evidence for the conversation, not a verdict to act on
unseen. It returns grounded — evidence attached, sources quoted, observation
separated from inference — and the main session keeps it grounded on the way
back per the relay discipline in `SKILL.md`. Convergence with the user's claim
is quiet confirmation; divergence is a finding either way — sometimes the check
shows the user wrong, sometimes it shows the README under-explains why the user
is right.
